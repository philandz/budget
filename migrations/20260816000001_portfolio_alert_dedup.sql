-- Portfolio alert preferences (Phase 4.2)
-- Per-user opt-in / channel toggles. Default channels are NULL until the
-- user updates the row. NULL means "use the default channel set" (email).
-- This table is user-scoped, not budget-scoped: a user has one global
-- preferences row covering all their portfolio alerts. Budget-scoped
-- overrides are out of scope for Phase 4.

CREATE TABLE IF NOT EXISTS portfolio_alert_preferences (
    user_id           VARCHAR(36)  NOT NULL PRIMARY KEY,
    email_enabled     BOOLEAN      NOT NULL DEFAULT TRUE,
    telegram_enabled  BOOLEAN      NOT NULL DEFAULT FALSE,
    price_alerts       BOOLEAN      NOT NULL DEFAULT TRUE,
    maturity_alerts    BOOLEAN      NOT NULL DEFAULT TRUE,
    rollover_alerts    BOOLEAN      NOT NULL DEFAULT TRUE,
    created_at        BIGINT       NOT NULL,
    updated_at        BIGINT       NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Portfolio alert dedup (Phase 4.2)
-- One row per (asset, event_type, local date). Inserts are idempotent
-- via the unique key, so a re-run of the maturity scan or refresh job
-- produces at most one notification per day per asset per event type.
-- The drainer checks this table before sending a mailer message and
-- skips on duplicate.

CREATE TABLE IF NOT EXISTS portfolio_alert_dedup (
    id                BIGINT       NOT NULL AUTO_INCREMENT PRIMARY KEY,
    user_id           VARCHAR(36)  NOT NULL,
    asset_id          VARCHAR(36)  NOT NULL,
    event_type        VARCHAR(48)  NOT NULL,
    local_date        DATE         NOT NULL,
    created_at        BIGINT       NOT NULL,

    UNIQUE KEY uk_portfolio_alert_dedup (user_id, asset_id, event_type, local_date),
    INDEX idx_portfolio_alert_dedup_user_date (user_id, local_date)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;