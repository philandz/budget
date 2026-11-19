//! Debt payment reminders (plan 2026-11-09-notification-plan, producer T3.4).
//!
//! A daily sweep looks at every active debt account and, from the same schedule projection
//! the debt book shows, decides which reminders are due today. Each reminder carries a
//! `dedup_key` and a deterministic event id, so running the sweep again (restart, several
//! replicas, a missed day) never notifies twice.

use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Ho_Chi_Minh;
use philand_event::{
    deterministic_event_id, MySqlOutbox, NotifyCategory, NotifyEvent, NotifySeverity,
};
use serde_json::json;

use super::schedule as sch;
use super::service::DebtBiz;

/// Days before the due date for the early reminder.
pub const EARLY_DAYS: i64 = 3;
/// Days before a rate change takes effect.
pub const RATE_NOTICE_DAYS: i64 = 7;
/// Overdue reminders go out once per threshold (days after the due date).
pub const OVERDUE_THRESHOLDS: [i64; 3] = [1, 7, 14];
const PAGE: i64 = 200;
const SOURCE: &str = "budget";

/// Roles that can pay a debt (Contributor and up).
const RECIPIENT_ROLES: [&str; 3] = ["owner", "manager", "contributor"];

/// One reminder for one account, before recipients are attached.
#[derive(Debug, Clone, PartialEq)]
pub struct Reminder {
    pub event_type: &'static str,
    pub severity: NotifySeverity,
    pub params: serde_json::Value,
    pub dedup_key: String,
    pub fallback_text: String,
}

/// What is due today for one account. `next_due` is the first row of its schedule
/// projection (due date, estimated total in minor VND).
pub fn plan_reminders(
    today: NaiveDate,
    account_id: &str,
    name: &str,
    next_due: Option<(NaiveDate, i64)>,
    rate_changes: &[NaiveDate],
) -> Vec<Reminder> {
    let mut out = Vec::new();

    if let Some((due, total)) = next_due {
        let days = (due - today).num_days();
        let due_s = sch::fmt_date(due);
        let base = |extra: serde_json::Value| {
            let mut v = json!({"debt_name": name, "amount": total, "due_date": due_s});
            if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            v
        };
        if days == EARLY_DAYS {
            out.push(Reminder {
                event_type: "debt.payment_due",
                severity: NotifySeverity::Info,
                params: base(json!({"days": days})),
                dedup_key: format!("debt:{account_id}:{due_s}:d{EARLY_DAYS}"),
                fallback_text: format!("Debt payment due in {days} days: {name}"),
            });
        } else if days == 0 {
            out.push(Reminder {
                event_type: "debt.payment_due_today",
                severity: NotifySeverity::Warning,
                params: base(json!({})),
                dedup_key: format!("debt:{account_id}:{due_s}:d0"),
                fallback_text: format!("Debt payment due today: {name}"),
            });
        } else if days < 0 {
            let overdue = -days;
            if let Some(t) = OVERDUE_THRESHOLDS.iter().rev().find(|t| **t <= overdue) {
                out.push(Reminder {
                    event_type: "debt.payment_overdue",
                    severity: NotifySeverity::Warning,
                    params: base(json!({"days": overdue})),
                    dedup_key: format!("debt:{account_id}:{due_s}:od{t}"),
                    fallback_text: format!("Debt payment overdue by {overdue} days: {name}"),
                });
            }
        }
    }

    for d in rate_changes {
        if (*d - today).num_days() == RATE_NOTICE_DAYS {
            let ds = sch::fmt_date(*d);
            out.push(Reminder {
                event_type: "debt.rate_period_ending",
                severity: NotifySeverity::Info,
                params: json!({"debt_name": name, "date": ds}),
                dedup_key: format!("debt:{account_id}:rate:{ds}"),
                fallback_text: format!("Interest rate changes on {ds}: {name}"),
            });
        }
    }
    out
}

/// Delay until the next `hour:00` Asia/Saigon strictly after `now`.
pub fn delay_until_next_run(now: DateTime<Utc>, hour: u32) -> Duration {
    let local = now.with_timezone(&Ho_Chi_Minh);
    let hour = hour.min(23);
    let mut day = local.date_naive();
    loop {
        let target = Ho_Chi_Minh
            .from_local_datetime(&day.and_hms_opt(hour, 0, 0).expect("valid time"))
            .earliest();
        if let Some(t) = target {
            if t.with_timezone(&Utc) > now {
                return (t.with_timezone(&Utc) - now)
                    .to_std()
                    .unwrap_or(Duration::from_secs(60));
            }
        }
        day += ChronoDuration::days(1);
    }
}

/// Today's date in Vietnam.
pub fn today_saigon(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&Ho_Chi_Minh).date_naive()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepStats {
    pub accounts: u32,
    pub enqueued: u32,
    pub skipped_no_recipients: u32,
}

impl DebtBiz {
    /// One pass over every active debt account. Safe to run repeatedly.
    pub async fn sweep_reminders(
        &self,
        outbox: &MySqlOutbox,
        today: NaiveDate,
    ) -> anyhow::Result<SweepStats> {
        let mut stats = SweepStats::default();
        let mut after = String::new();
        loop {
            let page = self.repo.list_active_after(&after, PAGE).await?;
            if page.is_empty() {
                break;
            }
            after = page.last().map(|a| a.id.clone()).unwrap_or_default();
            for acc in page {
                stats.accounts += 1;
                let l = match self.load(&acc.budget_id, &acc.id).await {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!("debt reminders: skip {}: {e}", acc.id);
                        continue;
                    }
                };
                let next_due = self
                    .project(&l, 1)
                    .rows
                    .first()
                    .map(|r| (r.due_date, r.total));
                let rate_changes: Vec<NaiveDate> = l
                    .db_rates
                    .iter()
                    .filter_map(|r| sch::parse_date(&r.effective_from))
                    .filter(|d| *d > today)
                    .collect();
                let plan = plan_reminders(today, &acc.id, &acc.name, next_due, &rate_changes);
                if plan.is_empty() {
                    continue;
                }
                let recipients = self.reminder_recipients(&acc.budget_id).await?;
                if recipients.is_empty() {
                    stats.skipped_no_recipients += plan.len() as u32;
                    continue;
                }
                for r in plan {
                    let ev = NotifyEvent::new(
                        SOURCE,
                        r.event_type,
                        NotifyCategory::Reminder,
                        recipients.clone(),
                        r.fallback_text,
                    )
                    .event_id(deterministic_event_id(&r.dedup_key))
                    .severity(r.severity)
                    .budget(&acc.budget_id)
                    .params(r.params)
                    .link(format!("/budgets/{}", acc.budget_id))
                    .dedup_key(r.dedup_key);
                    if outbox.enqueue_if_absent(self.repo.pool(), &ev).await? {
                        stats.enqueued += 1;
                    }
                }
            }
        }
        Ok(stats)
    }

    async fn reminder_recipients(&self, budget_id: &str) -> anyhow::Result<Vec<String>> {
        let members = self.budget_biz.repo.list_members(budget_id).await?;
        Ok(members
            .into_iter()
            .filter(|m| RECIPIENT_ROLES.contains(&m.role.as_str()))
            .map(|m| m.user_id)
            .collect())
    }
}

/// Runs the sweep every day at `hour` Asia/Saigon until the process stops.
pub async fn run_daily_reminders(biz: std::sync::Arc<DebtBiz>, outbox: MySqlOutbox, hour: u32) {
    loop {
        let wait = delay_until_next_run(Utc::now(), hour);
        tracing::info!(
            "debt reminders: next sweep in {}s (daily at {hour:02}:00 Asia/Saigon)",
            wait.as_secs()
        );
        tokio::time::sleep(wait).await;
        let today = today_saigon(Utc::now());
        match biz.sweep_reminders(&outbox, today).await {
            Ok(s) => tracing::info!(
                accounts = s.accounts,
                enqueued = s.enqueued,
                no_recipients = s.skipped_no_recipients,
                "debt reminders sweep done"
            ),
            Err(e) => tracing::error!("debt reminders sweep failed: {e}"),
        }
    }
}
