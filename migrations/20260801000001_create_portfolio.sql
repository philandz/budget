-- Asset Portfolio: typed asset records for Investment Budgets.
-- One independent row per purchase or deposit. Subtype tables enforce
-- class-specific invariants without nullable cross-class columns.

CREATE TABLE IF NOT EXISTS portfolio_assets (
    id               VARCHAR(36)  NOT NULL PRIMARY KEY,
    budget_id        VARCHAR(36)  NOT NULL,
    asset_class      VARCHAR(32)  NOT NULL,  -- savings_account | fixed_deposit | gold_lot | stock_lot
    display_name     VARCHAR(255) NOT NULL,
    currency         VARCHAR(8)   NOT NULL,
    status           VARCHAR(24)  NOT NULL DEFAULT 'active',  -- active | closed | matured | sold | archived | rolled_over | withdrawn | early_closed
    opened_on        BIGINT       NOT NULL,  -- unix epoch seconds, ICT business date
    closed_on        BIGINT       DEFAULT NULL,
    legacy_asset_id  VARCHAR(36)  DEFAULT NULL,  -- invest_assets.id when backfilled
    notes            TEXT         DEFAULT NULL,
    created_by       VARCHAR(36)  NOT NULL,
    created_at       BIGINT       NOT NULL,
    updated_at       BIGINT       NOT NULL,
    deleted_at       BIGINT       DEFAULT NULL,

    INDEX idx_portfolio_assets_budget (budget_id),
    INDEX idx_portfolio_assets_status (status),
    INDEX idx_portfolio_assets_class  (budget_id, asset_class),
    INDEX idx_portfolio_assets_legacy (budget_id, legacy_asset_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS portfolio_savings_accounts (
    asset_id                VARCHAR(36)  NOT NULL PRIMARY KEY,
    provider                VARCHAR(100) NOT NULL,
    account_reference_masked VARCHAR(64)  NOT NULL DEFAULT '',
    current_balance         BIGINT       NOT NULL DEFAULT 0,
    balance_as_of           BIGINT       NOT NULL,
    annual_rate             VARCHAR(32) NOT NULL DEFAULT '0',
    interest_method         VARCHAR(16)  NOT NULL DEFAULT 'simple',  -- simple | compound
    payout_type             VARCHAR(24)  NOT NULL DEFAULT 'on_demand', -- at_maturity | monthly | quarterly | on_demand
    opened_on               BIGINT       NOT NULL,
    notes                   TEXT         DEFAULT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS portfolio_fixed_deposits (
    asset_id                  VARCHAR(36)  NOT NULL PRIMARY KEY,
    provider                  VARCHAR(100) NOT NULL,
    product_name              VARCHAR(255) NOT NULL,
    principal                 BIGINT       NOT NULL,
    annual_rate               VARCHAR(32) NOT NULL DEFAULT '0',
    interest_method           VARCHAR(16)  NOT NULL DEFAULT 'simple',
    payout_type               VARCHAR(24)  NOT NULL DEFAULT 'at_maturity',
    deposit_date              BIGINT       NOT NULL,
    maturity_date             BIGINT       NOT NULL,
    auto_renewal_policy       VARCHAR(32)  NOT NULL DEFAULT 'none',
    rollover_from_asset_id    VARCHAR(36)  DEFAULT NULL,
    certificate_reference_masked VARCHAR(64) DEFAULT '',
    notes                     TEXT         DEFAULT NULL,

    INDEX idx_portfolio_fd_maturity (maturity_date, deposit_date)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS portfolio_gold_lots (
    asset_id                          VARCHAR(36)  NOT NULL PRIMARY KEY,
    provider                          VARCHAR(64)  NOT NULL,
    gold_type                         VARCHAR(64)  NOT NULL,
    purity                            VARCHAR(32)  NOT NULL DEFAULT 'other',  -- sjc_9999 | pnj_999 | pnj_995 | doji_9999 | other
    form                              VARCHAR(32)  NOT NULL DEFAULT 'other',  -- bar | ring | coin | jewelry | other
    quantity_original                 VARCHAR(40) NOT NULL,
    unit_original                     VARCHAR(16)  NOT NULL,  -- chi | luong | gram
    quantity_grams                    VARCHAR(40) NOT NULL,
    purchase_price_per_unit_original  BIGINT       NOT NULL DEFAULT 0,
    purchase_cost                     BIGINT       NOT NULL,
    fees                              BIGINT       NOT NULL DEFAULT 0,
    purchase_date                     BIGINT       NOT NULL,
    notes                             TEXT         DEFAULT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS portfolio_stock_lots (
    asset_id           VARCHAR(36)  NOT NULL PRIMARY KEY,
    ticker             VARCHAR(20)  NOT NULL,
    exchange           VARCHAR(16)  NOT NULL,  -- HOSE | HNX | UPCOM
    quantity_bought    VARCHAR(40) NOT NULL,
    quantity_open      VARCHAR(40) NOT NULL,
    buy_price_per_share BIGINT      NOT NULL,
    purchase_cost      BIGINT       NOT NULL,
    fees               BIGINT       NOT NULL DEFAULT 0,
    purchase_date      BIGINT       NOT NULL,
    settlement_date    BIGINT       DEFAULT NULL,
    notes              TEXT         DEFAULT NULL,

    INDEX idx_portfolio_stock_ticker (ticker, exchange)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS portfolio_stock_disposals (
    id                   VARCHAR(36)  NOT NULL PRIMARY KEY,
    asset_id             VARCHAR(36)  NOT NULL,
    disposal_date        BIGINT       NOT NULL,
    quantity_sold        VARCHAR(40) NOT NULL,
    sale_proceeds        BIGINT       NOT NULL,
    sale_fees            BIGINT       NOT NULL DEFAULT 0,
    realized_pnl         BIGINT       NOT NULL DEFAULT 0,
    cost_basis_allocated BIGINT       NOT NULL DEFAULT 0,

    INDEX idx_portfolio_stock_disposals_asset (asset_id, disposal_date)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS portfolio_stock_disposal_allocations (
    disposal_id VARCHAR(36)  NOT NULL,
    asset_id    VARCHAR(36)  NOT NULL,
    quantity    VARCHAR(40) NOT NULL,
    cost_basis  BIGINT       NOT NULL,

    PRIMARY KEY (disposal_id, asset_id),
    INDEX idx_portfolio_stock_alloc_asset (asset_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS portfolio_gold_disposals (
    id                   VARCHAR(36)  NOT NULL PRIMARY KEY,
    asset_id             VARCHAR(36)  NOT NULL,
    disposal_date        BIGINT       NOT NULL,
    quantity_grams_sold  VARCHAR(40) NOT NULL,
    sale_proceeds        BIGINT       NOT NULL,
    sale_fees            BIGINT       NOT NULL DEFAULT 0,
    realized_pnl         BIGINT       NOT NULL DEFAULT 0,
    cost_basis_allocated BIGINT       NOT NULL DEFAULT 0,

    INDEX idx_portfolio_gold_disposals_asset (asset_id, disposal_date)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS portfolio_price_observations (
    id               VARCHAR(36)  NOT NULL PRIMARY KEY,
    asset_id         VARCHAR(36)  NOT NULL,
    provider         VARCHAR(32)  NOT NULL DEFAULT 'manual',
    price_side       VARCHAR(16)  NOT NULL DEFAULT 'mid',
    unit_price       BIGINT       NOT NULL,
    currency         VARCHAR(8)   NOT NULL,
    observed_at      BIGINT       NOT NULL,
    source_reference VARCHAR(255) NOT NULL DEFAULT '',
    idempotency_key  VARCHAR(64)  DEFAULT NULL,
    notes            TEXT         DEFAULT NULL,

    UNIQUE KEY uk_portfolio_obs_idempotency (asset_id, idempotency_key),
    INDEX idx_portfolio_obs_asset_date (asset_id, observed_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS portfolio_asset_activities (
    id              VARCHAR(36) NOT NULL PRIMARY KEY,
    asset_id        VARCHAR(36) NOT NULL,
    budget_id       VARCHAR(36) NOT NULL,
    activity_type   VARCHAR(48) NOT NULL,
    actor_user_id   VARCHAR(36) NOT NULL,
    correlation_id  VARCHAR(64) DEFAULT NULL,
    idempotency_key VARCHAR(64) DEFAULT NULL,
    occurred_at     BIGINT      NOT NULL,
    payload_json    TEXT        DEFAULT NULL,
    created_at      BIGINT      NOT NULL,

    UNIQUE KEY uk_portfolio_act_idempotency (asset_id, idempotency_key),
    INDEX idx_portfolio_act_asset_time (asset_id, occurred_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS portfolio_outbox_events (
    id              VARCHAR(36)  NOT NULL PRIMARY KEY,
    event_type      VARCHAR(64)  NOT NULL,
    asset_id        VARCHAR(36)  DEFAULT NULL,
    budget_id       VARCHAR(36)  DEFAULT NULL,
    payload_json    TEXT         NOT NULL,
    enqueued_at     BIGINT       NOT NULL,
    delivered_at    BIGINT       DEFAULT NULL,
    attempts        INT          NOT NULL DEFAULT 0,
    last_error      TEXT         DEFAULT NULL,
    next_attempt_at BIGINT       DEFAULT NULL,

    INDEX idx_portfolio_outbox_pending (delivered_at, next_attempt_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;