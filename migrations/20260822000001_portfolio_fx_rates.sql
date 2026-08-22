-- FX rate table for multi-currency portfolio valuation.
-- Rates are stored as minor units (int64) so the existing money-math
-- convention (int64 minor units) applies consistently throughout.

CREATE TABLE IF NOT EXISTS portfolio_fx_rates (
    id              VARCHAR(36)  NOT NULL PRIMARY KEY,
    from_currency   VARCHAR(8)   NOT NULL,
    to_currency     VARCHAR(8)   NOT NULL,
    rate            BIGINT       NOT NULL COMMENT 'minor units of to_currency per unit of from_currency',
    effective_date  VARCHAR(32)  NOT NULL COMMENT 'YYYY-MM-DD per repo date-varchar convention',
    created_at      BIGINT       NOT NULL,
    updated_at      BIGINT       NOT NULL,
    deleted_at      BIGINT       DEFAULT NULL,

    INDEX idx_portfolio_fx_rates_ccy_date (from_currency, to_currency, effective_date)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Seed: 1 USD = 25,000 VND minor units.
-- Both directions are seeded so lookups are O(1) without requiring inverse math
-- at query time (though the service also handles inverse as a fallback).
INSERT INTO portfolio_fx_rates (id, from_currency, to_currency, rate, effective_date, created_at, updated_at)
VALUES (
    '00000000-0000-0000-0000-000000000001',
    'USD',
    'VND',
    25000,
    '2026-01-01',
    UNIX_TIMESTAMP(),
    UNIX_TIMESTAMP()
);

INSERT INTO portfolio_fx_rates (id, from_currency, to_currency, rate, effective_date, created_at, updated_at)
VALUES (
    '00000000-0000-0000-0000-000000000002',
    'VND',
    'USD',
    25000,
    '2026-01-01',
    UNIX_TIMESTAMP(),
    UNIX_TIMESTAMP()
);
