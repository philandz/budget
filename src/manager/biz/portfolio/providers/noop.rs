//! Noop provider: returns empty price list for every input.
//!
//! Default registry member. Lets the rest of the system wire up
//! before any real provider lands. Replaced in Phase 2 once SJC/DOJI/
//! PNJ or HOSE/HNX/UPCOM adapters are enabled.

use crate::pb::service::portfolio as pb;

use super::{PriceFuture, PriceProvider};

pub struct NoopProvider;

impl NoopProvider {
    pub fn new() -> Self {
        Self
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

    fn supports(&self, _asset: &pb::PortfolioAsset) -> bool {
        // Noop supports nothing — always returns empty. Other providers
        // take precedence.
        false
    }

    fn fetch<'a>(
        &'a self,
        _client: &'a reqwest::Client,
        _assets: &'a [pb::PortfolioAsset],
    ) -> PriceFuture<'a> {
        Box::pin(async move { Ok(vec![]) })
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
