//! Portfolio domain primitives: money, rate, quantity, gold unit
//! conversion, interest math, FIFO disposal, and lifecycle.
//!
//! All functions are pure. No DB, no network, no clock — the only
//! time-related input is the caller-supplied "today" date. This keeps
//! valuation logic deterministic and easy to unit-test.

pub mod biz;
pub mod fifo;
pub mod gold;
pub mod interest;
pub mod lifecycle;
pub mod maturity;
pub mod money;
pub mod outbox;
pub mod providers;
pub mod quantity;
pub mod rate;
pub mod refresh;
pub mod rollover;
pub mod transfers;

pub use fifo::{fifo_disposal_allocations, DisposalAllocation, Lot};
pub use gold::{grams_from_quantity, GoldUnit};
pub use interest::{
    compound_accrued, simple_accrued, simple_interest_only, InterestMethod, PayoutType,
};
pub use lifecycle::{next_status, LifecycleError, Transition};
pub use money::Money;
pub use quantity::Quantity;
pub use rate::Rate;

use crate::pb::service::portfolio as pb;

/// Asset class enum. Maps 1:1 to the proto `PortfolioAssetClass` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetClass {
    SavingsAccount,
    FixedDeposit,
    GoldLot,
    StockLot,
}

impl AssetClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            AssetClass::SavingsAccount => "savings_account",
            AssetClass::FixedDeposit => "fixed_deposit",
            AssetClass::GoldLot => "gold_lot",
            AssetClass::StockLot => "stock_lot",
        }
    }

    pub fn from_proto(value: i32) -> Option<Self> {
        match value {
            x if x == pb::PortfolioAssetClass::SavingsAccount as i32 => {
                Some(AssetClass::SavingsAccount)
            }
            x if x == pb::PortfolioAssetClass::FixedDeposit as i32 => {
                Some(AssetClass::FixedDeposit)
            }
            x if x == pb::PortfolioAssetClass::GoldLot as i32 => Some(AssetClass::GoldLot),
            x if x == pb::PortfolioAssetClass::StockLot as i32 => Some(AssetClass::StockLot),
            _ => None,
        }
    }

    pub fn to_proto(self) -> i32 {
        match self {
            AssetClass::SavingsAccount => pb::PortfolioAssetClass::SavingsAccount as i32,
            AssetClass::FixedDeposit => pb::PortfolioAssetClass::FixedDeposit as i32,
            AssetClass::GoldLot => pb::PortfolioAssetClass::GoldLot as i32,
            AssetClass::StockLot => pb::PortfolioAssetClass::StockLot as i32,
        }
    }
}

/// Asset status enum. Maps 1:1 to the proto `PortfolioAssetStatus` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetStatus {
    Active,
    Closed,
    Matured,
    Sold,
    Archived,
    RolledOver,
    Withdrawn,
    EarlyClosed,
}

impl AssetStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AssetStatus::Active => "active",
            AssetStatus::Closed => "closed",
            AssetStatus::Matured => "matured",
            AssetStatus::Sold => "sold",
            AssetStatus::Archived => "archived",
            AssetStatus::RolledOver => "rolled_over",
            AssetStatus::Withdrawn => "withdrawn",
            AssetStatus::EarlyClosed => "early_closed",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "closed" => AssetStatus::Closed,
            "matured" => AssetStatus::Matured,
            "sold" => AssetStatus::Sold,
            "archived" => AssetStatus::Archived,
            "rolled_over" => AssetStatus::RolledOver,
            "withdrawn" => AssetStatus::Withdrawn,
            "early_closed" => AssetStatus::EarlyClosed,
            // Default to active for safety; lifecycle::next_status enforces transitions.
            _ => AssetStatus::Active,
        }
    }

    pub fn to_db(self) -> &'static str {
        self.as_str()
    }
}

/// Price side enum (bid / ask / mid) for valuation snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSide {
    Bid,
    Ask,
    Mid,
}

impl PriceSide {
    pub fn from_db(value: &str) -> Self {
        match value {
            "bid" => PriceSide::Bid,
            "ask" => PriceSide::Ask,
            _ => PriceSide::Mid,
        }
    }

    pub fn to_db(self) -> &'static str {
        match self {
            PriceSide::Bid => "bid",
            PriceSide::Ask => "ask",
            PriceSide::Mid => "mid",
        }
    }
}

/// Price freshness labels. 7-day window for MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceFreshness {
    Fresh,
    Stale,
    Unpriced,
}

impl PriceFreshness {
    /// Derive freshness from the age in seconds of the latest observation.
    /// 7 calendar days matches the spec default.
    pub fn from_age_seconds(age_seconds: i64, has_observation: bool) -> Self {
        if !has_observation {
            return PriceFreshness::Unpriced;
        }
        const SEVEN_DAYS_SECS: i64 = 7 * 24 * 60 * 60;
        if age_seconds <= SEVEN_DAYS_SECS {
            PriceFreshness::Fresh
        } else {
            PriceFreshness::Stale
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_class_round_trip() {
        for c in [
            AssetClass::SavingsAccount,
            AssetClass::FixedDeposit,
            AssetClass::GoldLot,
            AssetClass::StockLot,
        ] {
            assert_eq!(AssetClass::from_proto(c.to_proto()), Some(c));
        }
    }

    #[test]
    fn asset_class_unknown_proto_returns_none() {
        assert_eq!(AssetClass::from_proto(9999), None);
        assert_eq!(AssetClass::from_proto(0), None); // UNSPECIFIED
    }

    #[test]
    fn asset_status_db_round_trip() {
        for s in [
            AssetStatus::Active,
            AssetStatus::Closed,
            AssetStatus::Matured,
            AssetStatus::Sold,
            AssetStatus::Archived,
            AssetStatus::RolledOver,
            AssetStatus::Withdrawn,
            AssetStatus::EarlyClosed,
        ] {
            assert_eq!(AssetStatus::from_db(s.to_db()), s);
        }
    }

    #[test]
    fn asset_status_unknown_db_defaults_active() {
        // The legacy `invest_assets.status` column has values like "active" and
        // "matured" only. Anything else defaults to active and lifecycle
        // enforcement happens on mutation, not on read.
        assert_eq!(AssetStatus::from_db(""), AssetStatus::Active);
        assert_eq!(AssetStatus::from_db("garbage"), AssetStatus::Active);
    }

    #[test]
    fn price_freshness_age_thresholds() {
        let day = 24 * 60 * 60;
        assert_eq!(
            PriceFreshness::from_age_seconds(0, true),
            PriceFreshness::Fresh
        );
        assert_eq!(
            PriceFreshness::from_age_seconds(7 * day, true),
            PriceFreshness::Fresh
        );
        assert_eq!(
            PriceFreshness::from_age_seconds(7 * day + 1, true),
            PriceFreshness::Stale
        );
        assert_eq!(
            PriceFreshness::from_age_seconds(0, false),
            PriceFreshness::Unpriced
        );
    }
}
