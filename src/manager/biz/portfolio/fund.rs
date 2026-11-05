//! Fund (saving / invest budget) cash pocket: summary, deposit/withdraw saga with the Entry
//! service, adjustments, and pocket-funded purchases (plan R2).
//!
//! Deposit  = Standard `TRANSFER_OUT` entry leg → pocket `transfer_in`.
//! Withdraw = pocket `transfer_out` → Standard `TRANSFER_IN` entry leg.
//! Each step is idempotent; a failure compensates the earlier step. A reconcile pass rolls back
//! transfers that were left `requested` by a crash.

use tonic::Status;
use uuid::Uuid;

use crate::converters::fund::{map_movement, NewFundMovement};
use crate::converters::portfolio::AssetClassNew;
use crate::manager::biz::portfolio::biz::PortfolioBiz;
use crate::manager::biz::portfolio::policy;
use crate::manager::repository::fund_cash::{FundCashError, TransferClaim};
use crate::pb::service::budget::{BudgetRole, BudgetType};
use crate::pb::service::entry::{CreateTransferLegRequest, EntryKind, VoidTransferLegRequest};
use crate::pb::service::portfolio as pb;
use philand_time::now_unix;

fn internal<E: ToString>(e: E) -> Status {
    Status::internal(e.to_string())
}

pub fn is_fund_type(t: BudgetType) -> bool {
    matches!(t, BudgetType::Saving | BudgetType::Invest)
}

/// Static request validation shared by deposit and withdraw (no I/O).
pub fn validate_fund_transfer(req: &pb::FundTransferRequest) -> Result<(), Status> {
    if req.amount_minor <= 0 {
        return Err(Status::invalid_argument("amount must be positive"));
    }
    if req.budget_id.is_empty() || req.counterparty_budget_id.is_empty() {
        return Err(Status::invalid_argument("budget ids are required"));
    }
    if req.budget_id == req.counterparty_budget_id {
        return Err(Status::invalid_argument("budgets must differ"));
    }
    if req.idempotency_key.trim().is_empty() || req.idempotency_key.len() > 60 {
        return Err(Status::invalid_argument(
            "idempotency_key is required (max 60 characters)",
        ));
    }
    if chrono::NaiveDate::parse_from_str(&req.occurred_on, "%Y-%m-%d").is_err() {
        return Err(Status::invalid_argument("occurred_on must be YYYY-MM-DD"));
    }
    if req.note.chars().count() > 500 {
        return Err(Status::invalid_argument("note too long"));
    }
    Ok(())
}

fn map_cash_err(e: FundCashError) -> Status {
    match e {
        FundCashError::Insufficient { balance, requested } => Status::failed_precondition(format!(
            "insufficient cash in fund: balance {balance}, requested {requested}"
        )),
        FundCashError::IdempotencyMismatch => {
            Status::failed_precondition("idempotency_key was reused with a different request")
        }
        FundCashError::Db(e) => internal(e),
    }
}

struct BudgetInfo {
    org_id: String,
    currency: String,
    budget_type: BudgetType,
}

impl PortfolioBiz {
    async fn budget_info(&self, budget_id: &str) -> Result<BudgetInfo, Status> {
        let b = self
            .budget_biz
            .repo
            .get_budget_by_id(budget_id)
            .await
            .map_err(|_| Status::not_found("budget not found"))?;
        Ok(BudgetInfo {
            org_id: b.org_id,
            currency: b.currency,
            budget_type: crate::converters::budget_type_from_db(&b.budget_type),
        })
    }

    async fn assert_fund(&self, budget_id: &str) -> Result<BudgetInfo, Status> {
        let info = self.budget_info(budget_id).await?;
        if !is_fund_type(info.budget_type) {
            return Err(Status::failed_precondition(
                "this budget is not a saving or invest fund",
            ));
        }
        Ok(info)
    }

    fn entry_client(
        &self,
    ) -> Result<&std::sync::Arc<tokio::sync::Mutex<crate::manager::client::EntryClient>>, Status>
    {
        self.entry_client
            .as_ref()
            .ok_or_else(|| Status::unavailable("entry service client not configured"))
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    pub async fn get_fund_summary(
        &self,
        user_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<pb::FundSummary, Status> {
        self.assert_member(budget_id, user_id, user_type).await?;
        let info = self.assert_fund(budget_id).await?;
        let cash = self
            .fund_repo
            .get_account(budget_id)
            .await
            .map_err(internal)?
            .map(|a| a.balance_minor)
            .unwrap_or(0);
        let net_contributed = self
            .fund_repo
            .net_contributed(budget_id)
            .await
            .map_err(internal)?;
        let valuated = self.list_valuated(user_id, budget_id, user_type).await?;
        let mut holdings = 0i64;
        let mut by_class: std::collections::BTreeMap<String, i64> = Default::default();
        let mut worst = pb::PriceFreshness::Fresh;
        let mut upcoming = Vec::new();
        let horizon = now_unix() + 90 * 86_400;
        for v in &valuated {
            let a = match v.asset.as_ref() {
                Some(a) => a,
                None => continue,
            };
            let converted = self
                .fx_svc
                .convert(v.current_value, &a.currency, &info.currency);
            holdings += converted;
            if converted == 0 {
                continue; // settled or empty positions don't belong in the allocation
            }
            *by_class
                .entry(format!(
                    "{:?}",
                    pb::PortfolioAssetClass::try_from(a.asset_class)
                        .unwrap_or(pb::PortfolioAssetClass::Unspecified)
                ))
                .or_insert(0) += converted;
            let f = pb::PriceFreshness::try_from(v.freshness)
                .unwrap_or(pb::PriceFreshness::Unspecified);
            worst = worse_freshness(worst, f);
        }
        // FD maturities within 90 days (from the asset summaries' maturity data when present).
        for m in self.upcoming_maturities(budget_id, horizon).await? {
            upcoming.push(m);
        }
        let total = cash + holdings;
        let growth = total - net_contributed;
        let growth_pct = if net_contributed > 0 {
            growth as f64 / net_contributed as f64 * 100.0
        } else {
            0.0
        };
        let mut allocation: Vec<pb::AllocationSlice> = by_class
            .into_iter()
            .map(|(label, value)| pb::AllocationSlice {
                label,
                value,
                pct: if total > 0 {
                    value as f64 / total as f64 * 100.0
                } else {
                    0.0
                },
            })
            .collect();
        if cash > 0 {
            allocation.push(pb::AllocationSlice {
                label: "cash".into(),
                value: cash,
                pct: if total > 0 {
                    cash as f64 / total as f64 * 100.0
                } else {
                    0.0
                },
            });
        }
        Ok(pb::FundSummary {
            budget_id: budget_id.into(),
            currency: info.currency,
            cash_balance: cash,
            holdings_value: holdings,
            total_value: total,
            net_contributed,
            growth,
            growth_pct,
            allocation,
            upcoming,
            worst_freshness: worst as i32,
        })
    }

    async fn upcoming_maturities(
        &self,
        budget_id: &str,
        horizon: i64,
    ) -> Result<Vec<pb::UpcomingMaturity>, Status> {
        let rows = self
            .fund_repo
            .upcoming_maturities(budget_id, now_unix(), horizon)
            .await
            .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(
                |(asset_id, display_name, maturity_date, principal)| pb::UpcomingMaturity {
                    asset_id,
                    display_name,
                    maturity_date,
                    expected_amount: principal,
                },
            )
            .collect())
    }

    pub async fn list_fund_cash_movements(
        &self,
        user_id: &str,
        req: &pb::ListFundCashMovementsRequest,
        user_type: Option<&str>,
    ) -> Result<pb::ListFundCashMovementsResponse, Status> {
        self.assert_member(&req.budget_id, user_id, user_type)
            .await?;
        self.assert_fund(&req.budget_id).await?;
        let page_size = if req.page_size <= 0 {
            20
        } else {
            req.page_size.min(100)
        } as i64;
        let page = req.page.max(1) as i64;
        let (rows, total) = self
            .fund_repo
            .list_movements(&req.budget_id, page_size, (page - 1) * page_size)
            .await
            .map_err(internal)?;
        Ok(pb::ListFundCashMovementsResponse {
            movements: rows.into_iter().map(map_movement).collect(),
            total_rows: total as i32,
        })
    }

    // -----------------------------------------------------------------------
    // Deposit / withdraw saga
    // -----------------------------------------------------------------------

    pub async fn deposit_to_fund(
        &self,
        user_id: &str,
        req: &pb::FundTransferRequest,
        user_type: Option<&str>,
    ) -> Result<pb::FundTransferResponse, Status> {
        validate_fund_transfer(req)?;
        self.assert_min_role(&req.budget_id, user_id, BudgetRole::Contributor, user_type)
            .await?;
        self.assert_min_role(
            &req.counterparty_budget_id,
            user_id,
            BudgetRole::Contributor,
            user_type,
        )
        .await?;
        let fund = self.assert_fund(&req.budget_id).await?;
        let std_info = self.budget_info(&req.counterparty_budget_id).await?;
        self.assert_pair(&fund, &std_info)?;

        let key = &req.idempotency_key;
        let (group_id, replay) = self
            .begin_fund_transfer(user_id, req, true, &fund.currency)
            .await?;
        if let Some(done) = replay {
            return Ok(done);
        }

        // 1. Standard side: TRANSFER_OUT entry leg.
        let leg = CreateTransferLegRequest {
            budget_id: req.counterparty_budget_id.clone(),
            kind: EntryKind::TransferOut as i32,
            amount: req.amount_minor,
            entry_date: req.occurred_on.clone(),
            description: if req.note.is_empty() {
                "Chuyển vào quỹ".into()
            } else {
                req.note.clone()
            },
            transfer_group_id: group_id.clone(),
            counterparty_budget_id: req.budget_id.clone(),
            counterparty_kind: "fund_cash".into(),
            idempotency_key: key.clone(),
            actor_user_id: user_id.into(),
        };
        let entry = {
            let mut c = self.entry_client()?.lock().await;
            c.create_transfer_leg(user_id, user_type, leg).await
        };
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                let _ = self
                    .fund_repo
                    .set_transfer_status(&group_id, "failed", None)
                    .await;
                return Err(e);
            }
        };

        // 2. Pocket side.
        let applied = self
            .fund_repo
            .apply(&NewFundMovement {
                budget_id: req.budget_id.clone(),
                currency: fund.currency.clone(),
                kind: pb::FundCashMovementKind::TransferIn,
                amount_minor: req.amount_minor,
                ref_type: Some("portfolio_transfer".into()),
                ref_id: Some(group_id.clone()),
                transfer_group_id: Some(group_id.clone()),
                idempotency_key: Some(key.clone()),
                actor_user_id: user_id.into(),
                note: opt(&req.note),
                occurred_on: req.occurred_on.clone(),
            })
            .await;
        match applied {
            Ok(a) => {
                let entry_id = entry
                    .base
                    .as_ref()
                    .map(|b| b.id.clone())
                    .unwrap_or_default();
                match self
                    .fund_repo
                    .set_transfer_status(&group_id, "completed", Some(&entry_id))
                    .await
                {
                    Ok(true) => Ok(pb::FundTransferResponse {
                        movement: Some(map_movement(a.movement)),
                        cash_balance: a.balance_after,
                        transfer_group_id: group_id,
                    }),
                    Ok(false) => {
                        // The reconciler claimed this transfer while we were working, so it owns
                        // the rollback. Undo our own steps: the money must not stay half moved.
                        if let Err(ce) = self
                            .compensate_transfer(
                                user_id,
                                user_type,
                                &req.counterparty_budget_id,
                                &req.budget_id,
                                &fund.currency,
                                key,
                                &group_id,
                            )
                            .await
                        {
                            tracing::error!(
                                "fund deposit compensation failed transfer_id={group_id} idempotency_key={key} err={ce}"
                            );
                        }
                        Err(Status::aborted(
                            "transfer was rolled back by reconciliation; please retry",
                        ))
                    }
                    Err(e) => Err(internal(e)),
                }
            }
            Err(e) => {
                // Compensate: void the entry leg so the standard budget is not debited.
                match self
                    .compensate_transfer(
                        user_id,
                        user_type,
                        &req.counterparty_budget_id,
                        &req.budget_id,
                        &fund.currency,
                        key,
                        &group_id,
                    )
                    .await
                {
                    Ok(()) => {
                        let _ = self
                            .fund_repo
                            .set_transfer_status(&group_id, "failed", None)
                            .await;
                    }
                    Err(ce) => {
                        // The compensation did not go through: keep the row `requested` so
                        // `reconcile_stale_fund_transfers` retries the rollback instead of
                        // losing the debit of the standard budget.
                        tracing::error!(
                            "fund deposit compensation failed transfer_id={group_id} idempotency_key={key} err={ce}"
                        );
                    }
                }
                Err(map_cash_err(e))
            }
        }
    }

    pub async fn withdraw_from_fund(
        &self,
        user_id: &str,
        req: &pb::FundTransferRequest,
        user_type: Option<&str>,
    ) -> Result<pb::FundTransferResponse, Status> {
        validate_fund_transfer(req)?;
        self.assert_min_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        self.assert_min_role(
            &req.counterparty_budget_id,
            user_id,
            BudgetRole::Contributor,
            user_type,
        )
        .await?;
        let fund = self.assert_fund(&req.budget_id).await?;
        let std_info = self.budget_info(&req.counterparty_budget_id).await?;
        self.assert_pair(&fund, &std_info)?;

        let key = &req.idempotency_key;
        let (group_id, replay) = self
            .begin_fund_transfer(user_id, req, false, &fund.currency)
            .await?;
        if let Some(done) = replay {
            return Ok(done);
        }

        // 1. Pocket side first so the balance check (I4) happens before anything else moves.
        let applied = self
            .fund_repo
            .apply(&NewFundMovement {
                budget_id: req.budget_id.clone(),
                currency: fund.currency.clone(),
                kind: pb::FundCashMovementKind::TransferOut,
                amount_minor: -req.amount_minor,
                ref_type: Some("portfolio_transfer".into()),
                ref_id: Some(group_id.clone()),
                transfer_group_id: Some(group_id.clone()),
                idempotency_key: Some(key.clone()),
                actor_user_id: user_id.into(),
                note: opt(&req.note),
                occurred_on: req.occurred_on.clone(),
            })
            .await;
        let applied = match applied {
            Ok(a) => a,
            Err(e) => {
                let _ = self
                    .fund_repo
                    .set_transfer_status(&group_id, "failed", None)
                    .await;
                return Err(map_cash_err(e));
            }
        };

        // 2. Standard side: TRANSFER_IN entry leg.
        let leg = CreateTransferLegRequest {
            budget_id: req.counterparty_budget_id.clone(),
            kind: EntryKind::TransferIn as i32,
            amount: req.amount_minor,
            entry_date: req.occurred_on.clone(),
            description: if req.note.is_empty() {
                "Rút từ quỹ".into()
            } else {
                req.note.clone()
            },
            transfer_group_id: group_id.clone(),
            counterparty_budget_id: req.budget_id.clone(),
            counterparty_kind: "fund_cash".into(),
            idempotency_key: key.clone(),
            actor_user_id: user_id.into(),
        };
        let entry = {
            let mut c = self.entry_client()?.lock().await;
            c.create_transfer_leg(user_id, user_type, leg).await
        };
        match entry {
            Ok(e) => {
                let entry_id = e.base.as_ref().map(|b| b.id.clone()).unwrap_or_default();
                match self
                    .fund_repo
                    .set_transfer_status(&group_id, "completed", Some(&entry_id))
                    .await
                {
                    Ok(true) => Ok(pb::FundTransferResponse {
                        movement: Some(map_movement(applied.movement)),
                        cash_balance: applied.balance_after,
                        transfer_group_id: group_id,
                    }),
                    Ok(false) => {
                        // The reconciler claimed this transfer while we were working, so it owns
                        // the rollback. Undo our own steps: the money must not stay half moved.
                        if let Err(ce) = self
                            .compensate_transfer(
                                user_id,
                                user_type,
                                &req.counterparty_budget_id,
                                &req.budget_id,
                                &fund.currency,
                                key,
                                &group_id,
                            )
                            .await
                        {
                            tracing::error!(
                                "fund withdraw compensation failed transfer_id={group_id} idempotency_key={key} err={ce}"
                            );
                        }
                        Err(Status::aborted(
                            "transfer was rolled back by reconciliation; please retry",
                        ))
                    }
                    Err(e) => Err(internal(e)),
                }
            }
            Err(e) => {
                // Compensate: put the money back in the pocket.
                match self
                    .compensate_transfer(
                        user_id,
                        user_type,
                        &req.counterparty_budget_id,
                        &req.budget_id,
                        &fund.currency,
                        key,
                        &group_id,
                    )
                    .await
                {
                    Ok(()) => {
                        let _ = self
                            .fund_repo
                            .set_transfer_status(&group_id, "failed", None)
                            .await;
                    }
                    Err(ce) => {
                        // The compensation did not go through: keep the row `requested` so
                        // `reconcile_stale_fund_transfers` retries the rollback instead of losing
                        // the money taken out of the pocket.
                        tracing::error!(
                            "fund withdraw compensation failed transfer_id={group_id} idempotency_key={key} err={ce}"
                        );
                    }
                }
                Err(e)
            }
        }
    }

    fn assert_pair(&self, fund: &BudgetInfo, std_info: &BudgetInfo) -> Result<(), Status> {
        if std_info.budget_type != BudgetType::Standard {
            return Err(Status::failed_precondition(
                "the other side must be a standard budget",
            ));
        }
        if fund.org_id != std_info.org_id {
            return Err(Status::failed_precondition(
                "budgets must be in the same organisation",
            ));
        }
        if fund.currency != std_info.currency {
            return Err(Status::failed_precondition(
                "budgets must use the same currency",
            ));
        }
        Ok(())
    }

    /// Creates the `portfolio_transfers` row, or returns the finished result for a replayed key.
    async fn begin_fund_transfer(
        &self,
        user_id: &str,
        req: &pb::FundTransferRequest,
        deposit: bool,
        currency: &str,
    ) -> Result<(String, Option<pb::FundTransferResponse>), Status> {
        let (source, counter, direction) = if deposit {
            (
                &req.counterparty_budget_id,
                &req.budget_id,
                pb::TransferDirection::StandardToInvest,
            )
        } else {
            (
                &req.budget_id,
                &req.counterparty_budget_id,
                pb::TransferDirection::InvestToStandard,
            )
        };
        if let Some(existing) = self
            .repo
            .find_transfer_by_idempotency(
                source.clone(),
                counter.clone(),
                req.idempotency_key.clone(),
            )
            .await
            .map_err(internal)?
        {
            if existing.amount_minor != req.amount_minor {
                return Err(Status::failed_precondition(
                    "idempotency_key was reused with a different amount",
                ));
            }
            return match existing.status.as_str() {
                "completed" => {
                    let m = self
                        .fund_repo
                        .find_movement_by_key(&req.budget_id, &req.idempotency_key)
                        .await
                        .map_err(internal)?
                        .ok_or_else(|| Status::internal("completed transfer has no movement"))?;
                    let bal = self
                        .fund_repo
                        .get_account(&req.budget_id)
                        .await
                        .map_err(internal)?
                        .map(|a| a.balance_minor)
                        .unwrap_or(0);
                    Ok((
                        existing.group_id.clone(),
                        Some(pb::FundTransferResponse {
                            movement: Some(map_movement(m)),
                            cash_balance: bal,
                            transfer_group_id: existing.group_id,
                        }),
                    ))
                }
                "requested" => Err(Status::aborted(
                    "this transfer is still being processed; retry shortly",
                )),
                "reconciling" => Err(Status::aborted(
                    "this transfer is being rolled back; retry shortly",
                )),
                _ => Err(Status::failed_precondition(
                    "this idempotency_key belongs to a failed transfer; use a new key",
                )),
            };
        }
        let id = Uuid::new_v4().to_string();
        let now = now_unix();
        let new = crate::converters::portfolio::NewTransfer {
            id: id.clone(),
            group_id: id.clone(),
            source_budget_id: source.clone(),
            counterparty_budget_id: counter.clone(),
            direction: direction as i32,
            amount_minor: req.amount_minor,
            currency: currency.to_string(),
            status: pb::TransferStatus::Requested as i32,
            linked_entry_source_id: None,
            linked_entry_counterparty_id: None,
            idempotency_key: req.idempotency_key.clone(),
            actor_user_id: user_id.to_string(),
            created_at: now,
            completed_at: None,
            notes: opt(&req.note),
        };
        let mut tx = self.repo.begin().await.map_err(internal)?;
        self.repo
            .insert_transfer(&mut tx, &new)
            .await
            .map_err(|e| {
                // A concurrent request with the same key wins the unique index.
                Status::aborted(format!("transfer already in progress: {e}"))
            })?;
        tx.commit().await.map_err(internal)?;
        Ok((id, None))
    }

    /// Voids the entry leg of a transfer.
    ///
    /// `Ok(())` means the standard budget is not (or no longer) debited: a missing leg counts as
    /// already voided. Any other error means the compensation did not happen.
    async fn void_leg(
        &self,
        user_id: &str,
        user_type: Option<&str>,
        budget_id: &str,
        key: &str,
    ) -> Result<(), Status> {
        let c = self.entry_client()?;
        let mut c = c.lock().await;
        match c
            .void_transfer_leg(
                user_id,
                user_type,
                VoidTransferLegRequest {
                    budget_id: budget_id.into(),
                    idempotency_key: key.into(),
                },
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.code() == tonic::Code::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Reverses the local side effects of a transfer: the entry leg (if it exists) and the
    /// pocket movement (if it exists).
    ///
    /// Both steps are idempotent and share stable keys, so the request path and the reconciler
    /// can both run this without reversing the money twice. The pocket reversal uses the
    /// mirrored movement kind (`transfer_in` ↔ `transfer_out`) so `net_contributed` stays
    /// consistent: a compensated transfer cancels out instead of staying counted.
    #[allow(clippy::too_many_arguments)]
    async fn compensate_transfer(
        &self,
        user_id: &str,
        user_type: Option<&str>,
        std_budget: &str,
        fund_budget: &str,
        currency: &str,
        key: &str,
        group_id: &str,
    ) -> Result<(), String> {
        let mut errors: Vec<String> = Vec::new();
        if let Err(e) = self.void_leg(user_id, user_type, std_budget, key).await {
            errors.push(format!("void entry leg: {e}"));
        }
        match self.fund_repo.find_movement_by_key(fund_budget, key).await {
            Ok(Some(m)) if belongs_to(&m, group_id) => {
                let undo = NewFundMovement {
                    budget_id: fund_budget.to_string(),
                    currency: currency.to_string(),
                    kind: mirror_kind(&m.kind),
                    amount_minor: -m.amount_minor,
                    ref_type: Some("portfolio_transfer".into()),
                    ref_id: m.ref_id.clone(),
                    transfer_group_id: m.transfer_group_id.clone(),
                    idempotency_key: Some(compensation_key(key)),
                    actor_user_id: user_id.to_string(),
                    note: Some(format!("rollback of transfer {key}")),
                    occurred_on: m.occurred_on.clone(),
                };
                if let Err(e) = self.fund_repo.apply(&undo).await {
                    errors.push(format!("reverse pocket movement: {e}"));
                }
            }
            // No movement, or a movement of another transfer that reused the key: nothing of
            // ours to reverse.
            Ok(_) => {}
            Err(e) => errors.push(format!("read pocket movement: {e}")),
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    // -----------------------------------------------------------------------
    // Adjustments
    // -----------------------------------------------------------------------

    pub async fn adjust_fund_cash(
        &self,
        user_id: &str,
        req: &pb::AdjustFundCashRequest,
        user_type: Option<&str>,
    ) -> Result<pb::FundCashMovement, Status> {
        if req.amount_minor == 0 {
            return Err(Status::invalid_argument("amount must be non-zero"));
        }
        if req.note.trim().is_empty() {
            return Err(Status::invalid_argument("note is required for adjustments"));
        }
        if req.idempotency_key.trim().is_empty() || req.idempotency_key.len() > 60 {
            return Err(Status::invalid_argument(
                "idempotency_key is required (max 60 characters)",
            ));
        }
        if chrono::NaiveDate::parse_from_str(&req.occurred_on, "%Y-%m-%d").is_err() {
            return Err(Status::invalid_argument("occurred_on must be YYYY-MM-DD"));
        }
        self.assert_min_role(&req.budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let info = self.assert_fund(&req.budget_id).await?;
        let a = self
            .fund_repo
            .apply(&NewFundMovement {
                budget_id: req.budget_id.clone(),
                currency: info.currency,
                kind: pb::FundCashMovementKind::Adjustment,
                amount_minor: req.amount_minor,
                ref_type: None,
                ref_id: None,
                transfer_group_id: None,
                idempotency_key: Some(req.idempotency_key.clone()),
                actor_user_id: user_id.into(),
                note: Some(req.note.clone()),
                occurred_on: req.occurred_on.clone(),
            })
            .await
            .map_err(map_cash_err)?;
        Ok(map_movement(a.movement))
    }

    // -----------------------------------------------------------------------
    // Pocket-funded purchases and redemptions
    // -----------------------------------------------------------------------

    /// Debits the pocket for a purchase. Returns `None` when the request does not use the pocket.
    /// The caller must call [`refund_purchase`] if creating the asset then fails.
    pub async fn debit_for_purchase(
        &self,
        user_id: &str,
        budget_id: &str,
        funding_source: i32,
        amount: i64,
        idempotency_key: Option<&str>,
        note: &str,
    ) -> Result<Option<String>, Status> {
        if funding_source != pb::FundingSource::CashPocket as i32 {
            return Ok(None);
        }
        let key = idempotency_key.filter(|k| !k.is_empty()).ok_or_else(|| {
            Status::invalid_argument("idempotency_key is required when paying from the cash pocket")
        })?;
        let info = self.assert_fund(budget_id).await?;
        let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mv_key = format!("buy:{key}");
        self.fund_repo
            .apply(&NewFundMovement {
                budget_id: budget_id.into(),
                currency: info.currency,
                kind: pb::FundCashMovementKind::AssetPurchase,
                amount_minor: -amount,
                ref_type: Some("portfolio_asset".into()),
                ref_id: None,
                transfer_group_id: None,
                idempotency_key: Some(mv_key.clone()),
                actor_user_id: user_id.into(),
                note: Some(note.into()),
                occurred_on: day,
            })
            .await
            .map_err(map_cash_err)?;
        Ok(Some(mv_key))
    }

    pub async fn refund_purchase(&self, user_id: &str, budget_id: &str, amount: i64, mv_key: &str) {
        if let Ok(info) = self.budget_info(budget_id).await {
            let _ = self
                .fund_repo
                .apply(&NewFundMovement {
                    budget_id: budget_id.into(),
                    currency: info.currency,
                    kind: pb::FundCashMovementKind::Adjustment,
                    amount_minor: amount,
                    ref_type: Some("portfolio_asset".into()),
                    ref_id: None,
                    transfer_group_id: None,
                    idempotency_key: Some(format!("{mv_key}:undo")),
                    actor_user_id: user_id.into(),
                    note: Some("rollback: asset was not created".into()),
                    occurred_on: chrono::Utc::now().format("%Y-%m-%d").to_string(),
                })
                .await;
        }
    }

    /// Credits redemption proceeds (maturity, gold sale) to the pocket when asked.
    pub async fn credit_redemption(
        &self,
        user_id: &str,
        budget_id: &str,
        funding_source: i32,
        asset_id: &str,
        amount: i64,
        idempotency_key: Option<&str>,
    ) -> Result<(), Status> {
        if funding_source != pb::FundingSource::CashPocket as i32 || amount <= 0 {
            return Ok(());
        }
        let key = idempotency_key.filter(|k| !k.is_empty()).ok_or_else(|| {
            Status::invalid_argument("idempotency_key is required when crediting the cash pocket")
        })?;
        let info = self.assert_fund(budget_id).await?;
        self.fund_repo
            .apply(&NewFundMovement {
                budget_id: budget_id.into(),
                currency: info.currency,
                kind: pb::FundCashMovementKind::AssetRedemption,
                amount_minor: amount,
                ref_type: Some("portfolio_asset".into()),
                ref_id: Some(asset_id.into()),
                transfer_group_id: None,
                idempotency_key: Some(format!("redeem:{key}")),
                actor_user_id: user_id.into(),
                note: None,
                occurred_on: chrono::Utc::now().format("%Y-%m-%d").to_string(),
            })
            .await
            .map_err(map_cash_err)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Asset policy (I8)
    // -----------------------------------------------------------------------

    pub async fn enforce_asset_policy(
        &self,
        budget_id: &str,
        class: AssetClassNew,
    ) -> Result<(), Status> {
        let t = self.budget_info(budget_id).await?.budget_type;
        match policy::check(t, class, policy::enforce_invest_from_env()) {
            policy::PolicyVerdict::Allowed => Ok(()),
            policy::PolicyVerdict::Warn => {
                tracing::warn!(
                    "portfolio_policy_violation_total budget_type={:?} class={:?}",
                    t,
                    class
                );
                Ok(())
            }
            policy::PolicyVerdict::Denied => Err(Status::failed_precondition(format!(
                "{class:?} is not allowed in a {t:?} budget"
            ))),
        }
    }

    // -----------------------------------------------------------------------
    // Reconcile
    // -----------------------------------------------------------------------

    /// Rolls back fund transfers left in `requested` for more than `older_than_secs`: the entry leg
    /// is voided and any pocket movement reversed, then the transfer is marked failed. Safe to call
    /// repeatedly; every step is idempotent.
    ///
    /// A transfer is claimed before any money is touched, so a request that is still in flight (or
    /// that already completed) can never be rolled back by this sweep: the claim only succeeds
    /// while the row is still `requested`.
    pub async fn reconcile_stale_fund_transfers(
        &self,
        older_than_secs: i64,
    ) -> Result<usize, Status> {
        let stale = self
            .fund_repo
            .list_stale_requested(now_unix() - older_than_secs)
            .await
            .map_err(internal)?;
        let mut rolled_back = 0;
        for t in stale {
            let deposit = t.direction == "standard_to_invest";
            let (std_budget, fund_budget) = if deposit {
                (t.source_budget_id.clone(), t.counterparty_budget_id.clone())
            } else {
                (t.counterparty_budget_id.clone(), t.source_budget_id.clone())
            };
            let key = t.idempotency_key.clone().unwrap_or_default();
            if key.is_empty() {
                continue;
            }
            // Claim the transfer before touching the money. A transfer that already reached
            // `completed` cannot be claimed and is never voided here.
            match self
                .fund_repo
                .claim_transfer(&t.id)
                .await
                .map_err(internal)?
            {
                TransferClaim::Lost => continue,
                // `AlreadyClaimed`: a previous sweep claimed it but never finished — retry the
                // rollback.
                TransferClaim::Claimed | TransferClaim::AlreadyClaimed => {}
            }
            match self
                .compensate_transfer(
                    &t.actor_user_id,
                    None,
                    &std_budget,
                    &fund_budget,
                    &t.currency,
                    &key,
                    &t.group_id,
                )
                .await
            {
                Ok(()) => {
                    if self
                        .fund_repo
                        .mark_reconciled_failed(&t.id)
                        .await
                        .map_err(internal)?
                    {
                        rolled_back += 1;
                    }
                }
                Err(e) => {
                    // The rollback did not go through: keep the row claimed so the next sweep
                    // retries it. Never mark it failed: the money would be lost.
                    tracing::error!(
                        "fund reconcile compensation failed transfer_id={} idempotency_key={} err={}",
                        t.id,
                        key,
                        e
                    );
                }
            }
        }
        Ok(rolled_back)
    }
}

fn opt(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// The compensating movement of a transfer: `transfer_in` ↔ `transfer_out`.
///
/// `net_contributed` counts both kinds, so a mirrored pair cancels out and the ledger stays
/// honest after a rolled-back transfer. `adjustment` stays reserved for genuinely manual
/// adjustments.
fn mirror_kind(db_kind: &str) -> pb::FundCashMovementKind {
    match db_kind {
        "transfer_in" => pb::FundCashMovementKind::TransferOut,
        "transfer_out" => pb::FundCashMovementKind::TransferIn,
        _ => pb::FundCashMovementKind::Adjustment,
    }
}

/// True when the movement belongs to the given transfer group. Guards the compensation against
/// reversing a movement of another transfer that reused the same idempotency key.
fn belongs_to(m: &crate::converters::fund::DbFundCashMovement, group_id: &str) -> bool {
    m.transfer_group_id.as_deref() == Some(group_id) || m.ref_id.as_deref() == Some(group_id)
}

/// Idempotency key of the compensating movement of a transfer. The suffix is short enough to
/// stay inside the 64-char column even for the longest accepted request key.
fn compensation_key(key: &str) -> String {
    const SUFFIX: &str = ":undo";
    let room = 64 - SUFFIX.len();
    if key.chars().count() > room {
        let head: String = key.chars().take(room).collect();
        format!("{head}{SUFFIX}")
    } else {
        format!("{key}{SUFFIX}")
    }
}

fn worse_freshness(a: pb::PriceFreshness, b: pb::PriceFreshness) -> pb::PriceFreshness {
    let rank = |f: pb::PriceFreshness| match f {
        pb::PriceFreshness::Unpriced => 3,
        pb::PriceFreshness::Stale => 2,
        pb::PriceFreshness::Fresh => 1,
        pb::PriceFreshness::Unspecified => 0,
    };
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}
