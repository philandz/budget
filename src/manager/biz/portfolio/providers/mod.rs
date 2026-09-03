//! Price provider trait and registry.
//!
//! Each provider is a `dyn PriceProvider` that knows how to fetch latest
//! prices for a subset of assets (gold bars, HOSE stocks, etc.). Phase 2
//! starts with a noop provider so the rest of the system can wire
//! without requiring real adapters.

pub mod doji;
pub mod hnx;
pub mod hose;
pub mod noop;
pub mod pnj;
pub mod registry;
pub mod sjc;
pub mod upcom;

pub use doji::DojiProvider;
pub use hnx::HnxProvider;
pub use hose::HoseProvider;
pub use noop::NoopProvider;
pub use pnj::PnjProvider;
pub use registry::{ProviderRegistry, SharedProviderRegistry};
pub use sjc::SjcProvider;
pub use upcom::UpcomProvider;

use std::future::Future;
use std::pin::Pin;

use crate::pb::service::portfolio as pb;

/// One price observation produced by a provider.
#[derive(Debug, Clone)]
pub struct ProviderPrice {
    /// Asset id this observation belongs to.
    pub asset_id: String,
    /// Provider name (e.g. "sjc", "hose"). Persisted in
    /// `portfolio_price_observations.provider` so audit trail keeps
    /// the data source.
    pub provider: String,
    /// Unit price in the asset's currency, in minor units.
    pub unit_price: i64,
    /// Side: bid / ask / mid. Persisted as DB string in the price
    /// observations table.
    pub price_side: &'static str,
    /// Optional source reference (URL, batch id, etc.).
    pub source_reference: String,
    /// Optional free-form note.
    pub notes: String,
}

/// Future alias for async provider methods.
pub type PriceFuture<'a> =
    Pin<Box<dyn Future<Output = anyhow::Result<Vec<ProviderPrice>>> + Send + 'a>>;

/// Trait implemented by every price provider. Phase 1 is manual only
/// so the `fetch` method can return Ok(vec![]). Phase 2 adapters return
/// live observations.
pub trait PriceProvider: Send + Sync {
    /// Stable identifier for logs and config (e.g. "sjc", "tcbs").
    fn name(&self) -> &'static str;

    /// `true` if this provider claims responsibility for the given
    /// asset. Implementations match by `asset_class` and asset fields
    /// (ticker, provider, purity, ...).
    fn supports(&self, asset: &pb::PortfolioAsset) -> bool;

    /// Fetch latest prices for the supplied assets. Returns an empty
    /// vec when the provider has nothing to report. Implementations
    /// should not return `Err` for transient network errors; the
    /// registry retries on its own schedule.
    fn fetch<'a>(
        &'a self,
        client: &'a reqwest::Client,
        assets: &'a [pb::PortfolioAsset],
    ) -> PriceFuture<'a>;
}
