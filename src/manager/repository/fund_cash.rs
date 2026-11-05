//! Fund cash pocket persistence.
//!
//! Invariant I4: the pocket balance never goes negative. Every write locks the account row
//! (`SELECT … FOR UPDATE`), re-checks the balance inside the transaction and also guards the
//! UPDATE with `balance_minor + ? >= 0`, so two concurrent withdrawals cannot both succeed.

use sqlx::MySqlPool;

use crate::converters::fund::{kind_to_db, DbFundCashAccount, DbFundCashMovement, NewFundMovement};
use philand_time::now_unix;

#[derive(Debug, thiserror::Error)]
pub enum FundCashError {
    #[error("insufficient cash: balance {balance}, requested {requested}")]
    Insufficient { balance: i64, requested: i64 },
    #[error("idempotency key was reused with a different request")]
    IdempotencyMismatch,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Debug)]
pub struct AppliedMovement {
    pub movement: DbFundCashMovement,
    pub balance_after: i64,
    /// true when an earlier request with the same idempotency key was returned unchanged.
    pub replayed: bool,
}

/// Outcome of trying to claim a transfer for rollback.
///
/// The claim is the mutual-exclusion point between a request that is still in flight and the
/// reconciler: exactly one of them can move the row out of `requested`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferClaim {
    /// The row was still `requested`; this caller now owns the rollback.
    Claimed,
    /// The row was already claimed (`reconciling`) by an earlier sweep that never finished.
    AlreadyClaimed,
    /// The row moved on (`completed` / `failed`): it must not be touched.
    Lost,
}

/// MySQL deadlock (1213) or lock wait timeout (1205): safe to retry the whole transaction.
fn is_retryable(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .map(|c| c == "40001" || c == "HY000" && e.to_string().contains("1205"))
        .unwrap_or(false)
}

#[derive(Clone)]
pub struct FundCashRepository {
    pool: MySqlPool,
}

const SELECT_MOVEMENT: &str =
    "SELECT id, budget_id, kind, amount_minor, ref_type, ref_id, transfer_group_id,
        idempotency_key, actor_user_id, note, occurred_on, created_at FROM fund_cash_movements";

impl FundCashRepository {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }

    pub async fn get_account(
        &self,
        budget_id: &str,
    ) -> Result<Option<DbFundCashAccount>, sqlx::Error> {
        sqlx::query_as::<_, DbFundCashAccount>(
            "SELECT budget_id, currency, balance_minor, version FROM fund_cash_accounts WHERE budget_id = ?",
        )
        .bind(budget_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Applies one movement atomically. Replays an existing movement with the same
    /// `(budget_id, idempotency_key)` instead of inserting a duplicate.
    pub async fn apply(&self, m: &NewFundMovement) -> Result<AppliedMovement, FundCashError> {
        // Concurrent writers on a brand-new account can deadlock on the first insert; the loser
        // simply retries and then queues on the row lock.
        let mut attempt = 0;
        loop {
            match self.apply_once(m).await {
                Err(FundCashError::Db(e)) if attempt < 5 && is_retryable(&e) => {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(20 * attempt)).await;
                }
                other => return other,
            }
        }
    }

    async fn apply_once(&self, m: &NewFundMovement) -> Result<AppliedMovement, FundCashError> {
        let now = now_unix();
        let mut tx = self.pool.begin().await?;

        let mut locked: Option<i64> = sqlx::query_scalar(
            "SELECT balance_minor FROM fund_cash_accounts WHERE budget_id = ? FOR UPDATE",
        )
        .bind(&m.budget_id)
        .fetch_optional(&mut *tx)
        .await?;
        if locked.is_none() {
            sqlx::query(
                "INSERT IGNORE INTO fund_cash_accounts (budget_id, currency, balance_minor, version, created_at, updated_at)
                 VALUES (?, ?, 0, 0, ?, ?)",
            )
            .bind(&m.budget_id)
            .bind(&m.currency)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            locked = sqlx::query_scalar(
                "SELECT balance_minor FROM fund_cash_accounts WHERE budget_id = ? FOR UPDATE",
            )
            .bind(&m.budget_id)
            .fetch_optional(&mut *tx)
            .await?;
        }
        let balance = locked.ok_or(sqlx::Error::RowNotFound)?;

        if let Some(key) = m.idempotency_key.as_deref() {
            let sql = format!("{SELECT_MOVEMENT} WHERE budget_id = ? AND idempotency_key = ?");
            if let Some(existing) = sqlx::query_as::<_, DbFundCashMovement>(&sql)
                .bind(&m.budget_id)
                .bind(key)
                .fetch_optional(&mut *tx)
                .await?
            {
                tx.rollback().await?;
                if existing.amount_minor != m.amount_minor || existing.kind != kind_to_db(m.kind) {
                    return Err(FundCashError::IdempotencyMismatch);
                }
                return Ok(AppliedMovement {
                    movement: existing,
                    balance_after: balance,
                    replayed: true,
                });
            }
        }

        if balance.saturating_add(m.amount_minor) < 0 {
            tx.rollback().await?;
            return Err(FundCashError::Insufficient {
                balance,
                requested: -m.amount_minor,
            });
        }

        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO fund_cash_movements
               (id, budget_id, kind, amount_minor, ref_type, ref_id, transfer_group_id,
                idempotency_key, actor_user_id, note, occurred_on, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&m.budget_id)
        .bind(kind_to_db(m.kind))
        .bind(m.amount_minor)
        .bind(&m.ref_type)
        .bind(&m.ref_id)
        .bind(&m.transfer_group_id)
        .bind(&m.idempotency_key)
        .bind(&m.actor_user_id)
        .bind(&m.note)
        .bind(&m.occurred_on)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let updated = sqlx::query(
            "UPDATE fund_cash_accounts
                SET balance_minor = balance_minor + ?, version = version + 1, updated_at = ?
              WHERE budget_id = ? AND balance_minor + ? >= 0",
        )
        .bind(m.amount_minor)
        .bind(now)
        .bind(&m.budget_id)
        .bind(m.amount_minor)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            tx.rollback().await?;
            return Err(FundCashError::Insufficient {
                balance,
                requested: -m.amount_minor,
            });
        }

        let sql = format!("{SELECT_MOVEMENT} WHERE id = ?");
        let movement = sqlx::query_as::<_, DbFundCashMovement>(&sql)
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(AppliedMovement {
            movement,
            balance_after: balance + m.amount_minor,
            replayed: false,
        })
    }

    pub async fn list_movements(
        &self,
        budget_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<DbFundCashMovement>, i64), sqlx::Error> {
        let sql = format!(
            "{SELECT_MOVEMENT} WHERE budget_id = ? ORDER BY occurred_on DESC, created_at DESC, id DESC LIMIT ? OFFSET ?"
        );
        let rows = sqlx::query_as::<_, DbFundCashMovement>(&sql)
            .bind(budget_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM fund_cash_movements WHERE budget_id = ?")
                .bind(budget_id)
                .fetch_one(&self.pool)
                .await?;
        Ok((rows, total))
    }

    /// Σ opening_balance + transfer_in + transfer_out (signed) — what the owner has put in net of what
    /// they took out. Purchases/redemptions/interest are excluded: they move value, not contributions.
    pub async fn net_contributed(&self, budget_id: &str) -> Result<i64, sqlx::Error> {
        let v: Option<i64> = sqlx::query_scalar(
            "SELECT CAST(SUM(amount_minor) AS SIGNED) FROM fund_cash_movements
              WHERE budget_id = ? AND kind IN ('opening_balance','transfer_in','transfer_out')",
        )
        .bind(budget_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(v.unwrap_or(0))
    }

    /// Audit helper: the cached balance must equal the movement sum.
    pub async fn movement_sum(&self, budget_id: &str) -> Result<i64, sqlx::Error> {
        let v: Option<i64> = sqlx::query_scalar(
            "SELECT CAST(SUM(amount_minor) AS SIGNED) FROM fund_cash_movements WHERE budget_id = ?",
        )
        .bind(budget_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(v.unwrap_or(0))
    }
}

// ---------------------------------------------------------------------------
// Saga bookkeeping on `portfolio_transfers`
// ---------------------------------------------------------------------------

impl FundCashRepository {
    pub async fn find_movement_by_key(
        &self,
        budget_id: &str,
        key: &str,
    ) -> Result<Option<DbFundCashMovement>, sqlx::Error> {
        let sql = format!("{SELECT_MOVEMENT} WHERE budget_id = ? AND idempotency_key = ?");
        sqlx::query_as::<_, DbFundCashMovement>(&sql)
            .bind(budget_id)
            .bind(key)
            .fetch_optional(&self.pool)
            .await
    }

    /// `status` is one of the `portfolio_transfers.status` strings (e.g. "completed", "failed").
    ///
    /// The write only lands while the row is still `requested`. A transfer the reconciler has
    /// claimed (`reconciling`) or that is already `completed` / `failed` is never overwritten,
    /// so a late writer cannot resurrect a transfer that was rolled back.
    ///
    /// Returns true when the row was updated (the caller still owned the transfer).
    pub async fn set_transfer_status(
        &self,
        transfer_id: &str,
        status: &str,
        linked_entry_id: Option<&str>,
    ) -> Result<bool, sqlx::Error> {
        let completed_at = (status == "completed").then(now_unix);
        let res = sqlx::query(
            "UPDATE portfolio_transfers
                SET status = ?, completed_at = COALESCE(?, completed_at),
                    linked_entry_source_id = COALESCE(?, linked_entry_source_id)
              WHERE id = ? AND status = 'requested'",
        )
        .bind(status)
        .bind(completed_at)
        .bind(linked_entry_id.filter(|s| !s.is_empty()))
        .bind(transfer_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Claims a transfer for rollback by moving it out of `requested` into `reconciling`.
    ///
    /// The conditional UPDATE is the only place where the request path and the reconciler meet:
    /// a request that completes at the same time loses the row and is told to compensate, and a
    /// transfer that already reached `completed` can never be claimed — so the reconciler never
    /// voids a finished transfer.
    pub async fn claim_transfer(&self, transfer_id: &str) -> Result<TransferClaim, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE portfolio_transfers SET status = 'reconciling' WHERE id = ? AND status = 'requested'",
        )
        .bind(transfer_id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 1 {
            return Ok(TransferClaim::Claimed);
        }
        // Not `requested` any more: either an earlier sweep left it claimed, or it is done.
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM portfolio_transfers WHERE id = ?")
                .bind(transfer_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(match status.as_deref() {
            Some("reconciling") => TransferClaim::AlreadyClaimed,
            _ => TransferClaim::Lost,
        })
    }

    /// Marks a claimed transfer as failed. Only lands while the row is still `reconciling`, so a
    /// transfer that finished in the meantime is never demoted.
    pub async fn mark_reconciled_failed(&self, transfer_id: &str) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE portfolio_transfers SET status = 'failed' WHERE id = ? AND status = 'reconciling'",
        )
        .bind(transfer_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Transfers that are still waiting for their saga to finish: `requested` rows and rows a
    /// previous sweep claimed (`reconciling`) but never managed to roll back.
    pub async fn list_stale_requested(
        &self,
        created_before: i64,
    ) -> Result<Vec<crate::converters::portfolio::DbTransfer>, sqlx::Error> {
        sqlx::query_as::<_, crate::converters::portfolio::DbTransfer>(
            r#"SELECT id, group_id, source_budget_id, counterparty_budget_id,
                      direction, amount_minor, currency, status,
                      linked_entry_source_id, linked_entry_counterparty_id,
                      idempotency_key, actor_user_id, created_at, completed_at, notes
                 FROM portfolio_transfers
                WHERE status IN ('requested', 'reconciling') AND created_at < ?
                  AND direction IN ('standard_to_invest','invest_to_standard')"#,
        )
        .bind(created_before)
        .fetch_all(&self.pool)
        .await
    }

    /// Fixed deposits of a budget that mature inside `[from, to]`.
    /// Returns `(asset_id, display_name, maturity_date, principal)`.
    pub async fn upcoming_maturities(
        &self,
        budget_id: &str,
        from: i64,
        to: i64,
    ) -> Result<Vec<(String, String, i64, i64)>, sqlx::Error> {
        sqlx::query_as::<_, (String, String, i64, i64)>(
            r#"SELECT a.id, a.display_name, fd.maturity_date, fd.principal
                 FROM portfolio_assets a
                 JOIN portfolio_fixed_deposits fd ON fd.asset_id = a.id
                WHERE a.budget_id = ? AND a.deleted_at IS NULL AND a.status = 'active'
                  AND fd.maturity_date BETWEEN ? AND ?
                ORDER BY fd.maturity_date"#,
        )
        .bind(budget_id)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
    }
}
