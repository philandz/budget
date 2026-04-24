-- Budget templates: pre-configured category structures users can apply on creation
CREATE TABLE IF NOT EXISTS budget_templates (
    id          VARCHAR(36)   NOT NULL PRIMARY KEY,
    name        VARCHAR(255)  NOT NULL,
    description TEXT,
    budget_type VARCHAR(32)   NOT NULL DEFAULT 'standard',
    created_at  BIGINT        NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Seed built-in templates (idempotent via INSERT IGNORE)
INSERT IGNORE INTO budget_templates (id, name, description, budget_type, created_at) VALUES
(
    '00000000-0000-0000-0000-000000000001',
    '50/30/20 Household',
    'Allocates 50% to needs, 30% to wants, 20% to savings. Ideal for couples and families.',
    'standard',
    UNIX_TIMESTAMP() * 1000
),
(
    '00000000-0000-0000-0000-000000000002',
    'Freelancer Monthly',
    'Separates business expenses, personal living costs, and tax set-aside for self-employed users.',
    'standard',
    UNIX_TIMESTAMP() * 1000
),
(
    '00000000-0000-0000-0000-000000000003',
    'Trip / Event Sharing',
    'Optimised for one-off shared expenses between a group. Tracks who paid and auto-calculates splits.',
    'sharing',
    UNIX_TIMESTAMP() * 1000
),
(
    '00000000-0000-0000-0000-000000000004',
    'Debt Payoff Tracker',
    'Single-purpose tracker for paying down a liability (loan, credit card, BNPL).',
    'debt',
    UNIX_TIMESTAMP() * 1000
),
(
    '00000000-0000-0000-0000-000000000005',
    'Emergency Fund',
    'Saving budget targeting a 3–6 month emergency reserve.',
    'saving',
    UNIX_TIMESTAMP() * 1000
);
