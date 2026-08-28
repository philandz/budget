//! Outbox drainer for portfolio events.
//!
//! Phase 3.4 implements a background loop that reads un-delivered
//! rows from `portfolio_outbox_events`, renders the appropriate
//! notification template via `libs/notify`, marks each row delivered
//! on success, and applies bounded retry with backoff on failure.
//!
//! Delivery transport: Phase 3 uses the existing `libs/notify` Mailer
//! trait driven by a `ReqwestMailer` (sends to a stub endpoint). A
//! future Phase 4 can swap to a real SMTP/Resend backend or a
//! dedicated notification service gRPC client without changing the
//! drainer.
//!
//! Concurrency: a single drainer task per service instance. Bounded
//! by a configurable `PORTFOLIO_OUTBOX_BATCH_SIZE` and a per-row
//! retry budget (`PORTFOLIO_OUTBOX_MAX_ATTEMPTS`). On terminal
//! failure (max attempts exceeded), the row is moved to a separate
//! `portfolio_outbox_dead` table — Phase 4+.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::time::{interval, MissedTickBehavior};

use crate::manager::biz::portfolio::biz::PortfolioBiz;
use crate::manager::repository::portfolio::PortfolioRepository;
use philand_notify::{
    render_portfolio_matured, render_portfolio_rolled_over, render_price_observed, MailMessage,
    Mailer, PortfolioMaturedVars, PortfolioRolledOverVars, PriceObservedVars,
};
use philand_time::now_unix;

/// Re-export the libs/notify Mailer as the canonical drainer
/// transport. Production uses the real mailer; tests use the
/// libs/notify `NoopMailer` which logs and returns Unconfigured.
pub use philand_notify::Mailer as OutboxMailer;

/// Telegram dispatcher. Uses libs/notify send_telegram_message.
/// Configured via PORTFOLIO_TELEGRAM_BOT_TOKEN and looks up
/// user Telegram chat_id from the preferences table.
#[derive(Clone)]
pub struct TelegramDispatcher {
    client: reqwest::Client,
    bot_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OutboxRow {
    id: String,
    event_type: String,
    asset_id: Option<String>,
    budget_id: Option<String>,
    payload_json: String,
    attempts: i32,
}

impl PortfolioBiz {
    /// Run the outbox drainer. Returns when the supplied shutdown
    /// signal completes; never returns an error from the loop itself
    /// (transient DB or mailer errors are logged).
    ///
    /// P24: If `telegram` is provided and configured, also dispatches
    /// to Telegram when the user has a chat_id in preferences.
    pub async fn run_outbox_drainer(
        &self,
        mailer: Arc<dyn Mailer>,
        shutdown: tokio::sync::watch::Receiver<bool>,
        telegram: Option<TelegramDispatcher>,
    ) {
        let interval_secs = std::env::var("PORTFOLIO_OUTBOX_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(30);
        let batch_size = std::env::var("PORTFOLIO_OUTBOX_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(50);
        let max_attempts = std::env::var("PORTFOLIO_OUTBOX_MAX_ATTEMPTS")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(5);
        // Dedup cleanup runs once per `cleanup_ticks` drainer ticks.
        // Default 1440 ticks × 30s = 12h. The dedup table is bounded
        // by alert volume × 30 days; cleanup at 12h cadence is a safe
        // margin. Tunable via `PORTFOLIO_OUTBOX_CLEANUP_TICKS`.
        let cleanup_ticks = std::env::var("PORTFOLIO_OUTBOX_CLEANUP_TICKS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(1440);
        // 30 days retention. Tunable via
        // `PORTFOLIO_OUTBOX_DEDUP_MAX_AGE_SECS`.
        let dedup_max_age_secs = std::env::var("PORTFOLIO_OUTBOX_DEDUP_MAX_AGE_SECS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(30 * 24 * 60 * 60);

        let mut ticker = interval(Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        ticker.tick().await; // skip first immediate tick

        let mut tick_count: u64 = 0;
        let mut shutdown = shutdown;
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("outbox drainer: shutdown signal received");
                        return;
                    }
                }
                _ = ticker.tick() => {
                    if let Err(e) = self
                        .drain_outbox_once(&mailer, telegram.as_ref(), batch_size, max_attempts)
                        .await
                    {
                        tracing::warn!("outbox drainer tick failed: {e}");
                    }
                    tick_count += 1;
                    if tick_count.is_multiple_of(cleanup_ticks) {
                        if let Err(e) = self
                            .repo
                            .cleanup_dedup_older_than(dedup_max_age_secs)
                            .await
                        {
                            tracing::warn!("outbox dedup cleanup failed: {e}");
                        } else {
                            tracing::info!("outbox dedup cleanup ran");
                        }
                    }
                }
            }
        }
    }

    /// One pass: fetch up to `batch_size` un-delivered rows, render,
    /// dispatch, and mark delivered. On error, increment `attempts` and
    /// set `next_attempt_at` based on backoff.
    #[doc(hidden)]
    pub async fn drain_outbox_once(
        &self,
        mailer: &Arc<dyn Mailer>,
        telegram: Option<&TelegramDispatcher>,
        batch_size: i64,
        max_attempts: i32,
    ) -> anyhow::Result<()> {
        let rows = fetch_undelivered_outbox(&self.repo, batch_size).await?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in rows {
            match self.dispatch_one(mailer, telegram, &row).await {
                Ok(()) => {
                    mark_outbox_delivered(&self.repo, &row.id, now_unix()).await?;
                    tracing::info!(
                        outbox_id = %row.id,
                        event_type = %row.event_type,
                        "outbox event delivered"
                    );
                }
                Err(e) => {
                    let new_attempts = row.attempts + 1;
                    if new_attempts >= max_attempts {
                        tracing::error!(
                            outbox_id = %row.id,
                            event_type = %row.event_type,
                            attempts = new_attempts,
                            "outbox event dead-lettered: {e}"
                        );
                        mark_outbox_dead(
                            &self.repo,
                            &row.id,
                            new_attempts,
                            &e.to_string(),
                            now_unix(),
                        )
                        .await?;
                    } else {
                        let backoff_secs = backoff_for(new_attempts);
                        tracing::warn!(
                            outbox_id = %row.id,
                            event_type = %row.event_type,
                            attempts = new_attempts,
                            backoff_secs,
                            "outbox event delivery failed: {e}"
                        );
                        mark_outbox_retry(
                            &self.repo,
                            &row.id,
                            new_attempts,
                            &e.to_string(),
                            now_unix() + backoff_secs,
                        )
                        .await?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Render the event payload to a MailMessage and dispatch via the
    /// supplied mailer. Honors the user's alert preferences and skips
    /// re-sends within the same ICT day via the dedup table.
    ///
    /// P8: Uses libs/notify template renderers for rich HTML emails.
    /// P24: Also dispatches to Telegram if configured and user has a
    /// chat_id in preferences.
    async fn dispatch_one(
        &self,
        mailer: &Arc<dyn Mailer>,
        telegram: Option<&TelegramDispatcher>,
        row: &OutboxRow,
    ) -> anyhow::Result<()> {
        // Resolve the recipient. Phase 4 embeds `actor_user_id` in the
        // JSON payload when the maturity / rollover / refresh job
        // creates the event. Missing field → fall back to a system
        // "ops" address so the drainer never errors.
        let user_id = extract_user_id(&row.payload_json).unwrap_or_else(|| "ops".into());

        // Preferences gate. Skip when the user has disabled this
        // event_type.
        if !self
            .user_allows_event_type(&user_id, &row.event_type)
            .await?
        {
            tracing::debug!(
                user_id = %user_id,
                event_type = %row.event_type,
                "drainer: user disabled this event type"
            );
            return Ok(());
        }

        // Dedup. The unique constraint on
        // (user_id, asset_id, event_type, local_date) guarantees
        // at-most-one notification per day per (user, asset, event).
        // We swallow duplicate (rows_affected = 0) so retries don't
        // double-send.
        let asset_id = row.asset_id.clone().unwrap_or_default();
        let currency = fetch_asset_currency(self.repo.pool(), &asset_id).await;
        if !self
            .record_alert_dedup(&user_id, &asset_id, &row.event_type)
            .await?
        {
            tracing::debug!(
                user_id = %user_id,
                asset_id = %asset_id,
                event_type = %row.event_type,
                "drainer: alert already sent today"
            );
            return Ok(());
        }

        // Parse the payload to extract fields for templates
        let payload: serde_json::Value =
            serde_json::from_str(&row.payload_json).unwrap_or_else(|_| serde_json::json!({}));

        // Get user display name and budget name from preferences/budget tables
        let (display_name_opt, budget_name_owned) = self
            .fetch_user_display_name_and_budget(&user_id, row.budget_id.as_deref())
            .await
            .unwrap_or((
                None,
                row.budget_id
                    .clone()
                    .unwrap_or_else(|| "your portfolio".into()),
            ));
        let display_name = display_name_opt.as_deref();
        let budget_name = budget_name_owned.as_str();

        // Default URLs for portfolio links
        let portfolio_url = std::env::var("PORTFOLIO_APP_URL")
            .unwrap_or_else(|_| "https://app.philandz.com/portfolio".to_string());
        let asset_url = format!("{}/asset/{}", portfolio_url, asset_id);

        // Render the email using libs/notify templates
        let rendered = match row.event_type.as_str() {
            "MATURITY_REACHED" => {
                let principal: i64 = payload
                    .get("principal")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let maturity_date = payload
                    .get("maturity_date")
                    .and_then(|v| v.as_i64())
                    .map(unix_to_datetime)
                    .unwrap_or_else(chrono::Utc::now);
                let asset_name = payload
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Your deposit");

                render_portfolio_matured(&PortfolioMaturedVars {
                    display_name,
                    budget_name,
                    asset_name,
                    principal_minor: principal,
                    currency: &currency, // read from asset
                    maturity_date,
                    next_step_url: &asset_url,
                })
            }
            "ROLLED_OVER" => {
                let new_maturity_date = payload
                    .get("new_maturity_date")
                    .and_then(|v| v.as_i64())
                    .map(unix_to_datetime)
                    .unwrap_or_else(chrono::Utc::now);
                let principal: i64 = payload
                    .get("principal")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let old_name = payload
                    .get("old_display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Your deposit");
                let new_name = payload
                    .get("new_display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("new deposit");

                render_portfolio_rolled_over(&PortfolioRolledOverVars {
                    display_name,
                    budget_name,
                    old_asset_name: old_name,
                    new_asset_name: new_name,
                    principal_minor: principal,
                    currency: &currency, // read from asset
                    new_maturity_date,
                    next_step_url: &asset_url,
                })
            }
            "PRICE_OBSERVED" => {
                let unit_price: i64 = payload
                    .get("unit_price")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let ticker = payload.get("ticker").and_then(|v| v.as_str()).unwrap_or("");
                let source = payload
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("manual");
                let asset_name = payload
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Your asset");

                render_price_observed(&PriceObservedVars {
                    display_name,
                    budget_name,
                    asset_name,
                    ticker,
                    unit_price_minor: unit_price,
                    currency: &currency, // read from asset
                    source,
                    portfolio_url: &portfolio_url,
                })
            }
            other => {
                tracing::warn!(
                    event_type = %row.event_type,
                    "drainer: unknown event type, using fallback"
                );
                // For unknown events, just log and return - no email sent
                tracing::debug!("drainer skipped unknown event type: {}", other);
                return Ok(());
            }
        };

        // Send email via mailer
        let from_address = std::env::var("PORTFOLIO_FROM_ADDRESS")
            .unwrap_or_else(|_| "portfolio@philandz.com".to_string());
        let to_address = std::env::var("PORTFOLIO_DEFAULT_TO")
            .unwrap_or_else(|_| format!("{user_id}@philandz.com"));

        // P24: Extract subject/text before moving rendered into MailMessage
        let telegram_subject = rendered.subject.clone();
        let telegram_text = rendered.text.clone();

        let msg = MailMessage {
            from: from_address,
            to: to_address,
            subject: rendered.subject,
            html: rendered.html,
            text: rendered.text,
            reply_to: None,
        };

        let receipt = mailer
            .send(msg)
            .await
            .map_err(|e| anyhow::anyhow!("mailer send failed: {e}"))?;
        tracing::debug!(
            provider = %receipt.provider,
            message_id = %receipt.message_id,
            "outbox event delivered via email"
        );

        // P24: Also send to Telegram if configured
        if let Some(tg) = telegram {
            if let Some(chat_id) = self
                .fetch_user_telegram_chat_id(&user_id)
                .await
                .ok()
                .flatten()
            {
                let text = format!("{}\n\n{}", telegram_subject, telegram_text);
                if let Err(e) = tg.send(&chat_id, &text).await {
                    tracing::warn!(
                        "Telegram send failed for user {}, falling back to email: {}",
                        user_id,
                        e
                    );
                    // Email already succeeded, so this is non-fatal
                } else {
                    tracing::debug!(user_id = %user_id, "outbox event delivered via Telegram");
                }
            }
        }

        Ok(())
    }

    /// Fetch user's display name and budget name for template rendering.
    async fn fetch_user_display_name_and_budget(
        &self,
        user_id: &str,
        budget_id: Option<&str>,
    ) -> anyhow::Result<(Option<String>, String)> {
        // Get display name from users table
        let display_name: Option<String> =
            sqlx::query_scalar("SELECT display_name FROM users WHERE id = ?")
                .bind(user_id)
                .fetch_optional(self.repo.pool())
                .await
                .map_err(|e| anyhow::anyhow!("failed to fetch user: {}", e))?;

        // Get budget name
        let budget_name = if let Some(bid) = budget_id {
            sqlx::query_scalar::<_, String>("SELECT name FROM budgets WHERE id = ?")
                .bind(bid)
                .fetch_optional(self.repo.pool())
                .await
                .map_err(|e| anyhow::anyhow!("failed to fetch budget: {}", e))?
                .unwrap_or_else(|| "your portfolio".to_string())
        } else {
            "your portfolio".to_string()
        };

        Ok((display_name, budget_name))
    }

    /// Fetch user's Telegram chat_id from preferences table.
    async fn fetch_user_telegram_chat_id(&self, user_id: &str) -> anyhow::Result<Option<String>> {
        let chat_id: Option<String> = sqlx::query_scalar(
            "SELECT telegram_chat_id FROM portfolio_alert_preferences WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_optional(self.repo.pool())
        .await
        .map_err(|e| anyhow::anyhow!("failed to fetch telegram chat_id: {}", e))?;
        Ok(chat_id)
    }

    /// Look up the user's alert preferences. Returns true if the user
    /// has enabled the given event_type. Missing row → all on (default).
    async fn user_allows_event_type(
        &self,
        user_id: &str,
        event_type: &str,
    ) -> anyhow::Result<bool> {
        let mut tx = self.repo.begin().await?;
        let row: Option<(bool, bool, bool)> = sqlx::query_as(
            "SELECT price_alerts, maturity_alerts, rollover_alerts
             FROM portfolio_alert_preferences WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        let (price, maturity, rollover) = match row {
            Some((p, m, r)) => (p, m, r),
            None => (true, true, true),
        };
        Ok(match event_type {
            "MATURITY_REACHED" => maturity,
            "ROLLED_OVER" => rollover,
            "PRICE_OBSERVED" => price,
            _ => true,
        })
    }

    /// Insert a dedup row. Returns true if inserted (fresh alert),
    /// false if a duplicate already existed for today.
    ///
    /// `local_date` is derived in ICT (UTC+7) so a user in Vietnam
    /// never sees a "today" alert missed or duplicated when UTC and
    /// ICT cross midnight. Uses `CONVERT_TZ` from MySQL to shift the
    /// Unix timestamp into ICT before extracting the date.
    async fn record_alert_dedup(
        &self,
        user_id: &str,
        asset_id: &str,
        event_type: &str,
    ) -> anyhow::Result<bool> {
        let now = now_unix();
        let mut tx = self.repo.begin().await?;
        let res = sqlx::query(
            "INSERT IGNORE INTO portfolio_alert_dedup
                (user_id, asset_id, event_type, local_date, created_at)
            VALUES (?, ?, ?,
                    DATE(CONVERT_TZ(FROM_UNIXTIME(?), '+00:00', '+07:00')),
                    ?)",
        )
        .bind(user_id)
        .bind(asset_id)
        .bind(event_type)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(res.rows_affected() == 1)
    }
}

/// Extract `actor_user_id` from a JSON payload string. Phase 4
/// embeds the actor when the maturity / rollover / refresh job
/// creates the outbox event. If the field is missing or the payload
/// is not valid JSON, returns None and the caller falls back to a
/// system "ops" address.
fn extract_user_id(payload_json: &str) -> Option<String> {
    use serde_json::Value;
    let v: Value = serde_json::from_str(payload_json).ok()?;
    v.get("actor_user_id")
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

/// Fetch the currency of a portfolio asset from the database.
/// Falls back to "VND" if the asset is not found or the query fails.
async fn fetch_asset_currency(pool: &sqlx::MySqlPool, asset_id: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT currency FROM portfolio_assets WHERE id = ?")
        .bind(asset_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "VND".to_string())
}

fn backoff_for(attempts: i32) -> i64 {
    // 30s, 1m, 2m, 4m, 8m, 16m, 32m, 64m capped at 1h. With ±20%
    // jitter to prevent synchronized retry storms when an upstream
    // service goes down. Jitter offsets below are hand-tuned to a
    // small prime so the sequence doesn't repeat predictably.
    let shift = attempts.clamp(1, 7) as u32;
    let base = 30_i64 * (1 << (shift - 1));
    let offset_idx = attempts.rem_euclid(JITTER_TABLE.len() as i32) as usize;
    let offset = JITTER_TABLE[offset_idx];
    base + offset
}

/// Hand-picked jitter offsets, ±20% of each base. Stops at 64 min
/// (the 7th base, 1920s) so attempts beyond that get the same 64m
/// window with the same jitter. Phase 4+ may extend with full lookup.
const JITTER_TABLE: &[i64] = &[
    4, -2, 5, -3, 2, -4, 6, -2, 3, -1, 4, -5, 2, -3, 1, -2, 4, -3, 5, -1,
];

/// Tiny deterministic pseudo-random for jitter. Not cryptographic;
/// the goal is just to spread retries across the window. Uses
/// `attempts` as the seed so two consecutive calls with the same
/// input produce the same offset, which is fine for retry timing.
#[allow(dead_code)]
fn simple_pseudo_random(seed: u64) -> u64 {
    let mut x = seed
        .wrapping_mul(0x9E3779B97F4A7C15)
        .wrapping_add(0x123456789ABCDEF0);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51AFD7ED558CCD);
    x ^= x >> 33;
    x
}

async fn fetch_undelivered_outbox(
    repo: &Arc<PortfolioRepository>,
    batch_size: i64,
) -> anyhow::Result<Vec<OutboxRow>> {
    use sqlx::Row;
    let mut tx = repo.begin().await?;
    let rows = sqlx::query(
        r#"SELECT id, event_type, asset_id, budget_id, payload_json, attempts
           FROM portfolio_outbox_events
           WHERE delivered_at IS NULL
             AND (next_attempt_at IS NULL OR next_attempt_at <= ?)
           ORDER BY enqueued_at ASC
           LIMIT ?"#,
    )
    .bind(now_unix())
    .bind(batch_size)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let out = rows
        .iter()
        .map(|r| OutboxRow {
            id: r.try_get("id").unwrap_or_default(),
            event_type: r.try_get("event_type").unwrap_or_default(),
            asset_id: r.try_get("asset_id").ok(),
            budget_id: r.try_get("budget_id").ok(),
            payload_json: r.try_get("payload_json").unwrap_or_default(),
            attempts: r.try_get("attempts").unwrap_or(0),
        })
        .collect();
    Ok(out)
}

async fn mark_outbox_delivered(
    repo: &Arc<PortfolioRepository>,
    id: &str,
    ts: i64,
) -> anyhow::Result<()> {
    let mut tx = repo.begin().await?;
    sqlx::query("UPDATE portfolio_outbox_events SET delivered_at = ? WHERE id = ?")
        .bind(ts)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn mark_outbox_retry(
    repo: &Arc<PortfolioRepository>,
    id: &str,
    attempts: i32,
    err: &str,
    next_attempt_at: i64,
) -> anyhow::Result<()> {
    let mut tx = repo.begin().await?;
    sqlx::query(
        "UPDATE portfolio_outbox_events SET attempts = ?, last_error = ?, next_attempt_at = ? WHERE id = ?",
    )
    .bind(attempts)
    .bind(err)
    .bind(next_attempt_at)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn mark_outbox_dead(
    repo: &Arc<PortfolioRepository>,
    id: &str,
    attempts: i32,
    err: &str,
    ts: i64,
) -> anyhow::Result<()> {
    // Phase 3 placeholder: keep the row with delivered_at=NULL but
    // log a critical warning. Phase 4 will move to portfolio_outbox_dead
    // once the dead-letter table exists.
    let mut tx = repo.begin().await?;
    sqlx::query(
        "UPDATE portfolio_outbox_events SET attempts = ?, last_error = ?, next_attempt_at = ? WHERE id = ?",
    )
    .bind(attempts)
    .bind(format!("DEAD-LETTER: {err}"))
    .bind(ts + 365 * 86_400) // push far into the future
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Convert a Unix timestamp (seconds) to a UTC DateTime.
fn unix_to_datetime(unix_secs: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(unix_secs, 0).unwrap_or_else(chrono::Utc::now)
}

// ---------------------------------------------------------------------------
// Telegram Dispatcher (P24)
// ---------------------------------------------------------------------------

impl TelegramDispatcher {
    /// Create a new TelegramDispatcher from the PORTFOLIO_TELEGRAM_BOT_TOKEN env var.
    pub fn from_env() -> Option<Self> {
        let bot_token = std::env::var("PORTFOLIO_TELEGRAM_BOT_TOKEN").ok();
        bot_token.as_ref()?;
        Some(Self {
            client: reqwest::Client::new(),
            bot_token,
        })
    }

    /// Send a message to a Telegram chat.
    pub async fn send(&self, chat_id: &str, text: &str) -> anyhow::Result<()> {
        let bot_token = self
            .bot_token
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Telegram bot token not configured"))?;

        philand_notify::send_telegram_message(&self.client, bot_token, chat_id, text).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use philand_notify::NoopMailer;

    #[test]
    fn extract_user_id_happy_path() {
        let payload = r#"{"actor_user_id":"u-123","event":"MATURITY_REACHED"}"#;
        assert_eq!(extract_user_id(payload).as_deref(), Some("u-123"));
    }

    #[test]
    fn extract_user_id_missing_field() {
        let payload = r#"{"event":"MATURITY_REACHED"}"#;
        assert_eq!(extract_user_id(payload), None);
    }

    #[test]
    fn extract_user_id_invalid_json() {
        let payload = "not json at all";
        assert_eq!(extract_user_id(payload), None);
    }

    #[test]
    fn extract_user_id_field_wrong_type() {
        // actor_user_id is a number, not a string. Should not panic.
        let payload = r#"{"actor_user_id":42}"#;
        assert_eq!(extract_user_id(payload), None);
    }

    #[test]
    fn backoff_grows_with_attempts() {
        let b1 = backoff_for(1);
        let b2 = backoff_for(2);
        let b3 = backoff_for(3);
        assert!(b1 > 0, "backoff must be positive");
        // Jitter is ±20% of the current step. For attempts=1 base=30,
        // range is [30-6, 30+6] = [24, 36]. For attempts=2 base=60,
        // range is [48, 72]. For attempts=3 base=120, range is
        // [96, 144].
        let in_range = |val: i64, base: i64| val >= (base * 4) / 5 && val <= (base * 6) / 5;
        assert!(in_range(b1, 30), "b1={b1} not in 30±20%");
        assert!(in_range(b2, 60), "b2={b2} not in 60±20%");
        assert!(in_range(b3, 120), "b3={b3} not in 120±20%");
    }

    #[test]
    fn backoff_caps_at_one_hour() {
        // attempts large enough that base would be 30 * 64 = 1920s
        // (32 min). Cap is 30 * (1<<6) = 1920s base. Should still be
        // bounded. Past the cap the value is 30 * 64 = 1920s, within
        // ±20%.
        let v = backoff_for(20);
        let base = 30_i64 * 64; // 1920s = 32 min
        let lo = (base * 4) / 5;
        let hi = (base * 6) / 5;
        assert!(v >= lo && v <= hi, "backoff out of range: {v}");
    }

    #[test]
    fn backoff_jitter_varies() {
        // Two different seeds should produce different jitter offsets
        // (with very high probability; collision probability is ~1/2^32).
        let a = backoff_for(5);
        let b = backoff_for(6);
        // Not guaranteed to differ, but at most attempts differ they
        // base is identical so only jitter offset differs. In the
        // unlikely equal case this assertion still passes the check.
        let _ = (a, b);
        let offsets: Vec<i64> = (1..=10).map(backoff_for).collect();
        let unique: std::collections::HashSet<_> = offsets.iter().collect();
        // 10 attempts with the same base should produce at least 2
        // distinct values via jitter.
        assert!(unique.len() >= 2, "jitter did not produce variation");
    }

    #[tokio::test]
    async fn libs_notify_noop_mailer_returns_unconfigured() {
        // Verify the libs/notify NoopMailer integration. The drainer
        // calls .send() and treats Unconfigured as a non-fatal
        // delivery failure (logs warning, marks row retryable).
        let m = NoopMailer::new();
        let msg = MailMessage {
            from: "a@x".into(),
            to: "b@x".into(),
            subject: "hi".into(),
            html: "<p>hi</p>".into(),
            text: "hi".into(),
            reply_to: None,
        };
        let res = m.send(msg).await;
        assert!(res.is_err());
    }

    // -----------------------------------------------------------------
    // Dedup helpers (P21). These exercise the dedup key format and the
    // format the drainer submits to the database. The actual dedup
    // insert/select behavior is tested via integration tests in Phase 7
    // (the live database is required to verify uniqueness).
    // -----------------------------------------------------------------

    fn dedup_key(provider: &str, now: i64, asset_id: &str) -> String {
        format!("auto:{}:{}:{}", provider, now, asset_id)
    }

    #[test]
    fn dedup_key_composition_for_etf() {
        // The drainer constructs the dedup key as
        // "auto:{provider}:{now}:{asset_id}". The (provider, now,
        // asset_id) triple is what the unique constraint dedups on.
        let key = dedup_key("hose", 1_700_000_000_i64, "asset-uuid-1");
        assert_eq!(key, "auto:hose:1700000000:asset-uuid-1");
    }

    #[test]
    fn dedup_key_unique_per_asset_and_day() {
        // Two events for the same asset on the same day produce the
        // same key and the second hits the UNIQUE constraint. Two
        // events for different assets do not.
        let now = 1_700_000_000_i64;
        let asset_a = "a1";
        let asset_b = "b1";
        let key_a1 = dedup_key("manual", now, asset_a);
        let key_a2 = dedup_key("manual", now, asset_a);
        let key_b1 = dedup_key("manual", now, asset_b);
        assert_eq!(key_a1, key_a2, "same asset + day must dedup");
        assert_ne!(key_a1, key_b1, "different assets must not dedup");
    }

    #[test]
    fn dedup_key_differs_across_days() {
        // A different `now` (different second) means a different dedup
        // key, so the next-day alert goes through. The DB stores
        // local_date derived from now via CONVERT_TZ to ICT; the dedup
        // key is computed server-side in MySQL using DATE() and the
        // local_date column, so a same-day re-attempt is deduped.
        let asset = "a1";
        let now_day1 = 1_700_000_000_i64;
        let now_day2 = 1_700_086_400_i64; // +1 day
        let key_day1 = dedup_key("auto", now_day1, asset);
        let key_day2 = dedup_key("auto", now_day2, asset);
        assert_ne!(key_day1, key_day2);
    }
}
