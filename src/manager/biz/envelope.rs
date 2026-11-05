//! Legacy envelope / burn-rate maths on top of budget periods (plan R3 §6.4).
//! The envelope is deprecated in favour of category plans, but keeps working and now follows
//! the budget's own period (start day + timezone) instead of the UTC calendar month.

use chrono::{NaiveDate, TimeZone};

/// `[from, to)` of the period `today` falls in, for a budget's period settings.
pub fn current_period(
    now_utc: chrono::DateTime<chrono::Utc>,
    start_day: u8,
    timezone: &str,
) -> (NaiveDate, NaiveDate) {
    let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::Asia::Ho_Chi_Minh);
    philand_time::period::period_of(now_utc, tz, start_day)
}

/// Projected end-of-period spend as a percentage of the limit. 0 when no limit is set.
pub fn burn_rate_pct(
    spend: i64,
    limit: i64,
    today: NaiveDate,
    period: (NaiveDate, NaiveDate),
) -> f64 {
    if limit <= 0 {
        return 0.0;
    }
    let total_days = (period.1 - period.0).num_days().max(1) as f64;
    let elapsed = if today < period.0 {
        return 0.0;
    } else if today >= period.1 {
        total_days
    } else {
        ((today - period.0).num_days() + 1) as f64
    };
    spend as f64 / elapsed * total_days / limit as f64 * 100.0
}

pub fn local_today(now_utc: chrono::DateTime<chrono::Utc>, timezone: &str) -> NaiveDate {
    let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::Asia::Ho_Chi_Minh);
    tz.from_utc_datetime(&now_utc.naive_utc()).date_naive()
}
