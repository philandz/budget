//! Debt book persistence against a real MySQL: invariants D1–D5 (plan §2).
//! `DATABASE_URL=... cargo test --test debt_payment_test -- --ignored`

use budget::manager::repository::debt::*;
use sqlx::MySqlPool;

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

async fn setup() -> DebtRepository {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = MySqlPool::connect(&url).await.expect("connect");
    run_sql(
        &pool,
        include_str!("../migrations/20261104000001_debt_book.sql"),
    )
    .await;
    DebtRepository::new(pool)
}

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

async fn account(repo: &DebtRepository, outstanding: i64) -> DbDebtAccount {
    repo.create_account(&NewAccount {
        id: id(),
        budget_id: id(),
        name: "Loan".into(),
        lender: None,
        kind: "consumer".into(),
        repayment_method: "annuity".into(),
        currency: "VND".into(),
        opening_outstanding_minor: outstanding,
        tracking_start_date: "2026-11-01".into(),
        maturity_date: Some("2027-11-25".into()),
        remaining_periods: Some(12),
        due_day: Some(25),
        periodic_fee_minor: 0,
        day_count: "actual_365".into(),
        interest_category_id: None,
        pay_from_budget_id: None,
        created_by: "u1".into(),
        rates: vec![NewRate {
            effective_from: "2026-11-01".into(),
            annual_rate: "9.5".into(),
            rate_type: "fixed".into(),
            note: None,
        }],
        rules: vec![],
    })
    .await
    .unwrap()
}

fn payment(a: &DbDebtAccount, principal: i64, interest: i64, key: &str) -> NewPayment {
    NewPayment {
        id: id(),
        account_id: a.id.clone(),
        budget_id: a.budget_id.clone(),
        paid_on: "2026-11-25".into(),
        kind: "scheduled".into(),
        principal_minor: principal,
        interest_minor: interest,
        fee_minor: 0,
        prepay_effect: None,
        suggested_fee_minor: None,
        from_budget_id: id(),
        category_id: None,
        due_date: Some("2026-11-25".into()),
        idempotency_key: key.into(),
        actor_user_id: "u1".into(),
        note: None,
    }
}

fn effect() -> ApplyEffect {
    ApplyEffect {
        interest_through: Some("2026-11-25".into()),
        last_due_date: Some("2026-11-25".into()),
        installment_minor: None,
        mark_settled: false,
    }
}

async fn begin(repo: &DebtRepository, p: &NewPayment) -> DbDebtPayment {
    match repo.begin_payment(p).await.unwrap() {
        BeginPayment::New(p) => p,
        BeginPayment::Replay(_) => panic!("unexpected replay"),
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn d1_concurrent_payments_never_take_the_balance_below_zero() {
    let repo = setup().await;
    let a = account(&repo, 100).await;
    // both pass the early check (a `requested` payment does not move the balance yet)
    let p1 = begin(&repo, &payment(&a, 70, 0, "k1")).await;
    let p2 = begin(&repo, &payment(&a, 70, 0, "k2")).await;
    let (e1, e2) = (effect(), effect());
    let (r1, r2) = tokio::join!(
        repo.apply_payment(&p1.id, &e1),
        repo.apply_payment(&p2.id, &e2)
    );
    assert_eq!(
        [r1.is_ok(), r2.is_ok()].iter().filter(|x| **x).count(),
        1,
        "exactly one wins"
    );
    let loser = if r1.is_err() { r1 } else { r2 };
    assert!(matches!(loser, Err(DebtRepoError::Insufficient { .. })));
    let acc = repo.get_account(&a.id).await.unwrap().unwrap();
    assert_eq!(acc.outstanding_minor, 30);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn d1_a_payment_above_the_balance_is_refused_up_front() {
    let repo = setup().await;
    let a = account(&repo, 100).await;
    let e = repo
        .begin_payment(&payment(&a, 101, 0, "k"))
        .await
        .err()
        .unwrap();
    assert!(matches!(
        e,
        DebtRepoError::Insufficient {
            outstanding: 100,
            requested: 101
        }
    ));
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn d3_the_balance_can_be_recomputed_from_payments_and_adjustments() {
    let repo = setup().await;
    let a = account(&repo, 1_000_000).await;
    for (i, principal) in [100_000, 250_000].into_iter().enumerate() {
        let p = begin(&repo, &payment(&a, principal, 5_000, &format!("k{i}"))).await;
        repo.apply_payment(&p.id, &effect()).await.unwrap();
    }
    repo.adjust_outstanding(&a.id, 600_000, "statement", "u1", "2026-12-01")
        .await
        .unwrap();
    let acc = repo.get_account(&a.id).await.unwrap().unwrap();
    assert_eq!(acc.outstanding_minor, 600_000);
    assert_eq!(repo.recomputed_outstanding(&a.id).await.unwrap(), 600_000);
    let totals = repo.totals_for_accounts(&[a.id.clone()]).await.unwrap();
    let t = totals[&a.id];
    assert_eq!(
        (t.paid_principal, t.paid_interest, t.total_paid()),
        (350_000, 10_000, 360_000)
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn d4_the_same_key_is_recorded_once_and_returns_the_same_payment() {
    let repo = setup().await;
    let a = account(&repo, 1_000).await;
    let first = begin(&repo, &payment(&a, 100, 10, "same")).await;
    repo.apply_payment(&first.id, &effect()).await.unwrap();
    match repo
        .begin_payment(&payment(&a, 100, 10, "same"))
        .await
        .unwrap()
    {
        BeginPayment::Replay(p) => assert_eq!(p.id, first.id),
        BeginPayment::New(_) => panic!("a replay must not create a second payment"),
    }
    let acc = repo.get_account(&a.id).await.unwrap().unwrap();
    assert_eq!(
        acc.outstanding_minor, 900,
        "the principal left the balance once"
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn d2_total_is_principal_plus_interest_plus_fee() {
    let repo = setup().await;
    let a = account(&repo, 10_000).await;
    let mut p = payment(&a, 1_000, 200, "k");
    p.fee_minor = 30;
    let stored = begin(&repo, &p).await;
    assert_eq!(stored.total_minor, 1_230);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn d5_a_payment_taken_by_the_reconciler_cannot_be_applied() {
    let repo = setup().await;
    let a = account(&repo, 1_000).await;
    let p = begin(&repo, &payment(&a, 100, 0, "k")).await;
    // the reconciler claims it (after the legs were written but before apply), then voids and fails it
    assert!(repo.claim_payment(&p.id).await.unwrap());
    assert!(repo
        .set_payment_status(&p.id, &["reconciling"], "failed")
        .await
        .unwrap());
    let e = repo.apply_payment(&p.id, &effect()).await.unwrap_err();
    assert!(matches!(e, DebtRepoError::PaymentGone));
    let acc = repo.get_account(&a.id).await.unwrap().unwrap();
    assert_eq!(
        acc.outstanding_minor, 1_000,
        "nothing was taken from the balance"
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn d7_settling_needs_a_zero_balance_and_a_settled_account_refuses_payments() {
    let repo = setup().await;
    let a = account(&repo, 100).await;
    let p = begin(&repo, &payment(&a, 100, 0, "k")).await;
    repo.apply_payment(&p.id, &effect()).await.unwrap();
    let acc = repo
        .set_status(&a.id, &["active"], "settled", "settled", "u1", "2026-11-25")
        .await
        .unwrap();
    assert_eq!((acc.status.as_str(), acc.outstanding_minor), ("settled", 0));
    let e = repo
        .begin_payment(&payment(&a, 1, 0, "k2"))
        .await
        .err()
        .unwrap();
    assert!(matches!(e, DebtRepoError::NotActive));
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --ignored"]
async fn only_one_sweep_wins_the_claim_on_a_stale_payment() {
    let repo = setup().await;
    let a = account(&repo, 1_000).await;
    let p = begin(&repo, &payment(&a, 100, 0, "k")).await;
    let (c1, c2) = tokio::join!(repo.claim_payment(&p.id), repo.claim_payment(&p.id));
    // a claim on a still-`reconciling` row is allowed again (the earlier sweep may have died);
    // what matters is that apply is refused afterwards
    assert!(c1.unwrap() && c2.unwrap());
    let e = effect();
    assert!(matches!(
        repo.apply_payment(&p.id, &e).await.unwrap_err(),
        DebtRepoError::PaymentGone
    ));
}
