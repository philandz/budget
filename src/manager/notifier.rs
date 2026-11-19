//! Fire-and-log access to the notify outbox for domain events (member added, role changed…).
//!
//! The event is written to `budget_notify_outbox` right after the business change succeeded.
//! A failure here is logged and never fails the request: losing a notification is better than
//! failing a member change. (Not in the same DB transaction as the change, because the member
//! repository calls are single statements; the outbox itself still gives at-least-once delivery
//! once the row exists.)

use philand_event::{MySqlOutbox, NotifyCategory, NotifyEvent, NotifySeverity};

use crate::converters::budget_role_to_db;
use crate::pb::service::budget::BudgetRole;

const SOURCE: &str = "budget";

#[derive(Clone)]
pub struct Notifier {
    outbox: MySqlOutbox,
    pool: sqlx::MySqlPool,
}

impl Notifier {
    pub fn new(outbox: MySqlOutbox, pool: sqlx::MySqlPool) -> Self {
        Self { outbox, pool }
    }

    /// Enqueue `event`; does nothing when it has no recipients.
    pub async fn send(&self, event: NotifyEvent) {
        if !event.has_recipients() {
            return;
        }
        let kind = event.event_type.clone();
        if let Err(e) = self.outbox.enqueue(&self.pool, &event).await {
            tracing::warn!("notify: could not enqueue {kind}: {e}");
        }
    }
}

fn budget_link(budget_id: &str) -> String {
    format!("/budgets/{budget_id}")
}

/// `actor` is the caller; it is dropped from the recipients (nobody is told about their own action).
pub fn member_added_event(
    budget_id: &str,
    budget_name: &str,
    user_id: &str,
    actor: Option<&str>,
    role: BudgetRole,
) -> NotifyEvent {
    let mut ev = NotifyEvent::new(
        SOURCE,
        "budget.member_added",
        NotifyCategory::Membership,
        vec![user_id.to_string()],
        format!("You were added to {budget_name}"),
    )
    .severity(NotifySeverity::Info)
    .budget(budget_id)
    .link(budget_link(budget_id))
    .params(serde_json::json!({
        "budget_name": budget_name,
        "role": budget_role_to_db(role),
    }));
    if let Some(a) = actor {
        ev = ev.actor(a);
    }
    ev.without_actor()
}

pub fn member_role_changed_event(
    budget_id: &str,
    budget_name: &str,
    user_id: &str,
    actor: Option<&str>,
    role: BudgetRole,
) -> NotifyEvent {
    let mut ev = NotifyEvent::new(
        SOURCE,
        "budget.member_role_changed",
        NotifyCategory::Membership,
        vec![user_id.to_string()],
        format!("Your role in {budget_name} changed"),
    )
    .severity(NotifySeverity::Info)
    .budget(budget_id)
    .link(budget_link(budget_id))
    .params(serde_json::json!({
        "budget_name": budget_name,
        "role": budget_role_to_db(role),
    }));
    if let Some(a) = actor {
        ev = ev.actor(a);
    }
    ev.without_actor()
}
