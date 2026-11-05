//! Asset-class policy per budget type (plan I8).
//!
//! Saving funds hold low-risk assets (savings account, fixed deposit, gold); Invest budgets hold
//! market assets (stock, ETF, crypto). Saving is enforced from day one because saving budgets had
//! no portfolio before. Invest only warns until `FEATURE_ASSET_POLICY_ENFORCE` is switched on, so
//! existing invest users keep working. Other budget types are not covered by this policy.

use crate::converters::portfolio::AssetClassNew;
use crate::pb::service::budget::BudgetType;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum PolicyVerdict {
    Allowed,
    /// Not allowed for this budget type but tolerated (warn + metric).
    Warn,
    Denied,
}

pub fn allowed_asset_classes(t: BudgetType) -> Option<&'static [AssetClassNew]> {
    use AssetClassNew::*;
    match t {
        BudgetType::Saving => Some(&[SavingsAccount, FixedDeposit, GoldLot]),
        BudgetType::Invest => Some(&[StockLot, EtfLot, CryptoLot]),
        _ => None,
    }
}

pub fn check(t: BudgetType, class: AssetClassNew, enforce_invest: bool) -> PolicyVerdict {
    match allowed_asset_classes(t) {
        None => PolicyVerdict::Allowed,
        Some(list) if list.contains(&class) => PolicyVerdict::Allowed,
        Some(_) if t == BudgetType::Invest && !enforce_invest => PolicyVerdict::Warn,
        Some(_) => PolicyVerdict::Denied,
    }
}

pub fn enforce_invest_from_env() -> bool {
    matches!(
        std::env::var("FEATURE_ASSET_POLICY_ENFORCE").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}
