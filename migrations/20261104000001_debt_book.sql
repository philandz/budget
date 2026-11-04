-- Debt book (plan 2026-11-04-debt-budget-plan): a Debt budget holds many debt accounts.
-- Additive only. outstanding_minor is a cache of
--   opening_outstanding_minor - SUM(debt_payments.principal_minor WHERE status='completed') + SUM(adjustments)
-- and every write goes through the repository, which locks the account row (invariants D1, D3).
CREATE TABLE IF NOT EXISTS debt_accounts (
  id                        VARCHAR(36)  NOT NULL PRIMARY KEY,
  budget_id                 VARCHAR(36)  NOT NULL,
  name                      VARCHAR(120) NOT NULL,
  lender                    VARCHAR(120) NULL,
  kind                      VARCHAR(32)  NOT NULL,           -- display label only (DB3)
  repayment_method          VARCHAR(32)  NOT NULL,           -- annuity | equal_principal | interest_only | fixed_installments | no_schedule
  currency                  VARCHAR(8)   NOT NULL,
  opening_outstanding_minor BIGINT       NOT NULL,
  outstanding_minor         BIGINT       NOT NULL,
  tracking_start_date       VARCHAR(10)  NOT NULL,
  maturity_date             VARCHAR(10)  NULL,
  remaining_periods         INT          NULL,
  due_day                   TINYINT      NULL,               -- 1..28, 31 = last day of month
  periodic_fee_minor        BIGINT       NOT NULL DEFAULT 0,
  day_count                 VARCHAR(16)  NOT NULL DEFAULT 'actual_365',
  interest_category_id      VARCHAR(36)  NULL,
  pay_from_budget_id        VARCHAR(36)  NULL,
  interest_through          VARCHAR(10)  NULL,               -- interest is settled up to this date
  last_due_date             VARCHAR(10)  NULL,               -- last scheduled due date that was paid
  installment_minor         BIGINT       NULL,               -- fixed instalment kept after a "reduce term" prepayment
  status                    VARCHAR(16)  NOT NULL DEFAULT 'active',  -- active | settled | closed
  version                   BIGINT       NOT NULL DEFAULT 0,
  created_by                VARCHAR(36)  NOT NULL,
  created_at                BIGINT       NOT NULL,
  updated_at                BIGINT       NOT NULL,
  deleted_at                BIGINT       NULL,
  INDEX idx_debt_accounts_budget (budget_id, status)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS debt_rate_periods (
  id              VARCHAR(36)   NOT NULL PRIMARY KEY,
  account_id      VARCHAR(36)   NOT NULL,
  effective_from  VARCHAR(10)   NOT NULL,
  annual_rate     DECIMAL(9,6)  NOT NULL,                    -- percent per year
  rate_type       VARCHAR(16)   NOT NULL DEFAULT 'fixed',    -- fixed | floating
  note            VARCHAR(255)  NULL,
  created_at      BIGINT        NOT NULL,
  UNIQUE KEY uk_debt_rate (account_id, effective_from)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS debt_prepay_rules (
  id                              VARCHAR(36)   NOT NULL PRIMARY KEY,
  account_id                      VARCHAR(36)   NOT NULL,
  valid_from                      VARCHAR(10)   NOT NULL,
  valid_to                        VARCHAR(10)   NULL,
  free_allowance_per_month_minor  BIGINT        NOT NULL DEFAULT 0,
  fee_pct                         DECIMAL(7,4)  NOT NULL DEFAULT 0,
  min_fee_minor                   BIGINT        NOT NULL DEFAULT 0,
  note                            VARCHAR(255)  NULL,
  created_at                      BIGINT        NOT NULL,
  INDEX idx_debt_prepay_account (account_id, valid_from)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS debt_payments (
  id                    VARCHAR(36)  NOT NULL PRIMARY KEY,
  account_id            VARCHAR(36)  NOT NULL,
  budget_id             VARCHAR(36)  NOT NULL,
  paid_on               VARCHAR(10)  NOT NULL,
  kind                  VARCHAR(16)  NOT NULL,                -- scheduled | prepayment | settlement
  principal_minor       BIGINT       NOT NULL,
  interest_minor        BIGINT       NOT NULL,
  fee_minor             BIGINT       NOT NULL,
  total_minor           BIGINT       NOT NULL,
  prepay_effect         VARCHAR(24)  NULL,                    -- reduce_term | reduce_installment
  suggested_fee_minor   BIGINT       NULL,
  from_budget_id        VARCHAR(36)  NULL,
  category_id           VARCHAR(36)  NULL,
  entry_group_id        VARCHAR(36)  NULL,
  due_date              VARCHAR(10)  NULL,
  status                VARCHAR(16)  NOT NULL,                -- requested | reconciling | completed | failed
  idempotency_key       VARCHAR(64)  NOT NULL,
  actor_user_id         VARCHAR(36)  NOT NULL,
  note                  TEXT         NULL,
  created_at            BIGINT       NOT NULL,
  updated_at            BIGINT       NOT NULL,
  UNIQUE KEY uk_debt_payment_idem (account_id, idempotency_key),
  INDEX idx_debt_payments_account (account_id, paid_on),
  INDEX idx_debt_payments_status (status, updated_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS debt_events (
  id             VARCHAR(36)  NOT NULL PRIMARY KEY,
  account_id     VARCHAR(36)  NOT NULL,
  budget_id      VARCHAR(36)  NOT NULL,
  kind           VARCHAR(32)  NOT NULL,   -- created | payment | rate_change | adjustment | prepay_rule_change | settled | closed | reopened | updated
  amount_minor   BIGINT       NULL,
  payload        TEXT         NULL,
  actor_user_id  VARCHAR(36)  NOT NULL,
  occurred_on    VARCHAR(10)  NOT NULL,
  created_at     BIGINT       NOT NULL,
  INDEX idx_debt_events_account (account_id, created_at),
  INDEX idx_debt_events_budget (budget_id, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
