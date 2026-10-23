//! T0.2 — budget_type is immutable after creation (invariant I6).

use budget::manager::validate::resolve_budget_type;
use budget::pb::service::budget::BudgetType;
use tonic::Code;

const ALL: [BudgetType; 5] = [
    BudgetType::Standard,
    BudgetType::Saving,
    BudgetType::Debt,
    BudgetType::Invest,
    BudgetType::Sharing,
];

#[test]
fn same_type_is_accepted() {
    for t in ALL {
        assert_eq!(resolve_budget_type(t, t).unwrap(), t);
    }
}

#[test]
fn unspecified_keeps_current_type() {
    // Previously an update without a type silently rewrote the budget to "standard".
    for t in ALL {
        assert_eq!(resolve_budget_type(t, BudgetType::Unspecified).unwrap(), t);
    }
}

#[test]
fn any_other_type_is_rejected_with_failed_precondition() {
    for current in ALL {
        for requested in ALL {
            if current == requested {
                continue;
            }
            let err = resolve_budget_type(current, requested)
                .expect_err("changing type must be rejected");
            assert_eq!(
                err.code(),
                Code::FailedPrecondition,
                "{current:?} -> {requested:?}"
            );
        }
    }
}
