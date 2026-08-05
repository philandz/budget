-- Portfolio transfers (Phase 3.1)
-- Stores cross-budget transfer records that link cash legs on
-- the source and counterparty budgets. The actual entry rows live
-- in the Entry service; the transfer here is the audit-trail bridge.

CREATE TABLE IF NOT EXISTS portfolio_transfers (
    id                              VARCHAR(36)  NOT NULL PRIMARY KEY,
    group_id                        VARCHAR(36)  NOT NULL,
    source_budget_id                VARCHAR(36)  NOT NULL,
    counterparty_budget_id          VARCHAR(36)  NOT NULL,
    direction                       VARCHAR(32)  NOT NULL,
    amount_minor                    BIGINT       NOT NULL,
    currency                        VARCHAR(8)   NOT NULL,
    status                          VARCHAR(24)  NOT NULL DEFAULT 'requested',
    linked_entry_source_id          VARCHAR(36)  DEFAULT NULL,
    linked_entry_counterparty_id    VARCHAR(36)  DEFAULT NULL,
    idempotency_key                 VARCHAR(64)  DEFAULT NULL,
    actor_user_id                   VARCHAR(36)  NOT NULL,
    created_at                      BIGINT       NOT NULL,
    completed_at                    BIGINT       DEFAULT NULL,
    notes                           TEXT         DEFAULT NULL,

    INDEX idx_portfolio_transfers_source (source_budget_id, created_at),
    INDEX idx_portfolio_transfers_counter (counterparty_budget_id, created_at),
    UNIQUE KEY uk_portfolio_transfers_idem (
        source_budget_id,
        counterparty_budget_id,
        idempotency_key
    )
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;