//! SJC gold price provider stub.
//!
//! IMPORTANT: The spec (§8.3) and external research both flag that
//! public gold price pages (SJC, DOJI, PNJ) lack stable public APIs.
//! Their HTML pages are subject to change, anti-bot controls, and
//! commercial terms of service.
//!
//! This stub returns empty results unless the `PORTFOLIO_ENABLE_SJC`
//! env var is set to "1". Even when enabled, scraping without a
//! written commercial agreement is risky. Wire this only after the
//! product team confirms the licensing position.

use std::boxed::Box;

use crate::pb::service::portfolio as pb;

use super::{PriceFuture, PriceProvider};

pub struct SjcProvider;

impl SjcProvider {
    pub fn new() -> Self {
        Self
    }

    /// True if the operator has explicitly enabled SJC scraping via
    /// `PORTFOLIO_ENABLE_SJC=1`. Default is false (no scraping).
    fn enabled() -> bool {
        std::env::var("PORTFOLIO_ENABLE_SJC")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }
}

impl Default for SjcProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PriceProvider for SjcProvider {
    fn name(&self) -> &'static str {
        "sjc"
    }

    fn supports(&self, asset: &pb::PortfolioAsset) -> bool {
        if !Self::enabled() {
            return false;
        }
        if asset.asset_class != pb::PortfolioAssetClass::GoldLot as i32 {
            return false;
        }
        // Only SJC-labelled purity. Matches by proto enum value since
        // purity is an i32 (GoldPurity enum), not a String.
        match &asset.details {
            Some(pb::portfolio_asset::Details::GoldLot(g)) => {
                matches!(
                    g.purity,
                    x if x == pb::GoldPurity::Sjc9999 as i32
                        || x == pb::GoldPurity::Doji9999 as i32
                )
            }
            _ => false,
        }
    }

    fn fetch<'a>(
        &'a self,
        _client: &'a reqwest::Client,
        _assets: &'a [pb::PortfolioAsset],
    ) -> PriceFuture<'a> {
        // Fetching from sjc.com.vn requires HTML scraping and is not
        // covered by a stable public API. Phase 2 leaves this empty
        // so the registry caller never blocks on a dead-end network
        // request. A future SJC adapter can implement this once the
        // product team signs a commercial data agreement.
        if !Self::enabled() {
            tracing::debug!("sjc provider disabled by config");
        } else {
            tracing::warn!("sjc provider enabled but no implementation yet — returning empty");
        }
        Box::pin(async move { Ok(vec![]) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::service::portfolio as pb;

    fn gold_asset(purity: i32) -> pb::PortfolioAsset {
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
            details: Some(pb::portfolio_asset::Details::GoldLot(
                pb::PortfolioGoldLot {
                    asset_id: "a1".into(),
                    provider: "SJC".into(),
                    gold_type: "ring".into(),
                    purity,
                    form: pb::GoldForm::Other as i32,
                    quantity_original: "1".into(),
                    unit_original: pb::GoldUnit::Chi as i32,
                    quantity_grams: "3.75".into(),
                    purchase_price_per_unit_original: 0,
                    purchase_cost: 0,
                    fees: 0,
                    purchase_date: 0,
                    notes: String::new(),
                },
            )),
        }
    }

    #[test]
    fn name_is_sjc() {
        assert_eq!(SjcProvider::new().name(), "sjc");
    }

    #[test]
    fn supports_returns_false_when_disabled() {
        std::env::remove_var("PORTFOLIO_ENABLE_SJC");
        assert!(!SjcProvider::new().supports(&gold_asset(pb::GoldPurity::Sjc9999 as i32)));
    }

    #[test]
    #[serial_test::serial]
    fn supports_returns_true_when_enabled() {
        std::env::set_var("PORTFOLIO_ENABLE_SJC", "1");
        let p = SjcProvider::new();
        assert!(p.supports(&gold_asset(pb::GoldPurity::Sjc9999 as i32)));
        assert!(p.supports(&gold_asset(pb::GoldPurity::Doji9999 as i32)));
        std::env::remove_var("PORTFOLIO_ENABLE_SJC");
    }
}
