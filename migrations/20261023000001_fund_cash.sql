-- Fund cash pocket (saving budgets). balance_minor is a cache of SUM(fund_cash_movements.amount_minor);
-- every write goes through the repository, which locks the account row and refuses a negative balance.
CREATE TABLE IF NOT EXISTS fund_cash_accounts (
  budget_id      VARCHAR(36) NOT NULL PRIMARY KEY,
  currency       VARCHAR(8)  NOT NULL,
  balance_minor  BIGINT      NOT NULL DEFAULT 0,
  version        BIGINT      NOT NULL DEFAULT 0,
  created_at     BIGINT      NOT NULL,
  updated_at     BIGINT      NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS fund_cash_movements (
  id                 VARCHAR(36) NOT NULL PRIMARY KEY,
  budget_id          VARCHAR(36) NOT NULL,
  kind               VARCHAR(32) NOT NULL,
  amount_minor       BIGINT      NOT NULL,
  ref_type           VARCHAR(32) NULL,
  ref_id             VARCHAR(36) NULL,
  transfer_group_id  VARCHAR(36) NULL,
  idempotency_key    VARCHAR(64) NULL,
  actor_user_id      VARCHAR(36) NOT NULL,
  note               TEXT        NULL,
  occurred_on        VARCHAR(10) NOT NULL,
  created_at         BIGINT      NOT NULL,
  INDEX idx_fcm_budget (budget_id, occurred_on),
  INDEX idx_fcm_ref (ref_type, ref_id),
  UNIQUE KEY uk_fcm_idem (budget_id, idempotency_key)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
