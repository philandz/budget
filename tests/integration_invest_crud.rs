//! Integration test: outbox currency is read from the asset, not hardcoded VND.
//!
//! Run with: DATABASE_URL=... cargo test --test integration_invest_crud -- --ignored --nocapture

use sqlx::MySqlPool;
use std::sync::Arc;

use budget::manager::biz::BudgetBiz;
use budget::manager::biz::portfolio::biz::PortfolioBiz;
use budget::manager::repository::portfolio::PortfolioRepository;
use budget::converters::portfolio::{NewPortfolioAsset, NewOutboxEvent, AssetClassNew, NewPriceObservation, NewGoldLot, NewStockLot, NewCryptoLot, DbPortfolioAsset};
use budget::manager::biz::portfolio::PriceSide;
use budget::pb::service::budget::{AssetType, BudgetType};
use async_trait::async_trait;
use philand_notify::{Mailer, MailMessage, MailReceipt, MailerError};
use philand_time::now_unix;

/// A mailer that captures the last message so we can assert on it.
struct CapturingMailer {
    inner: philand_notify::NoopMailer,
    last: std::sync::Mutex<Option<MailMessage>>,
}

impl CapturingMailer {
    fn new() -> Self {
        Self {
            inner: philand_notify::NoopMailer::new(),
            last: std::sync::Mutex::new(None),
        }
    }
    fn take(&self) -> Option<MailMessage> {
        self.last.lock().unwrap().take()
    }
}

#[async_trait]
impl Mailer for CapturingMailer {
    fn provider_name(&self) -> &'static str { "capturing" }

    async fn send(&self, msg: MailMessage) -> Result<MailReceipt, MailerError> {
        let _ = self.last.lock().unwrap().insert(msg.clone());
        self.inner.send(msg).await
    }
}

/// Seed a portfolio fixed-deposit asset with the given currency, returning its id.
async fn seed_asset(pool: &MySqlPool, currency: &str) -> String {
    let repo = PortfolioRepository::new(pool.clone());
    let now = now_unix();
    let asset = repo.insert_and_read_asset(NewPortfolioAsset {
        id: None,
        budget_id: "budget-test-1".to_string(),
        asset_class: AssetClassNew::FixedDeposit,
        display_name: "Test FD".to_string(),
        currency: currency.to_string(),
        opened_on: now,
        closed_on: None,
        legacy_asset_id: None,
        notes: None,
        created_by: "test-user".to_string(),
    }).await.expect("seed_asset failed");
    asset.id
}

/// Insert a MATURITY_REACHED outbox event for the given asset.
async fn trigger_outbox_event(pool: &MySqlPool, asset_id: &str) {
    let repo = PortfolioRepository::new(pool.clone());
    let mut tx = repo.begin().await.expect("begin tx");
    let evt = NewOutboxEvent {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: "MATURITY_REACHED".to_string(),
        asset_id: Some(asset_id.to_string()),
        budget_id: Some("budget-test-1".to_string()),
        payload_json: format!(
            r#"{{"asset_id":"{}","budget_id":"budget-test-1","principal":1000000,"display_name":"Test FD","maturity_date":{}}}"#,
            asset_id,
            now_unix()
        ),
        enqueued_at: now_unix(),
    };
    repo.insert_outbox(&mut tx, &evt).await.expect("insert_outbox failed");
    tx.commit().await.expect("commit failed");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; run with `--ignored`"]
async fn outbox_uses_asset_currency_not_hardcoded_vnd() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for --ignored tests");
    let pool = MySqlPool::connect(&url).await.expect("connect");

    // Run migrations first so the schema exists.
    {
        let mut m = sqlx::migrate!("./migrations");
        m.set_ignore_missing(true);
        if let Err(e) = m.run(&pool).await {
            let msg = e.to_string();
            let ignorable = msg.contains("VersionMismatch")
                || msg.contains("partially applied")
                || msg.contains("Duplicate column name")
                || msg.contains("Duplicate key name")
                || msg.contains("already exists");
            if !ignorable {
                eprintln!("migration error (will try to continue): {msg}");
            }
        }
    }

    // Set up the mailer capture and PortfolioBiz.
    let capturing = Arc::new(CapturingMailer::new());
    let mailer: Arc<dyn Mailer> = capturing.clone() as Arc<dyn Mailer>;
    let budget_biz = BudgetBiz::test_only_no_clients().await;
    let pbiz = PortfolioBiz::test_only_no_clients(Arc::new(budget_biz)).await;

    // Seed a USD asset and trigger an outbox event.
    let asset_id = seed_asset(&pool, "USD").await;
    trigger_outbox_event(&pool, &asset_id).await;

    // Run one drainer pass.
    pbiz.drain_outbox_once(&mailer, None, 50, 5)
        .await
        .expect("drain_outbox_once failed");

    // Check the captured email.
    let msg = capturing.take().expect("no email was sent");
    // The rendered email for MATURITY_REACHED includes the currency in the
    // subject or body.  Check the subject contains USD (not hardcoded VND).
    assert!(
        msg.subject.contains("USD") || msg.text.contains("USD"),
        "outbox must use asset.currency (USD), not hardcoded VND. subject={} text={}",
        msg.subject,
        msg.text
    );
}

// ---------------------------------------------------------------------------
// Invest asset CRUD round-trip helpers
// ---------------------------------------------------------------------------

/// Builds a BudgetRepository connected to DATABASE_URL. Each helper creates
/// its own instance so the repo stays confined to one task (BudgetRepository
/// is !Sync).
async fn make_repo() -> budget::manager::repository::BudgetRepository {
    // Unwrap is safe: default_for_tests() always succeeds (no I/O at construction).
    let config = philand_configs::BudgetServiceConfig::default_for_tests();
    budget::manager::repository::BudgetRepository::new(&config)
        .await
        .expect("BudgetRepository::new failed — check DATABASE_URL")
}

/// Create a standard budget and add the creator as owner. Returns the budget id.
async fn seed_budget(_pool: &MySqlPool) -> String {
    let repo = make_repo().await;
    let budget = repo
        .create_budget("test-org", "CRUD Round-Trip Test Budget", BudgetType::Standard, "VND", "test-user")
        .await
        .expect("seed_budget failed");
    budget.id
}

/// Insert an invest asset linked to the given budget. Returns the created DbInvestAsset.
async fn create_asset(
    _pool: &MySqlPool,
    budget_id: String,
    name: &str,
    asset_type: AssetType,
    quantity: f64,
) -> budget::converters::DbInvestAsset {
    let repo = make_repo().await;
    repo.create_invest_asset(
        &budget_id,
        budget::converters::asset_type_to_db(asset_type),
        name,
        "test-user",
        Some(10_000_000),        // principal: 10M VND
        Some(0.05),              // annual_rate: 5%
        Some("simple"),         // interest_type
        Some("2024-01-01"),     // start_date
        Some("2025-01-01"),     // maturity_date
        Some("Test Bank"),      // bank_name
        Some(quantity),          // quantity
        Some("shares"),          // unit
        Some(100_000),           // cost_basis_per_unit: 100k VND/share
        Some("AAPL"),           // ticker
        Some("NASDAQ"),         // exchange
        Some(150_000),          // avg_cost_per_share: 150k VND
        Some("2024-01-15"),     // purchase_date
        Some("Test notes"),     // notes
    )
    .await
    .expect("create_asset failed")
}

/// Read an invest asset by id.
async fn get_asset(_pool: &MySqlPool, asset_id: String) -> budget::converters::DbInvestAsset {
    let repo = make_repo().await;
    repo.get_invest_asset(&asset_id)
        .await
        .expect("get_asset failed")
}

/// Update an invest asset's quantity and return the updated row.
async fn update_asset(
    _pool: &MySqlPool,
    asset_id: String,
    _field: &str,
    new_quantity: f64,
) -> budget::converters::DbInvestAsset {
    let repo = make_repo().await;
    repo.update_invest_asset(
        &asset_id,
        None,                   // name: unchanged
        None,                   // annual_rate: unchanged
        None,                   // maturity_date: unchanged
        None,                   // bank_name: unchanged
        Some(new_quantity),     // quantity: updated
        None,                   // unit: unchanged
        None,                   // cost_basis_per_unit: unchanged
        None,                   // avg_cost_per_share: unchanged
        None,                   // notes: unchanged
    )
    .await
    .expect("update_asset failed")
}

/// List all non-deleted invest assets for a budget.
async fn list_assets(
    _pool: &MySqlPool,
    budget_id: String,
) -> Vec<budget::converters::DbInvestAsset> {
    let repo = make_repo().await;
    repo.list_invest_assets(&budget_id)
        .await
        .expect("list_assets failed")
}

/// Soft-delete an invest asset.
async fn delete_asset(_pool: &MySqlPool, asset_id: String) {
    let repo = make_repo().await;
    repo.delete_invest_asset(&asset_id)
        .await
        .expect("delete_asset failed")
}

// ---------------------------------------------------------------------------
// Invest asset CRUD round-trip test
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires DATABASE_URL; run with `--ignored`"]
async fn invest_crud_round_trip() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for --ignored tests");
    let pool = MySqlPool::connect(&url).await.expect("connect");

    // Run migrations so the schema exists.
    {
        let mut m = sqlx::migrate!("./migrations");
        m.set_ignore_missing(true);
        if let Err(e) = m.run(&pool).await {
            let msg = e.to_string();
            let ignorable = msg.contains("VersionMismatch")
                || msg.contains("partially applied")
                || msg.contains("Duplicate column name")
                || msg.contains("Duplicate key name")
                || msg.contains("already exists");
            if !ignorable {
                eprintln!("migration error (will try to continue): {msg}");
            }
        }
    }

    // Seed a budget (required for FK constraint on invest_assets.budget_id).
    let budget_id = seed_budget(&pool).await;

    // Create — insert a stock investment.
    let asset = create_asset(&pool, budget_id.clone(), "AAPL", AssetType::Stock, 10.0).await;
    assert_eq!(asset.name, "AAPL", "created asset name must match");
    assert_eq!(asset.budget_id, budget_id, "asset must belong to seeded budget");

    // Read — fetch the asset by id and verify it matches.
    let got = get_asset(&pool, asset.id.clone()).await;
    assert_eq!(got.id, asset.id, "get must return the created asset");
    assert_eq!(got.name, "AAPL", "name must be AAPL");
    assert_eq!(got.quantity, Some(10.0), "initial quantity must be 10");

    // Update — change quantity from 10 to 15.
    let updated = update_asset(&pool, asset.id.clone(), "quantity", 15.0).await;
    assert_eq!(
        updated.quantity,
        Some(15.0),
        "updated quantity must be 15"
    );

    // List — budget should now have 1 asset.
    let list = list_assets(&pool, budget_id.clone()).await;
    assert_eq!(list.len(), 1, "list must return exactly 1 asset after create");
    assert_eq!(list[0].id, asset.id, "listed asset must be our created asset");

    // Delete — soft-delete the asset.
    delete_asset(&pool, asset.id.clone()).await;

    // List again — budget should now have 0 assets.
    let after = list_assets(&pool, budget_id.clone()).await;
    assert!(
        after.is_empty(),
        "list must be empty after delete, but got {} assets",
        after.len()
    );
}

// ---------------------------------------------------------------------------
// Portfolio multi-currency helpers
// ---------------------------------------------------------------------------

/// Insert a portfolio asset (root row only) and return the read-back DbPortfolioAsset.
/// The caller is responsible for inserting the corresponding subtype row
/// (gold_lots / stock_lots / crypto_lots) if the asset class is priceable.
async fn seed_portfolio_asset(
    pool: &sqlx::MySqlPool,
    budget_id: String,
    asset_class: AssetClassNew,
    display_name: &str,
    currency: &str,
) -> DbPortfolioAsset {
    let repo = PortfolioRepository::new(pool.clone());
    let now = now_unix();
    repo.insert_and_read_asset(NewPortfolioAsset {
        id: None,
        budget_id,
        asset_class,
        display_name: display_name.to_string(),
        currency: currency.to_string(),
        opened_on: now,
        closed_on: None,
        legacy_asset_id: None,
        notes: None,
        created_by: "test-user".to_string(),
    })
    .await
    .expect("seed_portfolio_asset failed")
}

/// Insert a portfolio_gold_lots row for the given asset_id.
async fn seed_portfolio_gold_lot(
    pool: &sqlx::MySqlPool,
    asset_id: &str,
    quantity_original: &str,
    purchase_price_per_unit_original: i64,
    purchase_cost: i64,
) {
    let repo = PortfolioRepository::new(pool.clone());
    let mut tx = repo.begin().await.expect("begin tx");
    repo.insert_gold_lot(&mut tx, asset_id, &NewGoldLot {
        provider: "TEST".to_string(),
        gold_type: "sjc_9999".to_string(),
        purity: "sjc_9999".to_string(),
        form: "bar".to_string(),
        quantity_original: quantity_original.to_string(),
        unit: budget::manager::biz::portfolio::gold::GoldUnit::Chi,
        purchase_price_per_unit_original,
        purchase_cost,
        fees: 0,
        purchase_date: now_unix(),
        notes: None,
    })
    .await
    .expect("insert_gold_lot failed");
    tx.commit().await.expect("commit failed");
}

/// Insert a portfolio_stock_lots row for the given asset_id.
async fn seed_portfolio_stock_lot(
    pool: &sqlx::MySqlPool,
    asset_id: &str,
    ticker: &str,
    exchange: &str,
    quantity_bought: &str,
    buy_price_per_share: i64,
    purchase_cost: i64,
) {
    let repo = PortfolioRepository::new(pool.clone());
    let mut tx = repo.begin().await.expect("begin tx");
    repo.insert_stock_lot(&mut tx, asset_id, &NewStockLot {
        ticker: ticker.to_string(),
        exchange: exchange.to_string(),
        quantity_bought: quantity_bought.to_string(),
        buy_price_per_share,
        purchase_cost,
        fees: 0,
        purchase_date: now_unix(),
        settlement_date: None,
        notes: None,
    })
    .await
    .expect("insert_stock_lot failed");
    tx.commit().await.expect("commit failed");
}

/// Insert a portfolio_crypto_lots row for the given asset_id.
async fn seed_portfolio_crypto_lot(
    pool: &sqlx::MySqlPool,
    asset_id: &str,
    symbol: &str,
    network: &str,
    quantity_bought: &str,
    buy_price_per_unit: i64,
    purchase_cost: i64,
) {
    let repo = PortfolioRepository::new(pool.clone());
    let mut tx = repo.begin().await.expect("begin tx");
    repo.insert_crypto_lot(&mut tx, asset_id, &NewCryptoLot {
        symbol: symbol.to_string(),
        network: network.to_string(),
        custody_wallet: "test-wallet".to_string(),
        quantity_bought: quantity_bought.to_string(),
        quantity_open: quantity_bought.to_string(),
        buy_price_per_unit,
        purchase_cost,
        fees: 0,
        purchase_date: now_unix(),
        notes: None,
    })
    .await
    .expect("insert_crypto_lot failed");
    tx.commit().await.expect("commit failed");
}

/// Insert a price observation for the given portfolio asset.
/// unit_price is in the asset's native currency (stored as-is in the DB).
async fn seed_price(
    pool: &sqlx::MySqlPool,
    asset_id: String,
    unit_price: i64,
    currency: &str,
) {
    let repo = PortfolioRepository::new(pool.clone());
    let mut tx = repo.begin().await.expect("begin tx");
    let now = now_unix();
    let idempotency_key = format!("manual:{now}:{asset_id}");
    repo.insert_price_observation(&mut tx, &NewPriceObservation {
        id: None,
        asset_id,
        provider: "manual".to_string(),
        price_side: PriceSide::Mid,
        unit_price,
        currency: currency.to_string(),
        observed_at: now,
        source_reference: "".to_string(),
        idempotency_key: Some(idempotency_key),
        notes: None,
    })
    .await
    .expect("insert_price_observation failed");
    tx.commit().await.expect("commit failed");
}

// ---------------------------------------------------------------------------
// Portfolio multi-currency lifecycle test
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires DATABASE_URL; run with `--ignored`"]
async fn portfolio_multi_currency_lifecycle() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for --ignored tests");
    let pool = MySqlPool::connect(&url).await.expect("connect");

    // Run migrations so the portfolio schema exists.
    {
        let mut m = sqlx::migrate!("./migrations");
        m.set_ignore_missing(true);
        if let Err(e) = m.run(&pool).await {
            let msg = e.to_string();
            let ignorable = msg.contains("VersionMismatch")
                || msg.contains("partially applied")
                || msg.contains("Duplicate column name")
                || msg.contains("Duplicate key name")
                || msg.contains("already exists");
            if !ignorable {
                eprintln!("migration error (will try to continue): {msg}");
            }
        }
    }

    // Set up business-layer objects (test-only, no real clients).
    let budget_biz = BudgetBiz::test_only_no_clients().await;
    let pbiz = PortfolioBiz::test_only_no_clients(Arc::new(budget_biz)).await;
    let repo_pool = pbiz.repo.pool().clone();

    // Seed a legacy budget (provides budget_id for portfolio assets).
    let budget_id = seed_budget(&pool).await;

    // --- GOLD (VND) ---
    let gold = seed_portfolio_asset(
        &repo_pool,
        budget_id.clone(),
        AssetClassNew::GoldLot,
        "Test Gold",
        "VND",
    )
    .await;
    // Gold lot: 100 chi @ 7,500,000 VND/chi → current_value = 100 * 7_500_000 = 750_000_000
    seed_portfolio_gold_lot(&repo_pool, &gold.id, "100", 7_500_000, 750_000_000).await;
    seed_price(&repo_pool, gold.id.clone(), 7_500_000, "VND").await;

    // --- STOCK AAPL (USD) ---
    let stock = seed_portfolio_asset(
        &repo_pool,
        budget_id.clone(),
        AssetClassNew::StockLot,
        "AAPL",
        "USD",
    )
    .await;
    // Stock lot: 10 shares @ 150 USD/share → current_value = 10 * 150_00 = 150_000 (stored as cents)
    seed_portfolio_stock_lot(&repo_pool, &stock.id, "AAPL", "NASDAQ", "10", 150_00, 150_000).await;
    seed_price(&repo_pool, stock.id.clone(), 150_00, "USD").await;

    // --- CRYPTO BTC (USD) ---
    let btc = seed_portfolio_asset(
        &repo_pool,
        budget_id.clone(),
        AssetClassNew::CryptoLot,
        "BTC",
        "USD",
    )
    .await;
    // Crypto lot: 1 BTC @ 60,000 USD → current_value = 1 * 6000000000 = 6_000_000_000
    seed_portfolio_crypto_lot(&repo_pool, &btc.id, "BTC", "bitcoin", "1", 6_000_000_000, 6_000_000_000).await;
    seed_price(&repo_pool, btc.id.clone(), 6_000_000_000, "USD").await;

    // Trigger price refresh: call get_portfolio_summary which recomputes valuations
    // using the latest price observations for each asset.
    let summary = pbiz
        .get_portfolio_summary("test-user", &budget_id, None)
        .await
        .expect("get_portfolio_summary failed");

    // Verify the summary contains all 3 assets and a positive total value.
    // Note: prices are stored per-asset in their native currency; the summary
    // sums them as-is (no FX conversion). The total reflects the raw sum of
    // each asset's quantity × unit_price in its own currency.
    assert!(
        summary.total_current_value > 0,
        "total_current_value must be positive; got {}",
        summary.total_current_value
    );
    assert_eq!(
        summary.assets.len(),
        3,
        "summary must contain exactly 3 valuated assets"
    );
}
