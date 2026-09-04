//! Integration test: DELETE /budgets/{id} cascade for sharing + invest types.
//!
//! Tests:
//! - Sharing: budget + sharing_participants + sharing_expenses + sharing_expense_legs gone after delete
//! - Invest: budget + invest_assets + budget_members gone after delete
//!
//! Run with: DATABASE_URL=... cargo test --test integration_delete_cascade -- --ignored --nocapture

use sqlx::MySqlPool;

use budget::manager::biz::BudgetBiz;
use budget::manager::repository::BudgetRepository;
use budget::pb::service::budget::{BudgetRole, BudgetType};

/// Build a BudgetRepository connected to DATABASE_URL.
async fn make_repo() -> BudgetRepository {
    let config = philand_configs::BudgetServiceConfig::default_for_tests();
    BudgetRepository::new(&config)
        .await
        .expect("BudgetRepository::new failed — check DATABASE_URL")
}

/// Seed a sharing budget with org_id, created by test-user. Returns budget_id.
async fn seed_sharing_budget(org_id: &str, user_id: &str) -> String {
    let repo = make_repo().await;
    let budget = repo
        .create_budget(org_id, "Sharing Cascade Test", BudgetType::Sharing, "VND", user_id)
        .await
        .expect("seed_sharing_budget failed");
    budget.id
}

/// Seed an invest budget with org_id, created by test-user. Returns budget_id.
async fn seed_invest_budget(org_id: &str, user_id: &str) -> String {
    let repo = make_repo().await;
    let budget = repo
        .create_budget(org_id, "Invest Cascade Test", BudgetType::Invest, "VND", user_id)
        .await
        .expect("seed_invest_budget failed");
    budget.id
}

/// Add two members to a budget (one owner, one contributor).
async fn add_members(budget_id: &str, owner_id: &str, member_id: &str) {
    let repo = make_repo().await;
    repo.add_member(budget_id, owner_id, BudgetRole::Owner)
        .await
        .expect("add_owner failed");
    repo.add_member(budget_id, member_id, BudgetRole::Contributor)
        .await
        .expect("add_member failed");
}

/// Insert sharing_participants rows directly for cascade test.
async fn insert_sharing_participants(pool: &MySqlPool, budget_id: &str, user1: &str, user2: &str) {
    sqlx::query(
        r#"INSERT INTO sharing_participants (id, budget_id, participant_kind, user_id, display_name, joined_at, last_seen_at)
           VALUES (UUID(), ?, 'member', ?, 'Member One', UNIX_TIMESTAMP(), UNIX_TIMESTAMP()),
                  (UUID(), ?, 'member', ?, 'Member Two', UNIX_TIMESTAMP(), UNIX_TIMESTAMP())
        "#,
    )
    .bind(budget_id)
    .bind(user1)
    .bind(budget_id)
    .bind(user2)
    .execute(pool)
    .await
    .expect("insert_sharing_participants failed");
}

/// Insert a sharing expense with legs directly.
async fn insert_sharing_expense(pool: &MySqlPool, budget_id: &str, payer_id: &str) -> String {
    let expense_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"INSERT INTO sharing_expenses (id, budget_id, paid_by, total_amount, description, expense_date, split_method, created_by, created_at, updated_at)
           VALUES (?, ?, ?, 50000, 'Test expense', '2026-09-01', 'equal', ?, ?, ?)"#,
    )
    .bind(&expense_id)
    .bind(budget_id)
    .bind(payer_id)
    .bind(payer_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .expect("insert_sharing_expense failed");

    // Insert legs
    let leg1 = uuid::Uuid::new_v4().to_string();
    let leg2 = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        r#"INSERT INTO sharing_expense_legs (id, expense_id, user_id, amount, weight, created_at)
           VALUES (?, ?, ?, 25000, 0, ?), (?, ?, ?, 25000, 0, ?)"#,
    )
    .bind(&leg1)
    .bind(&expense_id)
    .bind(payer_id)
    .bind(now)
    .bind(&leg2)
    .bind(&expense_id)
    .bind(payer_id)
    .bind(now)
    .execute(pool)
    .await
    .expect("insert sharing_expense_legs failed");

    expense_id
}

/// Insert invest_assets directly for cascade test.
async fn insert_invest_assets(pool: &MySqlPool, budget_id: &str, user_id: &str) -> Vec<String> {
    let asset1_id = uuid::Uuid::new_v4().to_string();
    let asset2_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"INSERT INTO invest_assets (id, budget_id, asset_type, name, status, created_by, created_at, updated_at, principal, annual_rate, start_date, maturity_date, bank_name)
           VALUES (?, ?, 'savings_deposit', 'Test FD 1', 'active', ?, ?, ?, 10000000, 0.05, '2026-01-01', '2027-01-01', 'Test Bank'),
                  (?, ?, 'stock', 'AAPL Shares', 'active', ?, ?, ?, NULL, NULL, NULL, NULL, NULL)"#,
    )
    .bind(&asset1_id)
    .bind(budget_id)
    .bind(user_id)
    .bind(now)
    .bind(now)
    .bind(&asset2_id)
    .bind(budget_id)
    .bind(user_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .expect("insert_invest_assets failed");

    vec![asset1_id, asset2_id]
}

// ---------------------------------------------------------------------------
// Sharing budget cascade tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires DATABASE_URL; run with `--ignored`"]
async fn sharing_budget_delete_cascades_to_participants() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for --ignored tests");
    let pool = MySqlPool::connect(&url).await.expect("connect");

    // Run migrations
    {
        let mut m = sqlx::migrate!("./migrations");
        m.set_ignore_missing(true);
        if let Err(e) = m.run(&pool).await {
            let msg = e.to_string();
            let ignorable = msg.contains("VersionMismatch")
                || msg.contains("partially applied")
                || msg.contains("Duplicate column name")
                || msg.contains("Duplicate key name")
                || msg.contains("already exists");
            if !ignorable {
                eprintln!("migration error (will try to continue): {msg}");
            }
        }
    }

    let org_id = "test-org";
    let owner_id = "user-owner-sharing";
    let member_id = "user-member-sharing";
    let budget_id = seed_sharing_budget(org_id, owner_id).await;

    // Add members
    add_members(&budget_id, owner_id, member_id).await;

    // Insert sharing participants
    insert_sharing_participants(&pool, &budget_id, owner_id, member_id).await;

    // Insert an expense
    let _expense_id = insert_sharing_expense(&pool, &budget_id, owner_id).await;

    // Verify preconditions
    let pre_participants: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sharing_participants WHERE budget_id = ? AND revoked_at IS NULL",
    )
    .bind(&budget_id)
    .fetch_one(&pool)
    .await
    .expect("pre-check participants failed");
    assert_eq!(pre_participants.0, 2, "should have 2 participants before delete");

    let pre_expenses: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sharing_expenses WHERE budget_id = ? AND deleted_at IS NULL",
    )
    .bind(&budget_id)
    .fetch_one(&pool)
    .await
    .expect("pre-check expenses failed");
    assert_eq!(pre_expenses.0, 1, "should have 1 expense before delete");

    // Delete the budget via BudgetBiz
    let biz = BudgetBiz::test_only_no_clients().await;
    biz.delete_budget(owner_id, &budget_id, None)
        .await
        .expect("delete_budget failed");

    // Verify budget is soft-deleted
    let budget_deleted: Option<(i64,)> = sqlx::query_as(
        "SELECT deleted_at FROM budgets WHERE id = ?",
    )
    .bind(&budget_id)
    .fetch_optional(&pool)
    .await
    .expect("check budget deleted_at failed");
    assert!(
        budget_deleted.is_some(),
        "budget should be soft-deleted (deleted_at set)"
    );

    // Verify participants are revoked (soft-deleted via cascade)
    let post_participants: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sharing_participants WHERE budget_id = ? AND revoked_at IS NULL",
    )
    .bind(&budget_id)
    .fetch_one(&pool)
    .await
    .expect("post-check participants failed");
    assert_eq!(
        post_participants.0, 0,
        "sharing_participants should be soft-deleted (revoked_at set) after budget delete"
    );

    // Verify expenses are soft-deleted (cascade)
    let post_expenses: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sharing_expenses WHERE budget_id = ? AND deleted_at IS NULL",
    )
    .bind(&budget_id)
    .fetch_one(&pool)
    .await
    .expect("post-check expenses failed");
    assert_eq!(
        post_expenses.0, 0,
        "sharing_expenses should be soft-deleted after budget delete"
    );

    // Verify expense legs are gone (cascade via expense delete)
    let post_legs: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM sharing_expense_legs WHERE expense_id IN (SELECT id FROM sharing_expenses WHERE budget_id = ?)",
    )
    .bind(&budget_id)
    .fetch_one(&pool)
    .await
    .expect("post-check legs failed");
    assert_eq!(
        post_legs.0, 0,
        "sharing_expense_legs should be cascade-deleted after budget delete"
    );

    // Verify budget_members are soft-deleted
    let post_members: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM budget_members WHERE budget_id = ?",
    )
    .bind(&budget_id)
    .fetch_one(&pool)
    .await
    .expect("post-check members failed");
    assert_eq!(
        post_members.0, 0,
        "budget_members should be soft-deleted after budget delete"
    );
}

// ---------------------------------------------------------------------------
// Invest budget cascade tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires DATABASE_URL; run with `--ignored`"]
async fn invest_budget_delete_cascades_to_assets() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for --ignored tests");
    let pool = MySqlPool::connect(&url).await.expect("connect");

    // Run migrations
    {
        let mut m = sqlx::migrate!("./migrations");
        m.set_ignore_missing(true);
        if let Err(e) = m.run(&pool).await {
            let msg = e.to_string();
            let ignorable = msg.contains("VersionMismatch")
                || msg.contains("partially applied")
                || msg.contains("Duplicate column name")
                || msg.contains("Duplicate key name")
                || msg.contains("already exists");
            if !ignorable {
                eprintln!("migration error (will try to continue): {msg}");
            }
        }
    }

    let org_id = "test-org";
    let owner_id = "user-owner-invest";
    let budget_id = seed_invest_budget(org_id, owner_id).await;

    // Add members
    add_members(&budget_id, owner_id, "user-member-invest").await;

    // Insert invest assets
    let asset_ids = insert_invest_assets(&pool, &budget_id, owner_id).await;
    assert_eq!(asset_ids.len(), 2, "should have 2 assets");

    // Verify preconditions
    let pre_assets: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM invest_assets WHERE budget_id = ? AND deleted_at IS NULL",
    )
    .bind(&budget_id)
    .fetch_one(&pool)
    .await
    .expect("pre-check assets failed");
    assert_eq!(pre_assets.0, 2, "should have 2 assets before delete");

    // Delete the budget via BudgetBiz
    let biz = BudgetBiz::test_only_no_clients().await;
    biz.delete_budget(owner_id, &budget_id, None)
        .await
        .expect("delete_budget failed");

    // Verify budget is soft-deleted
    let budget_deleted: Option<(i64,)> = sqlx::query_as(
        "SELECT deleted_at FROM budgets WHERE id = ?",
    )
    .bind(&budget_id)
    .fetch_optional(&pool)
    .await
    .expect("check budget deleted_at failed");
    assert!(
        budget_deleted.is_some(),
        "budget should be soft-deleted"
    );

    // Verify invest_assets are soft-deleted (cascade)
    let post_assets: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM invest_assets WHERE budget_id = ? AND deleted_at IS NULL",
    )
    .bind(&budget_id)
    .fetch_one(&pool)
    .await
    .expect("post-check assets failed");
    assert_eq!(
        post_assets.0, 0,
        "invest_assets should be soft-deleted after budget delete"
    );

    // Verify budget_members are soft-deleted
    let post_members: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM budget_members WHERE budget_id = ?",
    )
    .bind(&budget_id)
    .fetch_one(&pool)
    .await
    .expect("post-check members failed");
    assert_eq!(
        post_members.0, 0,
        "budget_members should be soft-deleted after budget delete"
    );
}
