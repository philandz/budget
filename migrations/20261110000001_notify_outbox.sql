-- Notify outbox for a producer service (libs/event::MySqlOutbox).
-- Copy into the service's migrations/ and replace budget_notify_outbox with
-- "<service>_notify_outbox", e.g. budget_notify_outbox.
CREATE TABLE IF NOT EXISTS budget_notify_outbox (
    event_id        VARCHAR(36)  NOT NULL PRIMARY KEY,
    event_type      VARCHAR(64)  NOT NULL,
    payload_json    MEDIUMTEXT   NOT NULL,
    enqueued_at     BIGINT       NOT NULL,
    attempts        INT          NOT NULL DEFAULT 0,
    next_attempt_at BIGINT       DEFAULT NULL,
    delivered_at    BIGINT       DEFAULT NULL,
    dead_at         BIGINT       DEFAULT NULL,
    last_error      TEXT         DEFAULT NULL,
    INDEX idx_budget_notify_outbox_due (delivered_at, dead_at, next_attempt_at, enqueued_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
