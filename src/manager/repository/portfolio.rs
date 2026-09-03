//! Typed repository functions for the Asset Portfolio schema.
//!
//! Each asset class has its own insert / get / list / soft-delete path.
//! Stock and gold disposals allocate FIFO via the domain helper and
//! maintain derived `quantity_open` on the parent lot row.
//!
//! All mutations run inside a single MySQL transaction so that the
//! parent asset, the subtype row, the disposal, and the activity log
//! commit atomically.

use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use sqlx::{MySql, MySqlPool, Transaction};

use crate::converters::portfolio as pconv;
use crate::manager::biz::portfolio::fifo::{fifo_disposal_allocations, DisposalAllocation, Lot};
use crate::manager::biz::portfolio::gold::grams_from_quantity;
use crate::manager::biz::portfolio::AssetStatus;
use crate::pb::service::portfolio as pb;
use philand_time::now_unix;

#[derive(Clone)]
pub struct PortfolioRepository {
    pool: MySqlPool,
}

impl PortfolioRepository {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &MySqlPool {
        &self.pool
    }

    // -----------------------------------------------------------------------
    // Asset root CRUD
    // -----------------------------------------------------------------------

    pub async fn insert_asset(
        &self,
        tx: &mut Transaction<'_, MySql>,
        new: &pconv::NewPortfolioAsset,
    ) -> Result<pconv::DbPortfolioAsset> {
        let id = new.id.clone().unwrap_or_else(new_id);
        let now = now_unix();
        sqlx::query(
            r#"INSERT INTO portfolio_assets (
                id, budget_id, asset_class, display_name, currency, status,
                opened_on, closed_on, legacy_asset_id, notes,
                created_by, created_at, updated_at, deleted_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)"#,
        )
        .bind(&id)
        .bind(&new.budget_id)
        .bind(asset_class_to_db(&new.asset_class))
        .bind(&new.display_name)
        .bind(&new.currency)
        .bind(AssetStatus::Active.to_db())
        .bind(new.opened_on)
        .bind(new.closed_on)
        .bind(&new.legacy_asset_id)
        .bind(&new.notes)
        .bind(&new.created_by)
        .bind(now)
        .bind(now)
        .execute(&mut **tx)
        .await?;
        Ok(pconv::DbPortfolioAsset {
            id,
            budget_id: new.budget_id.clone(),
            asset_class: asset_class_to_db(&new.asset_class).to_string(),
            display_name: new.display_name.clone(),
            currency: new.currency.clone(),
            status: AssetStatus::Active.to_db().to_string(),
            opened_on: new.opened_on,
            closed_on: new.closed_on,
            legacy_asset_id: new.legacy_asset_id.clone(),
            notes: new.notes.clone(),
            created_by: new.created_by.clone(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
        })
    }

    pub async fn get_asset(
        &self,
        tx: &mut Transaction<'_, MySql>,
        budget_id: &str,
        asset_id: &str,
    ) -> Result<Option<pconv::DbPortfolioAsset>> {
        let row = sqlx::query_as::<_, pconv::DbPortfolioAsset>(
            r#"SELECT id, budget_id, asset_class, display_name, currency, status,
                      opened_on, closed_on, legacy_asset_id, notes,
                      created_by, created_at, updated_at, deleted_at
               FROM portfolio_assets
               WHERE budget_id = ? AND id = ? AND deleted_at IS NULL"#,
        )
        .bind(budget_id)
        .bind(asset_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    pub async fn list_assets_by_budget(
        &self,
        tx: &mut Transaction<'_, MySql>,
        budget_id: &str,
    ) -> Result<Vec<pconv::DbPortfolioAsset>> {
        let rows = sqlx::query_as::<_, pconv::DbPortfolioAsset>(
            r#"SELECT id, budget_id, asset_class, display_name, currency, status,
                      opened_on, closed_on, legacy_asset_id, notes,
                      created_by, created_at, updated_at, deleted_at
               FROM portfolio_assets
               WHERE budget_id = ? AND deleted_at IS NULL
               ORDER BY opened_on DESC, created_at DESC"#,
        )
        .bind(budget_id)
        .fetch_all(&mut **tx)
        .await?;
        Ok(rows)
    }

    /// List all non-deleted gold and stock assets across every budget.
    /// Used by the scheduled refresh job. Excludes savings accounts and
    /// fixed deposits (which are not externally priced).
    pub async fn begin(&self) -> Result<Transaction<'_, MySql>> {
        self.pool
            .begin()
            .await
            .map_err(|e| anyhow::anyhow!("begin tx: {e}"))
    }

    /// Insert an asset, then read it back in one transaction. Avoids
    /// the "commit + begin new tx + read" pattern (one extra round-trip
    /// and a brief window where the read could miss the write).
    /// Returns the read-back `DbPortfolioAsset`.
    pub async fn insert_and_read_asset(
        &self,
        new: pconv::NewPortfolioAsset,
    ) -> Result<pconv::DbPortfolioAsset> {
        let mut tx = self.pool.begin().await?;
        let inserted = self.insert_asset(&mut tx, &new).await?;
        let read = self
            .get_asset(&mut tx, &new.budget_id, &inserted.id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("asset {} disappeared after insert", inserted.id))?;
        tx.commit().await?;
        Ok(read)
    }

    /// Delete dedup rows older than `max_age_secs`. The dedup table is
    /// append-only and the unique constraint is per `(user_id,
    /// asset_id, event_type, local_date)`. Without cleanup the table
    /// grows linearly with daily alert volume.
    ///
    /// Returns the number of rows deleted. The drainer calls this
    /// opportunistically (e.g. once per day) to bound the table size.
    pub async fn cleanup_dedup_older_than(&self, max_age_secs: i64) -> Result<u64> {
        let cutoff = now_unix() - max_age_secs;
        let mut tx = self.pool.begin().await?;
        let res = sqlx::query(
            "DELETE FROM portfolio_alert_dedup
             WHERE created_at < ?",
        )
        .bind(cutoff)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(res.rows_affected())
    }

    pub async fn list_active_for_refresh(
        &self,
        tx: &mut Transaction<'_, MySql>,
    ) -> Result<Vec<pconv::DbPortfolioAsset>> {
        let rows = sqlx::query_as::<_, pconv::DbPortfolioAsset>(
            r#"SELECT id, budget_id, asset_class, display_name, currency, status,
                      opened_on, closed_on, legacy_asset_id, notes,
                      created_by, created_at, updated_at, deleted_at
               FROM portfolio_assets
               WHERE deleted_at IS NULL
                 AND status = 'active'
                 AND asset_class IN ('gold_lot', 'stock_lot')
               ORDER BY id ASC"#,
        )
        .fetch_all(&mut **tx)
        .await?;
        Ok(rows)
    }

    // -----------------------------------------------------------------------
    // Transfers
    // -----------------------------------------------------------------------

    pub async fn insert_transfer(
        &self,
        tx: &mut Transaction<'_, MySql>,
        new: &pconv::NewTransfer,
    ) -> Result<pconv::DbTransfer> {
        sqlx::query(
            r#"INSERT INTO portfolio_transfers
                (id, group_id, source_budget_id, counterparty_budget_id,
                 direction, amount_minor, currency, status,
                 linked_entry_source_id, linked_entry_counterparty_id,
                 idempotency_key, actor_user_id, created_at, completed_at, notes)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&new.id)
        .bind(&new.group_id)
        .bind(&new.source_budget_id)
        .bind(&new.counterparty_budget_id)
        .bind(transfer_direction_str(new.direction))
        .bind(new.amount_minor)
        .bind(&new.currency)
        .bind(transfer_status_str(new.status))
        .bind(&new.linked_entry_source_id)
        .bind(&new.linked_entry_counterparty_id)
        .bind(if new.idempotency_key.is_empty() {
            None
        } else {
            Some(new.idempotency_key.as_str())
        })
        .bind(&new.actor_user_id)
        .bind(new.created_at)
        .bind(new.completed_at)
        .bind(&new.notes)
        .execute(&mut **tx)
        .await?;
        Ok(pconv::DbTransfer {
            id: new.id.clone(),
            group_id: new.group_id.clone(),
            source_budget_id: new.source_budget_id.clone(),
            counterparty_budget_id: new.counterparty_budget_id.clone(),
            direction: transfer_direction_str(new.direction).to_string(),
            amount_minor: new.amount_minor,
            currency: new.currency.clone(),
            status: transfer_status_str(new.status).to_string(),
            linked_entry_source_id: new.linked_entry_source_id.clone(),
            linked_entry_counterparty_id: new.linked_entry_counterparty_id.clone(),
            idempotency_key: if new.idempotency_key.is_empty() {
                None
            } else {
                Some(new.idempotency_key.clone())
            },
            actor_user_id: new.actor_user_id.clone(),
            created_at: new.created_at,
            completed_at: new.completed_at,
            notes: new.notes.clone(),
        })
    }

    pub async fn get_transfer(
        &self,
        tx: &mut Transaction<'_, MySql>,
        transfer_id: &str,
    ) -> Result<Option<pconv::DbTransfer>> {
        let row = sqlx::query_as::<_, pconv::DbTransfer>(
            r#"SELECT id, group_id, source_budget_id, counterparty_budget_id,
                      direction, amount_minor, currency, status,
                      linked_entry_source_id, linked_entry_counterparty_id,
                      idempotency_key, actor_user_id, created_at, completed_at, notes
               FROM portfolio_transfers WHERE id = ?"#,
        )
        .bind(transfer_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    pub async fn find_transfer_by_idempotency(
        &self,
        source_budget_id: String,
        counterparty_budget_id: String,
        idempotency_key: String,
    ) -> Result<Option<pconv::DbTransfer>> {
        if idempotency_key.is_empty() {
            return Ok(None);
        }
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, pconv::DbTransfer>(
            r#"SELECT id, group_id, source_budget_id, counterparty_budget_id,
                      direction, amount_minor, currency, status,
                      linked_entry_source_id, linked_entry_counterparty_id,
                      idempotency_key, actor_user_id, created_at, completed_at, notes
               FROM portfolio_transfers
               WHERE source_budget_id = ?
                 AND counterparty_budget_id = ?
                 AND idempotency_key = ?
               LIMIT 1"#,
        )
        .bind(&source_budget_id)
        .bind(&counterparty_budget_id)
        .bind(&idempotency_key)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn update_status(
        &self,
        tx: &mut Transaction<'_, MySql>,
        budget_id: &str,
        asset_id: &str,
        new_status: AssetStatus,
        closed_on: Option<i64>,
    ) -> Result<()> {
        let now = now_unix();
        sqlx::query(
            r#"UPDATE portfolio_assets
                  SET status = ?, closed_on = ?, updated_at = ?
                WHERE budget_id = ? AND id = ? AND deleted_at IS NULL"#,
        )
        .bind(new_status.to_db())
        .bind(closed_on)
        .bind(now)
        .bind(budget_id)
        .bind(asset_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub async fn update_metadata(
        &self,
        tx: &mut Transaction<'_, MySql>,
        budget_id: &str,
        asset_id: &str,
        display_name: Option<&str>,
        notes: Option<&str>,
    ) -> Result<()> {
        let now = now_unix();
        sqlx::query(
            r#"UPDATE portfolio_assets
                  SET display_name = COALESCE(?, display_name),
                      notes        = COALESCE(?, notes),
                      updated_at   = ?
                WHERE budget_id = ? AND id = ? AND deleted_at IS NULL"#,
        )
        .bind(display_name)
        .bind(notes)
        .bind(now)
        .bind(budget_id)
        .bind(asset_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub async fn soft_delete_asset(
        &self,
        tx: &mut Transaction<'_, MySql>,
        budget_id: &str,
        asset_id: &str,
    ) -> Result<()> {
        let now = now_unix();
        sqlx::query(
            r#"UPDATE portfolio_assets
                  SET deleted_at = ?, updated_at = ?
                WHERE budget_id = ? AND id = ? AND deleted_at IS NULL"#,
        )
        .bind(now)
        .bind(now)
        .bind(budget_id)
        .bind(asset_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Subtype inserts
    // -----------------------------------------------------------------------

    pub async fn insert_savings_account(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        new: &pconv::NewSavingsAccount,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO portfolio_savings_accounts
                (asset_id, provider, account_reference_masked, current_balance,
                 balance_as_of, annual_rate, interest_method, payout_type,
                 opened_on, notes)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(asset_id)
        .bind(&new.provider)
        .bind(&new.account_reference_masked)
        .bind(new.current_balance)
        .bind(new.balance_as_of)
        .bind(&new.annual_rate)
        .bind(&new.interest_method)
        .bind(&new.payout_type)
        .bind(new.opened_on)
        .bind(&new.notes)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub async fn get_savings_account(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
    ) -> Result<Option<pconv::DbSavingsAccount>> {
        let row = sqlx::query_as::<_, pconv::DbSavingsAccount>(
            r#"SELECT asset_id, provider, account_reference_masked, current_balance,
                      balance_as_of, annual_rate, interest_method, payout_type,
                      opened_on, notes
               FROM portfolio_savings_accounts WHERE asset_id = ?"#,
        )
        .bind(asset_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    pub async fn insert_fixed_deposit(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        new: &pconv::NewFixedDeposit,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO portfolio_fixed_deposits
                (asset_id, provider, product_name, principal, annual_rate,
                 interest_method, payout_type, deposit_date, maturity_date,
                 auto_renewal_policy, rollover_from_asset_id,
                 certificate_reference_masked, notes)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(asset_id)
        .bind(&new.provider)
        .bind(&new.product_name)
        .bind(new.principal)
        .bind(&new.annual_rate)
        .bind(&new.interest_method)
        .bind(&new.payout_type)
        .bind(new.deposit_date)
        .bind(new.maturity_date)
        .bind(&new.auto_renewal_policy)
        .bind(&new.rollover_from_asset_id)
        .bind(&new.certificate_reference_masked)
        .bind(&new.notes)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub async fn get_fixed_deposit(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
    ) -> Result<Option<pconv::DbFixedDeposit>> {
        let row = sqlx::query_as::<_, pconv::DbFixedDeposit>(
            r#"SELECT asset_id, provider, product_name, principal, annual_rate,
                      interest_method, payout_type, deposit_date, maturity_date,
                      auto_renewal_policy, rollover_from_asset_id,
                      certificate_reference_masked, notes
               FROM portfolio_fixed_deposits WHERE asset_id = ?"#,
        )
        .bind(asset_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    pub async fn list_due_fixed_deposits(
        &self,
        tx: &mut Transaction<'_, MySql>,
        today: i64,
    ) -> Result<Vec<(String, String)>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            r#"SELECT pa.budget_id, pa.id
               FROM portfolio_assets pa
               JOIN portfolio_fixed_deposits fd ON fd.asset_id = pa.id
               WHERE pa.status = 'active'
                 AND pa.deleted_at IS NULL
                 AND fd.maturity_date <= ?
               ORDER BY fd.maturity_date ASC"#,
        )
        .bind(today)
        .fetch_all(&mut **tx)
        .await?;
        Ok(rows)
    }

    pub async fn insert_gold_lot(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        new: &pconv::NewGoldLot,
    ) -> Result<()> {
        let quantity_grams_decimal =
            grams_from_quantity(parse_decimal(&new.quantity_original)?, new.unit);
        let quantity_grams = quantity_grams_decimal.to_string();
        sqlx::query(
            r#"INSERT INTO portfolio_gold_lots
                (asset_id, provider, gold_type, purity, form,
                 quantity_original, unit_original, quantity_grams,
                 purchase_price_per_unit_original, purchase_cost,
                 fees, purchase_date, notes)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(asset_id)
        .bind(&new.provider)
        .bind(&new.gold_type)
        .bind(&new.purity)
        .bind(&new.form)
        .bind(&new.quantity_original)
        .bind(new.unit.to_db())
        .bind(quantity_grams)
        .bind(new.purchase_price_per_unit_original)
        .bind(new.purchase_cost)
        .bind(new.fees)
        .bind(new.purchase_date)
        .bind(&new.notes)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub async fn get_gold_lot(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
    ) -> Result<Option<pconv::DbGoldLot>> {
        let row = sqlx::query_as::<_, pconv::DbGoldLot>(
            r#"SELECT asset_id, provider, gold_type, purity, form,
                      quantity_original, unit_original, quantity_grams,
                      purchase_price_per_unit_original, purchase_cost,
                      fees, purchase_date, notes
               FROM portfolio_gold_lots WHERE asset_id = ?"#,
        )
        .bind(asset_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    pub async fn insert_stock_lot(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        new: &pconv::NewStockLot,
    ) -> Result<()> {
        let quantity_open = new.quantity_bought.clone();
        sqlx::query(
            r#"INSERT INTO portfolio_stock_lots
                (asset_id, ticker, exchange, quantity_bought, quantity_open,
                 buy_price_per_share, purchase_cost, fees,
                 purchase_date, settlement_date, notes)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(asset_id)
        .bind(&new.ticker)
        .bind(&new.exchange)
        .bind(&new.quantity_bought)
        .bind(quantity_open)
        .bind(new.buy_price_per_share)
        .bind(new.purchase_cost)
        .bind(new.fees)
        .bind(new.purchase_date)
        .bind(new.settlement_date)
        .bind(&new.notes)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub async fn get_stock_lot(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
    ) -> Result<Option<pconv::DbStockLot>> {
        let row = sqlx::query_as::<_, pconv::DbStockLot>(
            r#"SELECT asset_id, ticker, exchange, quantity_bought, quantity_open,
                      buy_price_per_share, purchase_cost, fees,
                      purchase_date, settlement_date, notes
               FROM portfolio_stock_lots WHERE asset_id = ?"#,
        )
        .bind(asset_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    // -----------------------------------------------------------------------
    // ETF lots
    // -----------------------------------------------------------------------

    pub async fn insert_etf_lot(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        new: &pconv::NewEtfLot,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO portfolio_etf_lots
                (asset_id, ticker, exchange, underlying_index, fund_provider,
                 quantity_bought, quantity_open, buy_price_per_unit,
                 purchase_cost, fees, purchase_date, settlement_date, notes)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(asset_id)
        .bind(&new.ticker)
        .bind(&new.exchange)
        .bind(&new.underlying_index)
        .bind(&new.fund_provider)
        .bind(&new.quantity_bought)
        .bind(&new.quantity_open)
        .bind(new.buy_price_per_unit)
        .bind(new.purchase_cost)
        .bind(new.fees)
        .bind(new.purchase_date)
        .bind(new.settlement_date)
        .bind(&new.notes)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub async fn get_etf_lot(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
    ) -> Result<Option<pconv::DbEtfLot>> {
        let row = sqlx::query_as::<_, pconv::DbEtfLot>(
            r#"SELECT asset_id, ticker, exchange, underlying_index, fund_provider,
                      quantity_bought, quantity_open, buy_price_per_unit,
                      purchase_cost, fees, purchase_date, settlement_date, notes
               FROM portfolio_etf_lots WHERE asset_id = ?"#,
        )
        .bind(asset_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    // -----------------------------------------------------------------------
    // Crypto lots
    // -----------------------------------------------------------------------

    pub async fn insert_crypto_lot(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        new: &pconv::NewCryptoLot,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO portfolio_crypto_lots
                (asset_id, symbol, network, custody_wallet,
                 quantity_bought, quantity_open, buy_price_per_unit,
                 purchase_cost, fees, purchase_date, notes)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(asset_id)
        .bind(&new.symbol)
        .bind(&new.network)
        .bind(&new.custody_wallet)
        .bind(&new.quantity_bought)
        .bind(&new.quantity_open)
        .bind(new.buy_price_per_unit)
        .bind(new.purchase_cost)
        .bind(new.fees)
        .bind(new.purchase_date)
        .bind(&new.notes)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub async fn get_crypto_lot(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
    ) -> Result<Option<pconv::DbCryptoLot>> {
        let row = sqlx::query_as::<_, pconv::DbCryptoLot>(
            r#"SELECT asset_id, symbol, network, custody_wallet,
                      quantity_bought, quantity_open, buy_price_per_unit,
                      purchase_cost, fees, purchase_date, notes
               FROM portfolio_crypto_lots WHERE asset_id = ?"#,
        )
        .bind(asset_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    pub async fn update_stock_quantity_open(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        quantity_open: String,
    ) -> Result<()> {
        sqlx::query(
            r#"UPDATE portfolio_stock_lots
                  SET quantity_open = ?
                WHERE asset_id = ?"#,
        )
        .bind(quantity_open)
        .bind(asset_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Disposals
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn record_stock_disposal(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        disposal_id: &str,
        disposal_date: i64,
        quantity_sold: String,
        sale_proceeds: i64,
        sale_fees: i64,
        realized_pnl: i64,
        cost_basis_allocated: i64,
        allocations: &[DisposalAllocation],
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO portfolio_stock_disposals
                (id, asset_id, disposal_date, quantity_sold, sale_proceeds,
                 sale_fees, realized_pnl, cost_basis_allocated)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(disposal_id)
        .bind(asset_id)
        .bind(disposal_date)
        .bind(quantity_sold)
        .bind(sale_proceeds)
        .bind(sale_fees)
        .bind(realized_pnl)
        .bind(cost_basis_allocated)
        .execute(&mut **tx)
        .await?;

        for a in allocations {
            sqlx::query(
                r#"INSERT INTO portfolio_stock_disposal_allocations
                    (disposal_id, asset_id, quantity, cost_basis)
                    VALUES (?, ?, ?, ?)"#,
            )
            .bind(disposal_id)
            .bind(&a.lot_id)
            .bind(a.quantity.to_string())
            .bind(a.cost_basis_minor)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_gold_disposal(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        disposal_id: &str,
        disposal_date: i64,
        quantity_grams_sold: String,
        sale_proceeds: i64,
        sale_fees: i64,
        realized_pnl: i64,
        cost_basis_allocated: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO portfolio_gold_disposals
                (id, asset_id, disposal_date, quantity_grams_sold,
                 sale_proceeds, sale_fees, realized_pnl, cost_basis_allocated)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(disposal_id)
        .bind(asset_id)
        .bind(disposal_date)
        .bind(quantity_grams_sold)
        .bind(sale_proceeds)
        .bind(sale_fees)
        .bind(realized_pnl)
        .bind(cost_basis_allocated)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Price observations
    // -----------------------------------------------------------------------

    pub async fn insert_price_observation(
        &self,
        tx: &mut Transaction<'_, MySql>,
        obs: &pconv::NewPriceObservation,
    ) -> Result<pconv::DbPriceObservation> {
        let id = obs.id.clone().unwrap_or_else(new_id);
        sqlx::query(
            r#"INSERT INTO portfolio_price_observations
                (id, asset_id, provider, price_side, unit_price, currency,
                 observed_at, source_reference, idempotency_key, notes)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&id)
        .bind(&obs.asset_id)
        .bind(&obs.provider)
        .bind(obs.price_side.to_db())
        .bind(obs.unit_price)
        .bind(&obs.currency)
        .bind(obs.observed_at)
        .bind(&obs.source_reference)
        .bind(&obs.idempotency_key)
        .bind(&obs.notes)
        .execute(&mut **tx)
        .await
        .map_err(|e: sqlx::Error| {
            // Idempotency dedup is via UNIQUE (asset_id, idempotency_key).
            // Return a typed error for the caller to handle.
            if e.to_string().contains("Duplicate entry") {
                anyhow!("duplicate idempotency_key for asset {}", obs.asset_id)
            } else {
                anyhow::Error::new(e)
            }
        })?;

        Ok(pconv::DbPriceObservation {
            id,
            asset_id: obs.asset_id.clone(),
            provider: obs.provider.clone(),
            price_side: obs.price_side.to_db().to_string(),
            unit_price: obs.unit_price,
            currency: obs.currency.clone(),
            observed_at: obs.observed_at,
            source_reference: obs.source_reference.clone(),
            idempotency_key: obs.idempotency_key.clone(),
            notes: obs.notes.clone(),
        })
    }

    pub async fn get_price_observation_by_idempotency(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<pconv::DbPriceObservation>> {
        let row = sqlx::query_as::<_, pconv::DbPriceObservation>(
            r#"SELECT id, asset_id, provider, price_side, unit_price, currency,
                      observed_at, source_reference, idempotency_key, notes
               FROM portfolio_price_observations
               WHERE asset_id = ? AND idempotency_key = ?
               LIMIT 1"#,
        )
        .bind(asset_id)
        .bind(idempotency_key)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    pub async fn list_price_observations(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        limit: i32,
    ) -> Result<Vec<pconv::DbPriceObservation>> {
        let cap = limit.clamp(1, 1000);
        let rows = sqlx::query_as::<_, pconv::DbPriceObservation>(
            r#"SELECT id, asset_id, provider, price_side, unit_price, currency,
                      observed_at, source_reference, idempotency_key, notes
               FROM portfolio_price_observations
               WHERE asset_id = ?
               ORDER BY observed_at DESC, id DESC
               LIMIT ?"#,
        )
        .bind(asset_id)
        .bind(cap)
        .fetch_all(&mut **tx)
        .await?;
        Ok(rows)
    }

    pub async fn latest_price_observation(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
    ) -> Result<Option<pconv::DbPriceObservation>> {
        let row = sqlx::query_as::<_, pconv::DbPriceObservation>(
            r#"SELECT id, asset_id, provider, price_side, unit_price, currency,
                      observed_at, source_reference, idempotency_key, notes
               FROM portfolio_price_observations
               WHERE asset_id = ?
               ORDER BY observed_at DESC, id DESC
               LIMIT 1"#,
        )
        .bind(asset_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row)
    }

    // -----------------------------------------------------------------------
    // Activity log
    // -----------------------------------------------------------------------

    pub async fn insert_activity(
        &self,
        tx: &mut Transaction<'_, MySql>,
        act: &pconv::NewActivity,
    ) -> Result<pconv::DbActivity> {
        let id = act.id.clone().unwrap_or_else(new_id);
        let now = now_unix();
        sqlx::query(
            r#"INSERT INTO portfolio_asset_activities
                (id, asset_id, budget_id, activity_type, actor_user_id,
                 correlation_id, idempotency_key, occurred_at, payload_json, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&id)
        .bind(&act.asset_id)
        .bind(&act.budget_id)
        .bind(&act.activity_type)
        .bind(&act.actor_user_id)
        .bind(&act.correlation_id)
        .bind(&act.idempotency_key)
        .bind(act.occurred_at)
        .bind(&act.payload_json)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(|e: sqlx::Error| {
            if e.to_string().contains("Duplicate entry") {
                anyhow!(
                    "duplicate idempotency_key={} for asset={}",
                    act.idempotency_key.as_deref().unwrap_or(""),
                    act.asset_id
                )
            } else {
                anyhow::Error::new(e)
            }
        })?;

        Ok(pconv::DbActivity {
            id,
            asset_id: act.asset_id.clone(),
            budget_id: act.budget_id.clone(),
            activity_type: act.activity_type.clone(),
            actor_user_id: act.actor_user_id.clone(),
            correlation_id: act.correlation_id.clone(),
            idempotency_key: act.idempotency_key.clone(),
            occurred_at: act.occurred_at,
            payload_json: act.payload_json.clone(),
            created_at: now,
        })
    }

    pub async fn list_activities(
        &self,
        tx: &mut Transaction<'_, MySql>,
        asset_id: &str,
        limit: i32,
    ) -> Result<Vec<pconv::DbActivity>> {
        let cap = limit.clamp(1, 1000);
        let rows = sqlx::query_as::<_, pconv::DbActivity>(
            r#"SELECT id, asset_id, budget_id, activity_type, actor_user_id,
                      correlation_id, idempotency_key, occurred_at, payload_json, created_at
               FROM portfolio_asset_activities
               WHERE asset_id = ?
               ORDER BY occurred_at DESC, id DESC
               LIMIT ?"#,
        )
        .bind(asset_id)
        .bind(cap)
        .fetch_all(&mut **tx)
        .await?;
        Ok(rows)
    }

    // -----------------------------------------------------------------------
    // Outbox
    // -----------------------------------------------------------------------

    pub async fn insert_outbox(
        &self,
        tx: &mut Transaction<'_, MySql>,
        evt: &pconv::NewOutboxEvent,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO portfolio_outbox_events
                (id, event_type, asset_id, budget_id, payload_json,
                 enqueued_at, attempts)
            VALUES (?, ?, ?, ?, ?, ?, 0)"#,
        )
        .bind(&evt.id)
        .bind(&evt.event_type)
        .bind(&evt.asset_id)
        .bind(&evt.budget_id)
        .bind(&evt.payload_json)
        .bind(evt.enqueued_at)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn transfer_direction_str(v: i32) -> &'static str {
    use crate::pb::service::portfolio::TransferDirection as T;
    match T::try_from(v).unwrap_or(T::Unspecified) {
        T::StandardToInvest => "standard_to_invest",
        T::InvestToStandard => "invest_to_standard",
        T::InternalRebalance => "internal_rebalance",
        _ => "unspecified",
    }
}

fn transfer_status_str(v: i32) -> &'static str {
    use crate::pb::service::portfolio::TransferStatus as S;
    match S::try_from(v).unwrap_or(S::Unspecified) {
        S::Requested => "requested",
        S::CashLegPending => "cash_leg_pending",
        S::AssetLegPending => "asset_leg_pending",
        S::Completed => "completed",
        S::Failed => "failed",
        S::CompensationPending => "compensation_pending",
        S::Compensated => "compensated",
        _ => "unspecified",
    }
}

fn asset_class_to_db(c: &pconv::AssetClassNew) -> &'static str {
    match c {
        pconv::AssetClassNew::SavingsAccount => "savings_account",
        pconv::AssetClassNew::FixedDeposit => "fixed_deposit",
        pconv::AssetClassNew::GoldLot => "gold_lot",
        pconv::AssetClassNew::StockLot => "stock_lot",
        pconv::AssetClassNew::EtfLot => "etf_lot",
        pconv::AssetClassNew::CryptoLot => "crypto_lot",
    }
}

fn parse_decimal(value: &str) -> Result<Decimal> {
    use std::str::FromStr;
    Decimal::from_str(value).map_err(|_| anyhow!("invalid decimal: {value}"))
}

// Re-export so callers do not need to import the biz fifo module.
pub use crate::manager::biz::portfolio::fifo::{
    DisposalAllocation as FifoAllocation, Lot as FifoLot,
};
pub type RepoLot = FifoLot;
pub type RepoDisposalAllocation = FifoAllocation;
pub fn run_fifo(lots: &[Lot], qty: Decimal) -> Result<Vec<DisposalAllocation>> {
    fifo_disposal_allocations(lots, qty).map_err(|e| anyhow!("fifo allocation failed: {e}"))
}

// Suppress unused warning for proto alias path during incremental bring-up.
#[allow(dead_code)]
fn _silence_unused(_: pb::PortfolioAssetClass) {}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn fifo_helper_compiles() {
        let lots = vec![
            Lot {
                id: "a".into(),
                quantity_open: Decimal::from_str("1").unwrap(),
                cost_per_unit_minor: 100,
            },
            Lot {
                id: "b".into(),
                quantity_open: Decimal::from_str("2").unwrap(),
                cost_per_unit_minor: 200,
            },
        ];
        let alloc = run_fifo(&lots, Decimal::from_str("2.5").unwrap()).unwrap();
        assert_eq!(alloc.len(), 2);
        assert_eq!(alloc[0].lot_id, "a");
        assert_eq!(alloc[1].lot_id, "b");
    }

    #[test]
    fn fifo_helper_rejects_excess() {
        let lots = vec![Lot {
            id: "a".into(),
            quantity_open: Decimal::from_str("1").unwrap(),
            cost_per_unit_minor: 100,
        }];
        let err = run_fifo(&lots, Decimal::from_str("5").unwrap()).unwrap_err();
        assert!(err.to_string().contains("fifo"));
    }
}
