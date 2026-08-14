-- ALTER DECIMAL columns on invest_assets to DOUBLE so the budget service
-- can bind Rust f64 values without a type-mismatch error. The legacy
-- migrations declared quantity DECIMAL(12,4) and annual_rate DECIMAL(6,4);
-- the Rust code expects Option<f64>. MySQL's sqlx driver refuses to bind
-- f64 directly into DECIMAL columns and refuses to decode DECIMAL into
-- f64. DOUBLE round-trips both directions without precision loss for the
-- ranges used here (quantity grams / ounces, annual_rate percentages).
ALTER TABLE invest_assets MODIFY COLUMN quantity DOUBLE NULL;
ALTER TABLE invest_assets MODIFY COLUMN annual_rate DOUBLE NULL;
