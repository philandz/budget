//! Noop provider: returns empty price list for every input by default.
//!
//! Default registry member. Lets the rest of the system wire up
//! before any real provider lands. Replaced in Phase 2 once SJC/DOJI/
//! PNJ or HOSE/HNX/UPCOM adapters are enabled.
//!
//! When `PORTFOLIO_TEST_GOLD_PRICE` is set to a JSON map of purity-name
//! to price-in-minor-units (e.g. `{"SJC9999":7500000}`), the noop
//! provider returns a single dummy price observation for gold assets
//! whose purity matches — useful for test environments where no live
//! gold scraping endpoint exists.

use std::collections::HashMap;

use crate::pb::service::portfolio as pb;

use super::{PriceFuture, PriceProvider};

pub struct NoopProvider {
    /// Test-mode gold prices: maps proto enum name (e.g. "SJC9999") to
    /// price in minor units (e.g. 7_500_000 VND/chi).
    test_gold_prices: HashMap<i32, i64>,
}

impl NoopProvider {
    pub fn new() -> Self {
        Self {
            test_gold_prices: Self::parse_test_gold_prices(),
        }
    }

    fn parse_test_gold_prices() -> HashMap<i32, i64> {
        let Some(json) = std::env::var("PORTFOLIO_TEST_GOLD_PRICE").ok() else {
            return HashMap::new();
        };
        // JSON map: {"SJC9999": 7500000, "DOJI9999": 7400000}
        let map: serde_json::Map<String, serde_json::Value> =
            match serde_json::from_str(&json) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("noop: invalid PORTFOLIO_TEST_GOLD_PRICE JSON: {e}");
                    return HashMap::new();
                }
            };
        let mut result = HashMap::new();
        for (k, v) in map {
            let price = v.as_i64().unwrap_or(0);
            let purity = match k.as_str() {
                "SJC9999" => pb::GoldPurity::Sjc9999 as i32,
                "PNJ999" => pb::GoldPurity::Pnj999 as i32,
                "PNJ995" => pb::GoldPurity::Pnj995 as i32,
                "DOJI9999" => pb::GoldPurity::Doji9999 as i32,
                _ => {
                    tracing::warn!("noop: unknown gold purity in PORTFOLIO_TEST_GOLD_PRICE: {k}");
                    continue;
                }
            };
            result.insert(purity, price);
        }
        result
    }

    fn test_price_for_asset(&self, asset: &pb::PortfolioAsset) -> Option<super::ProviderPrice> {
        let details = match &asset.details {
            Some(pb::portfolio_asset::Details::GoldLot(g)) => g,
            _ => return None,
        };
        let price = *self.test_gold_prices.get(&details.purity)?;
        Some(super::ProviderPrice {
            asset_id: asset.base.as_ref()?.id.clone(),
            provider: "noop-test".to_string(),
            unit_price: price,
            price_side: "mid",
            source_reference: String::new(),
            notes: String::new(),
        })
    }
}

impl Default for NoopProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PriceProvider for NoopProvider {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn supports(&self, asset: &pb::PortfolioAsset) -> bool {
        // In test mode, support gold assets when test prices are configured.
        if !self.test_gold_prices.is_empty() {
            if let Some(pb::portfolio_asset::Details::GoldLot(g)) = &asset.details {
                return self.test_gold_prices.contains_key(&g.purity);
            }
        }
        // Normal mode: noop supports nothing — other providers take precedence.
        false
    }

    fn fetch<'a>(
        &'a self,
        _client: &'a reqwest::Client,
        assets: &'a [pb::PortfolioAsset],
    ) -> PriceFuture<'a> {
        let prices: Vec<_> = assets
            .iter()
            .filter_map(|a| self.test_price_for_asset(a))
            .collect();
        Box::pin(async move { Ok(prices) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::service::portfolio as pb;

    fn dummy_asset() -> pb::PortfolioAsset {
        pb::PortfolioAsset {
            base: None,
            budget_id: "b1".into(),
            asset_class: pb::PortfolioAssetClass::GoldLot as i32,
            display_name: "x".into(),
            currency: "VND".into(),
            status: 0,
            opened_on: 0,
            closed_on: 0,
            legacy_asset_id: String::new(),
            notes: String::new(),
            details: None,
        }
    }

    #[test]
    fn noop_name() {
        assert_eq!(NoopProvider::new().name(), "noop");
    }

    #[test]
    fn noop_supports_nothing() {
        assert!(!NoopProvider::new().supports(&dummy_asset()));
    }

    #[tokio::test]
    async fn noop_fetch_returns_empty() {
        let p = NoopProvider::new();
        let client = reqwest::Client::new();
        let result = p.fetch(&client, &[dummy_asset()]).await.unwrap();
        assert!(result.is_empty());
    }
}
