//! Price observation snapshot recording and listing.
//!
//! Thin wrappers over `PortfolioRepository` that handle the per-asset
//! price snapshot lifecycle: insert with idempotency, list history,
//! and log the corresponding activity event.

use std::sync::Arc;
use tonic::Status;

use crate::converters::portfolio as pconv;
use crate::manager::biz::portfolio::PriceSide;
use crate::manager::repository::portfolio::PortfolioRepository;
use crate::pb::service::portfolio as pb;
use philand_time::now_unix;

pub struct PortfolioSnapshot {
    pub repo: Arc<PortfolioRepository>,
}

impl PortfolioSnapshot {
    pub fn new(repo: Arc<PortfolioRepository>) -> Self {
        Self { repo }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_price_observation(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
        asset_id: &str,
        provider: &str,
        price_side: &str,
        unit_price: i64,
        currency: &str,
        observed_at: i64,
        source_reference: &str,
        idempotency_key: Option<&str>,
        notes: Option<&str>,
    ) -> Result<pb::PriceObservation, Status> {
        if unit_price < 0 {
            return Err(Status::invalid_argument("unit_price must be >= 0"));
        }
        if let Some(key) = idempotency_key {
            if !key.is_empty() {
                if let Some(existing) = self
                    .repo
                    .get_price_observation_by_idempotency(tx, asset_id, key)
                    .await
                    .map_err(internal)?
                {
                    return Ok(pconv::map_price_observation(existing));
                }
            }
        }
        let new = pconv::NewPriceObservation {
            id: None,
            asset_id: asset_id.to_string(),
            provider: if provider.is_empty() {
                "manual".to_string()
            } else {
                provider.to_string()
            },
            price_side: parse_price_side(price_side),
            unit_price,
            currency: currency.to_string(),
            observed_at: if observed_at == 0 {
                now_unix()
            } else {
                observed_at
            },
            source_reference: source_reference.to_string(),
            idempotency_key: idempotency_key
                .filter(|k| !k.is_empty())
                .map(str::to_string),
            notes: notes.map(str::to_string),
        };
        let obs = self
            .repo
            .insert_price_observation(tx, &new)
            .await
            .map_err(internal)?;
        Ok(pconv::map_price_observation(obs))
    }

    pub async fn list_price_observations(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
        asset_id: &str,
        limit: i32,
    ) -> Result<Vec<pb::PriceObservation>, Status> {
        let rows = self
            .repo
            .list_price_observations(tx, asset_id, limit)
            .await
            .map_err(internal)?;
        Ok(rows.into_iter().map(pconv::map_price_observation).collect())
    }

    pub async fn list_asset_activity(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
        asset_id: &str,
        limit: i32,
    ) -> Result<Vec<pb::PortfolioActivity>, Status> {
        let rows = self
            .repo
            .list_activities(tx, asset_id, limit)
            .await
            .map_err(internal)?;
        Ok(rows.into_iter().map(pconv::map_activity).collect())
    }
}

fn internal<E: ToString>(e: E) -> Status {
    Status::internal(e.to_string())
}

fn parse_price_side(s: &str) -> PriceSide {
    match s {
        "bid" => PriceSide::Bid,
        "ask" => PriceSide::Ask,
        _ => PriceSide::Mid,
    }
}
