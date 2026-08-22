//! Integration test: outbox currency is read from the asset, not hardcoded VND.
//!
//! Run with: DATABASE_URL=... cargo test --test integration_invest_crud -- --ignored --nocapture

use sqlx::MySqlPool;
use std::sync::Arc;

use budget::manager::biz::BudgetBiz;
use budget::manager::biz::portfolio::biz::PortfolioBiz;
use budget::manager::repository::portfolio::PortfolioRepository;
use budget::converters::portfolio::{NewPortfolioAsset, NewOutboxEvent, AssetClassNew};
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
