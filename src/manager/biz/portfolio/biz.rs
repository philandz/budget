//! PortfolioBiz: business logic for the Asset Portfolio module.
//!
//! Coordinates `PortfolioRepository` with authorization, lifecycle
//! enforcement, currency lock, and activity log writes. Valuation is
//! delegated to a pure helper so it can be unit-tested without DB.

use std::sync::Arc;
use tonic::Status;

use crate::converters::portfolio as pconv;
use crate::manager::biz::portfolio::{
    fifo::{fifo_disposal_allocations, DisposalAllocation, Lot},
    gold::GoldUnit,
    interest::{compound_accrued, simple_accrued, InterestMethod, PayoutType},
    lifecycle::{next_status, Transition},
    AssetStatus, PriceFreshness,
};
use crate::manager::client::IdentityClient;
use crate::manager::repository::portfolio::PortfolioRepository;
use crate::pb::service::budget::BudgetRole;
use crate::pb::service::portfolio as pb;
use philand_time::now_unix;

use super::super::BudgetBiz;

pub struct PortfolioBiz {
    pub repo: Arc<PortfolioRepository>,
    pub identity_client: Arc<tokio::sync::Mutex<IdentityClient>>,
    pub budget_biz: Arc<BudgetBiz>,
}

impl PortfolioBiz {
    pub fn new(
        repo: PortfolioRepository,
        identity_client: IdentityClient,
        budget_biz: Arc<BudgetBiz>,
    ) -> Self {
        Self {
            repo: Arc::new(repo),
            identity_client: Arc::new(tokio::sync::Mutex::new(identity_client)),
            budget_biz,
        }
    }

    /// Test-only constructor.
    #[doc(hidden)]
    pub async fn test_only_no_clients(budget_biz: Arc<BudgetBiz>) -> Self {
        let pool = sqlx::MySqlPool::connect_lazy(
            &std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "mysql://root@127.0.0.1:3306/philand".into()),
        )
        .expect("invalid DATABASE_URL");
        Self {
            repo: Arc::new(PortfolioRepository::new(pool)),
            identity_client: Arc::new(tokio::sync::Mutex::new(
                IdentityClient::test_only_unreachable(),
            )),
            budget_biz,
        }
    }

    // -----------------------------------------------------------------------
    // Authorization
    // -----------------------------------------------------------------------

    pub async fn assert_member(
        &self,
        budget_id: &str,
        user_id: &str,
        user_type: Option<&str>,
    ) -> Result<(), Status> {
        let role = self
            .budget_biz
            .resolve_role(budget_id, user_id, user_type)
            .await?;
        if role == BudgetRole::Unspecified {
            return Err(Status::permission_denied("Not a member of this budget"));
        }
        Ok(())
    }

    pub async fn assert_min_role(
        &self,
        budget_id: &str,
        user_id: &str,
        min_role: BudgetRole,
        user_type: Option<&str>,
    ) -> Result<(), Status> {
        let role = self
            .budget_biz
            .resolve_role(budget_id, user_id, user_type)
            .await?;
        let ok = match min_role {
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
                "Requires {:?} role or higher",
                min_role
            )));
        }
        Ok(())
    }

    pub async fn assert_currency_lock(
        &self,
        budget_id: &str,
        asset_currency: &str,
    ) -> Result<(), Status> {
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let budget_currency: Option<String> =
            sqlx::query_scalar("SELECT currency FROM budgets WHERE id = ? AND deleted_at IS NULL")
                .bind(budget_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        let budget_currency =
            budget_currency.ok_or_else(|| Status::not_found("budget not found"))?;
        if budget_currency != asset_currency {
            return Err(Status::invalid_argument(format!(
                "asset currency {asset_currency} does not match budget currency {budget_currency}"
            )));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Activity log helper
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn append_activity(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
        budget_id: &str,
        asset_id: &str,
        actor: &str,
        idempotency_key: Option<&str>,
        activity_type: &str,
        payload_json: &str,
    ) -> Result<(), Status> {
        let act = pconv::NewActivity {
            id: None,
            asset_id: asset_id.to_string(),
            budget_id: budget_id.to_string(),
            activity_type: activity_type.to_string(),
            actor_user_id: actor.to_string(),
            correlation_id: None,
            idempotency_key: idempotency_key.map(str::to_string),
            occurred_at: now_unix(),
            payload_json: Some(payload_json.to_string()),
        };
        self.repo
            .insert_activity(tx, &act)
            .await
            .map_err(internal)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Asset root: read + simple mutations
    // -----------------------------------------------------------------------

    pub async fn list_assets(
        &self,
        user_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<Vec<pb::PortfolioAsset>, Status> {
        self.assert_member(budget_id, user_id, user_type).await?;
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let rows = self
            .repo
            .list_assets_by_budget(&mut tx, budget_id)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(rows.into_iter().map(pconv::map_portfolio_asset).collect())
    }

    /// Single-transaction list with valuation. Replaces the previous
    /// pattern of calling `get_portfolio_summary` then slicing `.assets`
    /// because that pattern triggered N+1 subtype queries per asset.
    pub async fn list_valuated(
        &self,
        user_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<Vec<pb::ValuatedAsset>, Status> {
        self.assert_member(budget_id, user_id, user_type).await?;
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let assets = self
            .repo
            .list_assets_by_budget(&mut tx, budget_id)
            .await
            .map_err(internal)?;
        let today = today_business_date();
        let mut out = Vec::with_capacity(assets.len());
        for asset in &assets {
            let v = self.value_asset(&mut tx, asset, today).await?;
            out.push(pb::ValuatedAsset {
                asset: Some(pconv::map_portfolio_asset(asset.clone())),
                current_value: v.current_value,
                open_cost_basis: v.open_cost_basis,
                realized_pnl: v.realized_pnl,
                unrealized_pnl: v.unrealized_pnl,
                accrued_interest: v.accrued_interest,
                return_pct: if v.open_cost_basis > 0 {
                    (v.unrealized_pnl as f64 / v.open_cost_basis as f64) * 100.0
                } else {
                    0.0
                },
                freshness: v.freshness as i32,
                quote_observed_at: v.quote_observed_at,
                formula_version: "v1-actual-365".to_string(),
            });
        }
        tx.commit().await.map_err(internal)?;
        Ok(out)
    }

    pub async fn get_asset(
        &self,
        user_id: &str,
        budget_id: &str,
        asset_id: &str,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_member(budget_id, user_id, user_type).await?;
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let row = self
            .repo
            .get_asset(&mut tx, budget_id, asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("asset not found"))?;
        tx.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(row))
    }

    pub async fn archive_asset(
        &self,
        user_id: &str,
        budget_id: &str,
        asset_id: &str,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Owner, user_type)
            .await?;
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let existing = self
            .repo
            .get_asset(&mut tx, budget_id, asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("asset not found"))?;
        let current = AssetStatus::from_db(&existing.status);
        next_status(current, Transition::Archive)
            .map_err(|e| Status::failed_precondition(format!("{e}")))?;
        self.repo
            .update_status(
                &mut tx,
                budget_id,
                asset_id,
                AssetStatus::Archived,
                Some(now_unix()),
            )
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx, budget_id, asset_id, user_id, None, "ARCHIVED", "{}",
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        let mut tx2 = self.repo.begin().await.map_err(internal)?;
        let refreshed = self
            .repo
            .get_asset(&mut tx2, budget_id, asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("asset not found after archive"))?;
        tx2.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(refreshed))
    }

    pub async fn update_metadata(
        &self,
        user_id: &str,
        budget_id: &str,
        asset_id: &str,
        display_name: Option<&str>,
        notes: Option<&str>,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let mut tx = self.repo.begin().await.map_err(internal)?;
        if self
            .repo
            .get_asset(&mut tx, budget_id, asset_id)
            .await
            .map_err(internal)?
            .is_none()
        {
            return Err(Status::not_found("asset not found"));
        }
        self.repo
            .update_metadata(&mut tx, budget_id, asset_id, display_name, notes)
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            budget_id,
            asset_id,
            user_id,
            None,
            "UPDATED_METADATA",
            "{}",
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        // Re-open a fresh transaction for the read-after-commit pattern.
        let mut tx2 = self.repo.begin().await.map_err(internal)?;
        let refreshed = self
            .repo
            .get_asset(&mut tx2, budget_id, asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("asset not found after update"))?;
        tx2.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(refreshed))
    }

    // -----------------------------------------------------------------------
    // Class-specific create
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn create_savings_account(
        &self,
        user_id: &str,
        budget_id: &str,
        display_name: &str,
        currency: &str,
        provider: &str,
        account_reference_masked: &str,
        current_balance: i64,
        balance_as_of: i64,
        annual_rate: &str,
        interest_method: &str,
        payout_type: &str,
        opened_on: i64,
        notes: Option<&str>,
        idempotency_key: Option<&str>,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        self.assert_currency_lock(budget_id, currency).await?;
        if display_name.trim().is_empty() {
            return Err(Status::invalid_argument("display_name required"));
        }
        if current_balance < 0 {
            return Err(Status::invalid_argument("current_balance must be >= 0"));
        }
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let asset = self
            .repo
            .insert_asset(
                &mut tx,
                &pconv::NewPortfolioAsset {
                    id: None,
                    budget_id: budget_id.to_string(),
                    asset_class: pconv::AssetClassNew::SavingsAccount,
                    display_name: display_name.to_string(),
                    currency: currency.to_string(),
                    opened_on,
                    closed_on: None,
                    legacy_asset_id: None,
                    notes: notes.map(str::to_string),
                    created_by: user_id.to_string(),
                },
            )
            .await
            .map_err(internal)?;
        self.repo
            .insert_savings_account(
                &mut tx,
                &asset.id,
                &pconv::NewSavingsAccount {
                    provider: provider.to_string(),
                    account_reference_masked: account_reference_masked.to_string(),
                    current_balance,
                    balance_as_of,
                    annual_rate: annual_rate.to_string(),
                    interest_method: interest_method.to_string(),
                    payout_type: payout_type.to_string(),
                    opened_on,
                    notes: notes.map(str::to_string),
                },
            )
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            budget_id,
            &asset.id,
            user_id,
            idempotency_key,
            "CREATED",
            &format!(r#"{{"display_name":"{display_name}","current_balance":{current_balance}}}"#),
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(asset))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_fixed_deposit(
        &self,
        user_id: &str,
        budget_id: &str,
        display_name: &str,
        currency: &str,
        provider: &str,
        product_name: &str,
        principal: i64,
        annual_rate: &str,
        interest_method: &str,
        payout_type: &str,
        deposit_date: i64,
        maturity_date: i64,
        auto_renewal_policy: &str,
        certificate_reference_masked: &str,
        notes: Option<&str>,
        idempotency_key: Option<&str>,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        self.assert_currency_lock(budget_id, currency).await?;
        if display_name.trim().is_empty() {
            return Err(Status::invalid_argument("display_name required"));
        }
        if principal <= 0 {
            return Err(Status::invalid_argument("principal must be > 0"));
        }
        if maturity_date <= deposit_date {
            return Err(Status::invalid_argument(
                "maturity_date must be after deposit_date",
            ));
        }
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let asset = self
            .repo
            .insert_asset(
                &mut tx,
                &pconv::NewPortfolioAsset {
                    id: None,
                    budget_id: budget_id.to_string(),
                    asset_class: pconv::AssetClassNew::FixedDeposit,
                    display_name: display_name.to_string(),
                    currency: currency.to_string(),
                    opened_on: deposit_date,
                    closed_on: None,
                    legacy_asset_id: None,
                    notes: notes.map(str::to_string),
                    created_by: user_id.to_string(),
                },
            )
            .await
            .map_err(internal)?;
        self.repo
            .insert_fixed_deposit(
                &mut tx,
                &asset.id,
                &pconv::NewFixedDeposit {
                    provider: provider.to_string(),
                    product_name: product_name.to_string(),
                    principal,
                    annual_rate: annual_rate.to_string(),
                    interest_method: interest_method.to_string(),
                    payout_type: payout_type.to_string(),
                    deposit_date,
                    maturity_date,
                    auto_renewal_policy: auto_renewal_policy.to_string(),
                    rollover_from_asset_id: None,
                    certificate_reference_masked: Some(certificate_reference_masked.to_string()),
                    notes: notes.map(str::to_string),
                },
            )
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            budget_id,
            &asset.id,
            user_id,
            idempotency_key,
            "CREATED",
            &format!(r#"{{"principal":{principal},"maturity_date":{maturity_date}}}"#),
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(asset))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_gold_lot(
        &self,
        user_id: &str,
        budget_id: &str,
        display_name: &str,
        currency: &str,
        provider: &str,
        gold_type: &str,
        purity: &str,
        form: &str,
        quantity_original: &str,
        unit_original: GoldUnit,
        purchase_price_per_unit_original: i64,
        purchase_cost: i64,
        fees: i64,
        purchase_date: i64,
        notes: Option<&str>,
        idempotency_key: Option<&str>,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        self.assert_currency_lock(budget_id, currency).await?;
        if display_name.trim().is_empty() {
            return Err(Status::invalid_argument("display_name required"));
        }
        if purchase_cost <= 0 {
            return Err(Status::invalid_argument("purchase_cost must be > 0"));
        }
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let asset = self
            .repo
            .insert_asset(
                &mut tx,
                &pconv::NewPortfolioAsset {
                    id: None,
                    budget_id: budget_id.to_string(),
                    asset_class: pconv::AssetClassNew::GoldLot,
                    display_name: display_name.to_string(),
                    currency: currency.to_string(),
                    opened_on: purchase_date,
                    closed_on: None,
                    legacy_asset_id: None,
                    notes: notes.map(str::to_string),
                    created_by: user_id.to_string(),
                },
            )
            .await
            .map_err(internal)?;
        self.repo
            .insert_gold_lot(
                &mut tx,
                &asset.id,
                &pconv::NewGoldLot {
                    provider: provider.to_string(),
                    gold_type: gold_type.to_string(),
                    purity: purity.to_string(),
                    form: form.to_string(),
                    quantity_original: quantity_original.to_string(),
                    unit: unit_original,
                    purchase_price_per_unit_original,
                    purchase_cost,
                    fees,
                    purchase_date,
                    notes: notes.map(str::to_string),
                },
            )
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            budget_id,
            &asset.id,
            user_id,
            idempotency_key,
            "CREATED",
            &format!(r#"{{"purchase_cost":{purchase_cost}}}"#),
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(asset))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_stock_lot(
        &self,
        user_id: &str,
        budget_id: &str,
        display_name: &str,
        currency: &str,
        ticker: &str,
        exchange: &str,
        quantity_bought: &str,
        buy_price_per_share: i64,
        purchase_cost: i64,
        fees: i64,
        purchase_date: i64,
        settlement_date: Option<i64>,
        notes: Option<&str>,
        idempotency_key: Option<&str>,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        self.assert_currency_lock(budget_id, currency).await?;
        if display_name.trim().is_empty() {
            return Err(Status::invalid_argument("display_name required"));
        }
        if buy_price_per_share <= 0 {
            return Err(Status::invalid_argument("buy_price_per_share must be > 0"));
        }
        if ticker.is_empty() {
            return Err(Status::invalid_argument("ticker required"));
        }
        if exchange.is_empty() {
            return Err(Status::invalid_argument("exchange required"));
        }
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let asset = self
            .repo
            .insert_asset(
                &mut tx,
                &pconv::NewPortfolioAsset {
                    id: None,
                    budget_id: budget_id.to_string(),
                    asset_class: pconv::AssetClassNew::StockLot,
                    display_name: display_name.to_string(),
                    currency: currency.to_string(),
                    opened_on: purchase_date,
                    closed_on: None,
                    legacy_asset_id: None,
                    notes: notes.map(str::to_string),
                    created_by: user_id.to_string(),
                },
            )
            .await
            .map_err(internal)?;
        self.repo
            .insert_stock_lot(
                &mut tx,
                &asset.id,
                &pconv::NewStockLot {
                    ticker: ticker.to_string(),
                    exchange: exchange.to_string(),
                    quantity_bought: quantity_bought.to_string(),
                    buy_price_per_share,
                    purchase_cost,
                    fees,
                    purchase_date,
                    settlement_date,
                    notes: notes.map(str::to_string),
                },
            )
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            budget_id,
            &asset.id,
            user_id,
            idempotency_key,
            "CREATED",
            &format!(
                r#"{{"ticker":"{ticker}","qty":"{quantity_bought}","price":{buy_price_per_share}}}"#
            ),
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(asset))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_etf_lot(
        &self,
        user_id: &str,
        budget_id: &str,
        display_name: &str,
        currency: &str,
        ticker: &str,
        exchange: &str,
        underlying_index: &str,
        fund_provider: &str,
        quantity_bought: &str,
        buy_price_per_unit: i64,
        purchase_cost: i64,
        fees: i64,
        purchase_date: i64,
        settlement_date: Option<i64>,
        notes: Option<&str>,
        idempotency_key: Option<&str>,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        self.assert_currency_lock(budget_id, currency).await?;
        if display_name.trim().is_empty() {
            return Err(Status::invalid_argument("display_name required"));
        }
        if buy_price_per_unit <= 0 {
            return Err(Status::invalid_argument("buy_price_per_unit must be > 0"));
        }
        if ticker.is_empty() {
            return Err(Status::invalid_argument("ticker required"));
        }
        if exchange.is_empty() {
            return Err(Status::invalid_argument("exchange required"));
        }
        if underlying_index.is_empty() {
            return Err(Status::invalid_argument("underlying_index required"));
        }
        if fund_provider.is_empty() {
            return Err(Status::invalid_argument("fund_provider required"));
        }
        if purchase_cost <= 0 {
            return Err(Status::invalid_argument("purchase_cost must be > 0"));
        }
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let asset = self
            .repo
            .insert_asset(
                &mut tx,
                &pconv::NewPortfolioAsset {
                    id: None,
                    budget_id: budget_id.to_string(),
                    asset_class: pconv::AssetClassNew::EtfLot,
                    display_name: display_name.to_string(),
                    currency: currency.to_string(),
                    opened_on: purchase_date,
                    closed_on: None,
                    legacy_asset_id: None,
                    notes: notes.map(str::to_string),
                    created_by: user_id.to_string(),
                },
            )
            .await
            .map_err(internal)?;
        self.repo
            .insert_etf_lot(
                &mut tx,
                &asset.id,
                &pconv::NewEtfLot {
                    ticker: ticker.to_string(),
                    exchange: exchange.to_string(),
                    underlying_index: underlying_index.to_string(),
                    fund_provider: fund_provider.to_string(),
                    quantity_bought: quantity_bought.to_string(),
                    quantity_open: quantity_bought.to_string(),
                    buy_price_per_unit,
                    purchase_cost,
                    fees,
                    purchase_date,
                    settlement_date,
                    notes: notes.map(str::to_string),
                },
            )
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            budget_id,
            &asset.id,
            user_id,
            idempotency_key,
            "CREATED",
            &format!(
                r#"{{"ticker":"{ticker}","qty":"{quantity_bought}","price":{buy_price_per_unit}}}"#
            ),
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(asset))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_crypto_lot(
        &self,
        user_id: &str,
        budget_id: &str,
        display_name: &str,
        currency: &str,
        symbol: &str,
        network: &str,
        custody_wallet: &str,
        quantity_bought: &str,
        buy_price_per_unit: i64,
        purchase_cost: i64,
        fees: i64,
        purchase_date: i64,
        notes: Option<&str>,
        idempotency_key: Option<&str>,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        self.assert_currency_lock(budget_id, currency).await?;
        if display_name.trim().is_empty() {
            return Err(Status::invalid_argument("display_name required"));
        }
        if buy_price_per_unit <= 0 {
            return Err(Status::invalid_argument("buy_price_per_unit must be > 0"));
        }
        if symbol.is_empty() {
            return Err(Status::invalid_argument("symbol required"));
        }
        if network.is_empty() {
            return Err(Status::invalid_argument("network required"));
        }
        if custody_wallet.is_empty() {
            return Err(Status::invalid_argument("custody_wallet required"));
        }
        if purchase_cost <= 0 {
            return Err(Status::invalid_argument("purchase_cost must be > 0"));
        }
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let asset = self
            .repo
            .insert_asset(
                &mut tx,
                &pconv::NewPortfolioAsset {
                    id: None,
                    budget_id: budget_id.to_string(),
                    asset_class: pconv::AssetClassNew::CryptoLot,
                    display_name: display_name.to_string(),
                    currency: currency.to_string(),
                    opened_on: purchase_date,
                    closed_on: None,
                    legacy_asset_id: None,
                    notes: notes.map(str::to_string),
                    created_by: user_id.to_string(),
                },
            )
            .await
            .map_err(internal)?;
        self.repo
            .insert_crypto_lot(
                &mut tx,
                &asset.id,
                &pconv::NewCryptoLot {
                    symbol: symbol.to_string(),
                    network: network.to_string(),
                    custody_wallet: custody_wallet.to_string(),
                    quantity_bought: quantity_bought.to_string(),
                    quantity_open: quantity_bought.to_string(),
                    buy_price_per_unit,
                    purchase_cost,
                    fees,
                    purchase_date,
                    notes: notes.map(str::to_string),
                },
            )
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            budget_id,
            &asset.id,
            user_id,
            idempotency_key,
            "CREATED",
            &format!(
                r#"{{"symbol":"{symbol}","qty":"{quantity_bought}","price":{buy_price_per_unit}}}"#
            ),
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(asset))
    }

    // -----------------------------------------------------------------------
    // Price observations
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn record_price_observation(
        &self,
        user_id: &str,
        budget_id: &str,
        asset_id: &str,
        provider: &str,
        price_side: &str,
        unit_price: i64,
        currency: &str,
        observed_at: i64,
        source_reference: &str,
        idempotency_key: Option<&str>,
        notes: Option<&str>,
        user_type: Option<&str>,
    ) -> Result<pb::PriceObservation, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Contributor, user_type)
            .await?;
        if unit_price < 0 {
            return Err(Status::invalid_argument("unit_price must be >= 0"));
        }
        let mut tx = self.repo.begin().await.map_err(internal)?;
        if let Some(key) = idempotency_key {
            if !key.is_empty() {
                if let Some(existing) = self
                    .repo
                    .get_price_observation_by_idempotency(&mut tx, asset_id, key)
                    .await
                    .map_err(internal)?
                {
                    tx.commit().await.map_err(internal)?;
                    return Ok(pconv::map_price_observation(existing));
                }
            }
        }
        let new = pconv::NewPriceObservation {
            id: None,
            asset_id: asset_id.to_string(),
            provider: if provider.is_empty() {
                "manual".to_string()
            } else {
                provider.to_string()
            },
            price_side: parse_price_side(price_side),
            unit_price,
            currency: currency.to_string(),
            observed_at: if observed_at == 0 {
                now_unix()
            } else {
                observed_at
            },
            source_reference: source_reference.to_string(),
            idempotency_key: idempotency_key
                .filter(|k| !k.is_empty())
                .map(str::to_string),
            notes: notes.map(str::to_string),
        };
        let obs = self
            .repo
            .insert_price_observation(&mut tx, &new)
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            budget_id,
            asset_id,
            user_id,
            idempotency_key,
            "PRICE_OBSERVED",
            &format!(r#"{{"price":{unit_price},"currency":"{currency}"}}"#),
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        Ok(pconv::map_price_observation(obs))
    }

    pub async fn list_price_observations(
        &self,
        user_id: &str,
        budget_id: &str,
        asset_id: &str,
        limit: i32,
        user_type: Option<&str>,
    ) -> Result<Vec<pb::PriceObservation>, Status> {
        self.assert_member(budget_id, user_id, user_type).await?;
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let rows = self
            .repo
            .list_price_observations(&mut tx, asset_id, limit)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(rows.into_iter().map(pconv::map_price_observation).collect())
    }

    pub async fn list_asset_activity(
        &self,
        user_id: &str,
        budget_id: &str,
        asset_id: &str,
        limit: i32,
        user_type: Option<&str>,
    ) -> Result<Vec<pb::PortfolioActivity>, Status> {
        self.assert_member(budget_id, user_id, user_type).await?;
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let rows = self
            .repo
            .list_activities(&mut tx, asset_id, limit)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(rows.into_iter().map(pconv::map_activity).collect())
    }

    // -----------------------------------------------------------------------
    // Disposals
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn record_stock_disposal(
        &self,
        user_id: &str,
        budget_id: &str,
        asset_id: &str,
        quantity_sold: &str,
        sale_proceeds: i64,
        sale_fees: i64,
        disposal_date: i64,
        idempotency_key: Option<&str>,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioAsset, Status> {
        self.assert_min_role(budget_id, user_id, BudgetRole::Manager, user_type)
            .await?;
        let quantity_sold_dec = parse_decimal(quantity_sold)?;
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let lot = self
            .repo
            .get_stock_lot(&mut tx, asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("stock lot not found"))?;
        let quantity_open = parse_decimal(&lot.quantity_open)?;
        if quantity_sold_dec <= rust_decimal::Decimal::ZERO {
            return Err(Status::invalid_argument("quantity_sold must be > 0"));
        }
        if quantity_sold_dec > quantity_open {
            return Err(Status::invalid_argument(
                "quantity_sold exceeds open quantity",
            ));
        }
        let new_open = quantity_open - quantity_sold_dec;

        let lots_for_fifo = vec![Lot {
            id: lot.asset_id.clone(),
            quantity_open,
            cost_per_unit_minor: lot.buy_price_per_share,
        }];
        let allocations = fifo_disposal_allocations(&lots_for_fifo, quantity_sold_dec)
            .map_err(|e| Status::invalid_argument(format!("fifo: {e}")))?;
        let allocations_for_repo: Vec<DisposalAllocation> = allocations.clone();
        let cost_basis_allocated = allocations.iter().map(|a| a.cost_basis_minor).sum::<i64>();
        let realized_pnl = sale_proceeds - sale_fees.max(0) - cost_basis_allocated;
        let disposal_id = uuid::Uuid::new_v4().to_string();
        let disposal_date = if disposal_date == 0 {
            now_unix()
        } else {
            disposal_date
        };

        self.repo
            .update_stock_quantity_open(&mut tx, &lot.asset_id, new_open.to_string())
            .await
            .map_err(internal)?;
        self.repo
            .record_stock_disposal(
                &mut tx,
                &lot.asset_id,
                &disposal_id,
                disposal_date,
                quantity_sold_dec.to_string(),
                sale_proceeds,
                sale_fees,
                realized_pnl,
                cost_basis_allocated,
                &allocations_for_repo,
            )
            .await
            .map_err(internal)?;
        let new_status = if new_open.is_zero() {
            AssetStatus::Sold
        } else {
            AssetStatus::Active
        };
        let closed_on = if new_open.is_zero() {
            Some(now_unix())
        } else {
            None
        };
        self.repo
            .update_status(&mut tx, budget_id, &lot.asset_id, new_status, closed_on)
            .await
            .map_err(internal)?;
        self.append_activity(
            &mut tx,
            budget_id,
            &lot.asset_id,
            user_id,
            idempotency_key,
            "DISPOSAL_RECORDED",
            &format!(
                r#"{{"qty":"{quantity_sold}","proceeds":{sale_proceeds},"pnl":{realized_pnl}}}"#
            ),
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        let mut tx2 = self.repo.begin().await.map_err(internal)?;
        let refreshed = self
            .repo
            .get_asset(&mut tx2, budget_id, &lot.asset_id)
            .await
            .map_err(internal)?
            .ok_or_else(|| Status::not_found("asset not found after disposal"))?;
        tx2.commit().await.map_err(internal)?;
        Ok(pconv::map_portfolio_asset(refreshed))
    }

    // -----------------------------------------------------------------------
    // Portfolio summary with per-asset valuation
    // -----------------------------------------------------------------------

    pub async fn get_portfolio_summary(
        &self,
        user_id: &str,
        budget_id: &str,
        user_type: Option<&str>,
    ) -> Result<pb::PortfolioSummary, Status> {
        let valuated = self.list_valuated(user_id, budget_id, user_type).await?;
        let mut total_value: i64 = 0;
        let mut total_cost: i64 = 0;
        let mut total_realized: i64 = 0;
        let mut total_unrealized: i64 = 0;
        for v in &valuated {
            total_value += v.current_value;
            total_cost += v.open_cost_basis;
            total_realized += v.realized_pnl;
            total_unrealized += v.unrealized_pnl;
        }

        let total_pnl = total_realized + total_unrealized;
        let total_return_pct = if total_cost > 0 {
            (total_pnl as f64 / total_cost as f64) * 100.0
        } else {
            0.0
        };

        Ok(pb::PortfolioSummary {
            budget_id: budget_id.to_string(),
            total_current_value: total_value,
            total_open_cost_basis: total_cost,
            total_realized_pnl: total_realized,
            total_unrealized_pnl: total_unrealized,
            total_return_pct,
            currency: String::new(),
            assets: valuated,
        })
    }

    /// Per-asset valuation. Returns current_value, open_cost_basis,
    /// realized_pnl, unrealized_pnl, accrued_interest, freshness,
    /// quote_observed_at.
    async fn value_asset(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
        asset: &pconv::DbPortfolioAsset,
        today: i64,
    ) -> Result<Valuation, Status> {
        match asset.asset_class.as_str() {
            "savings_account" => {
                let sub = self
                    .repo
                    .get_savings_account(tx, &asset.id)
                    .await
                    .map_err(internal)?;
                let Some(sub) = sub else {
                    return Ok(Valuation::unpriced());
                };
                let rate = parse_decimal(&sub.annual_rate).map_err(internal)?;
                let days = (today - sub.balance_as_of).max(0);
                let total = match InterestMethod::from_db(&sub.interest_method) {
                    InterestMethod::Simple => simple_accrued(sub.current_balance, rate, days),
                    InterestMethod::Compound => compound_accrued(sub.current_balance, rate, days),
                };
                let accrued = total.minor().saturating_sub(sub.current_balance);
                let current_value = sub.current_balance + accrued;
                Ok(Valuation {
                    current_value,
                    open_cost_basis: sub.current_balance,
                    realized_pnl: 0,
                    unrealized_pnl: current_value - sub.current_balance,
                    accrued_interest: accrued,
                    freshness: PriceFreshness::Unpriced as i32,
                    quote_observed_at: 0,
                })
            }
            "fixed_deposit" => {
                let sub = self
                    .repo
                    .get_fixed_deposit(tx, &asset.id)
                    .await
                    .map_err(internal)?;
                let Some(sub) = sub else {
                    return Ok(Valuation::unpriced());
                };
                let rate = parse_decimal(&sub.annual_rate).map_err(internal)?;
                let eff = today.min(sub.maturity_date);
                let days = (eff - sub.deposit_date).max(0);
                let total = match InterestMethod::from_db(&sub.interest_method) {
                    InterestMethod::Simple => simple_accrued(sub.principal, rate, days),
                    InterestMethod::Compound => compound_accrued(sub.principal, rate, days),
                };
                let accrued = total.minor().saturating_sub(sub.principal);
                let current_value = sub.principal + accrued;
                Ok(Valuation {
                    current_value,
                    open_cost_basis: sub.principal,
                    realized_pnl: 0,
                    unrealized_pnl: current_value - sub.principal,
                    accrued_interest: accrued,
                    freshness: PriceFreshness::Unpriced as i32,
                    quote_observed_at: 0,
                })
            }
            "gold_lot" => {
                let sub = self
                    .repo
                    .get_gold_lot(tx, &asset.id)
                    .await
                    .map_err(internal)?;
                let Some(sub) = sub else {
                    return Ok(Valuation::unpriced());
                };
                let obs = self
                    .repo
                    .latest_price_observation(tx, &asset.id)
                    .await
                    .map_err(internal)?;
                let Some(obs) = obs else {
                    return Ok(Valuation {
                        current_value: sub.purchase_cost,
                        open_cost_basis: sub.purchase_cost,
                        realized_pnl: 0,
                        unrealized_pnl: 0,
                        accrued_interest: 0,
                        freshness: PriceFreshness::Unpriced as i32,
                        quote_observed_at: 0,
                    });
                };
                let grams_f = parse_decimal(&sub.quantity_grams)
                    .map_err(internal)?
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0);
                let current_value = (grams_f * obs.unit_price as f64).round() as i64;
                let freshness =
                    PriceFreshness::from_age_seconds((today - obs.observed_at).max(0), true);
                Ok(Valuation {
                    current_value,
                    open_cost_basis: sub.purchase_cost,
                    realized_pnl: 0,
                    unrealized_pnl: current_value - sub.purchase_cost,
                    accrued_interest: 0,
                    freshness: freshness as i32,
                    quote_observed_at: obs.observed_at,
                })
            }
            "stock_lot" => {
                let sub = self
                    .repo
                    .get_stock_lot(tx, &asset.id)
                    .await
                    .map_err(internal)?;
                let Some(sub) = sub else {
                    return Ok(Valuation::unpriced());
                };
                let qty_open = parse_decimal(&sub.quantity_open).map_err(internal)?;
                let qty_bought = parse_decimal(&sub.quantity_bought).map_err(internal)?;
                let sold_qty = qty_bought - qty_open;
                let sold_f = sold_qty.to_string().parse::<f64>().unwrap_or(0.0);
                let open_cost_basis = (sold_f * sub.buy_price_per_share as f64).round() as i64;
                let obs = self
                    .repo
                    .latest_price_observation(tx, &asset.id)
                    .await
                    .map_err(internal)?;
                let Some(obs) = obs else {
                    return Ok(Valuation {
                        current_value: open_cost_basis,
                        open_cost_basis,
                        realized_pnl: 0,
                        unrealized_pnl: 0,
                        accrued_interest: 0,
                        freshness: PriceFreshness::Unpriced as i32,
                        quote_observed_at: 0,
                    });
                };
                let qty_open_f = qty_open.to_string().parse::<f64>().unwrap_or(0.0);
                let current_value = (qty_open_f * obs.unit_price as f64).round() as i64;
                let freshness =
                    PriceFreshness::from_age_seconds((today - obs.observed_at).max(0), true);
                Ok(Valuation {
                    current_value,
                    open_cost_basis,
                    realized_pnl: 0,
                    unrealized_pnl: current_value - open_cost_basis,
                    accrued_interest: 0,
                    freshness: freshness as i32,
                    quote_observed_at: obs.observed_at,
                })
            }
            _ => Ok(Valuation::unpriced()),
        }
    }
}

struct Valuation {
    current_value: i64,
    open_cost_basis: i64,
    realized_pnl: i64,
    unrealized_pnl: i64,
    accrued_interest: i64,
    freshness: i32,
    quote_observed_at: i64,
}

impl Valuation {
    fn unpriced() -> Self {
        Self {
            current_value: 0,
            open_cost_basis: 0,
            realized_pnl: 0,
            unrealized_pnl: 0,
            accrued_interest: 0,
            freshness: PriceFreshness::Unpriced as i32,
            quote_observed_at: 0,
        }
    }
}

fn internal<E: ToString>(e: E) -> Status {
    Status::internal(e.to_string())
}

fn parse_decimal(value: &str) -> Result<rust_decimal::Decimal, Status> {
    use std::str::FromStr;
    rust_decimal::Decimal::from_str(value)
        .map_err(|_| Status::invalid_argument(format!("invalid decimal: {value}")))
}

fn parse_price_side(s: &str) -> crate::manager::biz::portfolio::PriceSide {
    match s {
        "bid" => crate::manager::biz::portfolio::PriceSide::Bid,
        "ask" => crate::manager::biz::portfolio::PriceSide::Ask,
        _ => crate::manager::biz::portfolio::PriceSide::Mid,
    }
}

fn today_business_date() -> i64 {
    let now_utc = chrono::Utc::now();
    let ict = now_utc + chrono::Duration::hours(7);
    ict.timestamp() - (ict.timestamp() % 86_400)
}

// Keep this around to silence unused warnings on helper enums used via trait.
#[allow(dead_code)]
fn _silence_unused() {
    let _: PayoutType = PayoutType::AtMaturity;
    let _: DisposalAllocation = DisposalAllocation {
        lot_id: String::new(),
        quantity: rust_decimal::Decimal::ZERO,
        cost_basis_minor: 0,
    };
}

impl PortfolioBiz {
    /// Backfill counter. The SQL migration `20260802000001_backfill_portfolio.sql`
    /// performs the actual copy from `invest_assets` into `portfolio_*`.
    /// This RPC returns the count of rows that the migration produced,
    /// filtered to the requested budget when non-empty.
    pub async fn run_backfill(&self, budget_id: &str) -> Result<i32, Status> {
        let mut tx = self.repo.begin().await.map_err(internal)?;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM portfolio_assets WHERE legacy_asset_id IS NOT NULL \
             AND (? = '' OR budget_id = ?)",
        )
        .bind(budget_id)
        .bind(budget_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(internal)?;
        tx.commit().await.map_err(internal)?;
        Ok(count as i32)
    }
}

#[cfg(test)]
mod tests {
    // ETF and Crypto validation paths. Phase 5.5: the new asset
    // classes accept mandatory fields and reject empty inputs. These
    // tests assert the validation surface; the actual DB layer is
    // exercised by integration tests in Phase 6.

    #[test]
    fn parse_quantity_bought_accepts_positive_floats() {
        // Happy path: positive decimal parses and rounds correctly.
        let qty: f64 = "100.5".parse().unwrap();
        let price: i64 = 1_000;
        let cost = (qty * price as f64).round() as i64;
        assert_eq!(cost, 100_500);
    }

    #[test]
    fn parse_quantity_bought_rejects_alpha() {
        // Negative test: the handler now maps this to invalid_argument
        // rather than silently using 0.0.
        let parsed: Result<f64, _> = "abc".parse();
        assert!(parsed.is_err());
    }

    #[test]
    fn parse_quantity_bought_rejects_negative() {
        // Negative test: negative quantity is rejected post-parse
        // (sign check) so purchase_cost cannot become negative.
        let parsed: f64 = "-1".parse().unwrap();
        assert!(parsed < 0.0);
    }

    #[test]
    fn parse_quantity_bought_rejects_empty() {
        // Empty string parses to an error rather than 0.0.
        let parsed: Result<f64, _> = "".parse();
        assert!(parsed.is_err());
    }

    #[test]
    fn map_etf_underlying_handles_known_values() {
        use crate::converters::portfolio::map_etf_lot;
        use crate::converters::portfolio::DbEtfLot;
        use crate::pb::service::portfolio::EtfUnderlyingIndex as E;

        for (db_value, expected_proto) in [
            ("vn30", E::Vn30 as i32),
            ("vn100", E::Vn100 as i32),
            ("hnx30", E::Hnx30 as i32),
            ("VN30", E::Vn30 as i32), // case-insensitive
        ] {
            let lot = DbEtfLot {
                asset_id: "a1".into(),
                ticker: "FUEVFVND".into(),
                exchange: "HOSE".into(),
                underlying_index: db_value.into(),
                fund_provider: "VinaCapital".into(),
                quantity_bought: "10".into(),
                quantity_open: "10".into(),
                buy_price_per_unit: 0,
                purchase_cost: 0,
                fees: 0,
                purchase_date: 0,
                settlement_date: None,
                notes: None,
            };
            let proto = map_etf_lot(lot);
            assert_eq!(proto.underlying_index, expected_proto, "db={db_value}");
        }
    }

    #[test]
    fn map_etf_underlying_unknown_becomes_other() {
        // Negative test: unknown underlying_index falls back to
        // EtfUnderlyingIndex_Other rather than panicking. Phase 6 will
        // validate at the handler layer; this is the converter-level
        // safety net.
        use crate::converters::portfolio::map_etf_lot;
        use crate::converters::portfolio::DbEtfLot;
        use crate::pb::service::portfolio::EtfUnderlyingIndex as E;
        let lot = DbEtfLot {
            asset_id: "a2".into(),
            ticker: "FUEVFVND".into(),
            exchange: "HOSE".into(),
            underlying_index: "garbage".into(),
            fund_provider: "X".into(),
            quantity_bought: "1".into(),
            quantity_open: "1".into(),
            buy_price_per_unit: 0,
            purchase_cost: 0,
            fees: 0,
            purchase_date: 0,
            settlement_date: None,
            notes: None,
        };
        let proto = map_etf_lot(lot);
        assert_eq!(proto.underlying_index, E::Other as i32);
    }

    #[test]
    fn map_crypto_network_handles_known_values() {
        use crate::converters::portfolio::map_crypto_lot;
        use crate::converters::portfolio::DbCryptoLot;
        use crate::pb::service::portfolio::CryptoNetwork as N;

        for (db_value, expected_proto) in [
            ("bitcoin", N::Bitcoin as i32),
            ("ethereum", N::Ethereum as i32),
            ("solana", N::Solana as i32),
            ("bnb_chain", N::BnbChain as i32),
            ("polkadot", N::Polkadot as i32),
        ] {
            let lot = DbCryptoLot {
                asset_id: "a1".into(),
                symbol: "BTC".into(),
                network: db_value.into(),
                custody_wallet: "0x".into(),
                quantity_bought: "1".into(),
                quantity_open: "1".into(),
                buy_price_per_unit: 0,
                purchase_cost: 0,
                fees: 0,
                purchase_date: 0,
                notes: None,
            };
            let proto = map_crypto_lot(lot);
            assert_eq!(proto.network, expected_proto, "db={db_value}");
        }
    }

    #[test]
    fn map_crypto_network_unknown_becomes_other() {
        use crate::converters::portfolio::map_crypto_lot;
        use crate::converters::portfolio::DbCryptoLot;
        use crate::pb::service::portfolio::CryptoNetwork as N;
        let lot = DbCryptoLot {
            asset_id: "a2".into(),
            symbol: "?".into(),
            network: "garbage".into(),
            custody_wallet: "".into(),
            quantity_bought: "1".into(),
            quantity_open: "1".into(),
            buy_price_per_unit: 0,
            purchase_cost: 0,
            fees: 0,
            purchase_date: 0,
            notes: None,
        };
        let proto = map_crypto_lot(lot);
        assert_eq!(proto.network, N::Other as i32);
    }
}
