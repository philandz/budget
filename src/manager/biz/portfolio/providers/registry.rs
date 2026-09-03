//! Provider registry. Holds a list of `dyn PriceProvider` and routes
//! `fetch_all` calls to the right provider per asset.

use std::sync::{Arc, RwLock};

use crate::pb::service::portfolio as pb;

use super::noop::NoopProvider;
use super::{PriceFuture, PriceProvider};

/// Thread-safe handle to the registry. Cheap to clone.
#[derive(Clone)]
pub struct SharedProviderRegistry {
    inner: Arc<RwLock<ProviderRegistry>>,
}

impl SharedProviderRegistry {
    /// Create a new registry that contains the noop provider only.
    pub fn new_with_noop() -> Self {
        let mut reg = ProviderRegistry::default();
        reg.add(Arc::new(NoopProvider::new()));
        Self {
            inner: Arc::new(RwLock::new(reg)),
        }
    }

    /// Add a new provider at runtime (e.g. from a config flag).
    ///
    /// Recovers from a poisoned lock by extracting the inner data; a
    /// panic inside a previous holder should not brick subsequent
    /// reads/writes.
    pub fn add(&self, provider: Arc<dyn PriceProvider>) {
        match self.inner.write() {
            Ok(mut guard) => guard.add(provider),
            Err(poisoned) => poisoned.into_inner().add(provider),
        }
    }

    /// `true` when the only registered provider is `NoopProvider`.
    /// Used by the scheduled refresh job to short-circuit ticks that
    /// would otherwise emit "no prices returned" every interval.
    pub fn is_noop_only(&self) -> bool {
        let snapshot = match self.inner.read() {
            Ok(guard) => guard.providers(),
            Err(poisoned) => poisoned.into_inner().providers(),
        };
        snapshot.len() == 1 && snapshot[0].name() == "noop"
    }

    /// Iterate every provider and collect all returned prices.
    pub async fn fetch_all(
        &self,
        client: &reqwest::Client,
        assets: &[pb::PortfolioAsset],
    ) -> Vec<super::ProviderPrice> {
        let snapshot: Vec<Arc<dyn PriceProvider>> = match self.inner.read() {
            Ok(guard) => guard.providers.clone(),
            Err(poisoned) => poisoned.into_inner().providers.clone(),
        };
        let mut out = Vec::new();
        for provider in snapshot {
            let supported: Vec<pb::PortfolioAsset> = assets
                .iter()
                .filter(|a| provider.supports(a))
                .cloned()
                .collect();
            if supported.is_empty() {
                continue;
            }
            match provider.fetch(client, &supported).await {
                Ok(prices) => out.extend(prices),
                Err(e) => {
                    tracing::warn!(provider = provider.name(), "price provider failed: {e}");
                }
            }
        }
        out
    }
}

#[derive(Default)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn PriceProvider>>,
}

impl ProviderRegistry {
    pub fn add(&mut self, provider: Arc<dyn PriceProvider>) {
        self.providers.push(provider);
    }

    /// Snapshot the current list of providers. Cheap; used by the
    /// shared handle to release the read lock before invoking async
    /// provider methods.
    pub fn providers(&self) -> Vec<Arc<dyn PriceProvider>> {
        self.providers.clone()
    }
}

/// Convenience: `PriceFuture<'a>` re-export for callers.
pub type ProviderFetchFuture<'a> = PriceFuture<'a>;
