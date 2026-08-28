//! HOSE stock price provider stub.
//!
//! IMPORTANT: HOSE public market data is governed by Ho Chi Minh City
//! Stock Exchange. Scraping the public quote pages is not permitted
//! without a commercial data feed agreement. Real Vietnamese broker
//! APIs (SSI iBoard, TCBS tcinvest, VNDIRECT) require authentication
//! and licensing.
//!
//! This stub is disabled by default. Enable with
//! `PORTFOLIO_ENABLE_HOSE=1` after the product team confirms a
//! commercial data agreement. Even when enabled, the implementation
//! is intentionally empty.

use std::boxed::Box;

use crate::pb::service::portfolio as pb;

use super::{PriceFuture, PriceProvider};

pub struct HoseProvider;

impl HoseProvider {
    pub fn new() -> Self {
        Self
    }

    fn enabled() -> bool {
        std::env::var("PORTFOLIO_ENABLE_HOSE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }
}

impl Default for HoseProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PriceProvider for HoseProvider {
    fn name(&self) -> &'static str {
        "hose"
    }

    fn supports(&self, asset: &pb::PortfolioAsset) -> bool {
        if !Self::enabled() {
            return false;
        }
        if asset.asset_class != pb::PortfolioAssetClass::StockLot as i32 {
            return false;
        }
        // Match HOSE exchange; HNX and UPCOM have their own providers.
        matches!(
            asset.details,
            Some(pb::portfolio_asset::Details::StockLot(ref s)) if s.exchange == pb::StockExchange::Hose as i32
        )
    }

    fn fetch<'a>(
        &'a self,
        _client: &'a reqwest::Client,
        _assets: &'a [pb::PortfolioAsset],
    ) -> PriceFuture<'a> {
        if !Self::enabled() {
            tracing::debug!("hose provider disabled by config");
        } else {
            tracing::warn!("hose provider enabled but no implementation yet — returning empty");
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
                    ticker: "VNM".into(),
                    exchange,
                    quantity_bought: "100".into(),
                    quantity_open: "100".into(),
                    buy_price_per_share: 75_000,
                    purchase_cost: 7_500_000,
                    fees: 0,
                    purchase_date: 0,
                    settlement_date: 0,
                    notes: String::new(),
                },
            )),
        }
    }

    #[test]
    fn name_is_hose() {
        assert_eq!(HoseProvider::new().name(), "hose");
    }

    #[test]
    fn supports_returns_false_when_disabled() {
        std::env::remove_var("PORTFOLIO_ENABLE_HOSE");
        assert!(!HoseProvider::new().supports(&stock_asset(pb::StockExchange::Hose as i32)));
    }

    #[test]
    #[serial_test::serial]
    fn supports_returns_true_when_enabled() {
        std::env::set_var("PORTFOLIO_ENABLE_HOSE", "1");
        let p = HoseProvider::new();
        assert!(p.supports(&stock_asset(pb::StockExchange::Hose as i32)));
        // Other exchanges should not match.
        assert!(!p.supports(&stock_asset(pb::StockExchange::Hnx as i32)));
        assert!(!p.supports(&stock_asset(pb::StockExchange::Upcom as i32)));
        std::env::remove_var("PORTFOLIO_ENABLE_HOSE");
    }
}
