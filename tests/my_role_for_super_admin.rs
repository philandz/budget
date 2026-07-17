//! When a super admin reads /api/budget/budgets/{id} even though they're
//! not in budget_members, the returned `my_role` must be Owner (1), not
//! Viewer (4) (the converter's fallback).

use budget::manager::biz::BudgetBiz;
use budget::pb::service::budget::BudgetRole;

#[tokio::test]
async fn resolve_role_grants_owner_for_super_admin() {
    // We don't need a real DB / identity client here — the bypass is the
    // first thing resolve_role checks and returns immediately.
    let biz = BudgetBiz::test_only_no_clients().await;
    let role = biz
        .resolve_role_for_test("any-budget-id", "any-user-id", Some("super_admin"))
        .await
        .expect("super_admin must resolve");
    assert!(matches!(role, BudgetRole::Owner), "got {:?}", role);
}

#[tokio::test]
async fn resolve_role_does_not_grant_owner_for_normal_user() {
    let biz = BudgetBiz::test_only_no_clients().await;
    // No DB, no identity client → falls through every step. The repo call
    // may fail with NotFound or the identity fallback may fail. Either way,
    // the caller must NOT get BudgetRole::Owner from the super-admin bypass.
    let result = biz
        .resolve_role_for_test("any-budget-id", "any-user-id", Some("normal"))
        .await;
    match result {
        Ok(role) => assert!(
            !matches!(role, BudgetRole::Owner),
            "non-admin callers must NOT bypass, got {:?}",
            role
        ),
        Err(_) => { /* expected — repo calls fail in this minimal fixture */ }
    }
}
