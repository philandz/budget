-- Budgets: the financial container owned by an org
CREATE TABLE IF NOT EXISTS budgets (
    id          VARCHAR(36)  NOT NULL PRIMARY KEY,
    org_id      VARCHAR(36)  NOT NULL,
    name        VARCHAR(255) NOT NULL,
    budget_type VARCHAR(32)  NOT NULL DEFAULT 'standard',  -- standard | saving | debt | invest | sharing
    currency    VARCHAR(8)   NOT NULL DEFAULT 'VND',
    status      VARCHAR(16)  NOT NULL DEFAULT 'active',
    created_by  VARCHAR(36)  NOT NULL,
    created_at  BIGINT       NOT NULL,
    updated_at  BIGINT       NOT NULL,
    deleted_at  BIGINT                DEFAULT NULL,
    INDEX idx_budgets_org_id (org_id),
    INDEX idx_budgets_status (status)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Budget members: who belongs to a budget and with what role
CREATE TABLE IF NOT EXISTS budget_members (
    id          VARCHAR(36) NOT NULL PRIMARY KEY,
    budget_id   VARCHAR(36) NOT NULL,
    user_id     VARCHAR(36) NOT NULL,
    role        VARCHAR(16) NOT NULL DEFAULT 'viewer',  -- owner | manager | contributor | viewer
    created_at  BIGINT      NOT NULL,
    updated_at  BIGINT      NOT NULL,
    UNIQUE KEY uq_budget_member (budget_id, user_id),
    INDEX idx_budget_members_user (user_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Envelope limits: optional monthly spending cap per budget
CREATE TABLE IF NOT EXISTS budget_envelope_limits (
    id              VARCHAR(36) NOT NULL PRIMARY KEY,
    budget_id       VARCHAR(36) NOT NULL UNIQUE,
    monthly_limit   BIGINT      NOT NULL,  -- in smallest currency unit
    created_at      BIGINT      NOT NULL,
    updated_at      BIGINT      NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Rollover policy: what happens to unspent budget at month boundary
CREATE TABLE IF NOT EXISTS budget_rollover_policies (
    id          VARCHAR(36) NOT NULL PRIMARY KEY,
    budget_id   VARCHAR(36) NOT NULL UNIQUE,
    policy      VARCHAR(16) NOT NULL DEFAULT 'reset',  -- reset | carry_forward
    created_at  BIGINT      NOT NULL,
    updated_at  BIGINT      NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
