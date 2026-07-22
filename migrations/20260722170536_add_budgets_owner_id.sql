-- Add owner_id column to budgets table.
--
-- The budget service INSERT references `owner_id` (line 89 of
-- src/manager/repository/mod.rs) but the v1 schema never declared the
-- column. Any create_budget call on prod fails with:
--   Unknown column 'owner_id' in 'field list' (1054)
--
-- Idempotent: only adds if column is missing.

SET @dbname = DATABASE();
SET @tablename = 'budgets';
SET @columnname = 'owner_id';

SET @has_column := (SELECT COUNT(*) FROM INFORMATION_SCHEMA.COLUMNS
                    WHERE TABLE_SCHEMA = @dbname AND TABLE_NAME = @tablename
                      AND COLUMN_NAME = @columnname);

SET @sql := IF(@has_column = 0,
               'ALTER TABLE budgets ADD COLUMN owner_id VARCHAR(36) DEFAULT NULL AFTER org_id',
               'SELECT 1');
PREPARE stmt FROM @sql; EXECUTE stmt; DEALLOCATE PREPARE stmt;

SET @has_idx := (SELECT COUNT(*) FROM INFORMATION_SCHEMA.STATISTICS
                 WHERE TABLE_SCHEMA = @dbname AND TABLE_NAME = @tablename
                   AND INDEX_NAME = 'idx_budgets_owner_id');
SET @sql := IF(@has_idx = 0,
               'ALTER TABLE budgets ADD INDEX idx_budgets_owner_id (owner_id)',
               'SELECT 1');
PREPARE stmt FROM @sql; EXECUTE stmt; DEALLOCATE PREPARE stmt;