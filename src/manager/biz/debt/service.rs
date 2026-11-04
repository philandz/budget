//! DebtBiz: authorization, validation, quotes and the payment saga (plan §4–§5).
//!
//! Roles (DB12): viewers read; Owner/Manager manage accounts, rates and rules; Contributor and up
//! record payments. A payment needs Contributor+ on BOTH the debt book and the paying Standard budget.

use std::sync::Arc;

use chrono::{Datelike, NaiveDate};
use tokio::sync::Mutex;
use tonic::Status;
use uuid::Uuid;

use super::schedule::{self as sch, DayCount, Method};
use crate::manager::biz::BudgetBiz;
use crate::manager::client::EntryClient;
use crate::manager::repository::debt::*;
use crate::pb::service::budget::{BudgetRole, BudgetType};
use crate::pb::service::debt as pb;
use crate::pb::service::entry::{CreateDebtPaymentLegsRequest, VoidDebtPaymentLegsRequest};

const SUPPORTED_CURRENCY: &str = "VND";
const PAYMENT_KINDS: [&str; 3] = ["scheduled", "prepayment", "settlement"];

fn internal<E: ToString>(e: E) -> Status {
    tracing::error!("debt: {}", e.to_string());
    Status::internal("internal error")
}

fn map_err(e: DebtRepoError) -> Status {
    match e {
        DebtRepoError::NotFound => Status::not_found("debt account not found"),
        DebtRepoError::NotActive => {
            Status::failed_precondition("the debt account is not in a state that allows this")
        }
        DebtRepoError::Insufficient {
            outstanding,
            requested,
        } => Status::failed_precondition(format!(
            "principal {requested} exceeds the outstanding balance {outstanding}"
        )),
        DebtRepoError::VersionConflict { current } => Status::aborted(format!(
            "the debt account was changed by someone else (version {current}); reload and retry"
        )),
        DebtRepoError::Duplicate => Status::already_exists("a rate for that date already exists"),
        DebtRepoError::PaymentGone => {
            Status::aborted("the payment was rolled back by reconciliation; please retry")
        }
        DebtRepoError::Db(e) => internal(e),
    }
}

fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

pub fn today_ict() -> NaiveDate {
    (chrono::Utc::now() + chrono::Duration::hours(7)).date_naive()
}

fn date_arg(field: &str, v: &str) -> Result<NaiveDate, Status> {
    sch::parse_date(v)
        .ok_or_else(|| Status::invalid_argument(format!("{field} must be YYYY-MM-DD")))
}

/// `fee_pct` is stored as DECIMAL(7,4): at most four decimals.
fn fee_pct_arg(v: &str) -> Result<String, Status> {
    let micro = sch::parse_pct(v)
        .filter(|m| m % 100 == 0 && *m <= 100 * 1_000_000)
        .ok_or_else(|| Status::invalid_argument("fee_pct must be 0-100 with at most 4 decimals"))?;
    Ok(sch::fmt_pct(micro))
}

fn rate_arg(v: &str) -> Result<(i64, String), Status> {
    let micro = sch::parse_pct(v)
        .filter(|m| *m <= 100 * 1_000_000)
        .ok_or_else(|| {
            Status::invalid_argument("annual_rate must be 0-100 percent (up to 6 decimals)")
        })?;
    Ok((micro, sch::fmt_pct(micro)))
}

fn rate_type_arg(v: &str) -> Result<String, Status> {
    match v {
        "" => Ok("fixed".into()),
        "fixed" | "floating" => Ok(v.into()),
        _ => Err(Status::invalid_argument(
            "rate_type must be fixed or floating",
        )),
    }
}

pub struct DebtBiz {
    pub repo: Arc<DebtRepository>,
    pub budget_biz: Arc<BudgetBiz>,
    pub entry_client: Option<Arc<Mutex<EntryClient>>>,
}

/// Everything the engine needs about one account.
struct Loaded {
    acc: DbDebtAccount,
    rates: Vec<sch::Rate>,
    rules: Vec<sch::PrepayRule>,
    db_rates: Vec<DbRatePeriod>,
    db_rules: Vec<DbPrepayRule>,
}

pub struct Quote {
    pub principal: i64,
    pub interest: i64,
    pub fee: i64,
    pub suggested_fee: i64,
    pub allowance_used: i64,
    pub allowance_remaining: i64,
    pub due_date: Option<NaiveDate>,
    pub interest_from: NaiveDate,
    pub interest_to: NaiveDate,
    pub days: i64,
    pub micro_pct: i64,
    pub estimated: bool,
}

impl DebtBiz {
    pub fn new(
        repo: DebtRepository,
        budget_biz: Arc<BudgetBiz>,
        entry_client: Option<EntryClient>,
    ) -> Self {
        Self {
            repo: Arc::new(repo),
            budget_biz,
            entry_client: entry_client.map(|c| Arc::new(Mutex::new(c))),
        }
    }

    // ------------------------------------------------------------------ authorization

    async fn role(
        &self,
        budget_id: &str,
        user_id: &str,
        user_type: Option<&str>,
    ) -> Result<BudgetRole, Status> {
        self.budget_biz
            .resolve_role(budget_id, user_id, user_type)
            .await
    }

    async fn assert_role(
        &self,
        budget_id: &str,
        user_id: &str,
        min: BudgetRole,
        user_type: Option<&str>,
    ) -> Result<(), Status> {
        let role = self.role(budget_id, user_id, user_type).await?;
        let ok = match min {
            BudgetRole::Viewer => role != BudgetRole::Unspecified,
            BudgetRole::Contributor => matches!(
                role,
                BudgetRole::Owner | BudgetRole::Manager | BudgetRole::Contributor
            ),
            BudgetRole::Manager => matches!(role, BudgetRole::Owner | BudgetRole::Manager),
            BudgetRole::Owner => role == BudgetRole::Owner,
            BudgetRole::Unspecified => true,
        };
        if !ok {
            return Err(Status::permission_denied(format!(
                "Requires {min:?} role or higher"
            )));
        }
        Ok(())
    }

    /// The debt budget must exist, be of type debt and be VND (DB14).
    async fn assert_debt_budget(&self, budget_id: &str) -> Result<(), Status> {
        let b = self
            .budget_biz
            .repo
            .get_budget_by_id(budget_id)
            .await
            .map_err(|_| Status::not_found("budget not found"))?;
        if crate::converters::budget_type_from_db(&b.budget_type) != BudgetType::Debt {
            return Err(Status::failed_precondition(
                "this budget is not a debt budget",
            ));
        }
        if !b.currency.eq_ignore_ascii_case(SUPPORTED_CURRENCY) {
            return Err(Status::failed_precondition("debt budgets support VND only"));
        }
        if b.status == "archived" {
            return Err(Status::failed_precondition("this budget is archived"));
        }
        Ok(())
    }

    async fn load(&self, budget_id: &str, account_id: &str) -> Result<Loaded, Status> {
        let acc = self
            .repo
            .get_account(account_id)
            .await
            .map_err(internal)?
            .filter(|a| a.budget_id == budget_id)
            .ok_or_else(|| Status::not_found("debt account not found"))?;
        let ids = vec![acc.id.clone()];
        let db_rates = self.repo.list_rates_for(&ids).await.map_err(internal)?;
        let db_rules = self.repo.list_rules_for(&ids).await.map_err(internal)?;
        Ok(Loaded {
            rates: to_rates(&db_rates),
            rules: to_rules(&db_rules),
            acc,
            db_rates,
            db_rules,
        })
    }

    // ------------------------------------------------------------------ engine glue

    fn inputs<'a>(&self, l: &'a Loaded, max_rows: usize) -> Option<sch::Inputs<'a>> {
        let a = &l.acc;
        let method = Method::parse(&a.repayment_method)?;
        let through = a
            .interest_through
            .as_deref()
            .and_then(sch::parse_date)
            .or_else(|| sch::parse_date(&a.tracking_start_date))?;
        Some(sch::Inputs {
            method,
            day_count: DayCount::parse(&a.day_count).unwrap_or(DayCount::Actual365),
            outstanding: a.outstanding_minor,
            interest_through: through,
            last_due: a.last_due_date.as_deref().and_then(sch::parse_date),
            due_day: a.due_day.map(|d| d as u32).unwrap_or(1),
            maturity: a.maturity_date.as_deref().and_then(sch::parse_date),
            rates: &l.rates,
            periodic_fee: a.periodic_fee_minor,
            fixed_installment: a.installment_minor.filter(|v| *v > 0),
            max_rows,
        })
    }

    fn project(&self, l: &Loaded, max_rows: usize) -> sch::Schedule {
        if l.acc.status != "active" {
            return sch::Schedule::default();
        }
        self.inputs(l, max_rows)
            .map(|i| sch::project(&i))
            .unwrap_or_default()
    }

    async fn quote(
        &self,
        l: &Loaded,
        kind: &str,
        paid_on: NaiveDate,
        prepay_amount: i64,
    ) -> Result<Quote, Status> {
        let a = &l.acc;
        let inp = self
            .inputs(l, sch::MAX_ROWS)
            .ok_or_else(|| Status::failed_precondition("the debt account has invalid terms"))?;
        let day_count = inp.day_count;
        let outstanding = a.outstanding_minor;
        let month = format!("{:04}-{:02}", paid_on.year(), paid_on.month());
        let prepaid = self
            .repo
            .prepaid_in_month(&a.id, &month)
            .await
            .map_err(internal)?;

        let mut q = Quote {
            principal: 0,
            interest: 0,
            fee: 0,
            suggested_fee: 0,
            allowance_used: prepaid,
            allowance_remaining: 0,
            due_date: None,
            interest_from: inp.interest_through,
            interest_to: paid_on,
            days: 0,
            micro_pct: sch::rate_on(&l.rates, paid_on)
                .map(|r| r.micro_pct)
                .unwrap_or(0),
            estimated: false,
        };
        match kind {
            "scheduled" => {
                let s = sch::project(&inp);
                if let Some(r) = s.rows.first() {
                    q.principal = r.principal;
                    q.interest = r.interest;
                    q.fee = r.fee;
                    q.due_date = Some(r.due_date);
                    q.interest_to = r.due_date;
                    q.days = (r.due_date - inp.interest_through).num_days().max(0);
                    q.micro_pct = r.micro_pct;
                    q.estimated = r.estimated;
                }
            }
            "prepayment" | "settlement" => {
                let principal = if kind == "settlement" {
                    outstanding
                } else {
                    prepay_amount
                };
                let acc = sch::accrue(
                    outstanding,
                    inp.interest_through,
                    paid_on,
                    &l.rates,
                    day_count,
                    30,
                );
                q.principal = principal;
                q.interest = acc.interest;
                q.days = acc.days;
                q.micro_pct = acc.micro_pct;
                q.estimated = acc.estimated;
                // A settlement at (or after) maturity is the normal last payment, not a prepayment.
                let at_maturity =
                    kind == "settlement" && inp.maturity.map(|m| paid_on >= m).unwrap_or(false);
                if principal > 0 && !at_maturity {
                    let fee = sch::prepay_fee(&l.rules, paid_on, prepaid, principal);
                    q.suggested_fee = fee.fee;
                    q.fee = fee.fee;
                    q.allowance_remaining = if fee.has_rule {
                        fee.allowance_remaining
                    } else {
                        0
                    };
                } else if let Some(rule) = sch::rule_on(&l.rules, paid_on) {
                    q.allowance_remaining = (rule.free_allowance_per_month - prepaid).max(0);
                }
            }
            _ => {
                return Err(Status::invalid_argument(
                    "kind must be scheduled, prepayment or settlement",
                ))
            }
        }
        if kind == "scheduled" {
            if let Some(rule) = sch::rule_on(&l.rules, paid_on) {
                q.allowance_remaining = (rule.free_allowance_per_month - prepaid).max(0);
            }
        }
        Ok(q)
    }

    // ------------------------------------------------------------------ pb conversion

    fn account_pb(&self, l: &Loaded, totals: PaymentTotals) -> pb::DebtAccount {
        let a = &l.acc;
        let schedule = self.project(l, sch::MAX_ROWS);
        let next_due = schedule.rows.first().map(|r| pb::NextDue {
            due_date: sch::fmt_date(r.due_date),
            principal_minor: r.principal,
            interest_minor: r.interest,
            fee_minor: r.fee,
            total_minor: r.total,
            estimated: r.estimated,
        });
        pb::DebtAccount {
            id: a.id.clone(),
            budget_id: a.budget_id.clone(),
            name: a.name.clone(),
            lender: a.lender.clone().unwrap_or_default(),
            kind: a.kind.clone(),
            repayment_method: a.repayment_method.clone(),
            currency: a.currency.clone(),
            opening_outstanding_minor: a.opening_outstanding_minor,
            outstanding_minor: a.outstanding_minor,
            tracking_start_date: a.tracking_start_date.clone(),
            maturity_date: a.maturity_date.clone().unwrap_or_default(),
            remaining_periods: a.remaining_periods.unwrap_or(0),
            due_day: a.due_day.map(|d| d as i32).unwrap_or(0),
            periodic_fee_minor: a.periodic_fee_minor,
            day_count: a.day_count.clone(),
            interest_category_id: a.interest_category_id.clone().unwrap_or_default(),
            pay_from_budget_id: a.pay_from_budget_id.clone().unwrap_or_default(),
            status: a.status.clone(),
            version: a.version,
            created_at: a.created_at,
            updated_at: a.updated_at,
            interest_through: a.interest_through.clone().unwrap_or_default(),
            last_due_date: a.last_due_date.clone().unwrap_or_default(),
            installment_minor: a.installment_minor.unwrap_or(0),
            rate_periods: l.db_rates.iter().map(rate_pb).collect(),
            prepay_rules: l.db_rules.iter().map(rule_pb).collect(),
            next_due,
            paid_principal_minor: totals.paid_principal,
            total_paid_minor: totals.total_paid(),
            total_cost_minor: totals.cost(),
            estimated_payoff_date: schedule.payoff_date.map(sch::fmt_date).unwrap_or_default(),
        }
    }

    async fn account_view(&self, l: &Loaded) -> Result<pb::DebtAccount, Status> {
        let totals = self
            .repo
            .totals_for_accounts(&[l.acc.id.clone()])
            .await
            .map_err(internal)?
            .remove(&l.acc.id)
            .unwrap_or_default();
        Ok(self.account_pb(l, totals))
    }

    // ------------------------------------------------------------------ accounts

    pub async fn create_account(
        &self,
        user_id: &str,
        req: pb::CreateDebtAccountRequest,
        user_type: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        self.assert_debt_budget(&req.budget_id).await?;

        let name = req.name.trim();
        if name.is_empty() || name.chars().count() > 120 {
            return Err(Status::invalid_argument(
                "name is required (max 120 characters)",
            ));
        }
        let lender = opt(&req.lender);
        if lender
            .as_deref()
            .map(|l| l.chars().count() > 120)
            .unwrap_or(false)
        {
            return Err(Status::invalid_argument("lender too long"));
        }
        let kind = {
            let k = req.kind.trim();
            let k = if k.is_empty() { "other" } else { k };
            if k.len() > 32
                || !k
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
            {
                return Err(Status::invalid_argument(
                    "kind must be a short lowercase label",
                ));
            }
            k.to_string()
        };
        let method = Method::parse(&req.repayment_method)
            .ok_or_else(|| Status::invalid_argument("unknown repayment_method"))?;
        let currency = if req.currency.trim().is_empty() {
            SUPPORTED_CURRENCY.to_string()
        } else {
            req.currency.trim().to_uppercase()
        };
        if currency != SUPPORTED_CURRENCY {
            return Err(Status::invalid_argument("only VND is supported"));
        }
        if req.opening_outstanding_minor <= 0 {
            return Err(Status::invalid_argument(
                "opening_outstanding_minor must be positive",
            ));
        }
        let start = date_arg("tracking_start_date", &req.tracking_start_date)?;
        if req.periodic_fee_minor < 0 {
            return Err(Status::invalid_argument(
                "periodic_fee_minor must not be negative",
            ));
        }
        let day_count = if req.day_count.is_empty() {
            "actual_365".to_string()
        } else {
            req.day_count.clone()
        };
        if DayCount::parse(&day_count).is_none() {
            return Err(Status::invalid_argument(
                "day_count must be actual_365 or monthly",
            ));
        }

        // Terms of scheduled methods.
        let (due_day, maturity, remaining) = if method.is_scheduled() {
            if !(1..=31).contains(&req.due_day) {
                return Err(Status::invalid_argument(
                    "due_day must be 1-31 (31 = last day of the month)",
                ));
            }
            let due_day = req.due_day as u32;
            let (maturity, remaining) = match (req.maturity_date.trim(), req.remaining_periods) {
                ("", n) if n > 0 => {
                    if n > 600 {
                        return Err(Status::invalid_argument("remaining_periods is too large"));
                    }
                    (sch::nth_due_after(start, due_day, n as u32), Some(n))
                }
                (m, 0) if !m.is_empty() => {
                    let m = date_arg("maturity_date", m)?;
                    if m <= start {
                        return Err(Status::invalid_argument(
                            "maturity_date must be after the tracking start",
                        ));
                    }
                    (m, None)
                }
                _ => {
                    return Err(Status::invalid_argument(
                        "give either maturity_date or remaining_periods for a scheduled method",
                    ))
                }
            };
            (Some(req.due_day), Some(sch::fmt_date(maturity)), remaining)
        } else {
            (None, opt(&req.maturity_date), None)
        };
        if let Some(m) = maturity.as_deref() {
            date_arg("maturity_date", m)?;
        }

        let mut rates = vec![];
        let mut seen = std::collections::HashSet::new();
        for r in &req.rate_periods {
            let from = date_arg("rate effective_from", &r.effective_from)?;
            let (_, text) = rate_arg(&r.annual_rate)?;
            if !seen.insert(from) {
                return Err(Status::invalid_argument("two rates start on the same date"));
            }
            rates.push(NewRate {
                effective_from: sch::fmt_date(from),
                annual_rate: text,
                rate_type: rate_type_arg(&r.rate_type)?,
                note: opt(&r.note),
            });
        }
        if rates.is_empty()
            && matches!(
                method,
                Method::Annuity | Method::EqualPrincipal | Method::InterestOnly
            )
        {
            return Err(Status::invalid_argument(
                "add the interest rate (at least one rate period)",
            ));
        }
        let rules = req
            .prepay_rules
            .iter()
            .map(new_rule)
            .collect::<Result<Vec<_>, _>>()?;

        if let Some(from) = opt(&req.pay_from_budget_id) {
            self.assert_role(&from, user_id, BudgetRole::Contributor, user_type)
                .await?;
        }

        let db = self
            .repo
            .create_account(&NewAccount {
                id: Uuid::new_v4().to_string(),
                budget_id: req.budget_id.clone(),
                name: name.to_string(),
                lender,
                kind,
                repayment_method: method.as_str().to_string(),
                currency,
                opening_outstanding_minor: req.opening_outstanding_minor,
                tracking_start_date: sch::fmt_date(start),
                maturity_date: maturity,
                remaining_periods: remaining,
                due_day,
                periodic_fee_minor: req.periodic_fee_minor,
                day_count,
                interest_category_id: opt(&req.interest_category_id),
                pay_from_budget_id: opt(&req.pay_from_budget_id),
                created_by: user_id.to_string(),
                rates,
                rules,
            })
            .await
            .map_err(map_err)?;
        let l = self.load(&req.budget_id, &db.id).await?;
        self.account_view(&l).await
    }

    pub async fn update_account(
        &self,
        user_id: &str,
        req: pb::UpdateDebtAccountRequest,
        user_type: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        if l.acc.status == "closed" {
            return Err(Status::failed_precondition(
                "reopen the account before editing it",
            ));
        }
        let mut patch = AccountPatch::default();
        let mut changed = serde_json::Map::new();
        if let Some(n) = req.name.as_deref() {
            let n = n.trim();
            if n.is_empty() || n.chars().count() > 120 {
                return Err(Status::invalid_argument(
                    "name is required (max 120 characters)",
                ));
            }
            changed.insert("name".into(), n.into());
            patch.name = Some(n.into());
        }
        if let Some(v) = req.lender.as_deref() {
            if v.chars().count() > 120 {
                return Err(Status::invalid_argument("lender too long"));
            }
            patch.lender = Some(v.trim().into());
            changed.insert("lender".into(), v.into());
        }
        if let Some(v) = req.kind.as_deref() {
            if v.is_empty() || v.len() > 32 {
                return Err(Status::invalid_argument(
                    "kind must be a short lowercase label",
                ));
            }
            patch.kind = Some(v.into());
            changed.insert("kind".into(), v.into());
        }
        if let Some(d) = req.due_day {
            if !(1..=31).contains(&d) {
                return Err(Status::invalid_argument("due_day must be 1-31"));
            }
            patch.due_day = Some(d);
            changed.insert("due_day".into(), d.into());
        }
        if let Some(f) = req.periodic_fee_minor {
            if f < 0 {
                return Err(Status::invalid_argument(
                    "periodic_fee_minor must not be negative",
                ));
            }
            patch.periodic_fee_minor = Some(f);
            changed.insert("periodic_fee_minor".into(), f.into());
        }
        if let Some(m) = req.maturity_date.as_deref() {
            let md = date_arg("maturity_date", m)?;
            let start = l.acc.interest_through.as_deref().and_then(sch::parse_date);
            if start.map(|s| md <= s).unwrap_or(false) {
                return Err(Status::invalid_argument(
                    "maturity_date must be after the date interest is settled to",
                ));
            }
            patch.maturity_date = Some(sch::fmt_date(md));
            changed.insert("maturity_date".into(), m.into());
        }
        if let Some(d) = req.day_count.as_deref() {
            if DayCount::parse(d).is_none() {
                return Err(Status::invalid_argument(
                    "day_count must be actual_365 or monthly",
                ));
            }
            patch.day_count = Some(d.into());
            changed.insert("day_count".into(), d.into());
        }
        if let Some(c) = req.interest_category_id.as_deref() {
            patch.interest_category_id = opt(c);
            changed.insert("interest_category_id".into(), c.into());
        }
        if let Some(b) = req.pay_from_budget_id.as_deref() {
            if let Some(b) = opt(b) {
                self.assert_role(&b, user_id, BudgetRole::Contributor, user_type)
                    .await?;
                patch.pay_from_budget_id = Some(b);
            }
            changed.insert("pay_from_budget_id".into(), b.into());
        }
        self.repo
            .update_account(
                &req.account_id,
                &patch,
                req.expected_version,
                user_id,
                &sch::fmt_date(today_ict()),
                serde_json::Value::Object(changed).to_string(),
            )
            .await
            .map_err(map_err)?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        self.account_view(&l).await
    }

    async fn change_status(
        &self,
        user_id: &str,
        r: pb::DebtAccountRef,
        user_type: Option<&str>,
        from: &[&str],
        to: Option<&str>,
        event: &str,
        require_zero: bool,
    ) -> Result<pb::DebtAccount, Status> {
        self.assert_role(&r.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let l = self.load(&r.budget_id, &r.account_id).await?;
        if require_zero && l.acc.outstanding_minor != 0 {
            return Err(Status::failed_precondition(
                "the outstanding balance must be zero (record the final payment or adjust it first)",
            ));
        }
        let to = match to {
            Some(t) => t.to_string(),
            // reopen: back to active unless nothing is owed
            None => {
                if l.acc.outstanding_minor > 0 {
                    "active".into()
                } else {
                    "settled".into()
                }
            }
        };
        self.repo
            .set_status(
                &r.account_id,
                from,
                &to,
                event,
                user_id,
                &sch::fmt_date(today_ict()),
            )
            .await
            .map_err(map_err)?;
        let l = self.load(&r.budget_id, &r.account_id).await?;
        self.account_view(&l).await
    }

    pub async fn close_account(
        &self,
        u: &str,
        r: pb::DebtAccountRef,
        t: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.change_status(
            u,
            r,
            t,
            &["active", "settled"],
            Some("closed"),
            "closed",
            false,
        )
        .await
    }

    pub async fn reopen_account(
        &self,
        u: &str,
        r: pb::DebtAccountRef,
        t: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.change_status(u, r, t, &["closed"], None, "reopened", false)
            .await
    }

    /// D7: settling is explicit, and only at zero.
    pub async fn settle_account(
        &self,
        u: &str,
        r: pb::DebtAccountRef,
        t: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.change_status(u, r, t, &["active"], Some("settled"), "settled", true)
            .await
    }

    pub async fn get_account(
        &self,
        user_id: &str,
        r: pb::DebtAccountRef,
        user_type: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.assert_role(&r.budget_id, user_id, BudgetRole::Viewer, user_type)
            .await?;
        let l = self.load(&r.budget_id, &r.account_id).await?;
        self.account_view(&l).await
    }

    pub async fn list_accounts(
        &self,
        user_id: &str,
        req: pb::ListDebtAccountsRequest,
        user_type: Option<&str>,
    ) -> Result<pb::ListDebtAccountsResponse, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Viewer, user_type)
            .await?;
        let accounts = self
            .hydrate_book(&req.budget_id, req.include_closed)
            .await?;
        Ok(pb::ListDebtAccountsResponse { accounts })
    }

    async fn hydrate_book(
        &self,
        budget_id: &str,
        include_closed: bool,
    ) -> Result<Vec<pb::DebtAccount>, Status> {
        let accs = self
            .repo
            .list_accounts(budget_id, include_closed)
            .await
            .map_err(internal)?;
        let ids: Vec<String> = accs.iter().map(|a| a.id.clone()).collect();
        let rates = self.repo.list_rates_for(&ids).await.map_err(internal)?;
        let rules = self.repo.list_rules_for(&ids).await.map_err(internal)?;
        let totals = self
            .repo
            .totals_for_accounts(&ids)
            .await
            .map_err(internal)?;
        let mut out = vec![];
        for acc in accs {
            let db_rates: Vec<_> = rates
                .iter()
                .filter(|r| r.account_id == acc.id)
                .cloned()
                .collect();
            let db_rules: Vec<_> = rules
                .iter()
                .filter(|r| r.account_id == acc.id)
                .cloned()
                .collect();
            let t = totals.get(&acc.id).copied().unwrap_or_default();
            let l = Loaded {
                rates: to_rates(&db_rates),
                rules: to_rules(&db_rules),
                acc,
                db_rates,
                db_rules,
            };
            out.push(self.account_pb(&l, t));
        }
        Ok(out)
    }

    // ------------------------------------------------------------------ rates & rules

    pub async fn set_rate(
        &self,
        user_id: &str,
        req: pb::SetRatePeriodRequest,
        user_type: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        if l.acc.status == "closed" {
            return Err(Status::failed_precondition(
                "reopen the account before editing it",
            ));
        }
        let r = req
            .rate
            .ok_or_else(|| Status::invalid_argument("rate is required"))?;
        let from = date_arg("effective_from", &r.effective_from)?;
        let (_, text) = rate_arg(&r.annual_rate)?;
        let rate_id = opt(&r.id);
        self.repo
            .set_rate(
                &l.acc,
                rate_id.as_deref(),
                &NewRate {
                    effective_from: sch::fmt_date(from),
                    annual_rate: text,
                    rate_type: rate_type_arg(&r.rate_type)?,
                    note: opt(&r.note),
                },
                user_id,
                &sch::fmt_date(today_ict()),
            )
            .await
            .map_err(map_err)?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        self.account_view(&l).await
    }

    pub async fn delete_rate(
        &self,
        user_id: &str,
        req: pb::DeleteRatePeriodRequest,
        user_type: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        let needs_rate = matches!(
            Method::parse(&l.acc.repayment_method),
            Some(Method::Annuity | Method::EqualPrincipal | Method::InterestOnly)
        );
        if needs_rate && l.db_rates.len() <= 1 {
            return Err(Status::failed_precondition(
                "a scheduled loan needs at least one rate period",
            ));
        }
        self.repo
            .delete_rate(&l.acc, &req.rate_id, user_id, &sch::fmt_date(today_ict()))
            .await
            .map_err(map_err)?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        self.account_view(&l).await
    }

    pub async fn set_prepay_rule(
        &self,
        user_id: &str,
        req: pb::SetPrepayRuleRequest,
        user_type: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        let r = req
            .rule
            .ok_or_else(|| Status::invalid_argument("rule is required"))?;
        let rule = new_rule(&r)?;
        self.repo
            .set_rule(
                &l.acc,
                opt(&r.id).as_deref(),
                &rule,
                user_id,
                &sch::fmt_date(today_ict()),
            )
            .await
            .map_err(map_err)?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        self.account_view(&l).await
    }

    pub async fn delete_prepay_rule(
        &self,
        user_id: &str,
        req: pb::DeletePrepayRuleRequest,
        user_type: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        self.repo
            .delete_rule(&l.acc, &req.rule_id, user_id, &sch::fmt_date(today_ict()))
            .await
            .map_err(map_err)?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        self.account_view(&l).await
    }

    // ------------------------------------------------------------------ schedule & quote

    pub async fn get_schedule(
        &self,
        user_id: &str,
        req: pb::GetScheduleRequest,
        user_type: Option<&str>,
    ) -> Result<pb::GetScheduleResponse, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Viewer, user_type)
            .await?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        let max = if req.max_rows > 0 {
            (req.max_rows as usize).min(sch::MAX_ROWS)
        } else {
            sch::MAX_ROWS
        };
        let s = self.project(&l, max);
        Ok(pb::GetScheduleResponse {
            payoff_date: s.payoff_date.map(sch::fmt_date).unwrap_or_default(),
            total_interest_minor: s.total_interest,
            estimated: s.estimated,
            rows: s
                .rows
                .iter()
                .map(|r| pb::ScheduleRow {
                    seq: r.seq,
                    due_date: sch::fmt_date(r.due_date),
                    principal_minor: r.principal,
                    interest_minor: r.interest,
                    fee_minor: r.fee,
                    total_minor: r.total,
                    balance_after_minor: r.balance_after,
                    annual_rate: sch::fmt_pct(r.micro_pct),
                    estimated: r.estimated,
                })
                .collect(),
        })
    }

    pub async fn quote_payment(
        &self,
        user_id: &str,
        req: pb::QuotePaymentRequest,
        user_type: Option<&str>,
    ) -> Result<pb::QuotePaymentResponse, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Viewer, user_type)
            .await?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        if l.acc.status != "active" {
            return Err(Status::failed_precondition(
                "the debt account is not active",
            ));
        }
        let paid_on = if req.paid_on.is_empty() {
            today_ict()
        } else {
            date_arg("paid_on", &req.paid_on)?
        };
        if req.prepay_amount_minor < 0 || req.prepay_amount_minor > l.acc.outstanding_minor {
            return Err(Status::invalid_argument(
                "prepay_amount_minor must be between 0 and the outstanding balance",
            ));
        }
        let q = self
            .quote(&l, &req.kind, paid_on, req.prepay_amount_minor)
            .await?;
        Ok(pb::QuotePaymentResponse {
            principal_minor: q.principal,
            interest_minor: q.interest,
            fee_minor: q.fee,
            total_minor: q.principal + q.interest + q.fee,
            suggested_fee_minor: q.suggested_fee,
            allowance_used_minor: q.allowance_used,
            allowance_remaining_minor: q.allowance_remaining,
            due_date: q.due_date.map(sch::fmt_date).unwrap_or_default(),
            interest_from: sch::fmt_date(q.interest_from),
            interest_to: sch::fmt_date(q.interest_to),
            days: q.days as i32,
            annual_rate: sch::fmt_pct(q.micro_pct),
            outstanding_after_minor: l.acc.outstanding_minor - q.principal,
            estimated: q.estimated,
        })
    }

    // ------------------------------------------------------------------ payments (saga)

    fn entry(&self) -> Result<&Arc<Mutex<EntryClient>>, Status> {
        self.entry_client
            .as_ref()
            .ok_or_else(|| Status::unavailable("entry service client not configured"))
    }

    pub async fn record_payment(
        &self,
        user_id: &str,
        req: pb::RecordPaymentRequest,
        user_type: Option<&str>,
    ) -> Result<pb::RecordPaymentResponse, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Contributor, user_type)
            .await?;
        if !PAYMENT_KINDS.contains(&req.kind.as_str()) {
            return Err(Status::invalid_argument(
                "kind must be scheduled, prepayment or settlement",
            ));
        }
        if req.idempotency_key.trim().is_empty() || req.idempotency_key.len() > 60 {
            return Err(Status::invalid_argument(
                "idempotency_key is required (max 60 characters)",
            ));
        }
        if req.principal_minor < 0 || req.interest_minor < 0 || req.fee_minor < 0 {
            return Err(Status::invalid_argument("amounts must not be negative"));
        }
        if req.note.chars().count() > 500 {
            return Err(Status::invalid_argument("note too long"));
        }
        let paid_on = date_arg("paid_on", &req.paid_on)?;
        if req.from_budget_id.is_empty() || req.from_budget_id == req.budget_id {
            return Err(Status::invalid_argument(
                "from_budget_id (a Standard budget) is required",
            ));
        }
        self.assert_role(
            &req.from_budget_id,
            user_id,
            BudgetRole::Contributor,
            user_type,
        )
        .await?;

        let l = self.load(&req.budget_id, &req.account_id).await?;

        // D4: a replay is answered from the stored payment, before anything is recomputed.
        if let Some(existing) = self
            .repo
            .find_payment_by_key(&req.account_id, &req.idempotency_key)
            .await
            .map_err(internal)?
        {
            return self.replay(&l, existing, &req).await;
        }

        if l.acc.status != "active" {
            return Err(Status::failed_precondition(
                "the debt account is not active; reopen it to record a payment",
            ));
        }
        if paid_on < date_arg("tracking_start_date", &l.acc.tracking_start_date)? {
            return Err(Status::invalid_argument(
                "paid_on is before the tracking start date",
            ));
        }
        let outstanding = l.acc.outstanding_minor;
        match req.kind.as_str() {
            "prepayment" => {
                if req.principal_minor <= 0 {
                    return Err(Status::invalid_argument(
                        "a prepayment needs a principal amount",
                    ));
                }
                if !matches!(
                    req.prepay_effect.as_str(),
                    "reduce_term" | "reduce_installment"
                ) {
                    return Err(Status::invalid_argument(
                        "prepay_effect must be reduce_term or reduce_installment",
                    ));
                }
            }
            "settlement" => {
                if req.principal_minor != outstanding {
                    return Err(Status::failed_precondition(
                        "a settlement pays the whole outstanding balance",
                    ));
                }
            }
            _ => {}
        }
        if req.principal_minor > outstanding {
            return Err(Status::failed_precondition(format!(
                "principal {} exceeds the outstanding balance {outstanding}",
                req.principal_minor
            )));
        }

        // D8: the stored fee is the rules' suggestion unless the user overrides it.
        let (fee, suggested) = if req.kind == "scheduled" {
            (req.fee_minor, None)
        } else {
            let q = self
                .quote(&l, &req.kind, paid_on, req.principal_minor)
                .await?;
            let fee = if req.fee_overridden {
                req.fee_minor
            } else {
                q.suggested_fee
            };
            (fee, Some(q.suggested_fee))
        };
        // D2
        let total = req.principal_minor + req.interest_minor + fee;
        if total <= 0 {
            return Err(Status::invalid_argument("a payment needs a positive total"));
        }

        // Due date of the instalment being settled.
        let due_date = if req.kind == "scheduled" {
            let d = if req.due_date.is_empty() {
                self.quote(&l, "scheduled", paid_on, 0)
                    .await?
                    .due_date
                    .map(sch::fmt_date)
            } else {
                Some(sch::fmt_date(date_arg("due_date", &req.due_date)?))
            };
            d
        } else {
            None
        };

        // Category for interest + fee.
        let cost = req.interest_minor + fee;
        let category = opt(&req.category_id).or_else(|| l.acc.interest_category_id.clone());
        if cost > 0 && category.is_none() {
            return Err(Status::invalid_argument(
                "category_id is required: choose the Standard category for interest and fees",
            ));
        }

        // Schedule effect, decided now from the pre-payment state.
        let effect = self
            .effect_of(&l, &req, paid_on, due_date.as_deref())
            .await?;

        let id = Uuid::new_v4().to_string();
        let begun = self
            .repo
            .begin_payment(&NewPayment {
                id: id.clone(),
                account_id: req.account_id.clone(),
                budget_id: req.budget_id.clone(),
                paid_on: sch::fmt_date(paid_on),
                kind: req.kind.clone(),
                principal_minor: req.principal_minor,
                interest_minor: req.interest_minor,
                fee_minor: fee,
                prepay_effect: if req.kind == "prepayment" {
                    Some(req.prepay_effect.clone())
                } else {
                    None
                },
                suggested_fee_minor: suggested,
                from_budget_id: req.from_budget_id.clone(),
                category_id: category.clone(),
                due_date,
                idempotency_key: req.idempotency_key.clone(),
                actor_user_id: user_id.to_string(),
                note: opt(&req.note),
            })
            .await
            .map_err(map_err)?;
        let payment = match begun {
            BeginPayment::Replay(p) => return self.replay(&l, p, &req).await,
            BeginPayment::New(p) => p,
        };

        if fee != suggested.unwrap_or(fee) {
            tracing::info!(
                "debt prepayment fee overridden payment_id={} suggested={:?} stored={}",
                payment.id,
                suggested,
                fee
            );
        }

        // 2. Standard side: one transaction in entry.
        let legs = CreateDebtPaymentLegsRequest {
            group_id: payment.id.clone(),
            from_budget_id: req.from_budget_id.clone(),
            debt_budget_id: req.budget_id.clone(),
            principal_minor: req.principal_minor,
            interest_fee_minor: cost,
            category_id: category.clone().unwrap_or_default(),
            entry_date: sch::fmt_date(paid_on),
            description: format!("Trả nợ {}", l.acc.name),
            actor_user_id: user_id.to_string(),
        };
        let sent = {
            let mut c = self.entry()?.lock().await;
            c.create_debt_payment_legs(user_id, user_type, legs).await
        };
        if let Err(e) = sent {
            let _ = self
                .repo
                .set_payment_status(&payment.id, &["requested"], "failed")
                .await;
            return Err(e);
        }

        // 3. Debt side.
        match self.repo.apply_payment(&payment.id, &effect).await {
            Ok(acc) => {
                let done = self
                    .repo
                    .get_payment(&payment.id)
                    .await
                    .map_err(internal)?
                    .unwrap_or(payment);
                Ok(pb::RecordPaymentResponse {
                    payment: Some(payment_pb(&done)),
                    outstanding_minor: acc.outstanding_minor,
                    suggest_settle: acc.outstanding_minor == 0 && acc.status == "active",
                })
            }
            Err(e) => {
                // D5: undo the Standard legs; only then is the payment failed. If the void fails
                // the payment stays `requested` and the reconcile sweep retries.
                match self
                    .void_legs(user_id, user_type, &payment.id, &req.from_budget_id)
                    .await
                {
                    Ok(()) => {
                        let _ = self
                            .repo
                            .set_payment_status(
                                &payment.id,
                                &["requested", "reconciling"],
                                "failed",
                            )
                            .await;
                    }
                    Err(ce) => tracing::error!(
                        "debt payment compensation failed payment_id={} err={ce}",
                        payment.id
                    ),
                }
                Err(map_err(e))
            }
        }
    }

    async fn replay(
        &self,
        l: &Loaded,
        existing: DbDebtPayment,
        req: &pb::RecordPaymentRequest,
    ) -> Result<pb::RecordPaymentResponse, Status> {
        if existing.kind != req.kind
            || existing.paid_on != req.paid_on
            || existing.principal_minor != req.principal_minor
            || existing.interest_minor != req.interest_minor
        {
            return Err(Status::failed_precondition(
                "idempotency_key was reused with a different request",
            ));
        }
        match existing.status.as_str() {
            "completed" => {
                let acc = self
                    .repo
                    .get_account(&l.acc.id)
                    .await
                    .map_err(internal)?
                    .unwrap_or_else(|| l.acc.clone());
                Ok(pb::RecordPaymentResponse {
                    payment: Some(payment_pb(&existing)),
                    outstanding_minor: acc.outstanding_minor,
                    suggest_settle: acc.outstanding_minor == 0 && acc.status == "active",
                })
            }
            "failed" => Err(Status::failed_precondition(
                "the earlier attempt with this idempotency_key failed; retry with a new key",
            )),
            _ => Err(Status::aborted("this payment is still being processed")),
        }
    }

    async fn effect_of(
        &self,
        l: &Loaded,
        req: &pb::RecordPaymentRequest,
        paid_on: NaiveDate,
        due_date: Option<&str>,
    ) -> Result<ApplyEffect, Status> {
        let through = l.acc.interest_through.as_deref().and_then(sch::parse_date);
        Ok(match req.kind.as_str() {
            "scheduled" => ApplyEffect {
                interest_through: Some(
                    due_date
                        .map(String::from)
                        .unwrap_or_else(|| sch::fmt_date(paid_on)),
                ),
                last_due_date: due_date.map(String::from),
                installment_minor: None,
                mark_settled: false,
            },
            "prepayment" => {
                let installment = if req.prepay_effect == "reduce_term" {
                    // keep today's instalment; an instalment fixed earlier stays
                    match l.acc.installment_minor.filter(|v| *v > 0) {
                        Some(v) => Some(Some(v)),
                        None => {
                            let s = self.project(l, 2);
                            Some(
                                Method::parse(&l.acc.repayment_method)
                                    .and_then(|m| sch::current_installment(m, &s.rows)),
                            )
                        }
                    }
                } else {
                    Some(None)
                };
                ApplyEffect {
                    interest_through: (req.interest_minor > 0
                        && through.map(|t| paid_on > t).unwrap_or(true))
                    .then(|| sch::fmt_date(paid_on)),
                    last_due_date: None,
                    installment_minor: installment,
                    mark_settled: false,
                }
            }
            _ => ApplyEffect {
                interest_through: Some(sch::fmt_date(paid_on)),
                last_due_date: None,
                installment_minor: Some(None),
                mark_settled: true,
            },
        })
    }

    async fn void_legs(
        &self,
        user_id: &str,
        user_type: Option<&str>,
        group_id: &str,
        from_budget_id: &str,
    ) -> Result<(), Status> {
        let mut c = self.entry()?.lock().await;
        c.void_debt_payment_legs(
            user_id,
            user_type,
            VoidDebtPaymentLegsRequest {
                group_id: group_id.to_string(),
                from_budget_id: from_budget_id.to_string(),
            },
        )
        .await
        .map(|_| ())
    }

    /// Rolls back payments left half-done by a crash (older than `older_than_secs`). Returns how
    /// many were rolled back. Mirrors `reconcile_stale_fund_transfers`.
    pub async fn reconcile_stale_payments(&self, older_than_secs: i64) -> Result<usize, Status> {
        let stale = self
            .repo
            .stale_payments(older_than_secs)
            .await
            .map_err(internal)?;
        let mut n = 0;
        for p in stale {
            if !self.repo.claim_payment(&p.id).await.map_err(internal)? {
                continue;
            }
            let from = p.from_budget_id.clone().unwrap_or_default();
            match self.void_legs(&p.actor_user_id, None, &p.id, &from).await {
                Ok(()) => {
                    let _ = self
                        .repo
                        .set_payment_status(&p.id, &["reconciling"], "failed")
                        .await;
                    n += 1;
                }
                Err(e) => tracing::error!(
                    "debt reconcile could not void legs payment_id={} err={e}",
                    p.id
                ),
            }
        }
        Ok(n)
    }

    // ------------------------------------------------------------------ adjust, lists, summary

    pub async fn adjust_outstanding(
        &self,
        user_id: &str,
        req: pb::AdjustOutstandingRequest,
        user_type: Option<&str>,
    ) -> Result<pb::DebtAccount, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        if req.new_outstanding_minor < 0 {
            return Err(Status::invalid_argument(
                "new_outstanding_minor must not be negative",
            ));
        }
        let reason = req.reason.trim();
        if reason.is_empty() || reason.chars().count() > 255 {
            return Err(Status::invalid_argument(
                "reason is required (max 255 characters)",
            ));
        }
        let on = if req.occurred_on.is_empty() {
            sch::fmt_date(today_ict())
        } else {
            sch::fmt_date(date_arg("occurred_on", &req.occurred_on)?)
        };
        if l.acc.status == "closed" {
            return Err(Status::failed_precondition(
                "reopen the account before adjusting it",
            ));
        }
        self.repo
            .adjust_outstanding(
                &req.account_id,
                req.new_outstanding_minor,
                reason,
                user_id,
                &on,
            )
            .await
            .map_err(map_err)?;
        let l = self.load(&req.budget_id, &req.account_id).await?;
        self.account_view(&l).await
    }

    fn page(page: i32, page_size: i32) -> (i64, i64) {
        let size = if page_size <= 0 {
            20
        } else {
            page_size.min(100)
        } as i64;
        let p = page.max(0) as i64;
        (size, p * size)
    }

    pub async fn list_payments(
        &self,
        user_id: &str,
        req: pb::ListDebtPaymentsRequest,
        user_type: Option<&str>,
    ) -> Result<pb::ListDebtPaymentsResponse, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Viewer, user_type)
            .await?;
        let (limit, offset) = Self::page(req.page, req.page_size);
        let (rows, total) = self
            .repo
            .list_payments(
                &req.budget_id,
                opt(&req.account_id).as_deref(),
                limit,
                offset,
            )
            .await
            .map_err(internal)?;
        Ok(pb::ListDebtPaymentsResponse {
            payments: rows.iter().map(payment_pb).collect(),
            total_rows: total,
        })
    }

    pub async fn list_events(
        &self,
        user_id: &str,
        req: pb::ListDebtEventsRequest,
        user_type: Option<&str>,
    ) -> Result<pb::ListDebtEventsResponse, Status> {
        self.assert_role(&req.budget_id, user_id, BudgetRole::Viewer, user_type)
            .await?;
        let (limit, offset) = Self::page(req.page, req.page_size);
        let (rows, total) = self
            .repo
            .list_events(
                &req.budget_id,
                opt(&req.account_id).as_deref(),
                limit,
                offset,
            )
            .await
            .map_err(internal)?;
        Ok(pb::ListDebtEventsResponse {
            events: rows
                .into_iter()
                .map(|e| pb::DebtEvent {
                    id: e.id,
                    account_id: e.account_id,
                    kind: e.kind,
                    amount_minor: e.amount_minor.unwrap_or(0),
                    payload: e.payload.unwrap_or_default(),
                    actor_user_id: e.actor_user_id,
                    occurred_on: e.occurred_on,
                    created_at: e.created_at,
                })
                .collect(),
            total_rows: total,
        })
    }

    pub async fn book_summary(
        &self,
        user_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<pb::DebtBookSummary, Status> {
        self.assert_role(budget_id, user_id, BudgetRole::Viewer, user_type)
            .await?;
        let accounts = self.hydrate_book(budget_id, true).await?;
        let today = today_ict();
        let month_end = {
            let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
            let next = first
                .checked_add_months(chrono::Months::new(1))
                .unwrap_or(first);
            next.pred_opt().unwrap_or(today)
        };
        let mut s = pb::DebtBookSummary {
            budget_id: budget_id.to_string(),
            currency: SUPPORTED_CURRENCY.into(),
            ..Default::default()
        };
        let mut next: Option<(NaiveDate, i64, String)> = None;
        for a in &accounts {
            s.total_paid_minor += a.total_paid_minor;
            s.total_cost_minor += a.total_cost_minor;
            match a.status.as_str() {
                "active" => {
                    s.active_accounts += 1;
                    s.total_outstanding_minor += a.outstanding_minor;
                    if let Some(n) = a.next_due.as_ref() {
                        if let Some(d) = sch::parse_date(&n.due_date) {
                            if d <= month_end {
                                s.due_this_month_minor += n.total_minor;
                            }
                            if next.as_ref().map(|(nd, _, _)| d < *nd).unwrap_or(true) {
                                next = Some((d, n.total_minor, a.id.clone()));
                            }
                        }
                    }
                }
                "settled" => s.settled_accounts += 1,
                _ => {}
            }
        }
        if let Some((d, total, id)) = next {
            s.next_due_date = sch::fmt_date(d);
            s.next_due_minor = total;
            s.next_due_account_id = id;
        }
        Ok(s)
    }
}

// ---------------------------------------------------------------------- conversions

fn to_rates(db: &[DbRatePeriod]) -> Vec<sch::Rate> {
    db.iter()
        .filter_map(|r| {
            Some(sch::Rate {
                from: sch::parse_date(&r.effective_from)?,
                micro_pct: sch::parse_pct(&r.annual_rate).unwrap_or(0),
                floating: r.rate_type == "floating",
            })
        })
        .collect()
}

fn to_rules(db: &[DbPrepayRule]) -> Vec<sch::PrepayRule> {
    db.iter()
        .filter_map(|r| {
            Some(sch::PrepayRule {
                valid_from: sch::parse_date(&r.valid_from)?,
                valid_to: r.valid_to.as_deref().and_then(sch::parse_date),
                free_allowance_per_month: r.free_allowance_per_month_minor,
                fee_pct_1e4: sch::parse_pct(&r.fee_pct).map(|m| m / 100).unwrap_or(0),
                min_fee: r.min_fee_minor,
            })
        })
        .collect()
}

fn rate_pb(r: &DbRatePeriod) -> pb::RatePeriod {
    pb::RatePeriod {
        id: r.id.clone(),
        effective_from: r.effective_from.clone(),
        annual_rate: sch::parse_pct(&r.annual_rate)
            .map(sch::fmt_pct)
            .unwrap_or_else(|| r.annual_rate.clone()),
        rate_type: r.rate_type.clone(),
        note: r.note.clone().unwrap_or_default(),
    }
}

fn rule_pb(r: &DbPrepayRule) -> pb::PrepayRule {
    pb::PrepayRule {
        id: r.id.clone(),
        valid_from: r.valid_from.clone(),
        valid_to: r.valid_to.clone().unwrap_or_default(),
        free_allowance_per_month_minor: r.free_allowance_per_month_minor,
        fee_pct: sch::parse_pct(&r.fee_pct)
            .map(sch::fmt_pct)
            .unwrap_or_else(|| r.fee_pct.clone()),
        min_fee_minor: r.min_fee_minor,
        note: r.note.clone().unwrap_or_default(),
    }
}

fn new_rule(r: &pb::PrepayRule) -> Result<NewRule, Status> {
    let from = date_arg("valid_from", &r.valid_from)?;
    let to = if r.valid_to.is_empty() {
        None
    } else {
        let t = date_arg("valid_to", &r.valid_to)?;
        if t < from {
            return Err(Status::invalid_argument("valid_to is before valid_from"));
        }
        Some(sch::fmt_date(t))
    };
    if r.free_allowance_per_month_minor < 0 || r.min_fee_minor < 0 {
        return Err(Status::invalid_argument(
            "allowance and minimum fee must not be negative",
        ));
    }
    Ok(NewRule {
        valid_from: sch::fmt_date(from),
        valid_to: to,
        free_allowance_per_month_minor: r.free_allowance_per_month_minor,
        fee_pct: fee_pct_arg(if r.fee_pct.is_empty() {
            "0"
        } else {
            &r.fee_pct
        })?,
        min_fee_minor: r.min_fee_minor,
        note: opt(&r.note),
    })
}

fn payment_pb(p: &DbDebtPayment) -> pb::DebtPayment {
    pb::DebtPayment {
        id: p.id.clone(),
        account_id: p.account_id.clone(),
        budget_id: p.budget_id.clone(),
        paid_on: p.paid_on.clone(),
        kind: p.kind.clone(),
        principal_minor: p.principal_minor,
        interest_minor: p.interest_minor,
        fee_minor: p.fee_minor,
        total_minor: p.total_minor,
        prepay_effect: p.prepay_effect.clone().unwrap_or_default(),
        suggested_fee_minor: p.suggested_fee_minor.unwrap_or(0),
        from_budget_id: p.from_budget_id.clone().unwrap_or_default(),
        entry_group_id: p.entry_group_id.clone().unwrap_or_default(),
        status: p.status.clone(),
        note: p.note.clone().unwrap_or_default(),
        due_date: p.due_date.clone().unwrap_or_default(),
        created_at: p.created_at,
        actor_user_id: p.actor_user_id.clone(),
    }
}
