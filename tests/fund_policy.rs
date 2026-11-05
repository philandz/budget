//! Fund rules that need no database: asset-class policy (I8) and transfer request validation.

use budget::converters::portfolio::AssetClassNew::*;
use budget::manager::biz::portfolio::fund::{is_fund_type, validate_fund_transfer};
use budget::manager::biz::portfolio::policy::{allowed_asset_classes, check, PolicyVerdict::*};
use budget::pb::service::budget::BudgetType;
use budget::pb::service::portfolio::FundTransferRequest;
use tonic::Code;

#[test]
fn saving_allows_low_risk_classes_only() {
    for c in [SavingsAccount, FixedDeposit, GoldLot] {
        assert_eq!(check(BudgetType::Saving, c, false), Allowed);
    }
    for c in [StockLot, EtfLot, CryptoLot] {
        assert_eq!(check(BudgetType::Saving, c, false), Denied);
        assert_eq!(check(BudgetType::Saving, c, true), Denied);
    }
}

#[test]
fn invest_warns_until_enforced() {
    for c in [StockLot, EtfLot, CryptoLot] {
        assert_eq!(check(BudgetType::Invest, c, false), Allowed);
    }
    for c in [SavingsAccount, FixedDeposit, GoldLot] {
        assert_eq!(check(BudgetType::Invest, c, false), Warn);
        assert_eq!(check(BudgetType::Invest, c, true), Denied);
    }
}

#[test]
fn other_budget_types_are_not_covered() {
    for t in [BudgetType::Standard, BudgetType::Debt, BudgetType::Sharing] {
        assert!(allowed_asset_classes(t).is_none());
        assert_eq!(check(t, StockLot, true), Allowed);
    }
}

#[test]
fn only_saving_and_invest_are_funds() {
    assert!(is_fund_type(BudgetType::Saving));
    assert!(is_fund_type(BudgetType::Invest));
    assert!(!is_fund_type(BudgetType::Standard));
    assert!(!is_fund_type(BudgetType::Sharing));
}

fn req() -> FundTransferRequest {
    FundTransferRequest {
        budget_id: "fund".into(),
        counterparty_budget_id: "std".into(),
        amount_minor: 1000,
        occurred_on: "2026-10-23".into(),
        note: String::new(),
        idempotency_key: "k1".into(),
    }
}

#[test]
fn fund_transfer_validation() {
    assert!(validate_fund_transfer(&req()).is_ok());
    let bad: Vec<FundTransferRequest> = vec![
        FundTransferRequest {
            amount_minor: 0,
            ..req()
        },
        FundTransferRequest {
            amount_minor: -1,
            ..req()
        },
        FundTransferRequest {
            counterparty_budget_id: "fund".into(),
            ..req()
        },
        FundTransferRequest {
            idempotency_key: " ".into(),
            ..req()
        },
        FundTransferRequest {
            idempotency_key: "x".repeat(61),
            ..req()
        },
        FundTransferRequest {
            occurred_on: "23-10-2026".into(),
            ..req()
        },
        FundTransferRequest {
            note: "n".repeat(501),
            ..req()
        },
        FundTransferRequest {
            budget_id: String::new(),
            ..req()
        },
    ];
    for r in bad {
        assert_eq!(
            validate_fund_transfer(&r).unwrap_err().code(),
            Code::InvalidArgument,
            "{r:?}"
        );
    }
}
