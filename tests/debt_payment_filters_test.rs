//! Debt payment list filters against a real MySQL.
//! `DATABASE_URL=... cargo test --test debt_payment_filters_test -- --ignored`

use budget::manager::repository::debt::{DebtRepository, PaymentFilter};
use sqlx::MySqlPool;

async fn put(
    pool: &MySqlPool,
    budget: &str,
    acct: &str,
    on: &str,
    kind: &str,
    status: &str,
    n: i64,
) {
    sqlx::query(
        "INSERT INTO debt_payments (id, account_id, budget_id, paid_on, kind, principal_minor, interest_minor,
           fee_minor, total_minor, status, idempotency_key, actor_user_id, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, 0, 0, ?, ?, ?, 'u', ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(acct)
    .bind(budget)
    .bind(on)
    .bind(kind)
    .bind(n)
    .bind(n)
    .bind(status)
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(n)
    .bind(n)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn date_and_kind_filters_page_and_ignore_other_accounts_and_pending() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = MySqlPool::connect(&url).await.unwrap();
    let repo = DebtRepository::new(pool.clone());
    let (b, a1, a2) = (
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    for (i, d) in ["2026-08-01", "2026-09-01", "2026-10-01", "2026-11-01"]
        .iter()
        .enumerate()
    {
        put(&pool, &b, &a1, d, "scheduled", "completed", 10 + i as i64).await;
    }
    put(&pool, &b, &a1, "2026-10-15", "prepayment", "completed", 100).await;
    put(&pool, &b, &a1, "2026-10-20", "scheduled", "requested", 7).await; // not completed
    put(&pool, &b, &a2, "2026-10-01", "scheduled", "completed", 5).await; // other account

    let run = |f: PaymentFilter, limit: i64, off: i64| {
        let (repo, b, a1) = (&repo, b.clone(), a1.clone());
        async move {
            repo.list_payments(&b, Some(&a1), &f, limit, off)
                .await
                .unwrap()
        }
    };
    let none = PaymentFilter::default();

    let (rows, total) = run(none.clone(), 50, 0).await;
    assert_eq!((rows.len() as i64, total), (5, 5));
    assert_eq!(rows[0].paid_on, "2026-11-01", "newest first");

    let (rows, total) = run(
        PaymentFilter {
            date_from: Some("2026-09-01".into()),
            date_to: Some("2026-10-15".into()),
            ..none.clone()
        },
        50,
        0,
    )
    .await;
    assert_eq!(total, 3, "both bounds inclusive");
    assert_eq!(rows.len(), 3);

    let (rows, total) = run(
        PaymentFilter {
            kind: Some("prepayment".into()),
            ..none.clone()
        },
        50,
        0,
    )
    .await;
    assert_eq!((rows.len(), total), (1, 1));

    // paging: total is the full match, pages do not overlap
    let (p1, total) = run(none.clone(), 2, 0).await;
    let (p2, _) = run(none.clone(), 2, 2).await;
    let (p3, _) = run(none.clone(), 2, 4).await;
    assert_eq!((p1.len(), p2.len(), p3.len(), total), (2, 2, 1, 5));
    let mut ids: Vec<_> = p1
        .iter()
        .chain(&p2)
        .chain(&p3)
        .map(|p| p.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 5);

    // injection-looking input is only ever a bound value
    let (rows, _) = run(
        PaymentFilter {
            kind: Some("x' OR '1'='1".into()),
            ..none
        },
        50,
        0,
    )
    .await;
    assert!(rows.is_empty());
}
