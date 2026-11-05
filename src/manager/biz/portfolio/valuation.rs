//! Portfolio valuation: per-asset current-value, cost-basis, and P&L rollup.
//!
//! The core `value_asset` function is pure in its inputs — it takes an
//! asset snapshot plus a "today" date and returns a `Valuation` record.
//! It calls into `fifo.rs` for the FIFO disposal primitive on stock lots.

use std::sync::Arc;
use tonic::Status;

use crate::converters::portfolio as pconv;
use crate::manager::biz::portfolio::interest::{compound_accrued, simple_accrued};
use crate::manager::biz::portfolio::{AssetStatus, InterestMethod, PriceFreshness};
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
        // Assets that left the portfolio (sold, withdrawn, rolled over, closed, archived) no longer
        // hold value; counting them would double-count rollovers and settled deposits.
        if !holds_value(AssetStatus::from_db(&asset.status)) {
            return Ok(Valuation::settled());
        }
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
                // Partial sales shrink the lot: value only the grams still held, at the
                // proportional share of the original cost.
                let total_grams = parse_decimal(&sub.quantity_grams).map_err(internal)?;
                let sold_grams = self
                    .repo
                    .gold_sold_grams(tx, &asset.id)
                    .await
                    .map_err(internal)?;
                let (open_grams, open_cost) =
                    gold_open_position(total_grams, sold_grams, sub.purchase_cost);
                let obs = self
                    .repo
                    .latest_price_observation(tx, &asset.id)
                    .await
                    .map_err(internal)?;
                let Some(obs) = obs else {
                    return Ok(Valuation {
                        current_value: open_cost,
                        open_cost_basis: open_cost,
                        realized_pnl: 0,
                        unrealized_pnl: 0,
                        accrued_interest: 0,
                        freshness: PriceFreshness::Unpriced as i32,
                        quote_observed_at: 0,
                    });
                };
                let grams_f = open_grams.to_string().parse::<f64>().unwrap_or(0.0);
                let current_value = (grams_f * obs.unit_price as f64).round() as i64;
                let freshness =
                    PriceFreshness::from_age_seconds((today - obs.observed_at).max(0), true);
                Ok(Valuation {
                    current_value,
                    open_cost_basis: open_cost,
                    realized_pnl: 0,
                    unrealized_pnl: current_value - open_cost,
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

/// Whether an asset in this status still counts towards portfolio value.
pub fn holds_value(status: AssetStatus) -> bool {
    matches!(status, AssetStatus::Active | AssetStatus::Matured)
}

/// Grams still held and the proportional share of the original cost they carry.
pub fn gold_open_position(
    total_grams: rust_decimal::Decimal,
    sold_grams: rust_decimal::Decimal,
    purchase_cost: i64,
) -> (rust_decimal::Decimal, i64) {
    use rust_decimal::prelude::ToPrimitive;
    let open = (total_grams - sold_grams).max(rust_decimal::Decimal::ZERO);
    if total_grams <= rust_decimal::Decimal::ZERO {
        return (open, 0);
    }
    let cost = (rust_decimal::Decimal::from(purchase_cost) * open / total_grams)
        .round()
        .to_i64()
        .unwrap_or(0);
    (open, cost)
}

impl Valuation {
    fn settled() -> Self {
        Self {
            current_value: 0,
            open_cost_basis: 0,
            realized_pnl: 0,
            unrealized_pnl: 0,
            accrued_interest: 0,
            freshness: PriceFreshness::Fresh as i32,
            quote_observed_at: 0,
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn only_active_and_matured_assets_hold_value() {
        assert!(holds_value(AssetStatus::Active));
        assert!(holds_value(AssetStatus::Matured));
        for s in [
            AssetStatus::Closed,
            AssetStatus::Sold,
            AssetStatus::Archived,
            AssetStatus::RolledOver,
            AssetStatus::Withdrawn,
            AssetStatus::EarlyClosed,
        ] {
            assert!(!holds_value(s), "{s:?}");
        }
    }

    #[test]
    fn gold_partial_sale_keeps_proportional_cost() {
        // 10 g bought for 10,000,000; 4 g sold → 6 g and 6,000,000 left.
        let (open, cost) = gold_open_position(d("10"), d("4"), 10_000_000);
        assert_eq!(open, d("6"));
        assert_eq!(cost, 6_000_000);
        // sold everything
        assert_eq!(
            gold_open_position(d("10"), d("10"), 10_000_000),
            (d("0"), 0)
        );
        // over-sold input never goes negative
        assert_eq!(gold_open_position(d("10"), d("11"), 10_000_000).0, d("0"));
        // empty lot
        assert_eq!(gold_open_position(d("0"), d("0"), 5), (d("0"), 0));
    }
}
