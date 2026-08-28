-- ALTER snapshot_date on invest_price_snapshots from DATE to VARCHAR(32).
-- The legacy backfill + the budget service both store ISO-8601 strings
-- ("2026-02-01"). MySQL's sqlx driver refuses to bind String to DATE and
-- refuses to decode DATE into String. Loosen the column to VARCHAR(32) so
-- the existing ISO-string format works on both read and write.
ALTER TABLE invest_price_snapshots MODIFY COLUMN snapshot_date VARCHAR(32) NOT NULL;
