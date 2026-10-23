-- Budget period: a period starts on period_start_day (1-28) of the month in `timezone`.
ALTER TABLE budgets ADD COLUMN period_start_day TINYINT NOT NULL DEFAULT 1;
ALTER TABLE budgets ADD COLUMN timezone VARCHAR(64) NOT NULL DEFAULT 'Asia/Ho_Chi_Minh';
