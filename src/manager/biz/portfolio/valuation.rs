//! Portfolio valuation: per-asset current-value, cost-basis, and P&L rollup.
//!
//! The core `value_asset` function is pure in its inputs — it takes an
//! asset snapshot plus a "today" date and returns a `Valuation` record.
//! It calls into `fifo.rs` for the FIFO disposal primitive on stock lots.

use std::sync::Arc;
use tonic::Status;

use crate::converters::portfolio as pconv;
use crate::manager::biz::portfolio::interest::{compound_accrued, simple_accrued};
use crate::manager::biz::portfolio::{InterestMethod, PriceFreshness};
use crate::manager::repository::portfolio::PortfolioRepository;

pub struct PortfolioValuation {
    pub repo: Arc<PortfolioRepository>,
}

impl PortfolioValuation {
    pub fn new(repo: Arc<PortfolioRepository>) -> Self {
        Self { repo }
    }

    /// Per-asset valuation. Returns current_value, open_cost_basis,
    /// realized_pnl, unrealized_pnl, accrued_interest, freshness,
    /// quote_observed_at.
    pub async fn value_asset(
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
            "crypto_lot" => {
                let sub = self
                    .repo
                    .get_crypto_lot(tx, &asset.id)
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
                let qty_open = parse_decimal(&sub.quantity_open).map_err(internal)?;
                let qty_open_f = qty_open.to_string().parse::<f64>().unwrap_or(0.0);
                let current_value = (qty_open_f * obs.unit_price as f64).round() as i64;
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
            _ => Ok(Valuation::unpriced()),
        }
    }
}

pub struct Valuation {
    pub current_value: i64,
    pub open_cost_basis: i64,
    pub realized_pnl: i64,
    pub unrealized_pnl: i64,
    pub accrued_interest: i64,
    pub freshness: i32,
    pub quote_observed_at: i64,
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

/// Returns today's business date in UTC+7, aligned to midnight.
pub fn today_business_date() -> i64 {
    let now_utc = chrono::Utc::now();
    let ict = now_utc + chrono::Duration::hours(7);
    ict.timestamp() - (ict.timestamp() % 86_400)
}
