-- Budget period: a period starts on period_start_day (1-28) of the month in `timezone`.
--
-- Idempotent: only adds each column when it is missing, so re-running the
-- migration (or applying it to a database where a previous run already added
-- one of the columns) does not fail with "Duplicate column name".
-- Pattern follows `20260722170536_add_budgets_owner_id.sql`.

SET @dbname = DATABASE();
SET @tablename = 'budgets';

-- period_start_day
SET @columnname := 'period_start_day';
SET @has_column := (SELECT COUNT(*) FROM INFORMATION_SCHEMA.COLUMNS
                    WHERE TABLE_SCHEMA = @dbname AND TABLE_NAME = @tablename
                      AND COLUMN_NAME = @columnname);
SET @sql := IF(@has_column = 0,
               'ALTER TABLE budgets ADD COLUMN period_start_day TINYINT NOT NULL DEFAULT 1',
               'SELECT 1');
PREPARE stmt FROM @sql; EXECUTE stmt; DEALLOCATE PREPARE stmt;

-- timezone
SET @columnname := 'timezone';
SET @has_column := (SELECT COUNT(*) FROM INFORMATION_SCHEMA.COLUMNS
                    WHERE TABLE_SCHEMA = @dbname AND TABLE_NAME = @tablename
                      AND COLUMN_NAME = @columnname);
SET @sql := IF(@has_column = 0,
               'ALTER TABLE budgets ADD COLUMN timezone VARCHAR(64) NOT NULL DEFAULT ''Asia/Ho_Chi_Minh''',
               'SELECT 1');
PREPARE stmt FROM @sql; EXECUTE stmt; DEALLOCATE PREPARE stmt;
