-- Backfill portfolio_* tables from legacy invest_assets and
-- invest_price_snapshots. Idempotent: re-running produces no duplicate
-- portfolio_assets rows because we match on (budget_id, legacy_asset_id)
-- and skip rows that already have a portfolio entry.
--
-- Asset class mapping:
--   invest_assets.asset_type = 'savings_deposit' → portfolio_fixed_deposits
--   invest_assets.asset_type = 'gold'           → portfolio_gold_lots
--   invest_assets.asset_type = 'stock'          → portfolio_stock_lots
--
-- Rationale for collapsing savings_deposit into fixed_deposit:
-- the legacy schema did not distinguish between liquid savings
-- accounts and term deposits. After backfill, existing rows land
-- in fixed_deposit with interest_method = simple. Users can create
-- dedicated savings accounts in the new system.
--
-- price snapshots are converted to portfolio_price_observations
-- with provider = snapshot.source and source_reference =
-- snapshot.id (stable identifier).

-- ----------------------------------------------------------------------
-- Fixed deposit backfill
-- ----------------------------------------------------------------------
INSERT INTO portfolio_assets (
    id, budget_id, asset_class, display_name, currency, status,
    opened_on, closed_on, legacy_asset_id, notes,
    created_by, created_at, updated_at, deleted_at
)
SELECT
    UUID(),
    ia.budget_id,
    'fixed_deposit',
    ia.name,
    b.currency,
    CASE ia.status
        WHEN 'matured' THEN 'matured'
        WHEN 'closed'  THEN 'closed'
        WHEN 'archived' THEN 'archived'
        ELSE 'active'
    END,
    COALESCE(ia.start_date, FROM_UNIXTIME(ia.created_at)),
    ia.maturity_date,
    ia.id,
    ia.notes,
    ia.created_by,
    ia.created_at,
    ia.updated_at,
    ia.deleted_at
FROM invest_assets ia
JOIN budgets b ON b.id = ia.budget_id AND b.deleted_at IS NULL
WHERE ia.asset_type = 'savings_deposit'
  AND ia.deleted_at IS NULL
  AND NOT EXISTS (
      SELECT 1 FROM portfolio_assets pa
      WHERE pa.budget_id = ia.budget_id
        AND pa.legacy_asset_id = ia.id
  );

INSERT INTO portfolio_fixed_deposits (
    asset_id, provider, product_name, principal, annual_rate,
    interest_method, payout_type, deposit_date, maturity_date,
    auto_renewal_policy, rollover_from_asset_id,
    certificate_reference_masked, notes
)
SELECT
    pa.id,
    COALESCE(ia.bank_name, ''),
    ia.name,
    COALESCE(ia.principal, 0),
    CAST(COALESCE(ia.annual_rate, 0) AS CHAR),
    COALESCE(ia.interest_type, 'simple'),
    'at_maturity',
    COALESCE(UNIX_TIMESTAMP(ia.start_date), ia.created_at),
    COALESCE(UNIX_TIMESTAMP(ia.maturity_date), ia.created_at),
    'none',
    NULL,
    '',
    ia.notes
FROM invest_assets ia
JOIN portfolio_assets pa
  ON pa.budget_id = ia.budget_id
 AND pa.legacy_asset_id = ia.id
WHERE ia.asset_type = 'savings_deposit'
  AND ia.deleted_at IS NULL
  AND NOT EXISTS (
      SELECT 1 FROM portfolio_fixed_deposits fd
      WHERE fd.asset_id = pa.id
  );

-- ----------------------------------------------------------------------
-- Gold lot backfill
-- ----------------------------------------------------------------------
INSERT INTO portfolio_assets (
    id, budget_id, asset_class, display_name, currency, status,
    opened_on, closed_on, legacy_asset_id, notes,
    created_by, created_at, updated_at, deleted_at
)
SELECT
    UUID(),
    ia.budget_id,
    'gold_lot',
    ia.name,
    b.currency,
    CASE ia.status
        WHEN 'sold' THEN 'sold'
        WHEN 'closed' THEN 'closed'
        WHEN 'archived' THEN 'archived'
        ELSE 'active'
    END,
    COALESCE(UNIX_TIMESTAMP(ia.purchase_date), ia.created_at),
    NULL,
    ia.id,
    ia.notes,
    ia.created_by,
    ia.created_at,
    ia.updated_at,
    ia.deleted_at
FROM invest_assets ia
JOIN budgets b ON b.id = ia.budget_id AND b.deleted_at IS NULL
WHERE ia.asset_type = 'gold'
  AND ia.deleted_at IS NULL
  AND NOT EXISTS (
      SELECT 1 FROM portfolio_assets pa
      WHERE pa.budget_id = ia.budget_id
        AND pa.legacy_asset_id = ia.id
  );

-- quantity is stored as DECIMAL in legacy; map to grams via unit:
--   chi  → * 3.75
--   luong → * 37.5
--   gram → as-is
INSERT INTO portfolio_gold_lots (
    asset_id, provider, gold_type, purity, form,
    quantity_original, unit_original, quantity_grams,
    purchase_price_per_unit_original, purchase_cost, fees,
    purchase_date, notes
)
SELECT
    pa.id,
    '',
    ia.name,
    'other',
    'other',
    CAST(COALESCE(ia.quantity, 0) AS CHAR),
    COALESCE(ia.unit, 'gram'),
    CAST(
        CASE ia.unit
            WHEN 'chi'   THEN COALESCE(ia.quantity, 0) * 3.75
            WHEN 'luong' THEN COALESCE(ia.quantity, 0) * 37.5
            ELSE              COALESCE(ia.quantity, 0)
        END AS CHAR
    ),
    COALESCE(ia.cost_basis_per_unit, 0),
    COALESCE(ia.quantity, 0) * COALESCE(ia.cost_basis_per_unit, 0),
    0,
    COALESCE(UNIX_TIMESTAMP(ia.purchase_date), ia.created_at),
    ia.notes
FROM invest_assets ia
JOIN portfolio_assets pa
  ON pa.budget_id = ia.budget_id
 AND pa.legacy_asset_id = ia.id
WHERE ia.asset_type = 'gold'
  AND ia.deleted_at IS NULL
  AND NOT EXISTS (
      SELECT 1 FROM portfolio_gold_lots gl
      WHERE gl.asset_id = pa.id
  );

-- ----------------------------------------------------------------------
-- Stock lot backfill
-- ----------------------------------------------------------------------
INSERT INTO portfolio_assets (
    id, budget_id, asset_class, display_name, currency, status,
    opened_on, closed_on, legacy_asset_id, notes,
    created_by, created_at, updated_at, deleted_at
)
SELECT
    UUID(),
    ia.budget_id,
    'stock_lot',
    ia.name,
    b.currency,
    CASE ia.status
        WHEN 'sold' THEN 'sold'
        WHEN 'closed' THEN 'closed'
        WHEN 'archived' THEN 'archived'
        ELSE 'active'
    END,
    COALESCE(UNIX_TIMESTAMP(ia.purchase_date), ia.created_at),
    NULL,
    ia.id,
    ia.notes,
    ia.created_by,
    ia.created_at,
    ia.updated_at,
    ia.deleted_at
FROM invest_assets ia
JOIN budgets b ON b.id = ia.budget_id AND b.deleted_at IS NULL
WHERE ia.asset_type = 'stock'
  AND ia.deleted_at IS NULL
  AND NOT EXISTS (
      SELECT 1 FROM portfolio_assets pa
      WHERE pa.budget_id = ia.budget_id
        AND pa.legacy_asset_id = ia.id
  );

INSERT INTO portfolio_stock_lots (
    asset_id, ticker, exchange, quantity_bought, quantity_open,
    buy_price_per_share, purchase_cost, fees,
    purchase_date, settlement_date, notes
)
SELECT
    pa.id,
    COALESCE(ia.ticker, ''),
    COALESCE(ia.exchange, 'HOSE'),
    CAST(COALESCE(ia.quantity, 0) AS CHAR),
    CAST(COALESCE(ia.quantity, 0) AS CHAR),
    COALESCE(ia.avg_cost_per_share, 0),
    COALESCE(ia.quantity, 0) * COALESCE(ia.avg_cost_per_share, 0),
    0,
    COALESCE(UNIX_TIMESTAMP(ia.purchase_date), ia.created_at),
    NULL,
    ia.notes
FROM invest_assets ia
JOIN portfolio_assets pa
  ON pa.budget_id = ia.budget_id
 AND pa.legacy_asset_id = ia.id
WHERE ia.asset_type = 'stock'
  AND ia.deleted_at IS NULL
  AND NOT EXISTS (
      SELECT 1 FROM portfolio_stock_lots sl
      WHERE sl.asset_id = pa.id
  );

-- ----------------------------------------------------------------------
-- Price snapshot backfill
-- ----------------------------------------------------------------------
INSERT INTO portfolio_price_observations (
    id, asset_id, provider, price_side, unit_price, currency,
    observed_at, source_reference, idempotency_key, notes
)
SELECT
    UUID(),
    pa.id,
    ips.source,
    'mid',
    ips.price,
    b.currency,
    UNIX_TIMESTAMP(ips.snapshot_date),
    ips.id,
    CONCAT('legacy:', ips.id),
    ''
FROM invest_price_snapshots ips
JOIN invest_assets ia ON ia.id = ips.asset_id
JOIN portfolio_assets pa
  ON pa.budget_id = ia.budget_id
 AND pa.legacy_asset_id = ia.id
JOIN budgets b ON b.id = ia.budget_id
WHERE ia.deleted_at IS NULL
  AND ia.asset_type IN ('gold', 'stock')
  AND NOT EXISTS (
      SELECT 1 FROM portfolio_price_observations po
      WHERE po.asset_id = pa.id
        AND po.source_reference = ips.id
  );