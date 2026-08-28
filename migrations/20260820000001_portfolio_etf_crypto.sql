-- Portfolio ETF lots (Phase 5.5)
-- One row per ETF purchase lot. Mirrors the structure of
-- portfolio_stock_lots but for ETFs. Quantity is decimal-string
-- to support fractional units (e.g. 0.5 of a VN30 ETF unit).

CREATE TABLE IF NOT EXISTS portfolio_etf_lots (
    asset_id              VARCHAR(36)  NOT NULL PRIMARY KEY,
    ticker                VARCHAR(20)  NOT NULL,
    exchange              VARCHAR(16)  NOT NULL,
    underlying_index      VARCHAR(32)  NOT NULL,
    fund_provider         VARCHAR(100) NOT NULL,
    quantity_bought       VARCHAR(40)  NOT NULL,
    quantity_open         VARCHAR(40)  NOT NULL,
    buy_price_per_unit    BIGINT       NOT NULL,
    purchase_cost         BIGINT       NOT NULL,
    fees                  BIGINT       NOT NULL DEFAULT 0,
    purchase_date         BIGINT       NOT NULL,
    settlement_date       BIGINT       DEFAULT NULL,
    notes                 TEXT         DEFAULT NULL,

    INDEX idx_portfolio_etf_ticker (ticker, exchange)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Portfolio Crypto lots (Phase 5.5)
-- One row per crypto purchase. `custody_wallet` is a free-form
-- string; Phase 6 may validate by network (BTC bech32, ETH 0x + 40
-- hex, SOL base58). Quantity is decimal-string for sat-precision.

CREATE TABLE IF NOT EXISTS portfolio_crypto_lots (
    asset_id          VARCHAR(36)  NOT NULL PRIMARY KEY,
    symbol            VARCHAR(16)  NOT NULL,
    network           VARCHAR(32)  NOT NULL,
    custody_wallet    VARCHAR(255) NOT NULL,
    quantity_bought   VARCHAR(40)  NOT NULL,
    quantity_open     VARCHAR(40)  NOT NULL,
    buy_price_per_unit BIGINT     NOT NULL,
    purchase_cost     BIGINT       NOT NULL,
    fees              BIGINT       NOT NULL DEFAULT 0,
    purchase_date     BIGINT       NOT NULL,
    notes             TEXT         DEFAULT NULL,

    INDEX idx_portfolio_crypto_symbol (symbol, network)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;