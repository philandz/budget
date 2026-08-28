-- P24: Add telegram_chat_id to portfolio_alert_preferences
-- This allows the outbox drainer to send Telegram messages directly
-- to users who have linked their Telegram account.

ALTER TABLE portfolio_alert_preferences
    ADD COLUMN telegram_chat_id VARCHAR(64) DEFAULT NULL
    AFTER telegram_enabled;

-- Add index for fast lookup by chat_id
CREATE INDEX idx_portfolio_alert_prefs_telegram ON portfolio_alert_preferences(telegram_chat_id);
