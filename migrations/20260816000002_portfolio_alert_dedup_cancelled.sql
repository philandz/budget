-- Add cancelled_at column to portfolio_alert_dedup so users can
-- re-trigger a snoozed alert. Phase 4.5 will wire a frontend
-- snooze/cancel endpoint; this migration just provides the storage.

ALTER TABLE portfolio_alert_dedup
    ADD COLUMN cancelled_at BIGINT NULL DEFAULT NULL;

-- Updated query pattern (Phase 4.5):
--   SELECT ... FROM portfolio_alert_dedup
--   WHERE user_id = ? AND asset_id = ? AND event_type = ?
--     AND local_date = ? AND cancelled_at IS NULL
-- (the dedup key in `record_alert_dedup` stays INSERT IGNORE; the
--  drainer just adds a `cancelled_at IS NULL` predicate when reading
--  for a re-send flow).
