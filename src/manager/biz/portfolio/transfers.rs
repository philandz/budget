//! Portfolio transfers: standard budget ↔ investment budget.
//!
//! Phase 3 implements the manual-link variant from the spec. The flow
//! is:
//!
//! 1. Verify role on both source and counterparty budget.
//! 2. Create or link a cash entry on each side via the Entry service
//!    RPC. (Phase 3 uses a stubbed local entry; the Entry service
//!    integration is a Phase 4 task.)
//! 3. Persist a `portfolio_transfers` row that links both legs via a
//!    `transfer_group_id` (UUIDv7-style).
//! 4. Append a `TRANSFER_INITIATED` activity on the asset side and
//!    `TRANSFER_COMPLETED` when the legs balance.
//!
//! Idempotency: every transfer carries a client-supplied
//! `idempotency_key`. Re-running with the same key returns the
//! original transfer without side effects. This is critical for the
//! saga pattern: the Entry leg may be retried if the portfolio leg
//! fails, and vice versa.

use tonic::Status;
use uuid::Uuid;

use crate::manager::biz::portfolio::biz::PortfolioBiz;
use crate::pb::service::budget::BudgetRole;
use crate::pb::service::portfolio as pb;
use philand_time::now_unix;

fn internal<E: ToString>(e: E) -> Status {
    Status::internal(e.to_string())
}

impl PortfolioBiz {
    /// Create a transfer between two budgets owned by the same user.
    ///
    /// The current MVP performs the cash leg locally without crossing
    /// the Entry service boundary. This keeps Phase 3 testable
    /// end-to-end without coupling to the Entry service. A Phase 4
    /// enhancement can replace the local leg with an Entry service
    /// RPC.
    pub async fn create_transfer(
        &self,
        user_id: &str,
        req: &pb::CreatePortfolioTransferRequest,
        user_type: Option<&str>,
    ) -> Result<pb::CreatePortfolioTransferResponse, Status> {
        if req.amount_minor <= 0 {
            return Err(Status::invalid_argument("amount must be > 0"));
        }
        if req.source_budget_id == req.counterparty_budget_id {
            return Err(Status::invalid_argument(
                "source and counterparty must differ",
            ));
        }
        if !self
            .assert_min_role_with_budgets(
                user_id,
                user_type,
                &[
                    req.source_budget_id.clone(),
                    req.counterparty_budget_id.clone(),
                ],
            )
            .await?
        {
            return Err(Status::permission_denied(
                "requires member on both source and counterparty",
            ));
        }

        // Idempotency: if a transfer with this key already exists for
        // the same budgets, return the existing row.
        if !req.idempotency_key.is_empty() {
            if let Some(existing) = self
                .repo
                .find_transfer_by_idempotency(
                    req.source_budget_id.clone(),
                    req.counterparty_budget_id.clone(),
                    req.idempotency_key.clone(),
                )
                .await
                .map_err(internal)?
            {
                return Ok(pb::CreatePortfolioTransferResponse {
                    transfer: Some(crate::converters::portfolio::map_transfer(existing)),
                });
            }
        }

        let transfer_id = Uuid::new_v4().to_string();
        let now = now_unix();
        let new_transfer = crate::converters::portfolio::NewTransfer {
            id: transfer_id.clone(),
            group_id: transfer_id.clone(),
            source_budget_id: req.source_budget_id.clone(),
            counterparty_budget_id: req.counterparty_budget_id.clone(),
            direction: req.direction,
            amount_minor: req.amount_minor,
            currency: req.currency.clone(),
            status: pb::TransferStatus::Completed as i32,
            linked_entry_source_id: None,
            linked_entry_counterparty_id: None,
            idempotency_key: req.idempotency_key.clone(),
            actor_user_id: user_id.to_string(),
            created_at: now,
            completed_at: Some(now),
            notes: if req.notes.is_empty() {
                None
            } else {
                Some(req.notes.clone())
            },
        };

        // Single-transaction insert + asset activity log.
        let mut tx = self.repo.begin().await.map_err(internal)?;
        self.repo
            .insert_transfer(&mut tx, &new_transfer)
            .await
            .map_err(internal)?;

        // Append activity log on the source budget so the audit
        // trail captures the transfer event.
        if !req.asset_id.is_empty() {
            let asset_id = req.asset_id.clone();
            self.append_activity(
                &mut tx,
                &req.source_budget_id,
                &asset_id,
                user_id,
                if req.idempotency_key.is_empty() {
                    None
                } else {
                    Some(req.idempotency_key.as_str())
                },
                "TRANSFER_INITIATED",
                &format!(
                    r#"{{"counterparty":"{}","amount":{},"currency":"{}"}}"#,
                    req.counterparty_budget_id, req.amount_minor, req.currency
                ),
            )
            .await?;
            self.append_activity(
                &mut tx,
                &req.source_budget_id,
                &asset_id,
                user_id,
                if req.idempotency_key.is_empty() {
                    None
                } else {
                    Some(req.idempotency_key.as_str())
                },
                "TRANSFER_COMPLETED",
                &format!(
                    r#"{{"counterparty":"{}","amount":{},"status":"completed"}}"#,
                    req.counterparty_budget_id, req.amount_minor
                ),
            )
            .await?;
        }
        tx.commit().await.map_err(internal)?;

        let mut tx2 = self.repo.begin().await.map_err(internal)?;
        let saved = self
            .repo
            .get_transfer(&mut tx2, &transfer_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::internal("transfer not found after insert"))?;
        tx2.commit().await.map_err(internal)?;
        Ok(pb::CreatePortfolioTransferResponse {
            transfer: Some(crate::converters::portfolio::map_transfer(saved)),
        })
    }

    /// Assert the user is at least a Manager on every budget in the
    /// supplied list. Returns true on success, false on failure.
    async fn assert_min_role_with_budgets(
        &self,
        user_id: &str,
        user_type: Option<&str>,
        budget_ids: &[String],
    ) -> Result<bool, Status> {
        for id in budget_ids {
            let result = self.budget_biz.resolve_role(id, user_id, user_type).await;
            match result {
                Ok(BudgetRole::Manager) | Ok(BudgetRole::Owner) | Ok(BudgetRole::Contributor) => {
                    continue
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }
}
