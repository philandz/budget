//! Debt book persistence (plan §3).
//!
//! Invariants enforced here:
//! * D1 — `outstanding_minor` never goes negative: every write locks the account row
//!   (`SELECT … FOR UPDATE`) and the principal UPDATE is guarded by `outstanding_minor >= ?`.
//! * D3 — `outstanding` is changed only together with a completed payment or an adjustment event.
//! * D4 — `(account_id, idempotency_key)` is unique; a replay returns the stored payment.

use sqlx::{MySql, MySqlPool, Transaction};

use philand_time::now_unix;

#[derive(Debug, thiserror::Error)]
pub enum DebtRepoError {
    #[error("not found")]
    NotFound,
    #[error("account is not active")]
    NotActive,
    #[error("principal {requested} exceeds the outstanding {outstanding}")]
    Insufficient { outstanding: i64, requested: i64 },
    #[error("the account was changed by someone else (version {current})")]
    VersionConflict { current: i64 },
    #[error("a rate or rule for that date already exists")]
    Duplicate,
    #[error("the payment is no longer waiting to be applied")]
    PaymentGone,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Optional filters for `list_payments`; empty means "no filter".
#[derive(Debug, Default, Clone)]
pub struct PaymentFilter {
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub kind: Option<String>,
}

/// SQL fragment (starting with " AND ...") and its binds, in order. Values are only ever bound.
pub fn payment_conditions(account_id: Option<&str>, f: &PaymentFilter) -> (String, Vec<String>) {
    let mut cond = String::new();
    let mut binds = Vec::new();
    if let Some(a) = account_id {
        cond.push_str(" AND account_id = ?");
        binds.push(a.to_string());
    }
    if let Some(d) = f.date_from.as_deref() {
        cond.push_str(" AND paid_on >= ?");
        binds.push(d.to_string());
    }
    if let Some(d) = f.date_to.as_deref() {
        cond.push_str(" AND paid_on <= ?");
        binds.push(d.to_string());
    }
    if let Some(k) = f.kind.as_deref() {
        cond.push_str(" AND kind = ?");
        binds.push(k.to_string());
    }
    (cond, binds)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbDebtAccount {
    pub id: String,
    pub budget_id: String,
    pub name: String,
    pub lender: Option<String>,
    pub kind: String,
    pub repayment_method: String,
    pub currency: String,
    pub opening_outstanding_minor: i64,
    pub outstanding_minor: i64,
    pub tracking_start_date: String,
    pub maturity_date: Option<String>,
    pub remaining_periods: Option<i32>,
    pub due_day: Option<i8>,
    pub periodic_fee_minor: i64,
    pub day_count: String,
    pub interest_category_id: Option<String>,
    pub pay_from_budget_id: Option<String>,
    pub interest_through: Option<String>,
    pub last_due_date: Option<String>,
    pub installment_minor: Option<i64>,
    pub status: String,
    pub version: i64,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

const ACCOUNT_COLS: &str = "id, budget_id, name, lender, kind, repayment_method, currency,
    opening_outstanding_minor, outstanding_minor, tracking_start_date, maturity_date, remaining_periods,
    due_day, periodic_fee_minor, day_count, interest_category_id, pay_from_budget_id, interest_through,
    last_due_date, installment_minor, status, version, created_by, created_at, updated_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbRatePeriod {
    pub id: String,
    pub account_id: String,
    pub effective_from: String,
    pub annual_rate: String,
    pub rate_type: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbPrepayRule {
    pub id: String,
    pub account_id: String,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub free_allowance_per_month_minor: i64,
    pub fee_pct: String,
    pub min_fee_minor: i64,
    pub note: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbDebtPayment {
    pub id: String,
    pub account_id: String,
    pub budget_id: String,
    pub paid_on: String,
    pub kind: String,
    pub principal_minor: i64,
    pub interest_minor: i64,
    pub fee_minor: i64,
    pub total_minor: i64,
    pub prepay_effect: Option<String>,
    pub suggested_fee_minor: Option<i64>,
    pub from_budget_id: Option<String>,
    pub category_id: Option<String>,
    pub entry_group_id: Option<String>,
    pub due_date: Option<String>,
    pub status: String,
    pub idempotency_key: String,
    pub actor_user_id: String,
    pub note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

const PAYMENT_COLS: &str =
    "id, account_id, budget_id, paid_on, kind, principal_minor, interest_minor,
    fee_minor, total_minor, prepay_effect, suggested_fee_minor, from_budget_id, category_id,
    entry_group_id, due_date, status, idempotency_key, actor_user_id, note, created_at, updated_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbDebtEvent {
    pub id: String,
    pub account_id: String,
    pub budget_id: String,
    pub kind: String,
    pub amount_minor: Option<i64>,
    pub payload: Option<String>,
    pub actor_user_id: String,
    pub occurred_on: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, Default, sqlx::FromRow)]
pub struct PaymentTotals {
    pub paid_principal: i64,
    pub paid_interest: i64,
    pub paid_fee: i64,
}

impl PaymentTotals {
    pub fn total_paid(&self) -> i64 {
        self.paid_principal + self.paid_interest + self.paid_fee
    }
    pub fn cost(&self) -> i64 {
        self.paid_interest + self.paid_fee
    }
}

pub struct NewAccount {
    pub id: String,
    pub budget_id: String,
    pub name: String,
    pub lender: Option<String>,
    pub kind: String,
    pub repayment_method: String,
    pub currency: String,
    pub opening_outstanding_minor: i64,
    pub tracking_start_date: String,
    pub maturity_date: Option<String>,
    pub remaining_periods: Option<i32>,
    pub due_day: Option<i32>,
    pub periodic_fee_minor: i64,
    pub day_count: String,
    pub interest_category_id: Option<String>,
    pub pay_from_budget_id: Option<String>,
    pub created_by: String,
    pub rates: Vec<NewRate>,
    pub rules: Vec<NewRule>,
}

pub struct NewRate {
    pub effective_from: String,
    pub annual_rate: String,
    pub rate_type: String,
    pub note: Option<String>,
}

pub struct NewRule {
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub free_allowance_per_month_minor: i64,
    pub fee_pct: String,
    pub min_fee_minor: i64,
    pub note: Option<String>,
}

#[derive(Default)]
pub struct AccountPatch {
    pub name: Option<String>,
    pub lender: Option<String>,
    pub kind: Option<String>,
    pub due_day: Option<i32>,
    pub periodic_fee_minor: Option<i64>,
    pub maturity_date: Option<String>,
    pub day_count: Option<String>,
    pub interest_category_id: Option<String>,
    pub pay_from_budget_id: Option<String>,
}

pub struct NewPayment {
    pub id: String,
    pub account_id: String,
    pub budget_id: String,
    pub paid_on: String,
    pub kind: String,
    pub principal_minor: i64,
    pub interest_minor: i64,
    pub fee_minor: i64,
    pub prepay_effect: Option<String>,
    pub suggested_fee_minor: Option<i64>,
    pub from_budget_id: String,
    pub category_id: Option<String>,
    pub due_date: Option<String>,
    pub idempotency_key: String,
    pub actor_user_id: String,
    pub note: Option<String>,
}

pub enum BeginPayment {
    New(DbDebtPayment),
    /// An earlier request with the same key: the stored row, unchanged (D4).
    Replay(DbDebtPayment),
}

/// How a completed payment moves the schedule inputs of the account.
pub struct ApplyEffect {
    pub interest_through: Option<String>,
    pub last_due_date: Option<String>,
    /// `Some(None)` clears the fixed instalment, `Some(Some(v))` sets it, `None` leaves it.
    pub installment_minor: Option<Option<i64>>,
    pub mark_settled: bool,
}

pub struct NewEvent<'a> {
    pub account_id: &'a str,
    pub budget_id: &'a str,
    pub kind: &'a str,
    pub amount_minor: Option<i64>,
    pub payload: Option<String>,
    pub actor_user_id: &'a str,
    pub occurred_on: &'a str,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

async fn insert_event(
    tx: &mut Transaction<'_, MySql>,
    e: &NewEvent<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO debt_events (id, account_id, budget_id, kind, amount_minor, payload, actor_user_id, occurred_on, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(e.account_id)
    .bind(e.budget_id)
    .bind(e.kind)
    .bind(e.amount_minor)
    .bind(e.payload.as_deref())
    .bind(e.actor_user_id)
    .bind(e.occurred_on)
    .bind(now_unix())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[derive(Clone)]
pub struct DebtRepository {
    pool: MySqlPool,
}

impl DebtRepository {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }

    // ------------------------------------------------------------------ accounts

    pub async fn create_account(&self, a: &NewAccount) -> Result<DbDebtAccount, DebtRepoError> {
        let now = now_unix();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO debt_accounts (id, budget_id, name, lender, kind, repayment_method, currency,
                opening_outstanding_minor, outstanding_minor, tracking_start_date, maturity_date, remaining_periods,
                due_day, periodic_fee_minor, day_count, interest_category_id, pay_from_budget_id,
                interest_through, last_due_date, installment_minor, status, version, created_by, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, 'active', 0, ?, ?, ?)",
        )
        .bind(&a.id)
        .bind(&a.budget_id)
        .bind(&a.name)
        .bind(a.lender.as_deref())
        .bind(&a.kind)
        .bind(&a.repayment_method)
        .bind(&a.currency)
        .bind(a.opening_outstanding_minor)
        .bind(a.opening_outstanding_minor)
        .bind(&a.tracking_start_date)
        .bind(a.maturity_date.as_deref())
        .bind(a.remaining_periods)
        .bind(a.due_day)
        .bind(a.periodic_fee_minor)
        .bind(&a.day_count)
        .bind(a.interest_category_id.as_deref())
        .bind(a.pay_from_budget_id.as_deref())
        .bind(&a.tracking_start_date)
        .bind(&a.created_by)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        for r in &a.rates {
            insert_rate(&mut tx, &a.id, r).await?;
        }
        for r in &a.rules {
            insert_rule(&mut tx, &a.id, r).await?;
        }
        insert_event(
            &mut tx,
            &NewEvent {
                account_id: &a.id,
                budget_id: &a.budget_id,
                kind: "created",
                amount_minor: Some(a.opening_outstanding_minor),
                payload: None,
                actor_user_id: &a.created_by,
                occurred_on: &a.tracking_start_date,
            },
        )
        .await?;
        tx.commit().await?;
        self.get_account(&a.id)
            .await?
            .ok_or(DebtRepoError::NotFound)
    }

    pub async fn get_account(&self, id: &str) -> Result<Option<DbDebtAccount>, sqlx::Error> {
        let sql =
            format!("SELECT {ACCOUNT_COLS} FROM debt_accounts WHERE id = ? AND deleted_at IS NULL");
        sqlx::query_as::<_, DbDebtAccount>(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn list_accounts(
        &self,
        budget_id: &str,
        include_closed: bool,
    ) -> Result<Vec<DbDebtAccount>, sqlx::Error> {
        let filter = if include_closed {
            ""
        } else {
            " AND status <> 'closed'"
        };
        let sql = format!(
            "SELECT {ACCOUNT_COLS} FROM debt_accounts WHERE budget_id = ? AND deleted_at IS NULL{filter} ORDER BY created_at, id"
        );
        sqlx::query_as::<_, DbDebtAccount>(&sql)
            .bind(budget_id)
            .fetch_all(&self.pool)
            .await
    }

    pub async fn update_account(
        &self,
        id: &str,
        patch: &AccountPatch,
        expected_version: i64,
        actor: &str,
        today: &str,
        payload: String,
    ) -> Result<DbDebtAccount, DebtRepoError> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_account(&mut tx, id).await?;
        if expected_version > 0 && cur.version != expected_version {
            return Err(DebtRepoError::VersionConflict {
                current: cur.version,
            });
        }
        sqlx::query(
            "UPDATE debt_accounts SET
               name = COALESCE(?, name), lender = COALESCE(?, lender), kind = COALESCE(?, kind),
               due_day = COALESCE(?, due_day), periodic_fee_minor = COALESCE(?, periodic_fee_minor),
               maturity_date = COALESCE(?, maturity_date), day_count = COALESCE(?, day_count),
               interest_category_id = COALESCE(?, interest_category_id),
               pay_from_budget_id = COALESCE(?, pay_from_budget_id),
               version = version + 1, updated_at = ?
             WHERE id = ?",
        )
        .bind(patch.name.as_deref())
        .bind(patch.lender.as_deref())
        .bind(patch.kind.as_deref())
        .bind(patch.due_day)
        .bind(patch.periodic_fee_minor)
        .bind(patch.maturity_date.as_deref())
        .bind(patch.day_count.as_deref())
        .bind(patch.interest_category_id.as_deref())
        .bind(patch.pay_from_budget_id.as_deref())
        .bind(now_unix())
        .bind(id)
        .execute(&mut *tx)
        .await?;
        insert_event(
            &mut tx,
            &NewEvent {
                account_id: id,
                budget_id: &cur.budget_id,
                kind: "updated",
                amount_minor: None,
                payload: Some(payload),
                actor_user_id: actor,
                occurred_on: today,
            },
        )
        .await?;
        tx.commit().await?;
        self.get_account(id).await?.ok_or(DebtRepoError::NotFound)
    }

    /// Moves the account between statuses; `from` lists the statuses it may leave.
    pub async fn set_status(
        &self,
        id: &str,
        from: &[&str],
        to: &str,
        event_kind: &str,
        actor: &str,
        today: &str,
    ) -> Result<DbDebtAccount, DebtRepoError> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_account(&mut tx, id).await?;
        if !from.contains(&cur.status.as_str()) {
            return Err(DebtRepoError::NotActive);
        }
        sqlx::query("UPDATE debt_accounts SET status = ?, version = version + 1, updated_at = ? WHERE id = ?")
            .bind(to)
            .bind(now_unix())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        insert_event(
            &mut tx,
            &NewEvent {
                account_id: id,
                budget_id: &cur.budget_id,
                kind: event_kind,
                amount_minor: Some(cur.outstanding_minor),
                payload: None,
                actor_user_id: actor,
                occurred_on: today,
            },
        )
        .await?;
        tx.commit().await?;
        self.get_account(id).await?.ok_or(DebtRepoError::NotFound)
    }

    /// Aligns the balance with a statement (D3). Returns the account and the previous balance.
    pub async fn adjust_outstanding(
        &self,
        id: &str,
        new_outstanding: i64,
        reason: &str,
        actor: &str,
        occurred_on: &str,
    ) -> Result<(DbDebtAccount, i64), DebtRepoError> {
        let mut tx = self.pool.begin().await?;
        let cur = lock_account(&mut tx, id).await?;
        if cur.status != "active" && cur.status != "settled" {
            return Err(DebtRepoError::NotActive);
        }
        let status = if new_outstanding > 0 && cur.status == "settled" {
            "active"
        } else {
            cur.status.as_str()
        };
        sqlx::query(
            "UPDATE debt_accounts SET outstanding_minor = ?, status = ?, version = version + 1, updated_at = ? WHERE id = ?",
        )
        .bind(new_outstanding)
        .bind(status)
        .bind(now_unix())
        .bind(id)
        .execute(&mut *tx)
        .await?;
        insert_event(
            &mut tx,
            &NewEvent {
                account_id: id,
                budget_id: &cur.budget_id,
                kind: "adjustment",
                amount_minor: Some(new_outstanding - cur.outstanding_minor),
                payload: Some(
                    serde_json::json!({
                        "from": cur.outstanding_minor,
                        "to": new_outstanding,
                        "reason": reason
                    })
                    .to_string(),
                ),
                actor_user_id: actor,
                occurred_on,
            },
        )
        .await?;
        tx.commit().await?;
        let acc = self.get_account(id).await?.ok_or(DebtRepoError::NotFound)?;
        Ok((acc, cur.outstanding_minor))
    }

    // ------------------------------------------------------------------ rates & rules

    pub async fn list_rates(&self, account_id: &str) -> Result<Vec<DbRatePeriod>, sqlx::Error> {
        sqlx::query_as::<_, DbRatePeriod>(
            "SELECT id, account_id, effective_from, CAST(annual_rate AS CHAR) AS annual_rate, rate_type, note
             FROM debt_rate_periods WHERE account_id = ? ORDER BY effective_from",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn list_rates_for(&self, ids: &[String]) -> Result<Vec<DbRatePeriod>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let ph = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT id, account_id, effective_from, CAST(annual_rate AS CHAR) AS annual_rate, rate_type, note
             FROM debt_rate_periods WHERE account_id IN ({ph}) ORDER BY effective_from"
        );
        let mut q = sqlx::query_as::<_, DbRatePeriod>(&sql);
        for id in ids {
            q = q.bind(id);
        }
        q.fetch_all(&self.pool).await
    }

    /// Insert or replace the rate that starts on `effective_from` (the unique key), or update by id.
    pub async fn set_rate(
        &self,
        account: &DbDebtAccount,
        rate_id: Option<&str>,
        r: &NewRate,
        actor: &str,
        today: &str,
    ) -> Result<(), DebtRepoError> {
        let mut tx = self.pool.begin().await?;
        lock_account(&mut tx, &account.id).await?;
        let clash: Option<String> = sqlx::query_scalar(
            "SELECT id FROM debt_rate_periods WHERE account_id = ? AND effective_from = ?",
        )
        .bind(&account.id)
        .bind(&r.effective_from)
        .fetch_optional(&mut *tx)
        .await?;
        match (rate_id, clash) {
            (Some(id), Some(c)) if c != id => return Err(DebtRepoError::Duplicate),
            (Some(id), _) => {
                let n = sqlx::query(
                    "UPDATE debt_rate_periods SET effective_from = ?, annual_rate = ?, rate_type = ?, note = ?
                     WHERE id = ? AND account_id = ?",
                )
                .bind(&r.effective_from)
                .bind(&r.annual_rate)
                .bind(&r.rate_type)
                .bind(r.note.as_deref())
                .bind(id)
                .bind(&account.id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
                if n == 0 {
                    return Err(DebtRepoError::NotFound);
                }
            }
            (None, Some(c)) => {
                sqlx::query(
                    "UPDATE debt_rate_periods SET annual_rate = ?, rate_type = ?, note = ? WHERE id = ?",
                )
                .bind(&r.annual_rate)
                .bind(&r.rate_type)
                .bind(r.note.as_deref())
                .bind(c)
                .execute(&mut *tx)
                .await?;
            }
            (None, None) => insert_rate(&mut tx, &account.id, r).await?,
        }
        bump_version(&mut tx, &account.id).await?;
        insert_event(
            &mut tx,
            &NewEvent {
                account_id: &account.id,
                budget_id: &account.budget_id,
                kind: "rate_change",
                amount_minor: None,
                payload: Some(
                    serde_json::json!({
                        "effective_from": r.effective_from,
                        "annual_rate": r.annual_rate,
                        "rate_type": r.rate_type
                    })
                    .to_string(),
                ),
                actor_user_id: actor,
                occurred_on: today,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_rate(
        &self,
        account: &DbDebtAccount,
        rate_id: &str,
        actor: &str,
        today: &str,
    ) -> Result<(), DebtRepoError> {
        let mut tx = self.pool.begin().await?;
        lock_account(&mut tx, &account.id).await?;
        let n = sqlx::query("DELETE FROM debt_rate_periods WHERE id = ? AND account_id = ?")
            .bind(rate_id)
            .bind(&account.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(DebtRepoError::NotFound);
        }
        bump_version(&mut tx, &account.id).await?;
        insert_event(
            &mut tx,
            &NewEvent {
                account_id: &account.id,
                budget_id: &account.budget_id,
                kind: "rate_change",
                amount_minor: None,
                payload: Some(serde_json::json!({ "deleted": rate_id }).to_string()),
                actor_user_id: actor,
                occurred_on: today,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn count_rates(&self, account_id: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT COUNT(*) FROM debt_rate_periods WHERE account_id = ?")
            .bind(account_id)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn list_rules_for(&self, ids: &[String]) -> Result<Vec<DbPrepayRule>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let ph = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT id, account_id, valid_from, valid_to, free_allowance_per_month_minor,
                    CAST(fee_pct AS CHAR) AS fee_pct, min_fee_minor, note
             FROM debt_prepay_rules WHERE account_id IN ({ph}) ORDER BY valid_from"
        );
        let mut q = sqlx::query_as::<_, DbPrepayRule>(&sql);
        for id in ids {
            q = q.bind(id);
        }
        q.fetch_all(&self.pool).await
    }

    pub async fn set_rule(
        &self,
        account: &DbDebtAccount,
        rule_id: Option<&str>,
        r: &NewRule,
        actor: &str,
        today: &str,
    ) -> Result<(), DebtRepoError> {
        let mut tx = self.pool.begin().await?;
        lock_account(&mut tx, &account.id).await?;
        match rule_id {
            Some(id) => {
                let n = sqlx::query(
                    "UPDATE debt_prepay_rules SET valid_from = ?, valid_to = ?, free_allowance_per_month_minor = ?,
                            fee_pct = ?, min_fee_minor = ?, note = ? WHERE id = ? AND account_id = ?",
                )
                .bind(&r.valid_from)
                .bind(r.valid_to.as_deref())
                .bind(r.free_allowance_per_month_minor)
                .bind(&r.fee_pct)
                .bind(r.min_fee_minor)
                .bind(r.note.as_deref())
                .bind(id)
                .bind(&account.id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
                if n == 0 {
                    return Err(DebtRepoError::NotFound);
                }
            }
            None => insert_rule(&mut tx, &account.id, r).await?,
        }
        bump_version(&mut tx, &account.id).await?;
        insert_event(
            &mut tx,
            &NewEvent {
                account_id: &account.id,
                budget_id: &account.budget_id,
                kind: "prepay_rule_change",
                amount_minor: None,
                payload: Some(
                    serde_json::json!({
                        "valid_from": r.valid_from,
                        "valid_to": r.valid_to,
                        "free_allowance_per_month_minor": r.free_allowance_per_month_minor,
                        "fee_pct": r.fee_pct,
                        "min_fee_minor": r.min_fee_minor
                    })
                    .to_string(),
                ),
                actor_user_id: actor,
                occurred_on: today,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_rule(
        &self,
        account: &DbDebtAccount,
        rule_id: &str,
        actor: &str,
        today: &str,
    ) -> Result<(), DebtRepoError> {
        let mut tx = self.pool.begin().await?;
        lock_account(&mut tx, &account.id).await?;
        let n = sqlx::query("DELETE FROM debt_prepay_rules WHERE id = ? AND account_id = ?")
            .bind(rule_id)
            .bind(&account.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(DebtRepoError::NotFound);
        }
        bump_version(&mut tx, &account.id).await?;
        insert_event(
            &mut tx,
            &NewEvent {
                account_id: &account.id,
                budget_id: &account.budget_id,
                kind: "prepay_rule_change",
                amount_minor: None,
                payload: Some(serde_json::json!({ "deleted": rule_id }).to_string()),
                actor_user_id: actor,
                occurred_on: today,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    // ------------------------------------------------------------------ payments

    /// Step 1 of the saga: lock the account, validate and record the payment as `requested`.
    pub async fn begin_payment(&self, p: &NewPayment) -> Result<BeginPayment, DebtRepoError> {
        let mut tx = self.pool.begin().await?;
        let acc = lock_account(&mut tx, &p.account_id).await?;

        let sql = format!(
            "SELECT {PAYMENT_COLS} FROM debt_payments WHERE account_id = ? AND idempotency_key = ?"
        );
        if let Some(existing) = sqlx::query_as::<_, DbDebtPayment>(&sql)
            .bind(&p.account_id)
            .bind(&p.idempotency_key)
            .fetch_optional(&mut *tx)
            .await?
        {
            tx.rollback().await?;
            return Ok(BeginPayment::Replay(existing));
        }
        if acc.status != "active" {
            return Err(DebtRepoError::NotActive);
        }
        if p.principal_minor > acc.outstanding_minor {
            return Err(DebtRepoError::Insufficient {
                outstanding: acc.outstanding_minor,
                requested: p.principal_minor,
            });
        }
        let now = now_unix();
        sqlx::query(
            "INSERT INTO debt_payments (id, account_id, budget_id, paid_on, kind, principal_minor, interest_minor,
                fee_minor, total_minor, prepay_effect, suggested_fee_minor, from_budget_id, category_id,
                entry_group_id, due_date, status, idempotency_key, actor_user_id, note, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'requested', ?, ?, ?, ?, ?)",
        )
        .bind(&p.id)
        .bind(&p.account_id)
        .bind(&p.budget_id)
        .bind(&p.paid_on)
        .bind(&p.kind)
        .bind(p.principal_minor)
        .bind(p.interest_minor)
        .bind(p.fee_minor)
        .bind(p.principal_minor + p.interest_minor + p.fee_minor)
        .bind(p.prepay_effect.as_deref())
        .bind(p.suggested_fee_minor)
        .bind(&p.from_budget_id)
        .bind(p.category_id.as_deref())
        .bind(&p.id) // entry_group_id = payment id
        .bind(p.due_date.as_deref())
        .bind(&p.idempotency_key)
        .bind(&p.actor_user_id)
        .bind(p.note.as_deref())
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(BeginPayment::New(
            self.get_payment(&p.id)
                .await?
                .ok_or(DebtRepoError::NotFound)?,
        ))
    }

    pub async fn find_payment_by_key(
        &self,
        account_id: &str,
        key: &str,
    ) -> Result<Option<DbDebtPayment>, sqlx::Error> {
        let sql = format!(
            "SELECT {PAYMENT_COLS} FROM debt_payments WHERE account_id = ? AND idempotency_key = ?"
        );
        sqlx::query_as::<_, DbDebtPayment>(&sql)
            .bind(account_id)
            .bind(key)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn get_payment(&self, id: &str) -> Result<Option<DbDebtPayment>, sqlx::Error> {
        let sql = format!("SELECT {PAYMENT_COLS} FROM debt_payments WHERE id = ?");
        sqlx::query_as::<_, DbDebtPayment>(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// Step 3 of the saga: take the principal off the balance and complete the payment, atomically.
    /// Refuses when the payment is no longer `requested` (the reconciler took it) or the balance
    /// would go negative (D1).
    pub async fn apply_payment(
        &self,
        payment_id: &str,
        effect: &ApplyEffect,
    ) -> Result<DbDebtAccount, DebtRepoError> {
        let mut tx = self.pool.begin().await?;
        let pay: DbDebtPayment = {
            let sql = format!("SELECT {PAYMENT_COLS} FROM debt_payments WHERE id = ? FOR UPDATE");
            sqlx::query_as(&sql)
                .bind(payment_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(DebtRepoError::NotFound)?
        };
        if pay.status != "requested" {
            return Err(DebtRepoError::PaymentGone);
        }
        let acc = lock_account(&mut tx, &pay.account_id).await?;
        if pay.principal_minor > acc.outstanding_minor {
            return Err(DebtRepoError::Insufficient {
                outstanding: acc.outstanding_minor,
                requested: pay.principal_minor,
            });
        }
        let installment: Option<i64> = match effect.installment_minor {
            None => acc.installment_minor,
            Some(v) => v,
        };
        let status = if effect.mark_settled {
            "settled"
        } else {
            acc.status.as_str()
        };
        let r = sqlx::query(
            "UPDATE debt_accounts SET outstanding_minor = outstanding_minor - ?,
                    interest_through = COALESCE(?, interest_through),
                    last_due_date = COALESCE(?, last_due_date),
                    installment_minor = ?, status = ?, version = version + 1, updated_at = ?
             WHERE id = ? AND outstanding_minor >= ?",
        )
        .bind(pay.principal_minor)
        .bind(effect.interest_through.as_deref())
        .bind(effect.last_due_date.as_deref())
        .bind(installment)
        .bind(status)
        .bind(now_unix())
        .bind(&pay.account_id)
        .bind(pay.principal_minor)
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() != 1 {
            return Err(DebtRepoError::Insufficient {
                outstanding: acc.outstanding_minor,
                requested: pay.principal_minor,
            });
        }
        sqlx::query("UPDATE debt_payments SET status = 'completed', updated_at = ? WHERE id = ?")
            .bind(now_unix())
            .bind(payment_id)
            .execute(&mut *tx)
            .await?;
        insert_event(
            &mut tx,
            &NewEvent {
                account_id: &pay.account_id,
                budget_id: &pay.budget_id,
                kind: "payment",
                amount_minor: Some(pay.total_minor),
                payload: Some(
                    serde_json::json!({
                        "payment_id": pay.id,
                        "kind": pay.kind,
                        "principal": pay.principal_minor,
                        "interest": pay.interest_minor,
                        "fee": pay.fee_minor,
                        "suggested_fee": pay.suggested_fee_minor,
                        "prepay_effect": pay.prepay_effect,
                    })
                    .to_string(),
                ),
                actor_user_id: &pay.actor_user_id,
                occurred_on: &pay.paid_on,
            },
        )
        .await?;
        tx.commit().await?;
        self.get_account(&pay.account_id)
            .await?
            .ok_or(DebtRepoError::NotFound)
    }

    /// `requested` → `failed` (or any `from` status), once the legs are known to be undone.
    pub async fn set_payment_status(
        &self,
        id: &str,
        from: &[&str],
        to: &str,
    ) -> Result<bool, sqlx::Error> {
        let ph = vec!["?"; from.len()].join(",");
        let sql = format!(
            "UPDATE debt_payments SET status = ?, updated_at = ? WHERE id = ? AND status IN ({ph})"
        );
        let mut q = sqlx::query(&sql).bind(to).bind(now_unix()).bind(id);
        for f in from {
            q = q.bind(*f);
        }
        Ok(q.execute(&self.pool).await?.rows_affected() == 1)
    }

    /// Payments stuck in `requested`/`reconciling` for longer than `older_than_secs`.
    pub async fn stale_payments(
        &self,
        older_than_secs: i64,
    ) -> Result<Vec<DbDebtPayment>, sqlx::Error> {
        let sql = format!(
            "SELECT {PAYMENT_COLS} FROM debt_payments
             WHERE status IN ('requested', 'reconciling') AND updated_at < ? ORDER BY updated_at LIMIT 100"
        );
        sqlx::query_as::<_, DbDebtPayment>(&sql)
            .bind(now_unix() - older_than_secs)
            .fetch_all(&self.pool)
            .await
    }

    /// Claims a stale payment for rollback; only one caller wins.
    pub async fn claim_payment(&self, id: &str) -> Result<bool, sqlx::Error> {
        let r = sqlx::query(
            "UPDATE debt_payments SET status = 'reconciling', updated_at = ?
             WHERE id = ? AND status IN ('requested', 'reconciling')",
        )
        .bind(now_unix())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Principal prepaid in the calendar month of `month` ("YYYY-MM"), completed prepayments only.
    pub async fn prepaid_in_month(
        &self,
        account_id: &str,
        month: &str,
    ) -> Result<i64, sqlx::Error> {
        let v: Option<i64> = sqlx::query_scalar(
            "SELECT CAST(COALESCE(SUM(principal_minor), 0) AS SIGNED) FROM debt_payments
             WHERE account_id = ? AND status = 'completed' AND kind IN ('prepayment', 'settlement')
               AND paid_on LIKE ?",
        )
        .bind(account_id)
        .bind(format!("{month}-%"))
        .fetch_one(&self.pool)
        .await?;
        Ok(v.unwrap_or(0))
    }

    pub async fn totals_for_accounts(
        &self,
        ids: &[String],
    ) -> Result<std::collections::HashMap<String, PaymentTotals>, sqlx::Error> {
        let mut out = std::collections::HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let ph = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT account_id,
                    CAST(COALESCE(SUM(principal_minor), 0) AS SIGNED) AS paid_principal,
                    CAST(COALESCE(SUM(interest_minor), 0) AS SIGNED) AS paid_interest,
                    CAST(COALESCE(SUM(fee_minor), 0) AS SIGNED) AS paid_fee
             FROM debt_payments WHERE status = 'completed' AND account_id IN ({ph}) GROUP BY account_id"
        );
        let mut q = sqlx::query_as::<_, (String, i64, i64, i64)>(&sql);
        for id in ids {
            q = q.bind(id);
        }
        for (account_id, p, i, f) in q.fetch_all(&self.pool).await? {
            out.insert(
                account_id,
                PaymentTotals {
                    paid_principal: p,
                    paid_interest: i,
                    paid_fee: f,
                },
            );
        }
        Ok(out)
    }

    pub async fn list_payments(
        &self,
        budget_id: &str,
        account_id: Option<&str>,
        filter: &PaymentFilter,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<DbDebtPayment>, i64), sqlx::Error> {
        let (cond, binds) = payment_conditions(account_id, filter);
        let sql = format!(
            "SELECT {PAYMENT_COLS} FROM debt_payments WHERE budget_id = ? AND status = 'completed'{cond}
             ORDER BY paid_on DESC, created_at DESC, id DESC LIMIT ? OFFSET ?"
        );
        let mut q = sqlx::query_as::<_, DbDebtPayment>(&sql).bind(budget_id);
        for b in &binds {
            q = q.bind(b);
        }
        let rows = q.bind(limit).bind(offset).fetch_all(&self.pool).await?;
        let csql = format!(
            "SELECT COUNT(*) FROM debt_payments WHERE budget_id = ? AND status = 'completed'{cond}"
        );
        let mut c = sqlx::query_scalar::<_, i64>(&csql).bind(budget_id);
        for b in &binds {
            c = c.bind(b);
        }
        Ok((rows, c.fetch_one(&self.pool).await?))
    }

    pub async fn list_events(
        &self,
        budget_id: &str,
        account_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<DbDebtEvent>, i64), sqlx::Error> {
        let cond = if account_id.is_some() {
            " AND account_id = ?"
        } else {
            ""
        };
        let sql = format!(
            "SELECT id, account_id, budget_id, kind, amount_minor, payload, actor_user_id, occurred_on, created_at
             FROM debt_events WHERE budget_id = ?{cond} ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
        );
        let mut q = sqlx::query_as::<_, DbDebtEvent>(&sql).bind(budget_id);
        if let Some(a) = account_id {
            q = q.bind(a);
        }
        let rows = q.bind(limit).bind(offset).fetch_all(&self.pool).await?;
        let csql = format!("SELECT COUNT(*) FROM debt_events WHERE budget_id = ?{cond}");
        let mut c = sqlx::query_scalar::<_, i64>(&csql).bind(budget_id);
        if let Some(a) = account_id {
            c = c.bind(a);
        }
        Ok((rows, c.fetch_one(&self.pool).await?))
    }

    /// Recomputes the balance from the ledger (D3); used by tests and the integrity check.
    pub async fn recomputed_outstanding(&self, account_id: &str) -> Result<i64, sqlx::Error> {
        let opening: i64 =
            sqlx::query_scalar("SELECT opening_outstanding_minor FROM debt_accounts WHERE id = ?")
                .bind(account_id)
                .fetch_one(&self.pool)
                .await?;
        let paid: i64 = sqlx::query_scalar(
            "SELECT CAST(COALESCE(SUM(principal_minor), 0) AS SIGNED) FROM debt_payments
             WHERE account_id = ? AND status = 'completed'",
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await?;
        let adj: i64 = sqlx::query_scalar(
            "SELECT CAST(COALESCE(SUM(amount_minor), 0) AS SIGNED) FROM debt_events
             WHERE account_id = ? AND kind = 'adjustment'",
        )
        .bind(account_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(opening - paid + adj)
    }
}

async fn lock_account(
    tx: &mut Transaction<'_, MySql>,
    id: &str,
) -> Result<DbDebtAccount, DebtRepoError> {
    let sql = format!(
        "SELECT {ACCOUNT_COLS} FROM debt_accounts WHERE id = ? AND deleted_at IS NULL FOR UPDATE"
    );
    sqlx::query_as::<_, DbDebtAccount>(&sql)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(DebtRepoError::NotFound)
}

async fn bump_version(tx: &mut Transaction<'_, MySql>, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE debt_accounts SET version = version + 1, updated_at = ? WHERE id = ?")
        .bind(now_unix())
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn insert_rate(
    tx: &mut Transaction<'_, MySql>,
    account_id: &str,
    r: &NewRate,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO debt_rate_periods (id, account_id, effective_from, annual_rate, rate_type, note, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(account_id)
    .bind(&r.effective_from)
    .bind(&r.annual_rate)
    .bind(&r.rate_type)
    .bind(r.note.as_deref())
    .bind(now_unix())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_rule(
    tx: &mut Transaction<'_, MySql>,
    account_id: &str,
    r: &NewRule,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO debt_prepay_rules (id, account_id, valid_from, valid_to, free_allowance_per_month_minor,
            fee_pct, min_fee_minor, note, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new_id())
    .bind(account_id)
    .bind(&r.valid_from)
    .bind(r.valid_to.as_deref())
    .bind(r.free_allowance_per_month_minor)
    .bind(&r.fee_pct)
    .bind(r.min_fee_minor)
    .bind(r.note.as_deref())
    .bind(now_unix())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod payment_filter_tests {
    use super::*;

    #[test]
    fn no_filter_adds_nothing() {
        let (c, b) = payment_conditions(None, &PaymentFilter::default());
        assert!(c.is_empty() && b.is_empty());
    }

    #[test]
    fn filters_bind_in_order_and_never_inline_values() {
        let f = PaymentFilter {
            date_from: Some("2026-01-01".into()),
            date_to: Some("2026-06-30".into()),
            kind: Some("prepayment".into()),
        };
        let (c, b) = payment_conditions(Some("acc"), &f);
        assert_eq!(
            c,
            " AND account_id = ? AND paid_on >= ? AND paid_on <= ? AND kind = ?"
        );
        assert_eq!(b, vec!["acc", "2026-01-01", "2026-06-30", "prepayment"]);
        assert!(!c.contains("prepayment") && !c.contains("2026"));
    }
}
