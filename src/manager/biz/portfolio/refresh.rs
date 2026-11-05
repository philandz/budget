//! Scheduled refresh job for portfolio price observations.
//!
//! Wakes every `PORTFOLIO_REFRESH_INTERVAL_SECS` (default 900 = 15
//! minutes), fetches all active gold and stock assets, runs the
//! provider registry, and inserts the returned prices into
//! `portfolio_price_observations`. Failures are logged and skipped;
//! the job never panics.
//!
//! The job is spawned by `main.rs` once at startup. Toggling providers
//! requires a service restart (env-var gated).

use std::sync::Arc;
use std::time::Duration;

use tokio::time::{interval, MissedTickBehavior};

use crate::converters::portfolio::{map_gold_lot, map_portfolio_asset, map_stock_lot};
use crate::manager::biz::portfolio::providers::{
    DojiProvider, HnxProvider, HoseProvider, PnjProvider, SharedProviderRegistry, SjcProvider,
    UpcomProvider,
};
use crate::manager::repository::portfolio::PortfolioRepository;
use crate::pb::service::portfolio as pb;

pub struct RefreshJob {
    pub repo: Arc<PortfolioRepository>,
    pub registry: SharedProviderRegistry,
    pub interval_secs: u64,
}

impl RefreshJob {
    pub fn new(repo: Arc<PortfolioRepository>) -> Self {
        let interval_secs = std::env::var("PORTFOLIO_REFRESH_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(900);
        let registry = build_registry();
        Self {
            repo,
            registry,
            interval_secs,
        }
    }

    /// Run the refresh loop forever. Each tick:
    /// 1. List all non-deleted gold and stock assets in the database.
    /// 2. Group by `asset_class`.
    /// 3. Call `registry.fetch_all` with the asset list.
    /// 4. Insert returned prices via `repo.insert_price_observation`.
    ///
    /// Actor model: refresh inserts `portfolio_price_observations` rows
    /// with `provider = "auto"`, `source_reference` empty, and an
    /// idempotency key derived from `{provider, observed_at, asset_id}`.
    /// No `portfolio_asset_activities` row is written by the refresh
    /// path. Audit trail of refresh events lives in the standard
    /// application logs (`tracing::info!`). A future Phase 4 enhancement
    /// can write a `system:refresh` activity row for full traceability.
    pub async fn run(self) {
        let mut ticker = interval(Duration::from_secs(self.interval_secs));
        // Skip missed ticks: if a single tick takes longer than the
        // configured interval, do not burst-fire. The next tick after
        // the current one resumes at the next interval boundary.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // Skip the first immediate tick so startup isn't blocked by
        // a network call before other components are ready.
        ticker.tick().await;

        let http = match reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("refresh: failed to build http client: {e}");
                return;
            }
        };

        loop {
            ticker.tick().await;
            if let Err(e) = self.tick(&http).await {
                tracing::warn!("refresh tick failed: {e}");
            }
        }
    }

    pub async fn tick(&self, http: &reqwest::Client) -> anyhow::Result<()> {
        let mut tx = self.repo.begin().await?;
        let assets = self.repo.list_active_for_refresh(&mut tx).await?;
        tx.commit().await?;
        if assets.is_empty() {
            tracing::debug!("refresh: no active gold/stock assets");
            return Ok(());
        }
        let count = assets.len();
        tracing::info!("refresh: fetching prices for {count} assets");
        // Build asset_id → currency lookup so price observations record
        // the asset's native currency instead of hardcoding "VND".
        // Must be built before assets is moved into build_proto_assets.
        let asset_currency: std::collections::HashMap<String, String> = assets
            .iter()
            .map(|a| (a.id.clone(), a.currency.clone()))
            .collect();
        let proto_assets = self.build_proto_assets(assets).await?;
        let prices = self.registry.fetch_all(http, &proto_assets).await;
        if prices.is_empty() {
            tracing::debug!("refresh: no prices returned (no providers match)");
            return Ok(());
        }

        let mut tx = self.repo.begin().await?;
        let mut inserted = 0_usize;
        for price in &prices {
            let now = philand_time::now_unix();
            let obs = crate::converters::portfolio::NewPriceObservation {
                id: None,
                asset_id: price.asset_id.clone(),
                provider: price.provider.clone(),
                price_side: convert_price_side(price.price_side),
                unit_price: price.unit_price,
                currency: asset_currency
                    .get(price.asset_id.as_str())
                    .cloned()
                    .unwrap_or_else(|| "VND".to_string()),
                observed_at: now,
                source_reference: price.source_reference.clone(),
                idempotency_key: Some(format!(
                    "auto:{}:{}:{}",
                    price.provider, now, price.asset_id
                )),
                notes: if price.notes.is_empty() {
                    None
                } else {
                    Some(price.notes.clone())
                },
            };
            match self.repo.insert_price_observation(&mut tx, &obs).await {
                Ok(_) => inserted += 1,
                Err(e) => {
                    // Idempotency dedup may fire on re-runs within the
                    // same second; treat as success.
                    let msg = e.to_string();
                    if msg.contains("duplicate") {
                        continue;
                    }
                    tracing::warn!("refresh: insert failed for asset {}: {e}", price.asset_id);
                }
            }
        }
        tx.commit().await?;
        tracing::info!("refresh: inserted {inserted}/{count} price observations");
        Ok(())
    }

    /// Convert database assets to proto, enriching gold and stock assets
    /// with their lot details so providers can evaluate `supports()` correctly.
    async fn build_proto_assets(
        &self,
        assets: Vec<crate::converters::portfolio::DbPortfolioAsset>,
    ) -> anyhow::Result<Vec<pb::PortfolioAsset>> {
        let mut tx = self.repo.begin().await?;
        let mut proto_assets = Vec::with_capacity(assets.len());
        for asset in &assets {
            let mut proto = map_portfolio_asset(asset.clone());
            match asset.asset_class.as_str() {
                "gold_lot" => {
                    if let Ok(Some(lot)) = self.repo.get_gold_lot(&mut tx, &asset.id).await {
                        proto.details =
                            Some(pb::portfolio_asset::Details::GoldLot(map_gold_lot(lot)));
                    }
                }
                "stock_lot" => {
                    if let Ok(Some(lot)) = self.repo.get_stock_lot(&mut tx, &asset.id).await {
                        proto.details =
                            Some(pb::portfolio_asset::Details::StockLot(map_stock_lot(lot)));
                    }
                }
                _ => {}
            }
            proto_assets.push(proto);
        }
        tx.commit().await?;
        Ok(proto_assets)
    }
}

/// Build the shared registry. Starts with `NoopProvider` always; adds
/// real providers when their env flag is set.
fn build_registry() -> SharedProviderRegistry {
    let registry = SharedProviderRegistry::new_with_noop();
    use std::sync::Arc;
    if env_flag("PORTFOLIO_ENABLE_SJC") {
        registry.add(Arc::new(SjcProvider::new()));
    }
    if env_flag("PORTFOLIO_ENABLE_DOJI") {
        registry.add(Arc::new(DojiProvider::new()));
    }
    if env_flag("PORTFOLIO_ENABLE_PNJ") {
        registry.add(Arc::new(PnjProvider::new()));
    }
    if env_flag("PORTFOLIO_ENABLE_HOSE") {
        registry.add(Arc::new(HoseProvider::new()));
    }
    if env_flag("PORTFOLIO_ENABLE_HNX") {
        registry.add(Arc::new(HnxProvider::new()));
    }
    if env_flag("PORTFOLIO_ENABLE_UPCOM") {
        registry.add(Arc::new(UpcomProvider::new()));
    }
    registry
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn convert_price_side(s: &str) -> crate::manager::biz::portfolio::PriceSide {
    use crate::manager::biz::portfolio::PriceSide;
    match s {
        "bid" => PriceSide::Bid,
        "ask" => PriceSide::Ask,
        _ => PriceSide::Mid,
    }
}

/// Convenience trait that gives a `&'static str` name for the
/// ProviderPrice. We can't add methods to the struct in this module
/// without orphan rules, so use a free impl.
#[allow(dead_code)]
trait ProviderPriceExt {
    fn provider_label(&self) -> &str;
}

impl ProviderPriceExt for crate::manager::biz::portfolio::providers::ProviderPrice {
    fn provider_label(&self) -> &str {
        // Provider name is not currently stored on ProviderPrice; the
        // source_reference carries it. For auto-refresh the
        // idempotency_key encodes the provider via "auto:{provider}:..."
        // so this default label is sufficient. Real provider labels
        // can be added by extending ProviderPrice with a name field.
        // Note: The label is always "auto" for price observations.
        "auto"
    }
}
