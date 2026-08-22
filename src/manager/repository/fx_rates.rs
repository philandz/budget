//! FX rate repository — reads from `portfolio_fx_rates`.
//!
//! All methods are cold-path (called only at startup / cache refresh).

use anyhow::Result;
use sqlx::MySqlPool;

/// DB row shape for `portfolio_fx_rates`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbFxRate {
    pub id: String,
    pub from_currency: String,
    pub to_currency: String,
    pub rate: i64,
    pub effective_date: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Clone)]
pub struct FxRateRepository {
    pool: MySqlPool,
}

impl FxRateRepository {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }

    /// Returns the most recent active rate for the given pair whose
    /// effective_date is on or before `as_of_date`.
    pub async fn find_latest(
        &self,
        from_currency: &str,
        to_currency: &str,
        as_of_date: &str,
    ) -> Result<Option<DbFxRate>> {
        let row = sqlx::query_as::<_, DbFxRate>(
            r#"SELECT id, from_currency, to_currency, rate, effective_date,
                      created_at, updated_at, deleted_at
               FROM portfolio_fx_rates
               WHERE from_currency   = ?
                 AND to_currency     = ?
                 AND effective_date <= ?
                 AND deleted_at IS NULL
               ORDER BY effective_date DESC
               LIMIT 1"#,
        )
        .bind(from_currency)
        .bind(to_currency)
        .bind(as_of_date)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Returns all non-deleted FX rate rows for cache warm-up.
    pub async fn list_all_active(&self) -> Result<Vec<DbFxRate>> {
        let rows = sqlx::query_as::<_, DbFxRate>(
            r#"SELECT id, from_currency, to_currency, rate, effective_date,
                      created_at, updated_at, deleted_at
               FROM portfolio_fx_rates
               WHERE deleted_at IS NULL
               ORDER BY from_currency, to_currency, effective_date DESC"#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// FxRateService
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::Arc;

/// In-memory cache keyed by (from_currency, to_currency).
type FxCache = HashMap<(String, String), CacheEntry>;

#[derive(Clone)]
#[allow(dead_code)]
struct CacheEntry {
    rate: i64,
    effective_date: String,
}

/// FX rate service — loads all active rates on startup and provides
/// zero-DB convert() lookups.
///
/// Falls back gracefully when no rate is found (logs a warning and returns
/// the original amount unchanged), matching the pattern used by
/// `fetch_asset_currency` in T1.1.
#[derive(Clone)]
pub struct FxRateService {
    cache: Arc<FxCache>,
}

impl FxRateService {
    /// Build the service and warm the cache from `pool`.
    pub async fn new(pool: MySqlPool) -> anyhow::Result<Self> {
        let repo = FxRateRepository::new(pool);
        let rates = repo.list_all_active().await?;

        let mut cache: FxCache = FxCache::new();
        // Deduplicate: keep the latest effective_date per (from, to) pair.
        // `list_all_active` returns rows sorted by ccy pair then DESC date,
        // so the first entry we see for a given pair is the most recent.
        for row in rates {
            let key = (row.from_currency.clone(), row.to_currency.clone());
            cache.entry(key).or_insert(CacheEntry {
                rate: row.rate,
                effective_date: row.effective_date,
            });
        }

        tracing::info!("FX rate cache warmed with {} entries", cache.len());
        Ok(Self { cache: Arc::new(cache) })
    }

    /// Convert `amount` (in minor units) from `from` currency to `to` currency.
    /// Returns `amount` unchanged when no rate is found (graceful degradation).
    ///
    /// Rate semantics: the stored `rate` for (from, to) means
    ///   `1 unit of from_currency = rate units of to_currency` (minor units).
    ///
    ///   - same currency → amount
    ///   - direct rate  → amount * rate
    ///   - inverse rate → amount / inverse_rate
    ///
    /// Examples with seed rate VND→USD = 25_000:
    ///   convert(750_000_000, "VND", "USD") = 750_000_000 / 25_000 = 30_000
    ///   convert(100,         "USD", "VND") = 100 * 25_000       = 2_500_000
    pub fn convert(&self, amount: i64, from: &str, to: &str) -> i64 {
        if from == to {
            return amount;
        }

        let key = (from.to_string(), to.to_string());

        // Direct rate: 1 from = rate units of to
        if let Some(entry) = self.cache.get(&key) {
            return amount / entry.rate;
        }

        // Inverse rate: 1 from = 1/inverse_rate units of to
        let inverse_key = (to.to_string(), from.to_string());
        if let Some(entry) = self.cache.get(&inverse_key) {
            return amount * entry.rate;
        }

        tracing::warn!(
            "no FX rate found for {}→{}, returning amount unchanged",
            from,
            to
        );
        amount
    }
}
