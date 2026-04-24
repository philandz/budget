-- Invest assets: financial holdings within an Invest budget
CREATE TABLE IF NOT EXISTS invest_assets (
    id                   VARCHAR(36)    NOT NULL PRIMARY KEY,
    budget_id            VARCHAR(36)    NOT NULL,
    asset_type           VARCHAR(20)    NOT NULL COMMENT 'savings_deposit | gold | stock',
    name                 VARCHAR(255)   NOT NULL,
    status               VARCHAR(20)    NOT NULL DEFAULT 'active' COMMENT 'active | matured | sold | closed',

    -- Savings deposit fields
    principal            BIGINT,
    annual_rate          DECIMAL(6,4),
    interest_type        VARCHAR(10),                    -- simple | compound
    start_date           DATE,
    maturity_date        DATE,
    bank_name            VARCHAR(100),

    -- Gold fields
    quantity             DECIMAL(12,4),
    unit                 VARCHAR(10),                    -- chi | luong | gram
    cost_basis_per_unit  BIGINT,

    -- Stock fields
    ticker               VARCHAR(20),
    exchange             VARCHAR(10),                    -- HOSE | HNX | UPCOM
    avg_cost_per_share   BIGINT,

    -- Shared
    purchase_date        DATE,
    notes                TEXT,
    created_by           VARCHAR(36)    NOT NULL,
    created_at           BIGINT         NOT NULL,
    updated_at           BIGINT         NOT NULL,
    deleted_at           BIGINT                  DEFAULT NULL,

    INDEX idx_invest_assets_budget (budget_id),
    INDEX idx_invest_assets_status (status)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Price snapshots for gold and stock assets
CREATE TABLE IF NOT EXISTS invest_price_snapshots (
    id            VARCHAR(36)  NOT NULL PRIMARY KEY,
    asset_id      VARCHAR(36)  NOT NULL,
    price         BIGINT       NOT NULL,
    source        VARCHAR(20)  NOT NULL DEFAULT 'manual' COMMENT 'manual | auto_sjc | auto_tcbs',
    snapshot_date DATE         NOT NULL,
    created_at    BIGINT       NOT NULL,

    UNIQUE KEY uk_asset_date (asset_id, snapshot_date),
    INDEX idx_snapshots_asset (asset_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
