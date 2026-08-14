-- The Rust code stores dates as ISO-8601 strings (YYYY-MM-DD). MySQL DATE
-- refuses to round-trip Option<String> because sqlx binds String to VARCHAR
-- and decodes DATE into chrono types. Loosen the columns to VARCHAR(32)
-- so the existing ISO-string format works on both read and write.
ALTER TABLE invest_assets MODIFY COLUMN start_date VARCHAR(32) NULL;
ALTER TABLE invest_assets MODIFY COLUMN maturity_date VARCHAR(32) NULL;
ALTER TABLE invest_assets MODIFY COLUMN purchase_date VARCHAR(32) NULL;
