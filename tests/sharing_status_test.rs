use budget::manager::validate::{settable_status, sharing_currency};
use budget::pb::service::budget::BudgetType;

#[test]
fn sharing_is_vnd_only() {
    assert_eq!(sharing_currency(BudgetType::Sharing, "").unwrap(), "VND");
    assert_eq!(sharing_currency(BudgetType::Sharing, "vnd").unwrap(), "VND");
    assert!(sharing_currency(BudgetType::Sharing, "USD").is_err());
}

#[test]
fn other_types_keep_currency() {
    assert_eq!(
        sharing_currency(BudgetType::Standard, "USD").unwrap(),
        "USD"
    );
}

#[test]
fn only_active_and_archived_settable() {
    assert!(settable_status("active").is_ok());
    assert!(settable_status("archived").is_ok());
    assert!(settable_status("deleted").is_err());
    assert!(settable_status("closed").is_err());
}
