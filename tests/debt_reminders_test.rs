//! Debt reminder planning, sink mapping and (ignored) repository paging.
//! DB part: `DATABASE_URL=... cargo test --test debt_reminders_test -- --include-ignored`

use budget::manager::biz::debt::reminders::*;
use budget::manager::client::notification::{classify, to_publish_request};
use budget::manager::repository::debt::*;
use chrono::{NaiveDate, TimeZone, Utc};
use philand_event::{NotifyCategory, NotifyEvent, NotifySeverity, SinkError};
use sqlx::MySqlPool;

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn plan(today: &str, due: Option<&str>, rates: &[&str]) -> Vec<Reminder> {
    let rates: Vec<NaiveDate> = rates.iter().map(|r| d(r)).collect();
    plan_reminders(
        d(today),
        "acc1",
        "Home loan",
        due.map(|x| (d(x), 5_000_000)),
        &rates,
    )
}

#[test]
fn nothing_is_due_far_from_the_due_date() {
    assert!(plan("2026-12-01", Some("2026-12-25"), &[]).is_empty());
    assert!(plan("2026-12-01", None, &[]).is_empty());
}

#[test]
fn three_days_before_gives_an_info_reminder_with_a_stable_key() {
    let r = plan("2026-12-22", Some("2026-12-25"), &[]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].event_type, "debt.payment_due");
    assert_eq!(r[0].severity, NotifySeverity::Info);
    assert_eq!(r[0].dedup_key, "debt:acc1:2026-12-25:d3");
    assert_eq!(r[0].params["days"], 3);
    assert_eq!(r[0].params["amount"], 5_000_000);
    assert_eq!(r[0].params["debt_name"], "Home loan");
}

#[test]
fn on_the_due_date_it_is_a_warning() {
    let r = plan("2026-12-25", Some("2026-12-25"), &[]);
    assert_eq!(r[0].event_type, "debt.payment_due_today");
    assert_eq!(r[0].severity, NotifySeverity::Warning);
    assert_eq!(r[0].dedup_key, "debt:acc1:2026-12-25:d0");
}

#[test]
fn overdue_reminders_fire_once_per_threshold_and_stop_after_the_last() {
    // Day 1, 7 and 14 map to their own key; days in between reuse the previous threshold's
    // key (so a missed day still notifies, and re-running never duplicates).
    let key = |today| {
        plan(today, Some("2026-12-25"), &[])
            .first()
            .map(|r| r.dedup_key.clone())
    };
    assert_eq!(
        key("2026-12-26").as_deref(),
        Some("debt:acc1:2026-12-25:od1")
    );
    assert_eq!(
        key("2026-12-30").as_deref(),
        Some("debt:acc1:2026-12-25:od1")
    );
    assert_eq!(
        key("2027-01-01").as_deref(),
        Some("debt:acc1:2026-12-25:od7")
    );
    assert_eq!(
        key("2027-01-08").as_deref(),
        Some("debt:acc1:2026-12-25:od14")
    );
    assert_eq!(
        key("2027-03-01").as_deref(),
        Some("debt:acc1:2026-12-25:od14")
    );
    let r = plan("2027-01-01", Some("2026-12-25"), &[]);
    assert_eq!(r[0].event_type, "debt.payment_overdue");
    assert_eq!(r[0].params["days"], 7);
}

#[test]
fn rate_change_notice_goes_out_seven_days_ahead_only() {
    let r = plan("2026-12-18", None, &["2026-12-25", "2027-06-01"]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].event_type, "debt.rate_period_ending");
    assert_eq!(r[0].dedup_key, "debt:acc1:rate:2026-12-25");
    assert!(plan("2026-12-19", None, &["2026-12-25"]).is_empty());
}

#[test]
fn due_and_rate_reminders_can_share_a_day() {
    let r = plan("2026-12-22", Some("2026-12-25"), &["2026-12-29"]);
    let types: Vec<_> = r.iter().map(|x| x.event_type).collect();
    assert_eq!(types, vec!["debt.payment_due", "debt.rate_period_ending"]);
}

#[test]
fn next_run_is_the_next_local_hour_in_saigon() {
    // 2026-11-10 00:00 UTC = 07:00 Saigon -> the 07:00 slot already passed, wait a full day.
    let now = Utc.with_ymd_and_hms(2026, 11, 10, 0, 0, 0).unwrap();
    assert_eq!(delay_until_next_run(now, 7).as_secs(), 24 * 3600);
    // 2026-11-09 23:00 UTC = 06:00 Saigon -> one hour to 07:00.
    let now = Utc.with_ymd_and_hms(2026, 11, 9, 23, 0, 0).unwrap();
    assert_eq!(delay_until_next_run(now, 7).as_secs(), 3600);
    // An out-of-range hour is clamped instead of panicking.
    assert!(delay_until_next_run(now, 99).as_secs() > 0);
}

#[test]
fn today_is_taken_in_vietnam_time() {
    let now = Utc.with_ymd_and_hms(2026, 11, 9, 20, 0, 0).unwrap(); // 03:00 on the 10th in Saigon
    assert_eq!(today_saigon(now), d("2026-11-10"));
}

#[test]
fn publish_request_carries_every_field() {
    let ev = NotifyEvent::new(
        "budget",
        "debt.payment_due",
        NotifyCategory::Reminder,
        vec!["u1".into(), "u2".into()],
        "Payment due",
    )
    .severity(NotifySeverity::Warning)
    .budget("b1")
    .link("/budgets/b1")
    .dedup_key("k")
    .params(serde_json::json!({"days": 3}));
    let r = to_publish_request(&ev);
    assert_eq!(r.event_id, ev.event_id);
    assert_eq!(r.r#type, "debt.payment_due");
    assert_eq!(r.recipient_user_ids, vec!["u1", "u2"]);
    assert_eq!(r.budget_id, "b1");
    assert_eq!(r.link, "/budgets/b1");
    assert_eq!(r.dedup_key, "k");
    assert_eq!(r.params_json, r#"{"days":3}"#);
    assert_eq!(r.category, 3); // REMINDER
    assert_eq!(r.severity, 2); // WARNING
    assert_eq!(r.actor_user_id, "");
}

#[test]
fn only_invalid_argument_is_dead_lettered() {
    assert!(matches!(
        classify(tonic::Code::InvalidArgument, "bad link"),
        SinkError::Permanent(_)
    ));
    for c in [
        tonic::Code::Unavailable,
        tonic::Code::DeadlineExceeded,
        tonic::Code::Unauthenticated,
        tonic::Code::PermissionDenied,
        tonic::Code::Internal,
    ] {
        assert!(matches!(classify(c, "x"), SinkError::Retryable(_)), "{c:?}");
    }
}

// --------------------------------------------------------------------- database

async fn setup() -> DebtRepository {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = MySqlPool::connect(&url).await.expect("connect");
    let sql = include_str!("../migrations/20261104000001_debt_book.sql");
    let cleaned: String = sql
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    for stmt in cleaned.split(';').filter(|s| !s.trim().is_empty()) {
        sqlx::query(stmt).execute(&pool).await.expect("migration");
    }
    // The sweep only needs to know whether a budget is archived.
    sqlx::query("CREATE TABLE IF NOT EXISTS budgets (id VARCHAR(36) PRIMARY KEY, status VARCHAR(16) NOT NULL)")
        .execute(&pool)
        .await
        .expect("budgets");
    DebtRepository::new(pool)
}

fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}

async fn add(repo: &DebtRepository, budget_id: &str) -> DbDebtAccount {
    repo.create_account(&NewAccount {
        id: id(),
        budget_id: budget_id.to_string(),
        name: "Loan".into(),
        lender: None,
        kind: "consumer".into(),
        repayment_method: "annuity".into(),
        currency: "VND".into(),
        opening_outstanding_minor: 1_000_000,
        tracking_start_date: "2026-11-01".into(),
        maturity_date: Some("2027-11-25".into()),
        remaining_periods: Some(12),
        due_day: Some(25),
        periodic_fee_minor: 0,
        day_count: "actual_365".into(),
        interest_category_id: None,
        pay_from_budget_id: None,
        created_by: "u1".into(),
        rates: vec![],
        rules: vec![],
    })
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL (MySQL); run with --include-ignored"]
async fn sweep_paging_skips_archived_budgets_and_closed_accounts() {
    let repo = setup().await;
    let live = id();
    let archived = id();
    for (b, st) in [(&live, "active"), (&archived, "archived")] {
        sqlx::query("INSERT INTO budgets (id, status) VALUES (?, ?)")
            .bind(b)
            .bind(st)
            .execute(repo.pool())
            .await
            .unwrap();
    }
    let a1 = add(&repo, &live).await;
    let a2 = add(&repo, &live).await;
    let _hidden = add(&repo, &archived).await;
    let closed = add(&repo, &live).await;
    sqlx::query("UPDATE debt_accounts SET status = 'closed' WHERE id = ?")
        .bind(&closed.id)
        .execute(repo.pool())
        .await
        .unwrap();

    // Walk every page with a tiny page size and keep only this test's budget.
    let mut seen = Vec::new();
    let mut after = String::new();
    loop {
        let page = repo.list_active_after(&after, 2).await.unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().unwrap().id.clone();
        seen.extend(
            page.into_iter()
                .filter(|a| a.budget_id == live || a.budget_id == archived),
        );
    }
    let ids: Vec<_> = seen.iter().map(|a| a.id.clone()).collect();
    assert!(ids.contains(&a1.id) && ids.contains(&a2.id));
    assert!(
        !ids.contains(&closed.id),
        "closed accounts are not reminded"
    );
    assert!(
        seen.iter().all(|a| a.budget_id != archived),
        "archived budgets are skipped"
    );
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "keyset paging returns ids in order");
}
