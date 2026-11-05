use budget::manager::biz::envelope::{burn_rate_pct, current_period, local_today};
use chrono::{NaiveDate, TimeZone, Utc};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

#[test]
fn period_follows_start_day_and_timezone() {
    // 23:30 UTC on 14 Oct is already 15 Oct in Vietnam: the period starting on the 15th
    let now = Utc.with_ymd_and_hms(2026, 10, 14, 23, 30, 0).unwrap();
    assert_eq!(
        current_period(now, 15, "Asia/Ho_Chi_Minh"),
        (d(2026, 10, 15), d(2026, 11, 15))
    );
    assert_eq!(
        current_period(now, 15, "UTC"),
        (d(2026, 9, 15), d(2026, 10, 15))
    );
    assert_eq!(local_today(now, "Asia/Ho_Chi_Minh"), d(2026, 10, 15));
}

#[test]
fn unknown_timezone_falls_back_to_vietnam() {
    let now = Utc.with_ymd_and_hms(2026, 10, 31, 23, 30, 0).unwrap();
    assert_eq!(local_today(now, "Nowhere/Land"), d(2026, 11, 1));
}

#[test]
fn burn_rate_projects_over_the_whole_period() {
    let period = (d(2026, 10, 1), d(2026, 11, 1)); // 31 days
                                                   // 10 days in, 1000 spent, limit 3100 -> projected 3100 -> 100%
    let pct = burn_rate_pct(1000, 3100, d(2026, 10, 10), period);
    assert!((pct - 100.0).abs() < 1e-9);
}

#[test]
fn burn_rate_is_zero_without_limit_or_before_the_period() {
    let period = (d(2026, 10, 1), d(2026, 11, 1));
    assert_eq!(burn_rate_pct(500, 0, d(2026, 10, 10), period), 0.0);
    assert_eq!(burn_rate_pct(500, 1000, d(2026, 9, 30), period), 0.0);
}

#[test]
fn finished_period_uses_actual_spend() {
    let period = (d(2026, 9, 1), d(2026, 10, 1));
    let pct = burn_rate_pct(500, 1000, d(2026, 10, 20), period);
    assert!((pct - 50.0).abs() < 1e-9);
}
