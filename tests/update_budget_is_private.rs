//! Integration test: UpdateBudget persists is_private flag correctly.
//!
//! Run with: DATABASE_URL=... cargo test --test update_budget_is_private -- --ignored --nocapture

use budget::manager::biz::BudgetBiz;
use budget::pb::service::budget::BudgetType;

/// Test that calling UpdateBudget with is_private=true persists the flag.
#[tokio::test]
#[ignore = "requires DATABASE_URL; run with `--ignored`"]
async fn update_budget_persists_is_private_true() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for --ignored tests");
    let pool = sqlx::MySqlPool::connect(&url).await.expect("connect");

    // Run migrations first so the schema exists.
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

    let biz = BudgetBiz::test_only_no_clients().await;

    // Create a sharing budget (is_private defaults to false in DB).
    let budget = biz
        .create_budget(
            "test-user",
            "test-org",
            "is_private test budget",
            BudgetType::Sharing,
            "VND",
            None,
        )
        .await
        .expect("create_budget failed");

    let budget_id = budget.base.as_ref().expect("budget has base").id.clone();
    eprintln!("created budget id={}", budget_id);

    // Initially is_private should be false (default).
    assert!(
        !budget.is_private,
        "new budget should have is_private=false by default"
    );

    // Update the budget with is_private=true.
    let updated = biz
        .update_budget(
            "test-user",
            &budget_id,
            "is_private test budget (updated)",
            BudgetType::Sharing,
            true, // is_private = true
            Some("owner"),
        )
        .await
        .expect("update_budget with is_private=true failed");

    eprintln!("updated budget is_private={}", updated.is_private);
    assert!(
        updated.is_private,
        "updated budget should have is_private=true"
    );

    // Read the budget back and verify the flag persisted.
    let fetched = biz
        .get_budget("test-user", &budget_id, Some("owner"))
        .await
        .expect("get_budget failed");

    eprintln!("fetched budget is_private={}", fetched.is_private);
    assert!(
        fetched.is_private,
        "fetched budget should have is_private=true after update"
    );
}

/// Test that calling UpdateBudget with is_private=false works correctly.
#[tokio::test]
#[ignore = "requires DATABASE_URL; run with `--ignored`"]
async fn update_budget_persists_is_private_false() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for --ignored tests");
    let pool = sqlx::MySqlPool::connect(&url).await.expect("connect");

    // Run migrations first so the schema exists.
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

    let biz = BudgetBiz::test_only_no_clients().await;

    // Create a sharing budget.
    let budget = biz
        .create_budget(
            "test-user",
            "test-org",
            "is_private false test budget",
            BudgetType::Sharing,
            "VND",
            None,
        )
        .await
        .expect("create_budget failed");

    let budget_id = budget.base.as_ref().expect("budget has base").id.clone();

    // Update the budget with is_private=false.
    let updated = biz
        .update_budget(
            "test-user",
            &budget_id,
            "is_private false test budget (updated)",
            BudgetType::Sharing,
            false, // is_private = false
            Some("owner"),
        )
        .await
        .expect("update_budget with is_private=false failed");

    eprintln!("updated budget is_private={}", updated.is_private);
    assert!(
        !updated.is_private,
        "updated budget should have is_private=false"
    );
}
