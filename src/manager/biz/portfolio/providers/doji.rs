//! DOJI gold price provider stub.
//!
//! Same licensing caveats as `sjc` provider. Disabled by default;
//! enable only after commercial agreement via `PORTFOLIO_ENABLE_DOJI=1`.

use std::boxed::Box;

use crate::pb::service::portfolio as pb;

use super::{PriceFuture, PriceProvider};

pub struct DojiProvider;

impl DojiProvider {
    pub fn new() -> Self {
        Self
    }

    fn enabled() -> bool {
        std::env::var("PORTFOLIO_ENABLE_DOJI")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }
}

impl Default for DojiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PriceProvider for DojiProvider {
    fn name(&self) -> &'static str {
        "doji"
    }

    fn supports(&self, asset: &pb::PortfolioAsset) -> bool {
        if !Self::enabled() {
            return false;
        }
        if asset.asset_class != pb::PortfolioAssetClass::GoldLot as i32 {
            return false;
        }
        match &asset.details {
            Some(pb::portfolio_asset::Details::GoldLot(g)) => {
                matches!(
                    g.purity,
                    x if x == pb::GoldPurity::Doji9999 as i32
                        || x == pb::GoldPurity::Pnj999 as i32
                        || x == pb::GoldPurity::Pnj995 as i32
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
        if !Self::enabled() {
            tracing::debug!("doji provider disabled by config");
        } else {
            tracing::warn!("doji provider enabled but no implementation yet — returning empty");
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
                    provider: "DOJI".into(),
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
    fn name_is_doji() {
        assert_eq!(DojiProvider::new().name(), "doji");
    }

    #[test]
    fn supports_returns_false_when_disabled() {
        std::env::remove_var("PORTFOLIO_ENABLE_DOJI");
        assert!(!DojiProvider::new().supports(&gold_asset(pb::GoldPurity::Doji9999 as i32)));
    }

    #[test]
    #[serial_test::serial]
    fn supports_returns_true_when_enabled() {
        std::env::set_var("PORTFOLIO_ENABLE_DOJI", "1");
        let p = DojiProvider::new();
        assert!(p.supports(&gold_asset(pb::GoldPurity::Doji9999 as i32)));
        assert!(p.supports(&gold_asset(pb::GoldPurity::Pnj999 as i32)));
        assert!(p.supports(&gold_asset(pb::GoldPurity::Pnj995 as i32)));
        std::env::remove_var("PORTFOLIO_ENABLE_DOJI");
    }
}
