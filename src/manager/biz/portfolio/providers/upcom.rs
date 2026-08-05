//! UPCOM stock price provider stub.
//!
//! Same licensing caveats as `hose`. Disabled by default;
//! enable only after commercial agreement via `PORTFOLIO_ENABLE_UPCOM=1`.

use std::boxed::Box;

use crate::pb::service::portfolio as pb;

use super::{PriceFuture, PriceProvider};

pub struct UpcomProvider;

impl UpcomProvider {
    pub fn new() -> Self {
        Self
    }

    fn enabled() -> bool {
        std::env::var("PORTFOLIO_ENABLE_UPCOM")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }
}

impl Default for UpcomProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PriceProvider for UpcomProvider {
    fn name(&self) -> &'static str {
        "upcom"
    }

    fn supports(&self, asset: &pb::PortfolioAsset) -> bool {
        if !Self::enabled() {
            return false;
        }
        if asset.asset_class != pb::PortfolioAssetClass::StockLot as i32 {
            return false;
        }
        matches!(
            asset.details,
            Some(pb::portfolio_asset::Details::StockLot(ref s)) if s.exchange == pb::StockExchange::Upcom as i32
        )
    }

    fn fetch<'a>(
        &'a self,
        _client: &'a reqwest::Client,
        _assets: &'a [pb::PortfolioAsset],
    ) -> PriceFuture<'a> {
        if !Self::enabled() {
            tracing::debug!("upcom provider disabled by config");
        } else {
            tracing::warn!("upcom provider enabled but no implementation yet — returning empty");
        }
        Box::pin(async move { Ok(vec![]) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::service::portfolio as pb;

    fn stock_asset(exchange: i32) -> pb::PortfolioAsset {
        pb::PortfolioAsset {
            base: None,
            budget_id: "b1".into(),
            asset_class: pb::PortfolioAssetClass::StockLot as i32,
            display_name: "x".into(),
            currency: "VND".into(),
            status: 0,
            opened_on: 0,
            closed_on: 0,
            legacy_asset_id: String::new(),
            notes: String::new(),
            details: Some(pb::portfolio_asset::Details::StockLot(
                pb::PortfolioStockLot {
                    asset_id: "a1".into(),
                    ticker: "VVS".into(),
                    exchange,
                    quantity_bought: "100".into(),
                    quantity_open: "100".into(),
                    buy_price_per_share: 8_000,
                    purchase_cost: 800_000,
                    fees: 0,
                    purchase_date: 0,
                    settlement_date: 0,
                    notes: String::new(),
                },
            )),
        }
    }

    #[test]
    fn name_is_upcom() {
        assert_eq!(UpcomProvider::new().name(), "upcom");
    }

    #[test]
    fn supports_returns_false_when_disabled() {
        std::env::remove_var("PORTFOLIO_ENABLE_UPCOM");
        assert!(!UpcomProvider::new().supports(&stock_asset(pb::StockExchange::Upcom as i32)));
    }

    #[test]
    #[serial_test::serial]
    fn supports_returns_true_when_enabled() {
        std::env::set_var("PORTFOLIO_ENABLE_UPCOM", "1");
        let p = UpcomProvider::new();
        assert!(p.supports(&stock_asset(pb::StockExchange::Upcom as i32)));
        assert!(!p.supports(&stock_asset(pb::StockExchange::Hose as i32)));
        assert!(!p.supports(&stock_asset(pb::StockExchange::Hnx as i32)));
        std::env::remove_var("PORTFOLIO_ENABLE_UPCOM");
    }
}
