//! Maturity scan for fixed deposit assets.
//!
//! Phase 3.2 implements a job that walks all active fixed deposit
//! rows whose `maturity_date <= today`, transitions the asset to
//! `MATURED` via the lifecycle FSM, writes a `MATURITY_REACHED`
//! activity row, and queues an outbox event for the notification
//! service (Phase 4).
//!
//! Idempotency: the lifecycle check rejects the
//! `ACTIVE → MATURED` transition if the asset is already in
//! `MATURED` or later. The activity log is append-only; re-running
//! the scan on the same day simply appends another event with a new
//! timestamp. The downstream notification dedupes by `(asset_id,
//! activity_type, local_date)`.

use crate::manager::biz::portfolio::biz::PortfolioBiz;
use crate::manager::biz::portfolio::lifecycle::Transition;
use crate::manager::biz::portfolio::AssetStatus;
use philand_time::now_unix;

impl PortfolioBiz {
    /// Run a single maturity scan. Returns the number of assets
    /// transitioned to `MATURED` during this scan.
    pub async fn run_maturity_scan(&self) -> Result<u32, tonic::Status> {
        let today = today_business_date();
        let mut tx = self.repo.begin().await.map_err(internal)?;
        // portfolio_fixed_deposits stores asset-specific fields keyed by
        // `asset_id` only; `budget_id` lives on portfolio_assets (the parent
        // table shared across all asset classes). The previous SELECT against
        // portfolio_fixed_deposits in isolation (`SELECT budget_id, id ...`)
        // referenced columns that don't exist on that table, causing a SQL
        // 1054 ("Unknown column 'budget_id'") on every cycle.
        //
        // Join to portfolio_assets for `budget_id`. Note portfolio_assets PK
        // is `id`, with per-class tables (`portfolio_fixed_deposits.asset_id`,
        // etc.) referencing it as a foreign key.
        let due: Vec<(String, String)> = sqlx::query_as(
            "SELECT pa.budget_id, pfd.asset_id
             FROM portfolio_fixed_deposits pfd
             JOIN portfolio_assets pa ON pa.id = pfd.asset_id
             WHERE pfd.maturity_date <= ? AND pa.status = 'ACTIVE'",
        )
        .bind(today)
        .fetch_all(&mut *tx)
        .await
        .map_err(internal)?;
        let total = due.len() as u32;
        if total == 0 {
            tx.commit().await.map_err(internal)?;
            return Ok(0);
        }
        for (budget_id, asset_id) in &due {
            // Confirm the asset is still ACTIVE before transitioning
            // (concurrent rollovers may have moved it already).
            let existing = self
                .repo
                .get_asset(&mut tx, budget_id, asset_id)
                .await
                .map_err(internal)?
                .ok_or_else(|| tonic::Status::not_found(format!("asset {asset_id} not found")))?;
            let current = AssetStatus::from_db(&existing.status);
            if current != AssetStatus::Active {
                continue;
            }
            crate::manager::biz::portfolio::lifecycle::next_status(current, Transition::Mature)
                .map_err(internal)?;
            self.repo
                .update_status(
                    &mut tx,
                    budget_id,
                    asset_id,
                    AssetStatus::Matured,
                    Some(now_unix()),
                )
                .await
                .map_err(internal)?;
            self.append_activity(
                &mut tx,
                budget_id,
                asset_id,
                "system:maturity_scan",
                None,
                "MATURITY_REACHED",
                &format!(r#"{{"maturity_date":{today}}}"#),
            )
            .await
            .map_err(internal)?;
            let evt = crate::converters::portfolio::NewOutboxEvent {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: "MATURITY_REACHED".into(),
                asset_id: Some(asset_id.clone()),
                budget_id: Some(budget_id.clone()),
                payload_json: format!(
                    r#"{{"asset_id":"{asset_id}","budget_id":"{budget_id}","maturity_date":{today}}}"#
                ),
                enqueued_at: now_unix(),
            };
            self.repo
                .insert_outbox(&mut tx, &evt)
                .await
                .map_err(internal)?;
        }
        tx.commit().await.map_err(internal)?;
        Ok(total)
    }
}

fn internal<E: ToString>(e: E) -> tonic::Status {
    tonic::Status::internal(e.to_string())
}

fn today_business_date() -> i64 {
    let now_utc = chrono::Utc::now();
    let ict = now_utc + chrono::Duration::hours(7);
    ict.timestamp() - (ict.timestamp() % 86_400)
}
