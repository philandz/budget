//! Fund cash pocket against a real MySQL (I4: never negative, even under concurrency).
//! `DATABASE_URL=... cargo test --test fund_cash_test -- --ignored`

use budget::converters::fund::NewFundMovement;
use budget::manager::repository::fund_cash::{FundCashError, FundCashRepository};
use budget::pb::service::portfolio::FundCashMovementKind as K;
use sqlx::MySqlPool;

/// Runs a migration file: comment lines are dropped first (they may contain ';'), then split on ';'.
async fn run_sql(pool: &MySqlPool, sql: &str) {
    let cleaned: String = sql
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    for stmt in cleaned.split(';') {
        if !stmt.trim().is_empty() {
            sqlx::query(stmt).execute(pool).await.expect("migration");
        }
    }
}

async fn setup() -> FundCashRepository {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = MySqlPool::connect(&url).await.expect("connect");
    run_sql(
        &pool,
        include_str!("../migrations/20261023000001_fund_cash.sql"),
    )
    .await;
    FundCashRepository::new(pool)
}

fn mv(budget: &str, kind: K, amount: i64, key: Option<&str>) -> NewFundMovement {
    NewFundMovement {
        budget_id: budget.into(),
        currency: "VND".into(),
        kind,
        amount_minor: amount,
        ref_type: None,
        ref_id: None,
        transfer_group_id: None,
        idempotency_key: key.map(str::to_string),
        actor_user_id: "u1".into(),
        note: None,
        occurred_on: "2026-10-23".into(),
    }
}

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn balance_equals_movement_sum_and_never_goes_negative() {
    let repo = setup().await;
    let b = id();
    repo.apply(&mv(&b, K::TransferIn, 1000, Some("in-1")))
        .await
        .unwrap();
    repo.apply(&mv(&b, K::AssetPurchase, -400, Some("buy-1")))
        .await
        .unwrap();
    let err = repo
        .apply(&mv(&b, K::TransferOut, -700, Some("out-1")))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            FundCashError::Insufficient {
                balance: 600,
                requested: 700
            }
        ),
        "{err:?}"
    );

    let acct = repo.get_account(&b).await.unwrap().unwrap();
    assert_eq!(acct.balance_minor, 600);
    assert_eq!(repo.movement_sum(&b).await.unwrap(), 600);
    // a refused withdrawal leaves no movement behind
    assert!(repo
        .find_movement_by_key(&b, "out-1")
        .await
        .unwrap()
        .is_none());
    // purchases are not "contributions"
    assert_eq!(repo.net_contributed(&b).await.unwrap(), 1000);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn idempotent_replay_returns_the_original_and_changes_nothing() {
    let repo = setup().await;
    let b = id();
    let first = repo
        .apply(&mv(&b, K::TransferIn, 500, Some("k")))
        .await
        .unwrap();
    assert!(!first.replayed);
    let again = repo
        .apply(&mv(&b, K::TransferIn, 500, Some("k")))
        .await
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.movement.id, first.movement.id);
    assert_eq!(
        repo.get_account(&b).await.unwrap().unwrap().balance_minor,
        500
    );
    // same key, different amount is a client bug, not a replay
    let err = repo
        .apply(&mv(&b, K::TransferIn, 999, Some("k")))
        .await
        .unwrap_err();
    assert!(matches!(err, FundCashError::IdempotencyMismatch));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn concurrent_withdrawals_cannot_overdraw() {
    let repo = setup().await;
    let b = id();
    repo.apply(&mv(&b, K::TransferIn, 1000, Some("seed")))
        .await
        .unwrap();

    let mut handles = Vec::new();
    for i in 0..8 {
        let (repo, b) = (repo.clone(), b.clone());
        handles.push(tokio::spawn(async move {
            repo.apply(&mv(&b, K::TransferOut, -300, Some(&format!("w{i}"))))
                .await
        }));
    }
    let mut ok = 0;
    for h in handles {
        if h.await.unwrap().is_ok() {
            ok += 1;
        }
    }
    assert_eq!(ok, 3, "1000 / 300 allows exactly three withdrawals");
    let acct = repo.get_account(&b).await.unwrap().unwrap();
    assert_eq!(acct.balance_minor, 100);
    assert_eq!(repo.movement_sum(&b).await.unwrap(), 100);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn stale_requested_transfers_are_found_until_their_status_changes() {
    let repo = setup().await;
    let url = std::env::var("DATABASE_URL").unwrap();
    let pool = MySqlPool::connect(&url).await.unwrap();
    run_sql(
        &pool,
        include_str!("../migrations/20260815000001_portfolio_transfers.sql"),
    )
    .await;
    let (tid, std_b, fund_b) = (id(), id(), id());
    sqlx::query(
        "INSERT INTO portfolio_transfers (id, group_id, source_budget_id, counterparty_budget_id, direction,
            amount_minor, currency, status, idempotency_key, actor_user_id, created_at)
         VALUES (?, ?, ?, ?, 'standard_to_invest', 100, 'VND', 'requested', 'stale-k', 'u1', 1000)",
    )
    .bind(&tid)
    .bind(&tid)
    .bind(&std_b)
    .bind(&fund_b)
    .execute(&pool)
    .await
    .unwrap();

    let stale = repo.list_stale_requested(2000).await.unwrap();
    assert!(stale.iter().any(|t| t.id == tid));
    // too recent to be stale
    assert!(!repo
        .list_stale_requested(500)
        .await
        .unwrap()
        .iter()
        .any(|t| t.id == tid));

    repo.set_transfer_status(&tid, "failed", None)
        .await
        .unwrap();
    assert!(!repo
        .list_stale_requested(2000)
        .await
        .unwrap()
        .iter()
        .any(|t| t.id == tid));
}
