//! Fixed deposit rollover.
//!
//! Phase 3.3 records a renewal of an existing fixed deposit: the
//! previous asset transitions to `ROLLED_OVER` and a new asset is
//! inserted with `rollover_from_asset_id` linking to its predecessor.
//! Money does not move — the rollover is a logical continuation of
//! the same principal, so we do not create a transfer record.
//!
//! Idempotency: the lifecycle check rejects re-rolling an asset that
//! is already in a terminal state (`ROLLED_OVER`, `WITHDRAWN`,
//! `ARCHIVED`, `EARLY_CLOSED`). A second call for the same
//! `rollover_from_asset_id` returns a `FAILED_PRECONDITION` error.
//!
//! Optional behavior:
//! - The new deposit inherits the previous one's currency, provider,
//!   and `rollover_from_asset_id`.
//! - `principal` defaults to the old principal; caller can override.
//! - `interest_method` is `SIMPLE` by default; new deposits can be
//!   `COMPOUND` if the bank allows.

use tonic::Status;
use uuid::Uuid;

use crate::converters::portfolio as pconv;
use crate::manager::biz::portfolio::biz::PortfolioBiz;
use crate::manager::biz::portfolio::lifecycle::Transition;
use crate::manager::biz::portfolio::AssetStatus;
use crate::manager::repository::portfolio::PortfolioRepository;
use crate::pb::service::portfolio as pb;
use philand_time::now_unix;

impl PortfolioBiz {
    /// Record a rollover. Creates a new fixed deposit asset linked to
    /// the previous one and transitions the old asset to `ROLLED_OVER`.
    /// Returns the new asset on success.
    pub async fn record_rollover(
        &self,
        user_id: &str,
        req: &pb::RecordFixedDepositRolloverRequest,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_min_role(
            &req.budget_id,
            user_id,
            crate::pb::service::budget::BudgetRole::Manager,
            user_type,
        )
        .await?;
        if req.budget_id.is_empty() || req.asset_id.is_empty() {
            return Err(Status::invalid_argument(
                "budget_id and asset_id are required",
            ));
        }
        if req.new_maturity_date <= today_business_date_epoch() {
            return Err(Status::invalid_argument(
                "new_maturity_date must be in the future",
            ));
        }
        if req.new_principal <= 0 {
            return Err(Status::invalid_argument("new_principal must be > 0"));
        }
        if req.new_annual_rate < 0.0 {
            return Err(Status::invalid_argument("new_annual_rate must be >= 0"));
        }

        // P16: bound the rollover chain depth. A new asset that
        // already inherits a 5-deep chain is rejected with a clear
        // message. The walk follows `rollover_from_asset_id` to the
        // root. If a cycle is detected (shouldn't happen, but
        // defence-in-depth), we treat it as depth = MAX+1.
        self.assert_rollover_chain_depth(&req.asset_id).await?;

        let mut tx = self.repo.begin().await.map_err(internal)?;
        let existing = self
            .repo
            .get_asset(&mut tx, &req.budget_id, &req.asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("asset not found"))?;
        let current = AssetStatus::from_db(&existing.status);
        if current != AssetStatus::Active {
            return Err(Status::failed_precondition(format!(
                "asset is in {} state; only active assets can be rolled over",
                existing.status
            )));
        }
        let old_fd = self
            .repo
            .get_fixed_deposit(&mut tx, &req.asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("old fixed deposit row not found"))?;

        // Create the new asset with the old asset's currency/provider.
        let new_asset =
            PortfolioBiz::make_rollover_asset(req, &existing, &old_fd, &self.repo, &mut tx).await?;

        // Transition the old asset to ROLLED_OVER with the rollover
        // timestamp. The lifecycle FSM rejects double-rollover.
        crate::manager::biz::portfolio::lifecycle::next_status(current, Transition::RollOver)
            .map_err(internal)?;
        self.repo
            .update_status(
                &mut tx,
                &req.budget_id,
                &req.asset_id,
                AssetStatus::RolledOver,
                Some(now_unix()),
            )
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            &req.budget_id,
            &req.asset_id,
            user_id,
            None,
            "ROLLED_OVER",
            &format!(
                r#"{{"new_asset_id":"{}","new_maturity_date":{}}}"#,
                new_asset.id, req.new_maturity_date
            ),
        )
        .await
        .map_err(internal)?;
        self.append_activity(
            &mut tx,
            &req.budget_id,
            &new_asset.id,
            user_id,
            None,
            "CREATED",
            &format!(
                r#"{{"rollover_from":"{}","principal":{}}}"#,
                req.asset_id, req.new_principal
            ),
        )
        .await
        .map_err(internal)?;
        tx.commit().await.map_err(internal)?;

        let mut tx2 = self.repo.begin().await.map_err(internal)?;
        let refreshed = self
            .repo
            .get_asset(&mut tx2, &req.budget_id, &new_asset.id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::internal("new asset not found after insert"))?;
        tx2.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(refreshed))
    }

    /// Build the rollover's `NewPortfolioAsset` and `NewFixedDeposit`
    /// values, insert the new asset + subtype row, and return the
    /// resulting `DbPortfolioAsset`. This is split out so the
    /// `create_fixed_deposit` path can also call it without rewriting
    /// the validation logic.
    async fn make_rollover_asset(
        req: &pb::RecordFixedDepositRolloverRequest,
        existing: &pconv::DbPortfolioAsset,
        old_fd: &pconv::DbFixedDeposit,
        repo: &PortfolioRepository,
        tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    ) -> Result<pconv::DbPortfolioAsset, Status> {
        let new_id = Uuid::new_v4().to_string();
        let now = now_unix();
        let asset_class = AssetClassNew::FixedDeposit;
        let new_asset = pconv::NewPortfolioAsset {
            id: Some(new_id.clone()),
            budget_id: existing.budget_id.clone(),
            asset_class,
            display_name: if req.new_display_name.is_empty() {
                existing.display_name.clone()
            } else {
                req.new_display_name.clone()
            },
            currency: existing.currency.clone(),
            opened_on: req.new_maturity_date, // proto has only new_maturity_date; use it
            closed_on: None,
            legacy_asset_id: None,
            notes: Some(format!("Rollover from {}", req.asset_id)),
            created_by: existing.created_by.clone(),
        };
        repo.insert_asset(tx, &new_asset).await.map_err(internal)?;

        // Proto has no new_interest_method; use simple by default and
        // honor the old deposit's choice when available.
        let interest_method = old_fd.interest_method.clone();
        let payout_type = old_fd.payout_type.clone();
        let auto_renewal = old_fd.auto_renewal_policy.clone();

        let new_fd = pconv::NewFixedDeposit {
            provider: old_fd.provider.clone(),
            product_name: old_fd.product_name.clone(),
            principal: req.new_principal,
            annual_rate: req.new_annual_rate.to_string(),
            interest_method,
            payout_type,
            deposit_date: req.new_maturity_date,
            maturity_date: req.new_maturity_date,
            auto_renewal_policy: auto_renewal,
            rollover_from_asset_id: Some(req.asset_id.clone()),
            certificate_reference_masked: old_fd.certificate_reference_masked.clone(),
            notes: Some(format!("Rollover from {}", req.asset_id)),
        };
        repo.insert_fixed_deposit(tx, &new_id, &new_fd)
            .await
            .map_err(internal)?;
        Ok(pconv::DbPortfolioAsset {
            id: new_id,
            budget_id: existing.budget_id.clone(),
            asset_class: asset_class_to_db(&asset_class).to_string(),
            display_name: new_asset.display_name.clone(),
            currency: existing.currency.clone(),
            status: AssetStatus::Active.to_db().to_string(),
            opened_on: req.new_maturity_date,
            closed_on: None,
            legacy_asset_id: None,
            notes: new_asset.notes.clone(),
            created_by: existing.created_by.clone(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
        })
    }

    /// P16: bound the rollover chain depth. Walk the parent chain
    /// via `rollover_from_asset_id` to count how deep this asset is.
    /// Reject if depth exceeds `MAX_ROLLOVER_CHAIN_DEPTH`. Defends
    /// against runaway rollover chains (a user clicking "rollover"
    /// many times in a row would otherwise create an unbounded chain).
    async fn assert_rollover_chain_depth(&self, asset_id: &str) -> Result<(), Status> {
        const MAX_ROLLOVER_CHAIN_DEPTH: i32 = 5;
        let mut current = asset_id.to_string();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        for depth in 0..MAX_ROLLOVER_CHAIN_DEPTH {
            if !visited.insert(current.clone()) {
                return Err(Status::failed_precondition(format!(
                    "rollover chain depth cycle detected at depth {depth}; refusing to roll over"
                )));
            }
            let row: Option<Option<String>> = sqlx::query_scalar(
                "SELECT rollover_from_asset_id FROM portfolio_fixed_deposits WHERE asset_id = ?",
            )
            .bind(&current)
            .fetch_optional(self.repo.pool())
            .await
            .map_err(internal)?;
            let parent = match row {
                Some(Some(p)) => p,
                _ => return Ok(()), // no parent → this is the root
            };
            current = parent;
        }
        // Loop completed without finding a root → depth >= MAX.
        Err(Status::failed_precondition(format!(
            "rollover chain depth exceeds {MAX_ROLLOVER_CHAIN_DEPTH}; refusing to roll over"
        )))
    }
}

fn asset_class_to_db(c: &AssetClassNew) -> &'static str {
    match c {
        AssetClassNew::SavingsAccount => "savings_account",
        AssetClassNew::FixedDeposit => "fixed_deposit",
        AssetClassNew::GoldLot => "gold_lot",
        AssetClassNew::StockLot => "stock_lot",
        AssetClassNew::EtfLot => "etf_lot",
        AssetClassNew::CryptoLot => "crypto_lot",
    }
}

/// Convert ETF underlying-index proto enum value to its lowercase
/// DB string. Unknown values fall back to `other` (matching the
/// converter's reading direction).
pub fn map_underlying_index_to_db(v: i32) -> String {
    use crate::pb::service::portfolio::EtfUnderlyingIndex as E;
    match E::try_from(v).unwrap_or(E::Other) {
        E::Vn30 => "vn30".to_string(),
        E::Vn100 => "vn100".to_string(),
        E::Hnx30 => "hnx30".to_string(),
        E::Other => "other".to_string(),
        _ => "other".to_string(),
    }
}

/// Convert crypto-network proto enum value to its lowercase DB
/// string. Unknown values fall back to `other`.
pub fn map_crypto_network_to_db(v: i32) -> String {
    use crate::pb::service::portfolio::CryptoNetwork as N;
    match N::try_from(v).unwrap_or(N::Other) {
        N::Bitcoin => "bitcoin".to_string(),
        N::Ethereum => "ethereum".to_string(),
        N::Solana => "solana".to_string(),
        N::BnbChain => "bnb_chain".to_string(),
        N::Polkadot => "polkadot".to_string(),
        N::Other => "other".to_string(),
        _ => "other".to_string(),
    }
}

fn today_business_date_epoch() -> i64 {
    let now_utc = chrono::Utc::now();
    let ict = now_utc + chrono::Duration::hours(7);
    ict.timestamp() - (ict.timestamp() % 86_400)
}

// Re-export so the local AssetClassNew / asset_class_to_db symbols
// resolve. (AssetClass lives in `manager::biz::portfolio`.)
use crate::converters::portfolio::AssetClassNew;

fn internal<E: ToString>(e: E) -> Status {
    Status::internal(e.to_string())
}
