//! One-off, idempotent migration: Saving budgets used to be plain income/expense ledgers.
//! The fund model keeps their money in a cash pocket, so each Saving budget gets an opening
//! balance equal to its historical net (income - expense) read from the entry service.
//!
//! usage: migrate_saving_to_fund [--apply]   (default is a dry run)
//! env:   DATABASE_URL, ENTRY_GRPC_URL
//! Re-running is safe: the movement idempotency key is `migrate-opening-<budget_id>`.

use budget::converters::fund::NewFundMovement;
use budget::manager::repository::fund_cash::FundCashRepository;
use budget::pb::service::entry::entry_service_client::EntryServiceClient;
use budget::pb::service::entry::PeriodTotalsRequest;
use budget::pb::service::portfolio::FundCashMovementKind;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::Row;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let apply = std::env::args().any(|a| a == "--apply");
    let db = std::env::var("DATABASE_URL")?;
    let entry_url =
        std::env::var("ENTRY_GRPC_URL").unwrap_or_else(|_| "http://127.0.0.1:50105".into());
    let pool = MySqlPoolOptions::new().max_connections(4).connect(&db).await?;
    let repo = FundCashRepository::new(pool.clone());
    let mut entry = EntryServiceClient::connect(entry_url).await?;

    let rows = sqlx::query(
        "SELECT id, currency, created_by FROM budgets WHERE budget_type = 'saving' AND deleted_at IS NULL",
    )
    .fetch_all(&pool)
    .await?;
    println!("{} saving budget(s); mode={}", rows.len(), if apply { "APPLY" } else { "DRY-RUN" });

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let (mut migrated, mut skipped) = (0, 0);
    for r in rows {
        let id: String = r.try_get("id")?;
        let currency: String = r.try_get("currency")?;
        let owner: String = r.try_get("created_by")?;
        let resp = entry
            .get_period_totals(PeriodTotalsRequest {
                budget_ids: vec![id.clone()],
                date_from: "1970-01-01".into(),
                date_to: "2999-12-31".into(),
                ..Default::default()
            })
            .await?
            .into_inner();
        let net: i64 = resp.rows.iter().map(|x| x.income - x.expense).sum();
        if net <= 0 {
            println!("skip {id}: net {net}");
            skipped += 1;
            continue;
        }
        println!("{} {id}: opening balance {net} {currency}", if apply { "migrate" } else { "would migrate" });
        if apply {
            repo.apply(&NewFundMovement {
                budget_id: id.clone(),
                currency,
                kind: FundCashMovementKind::OpeningBalance,
                amount_minor: net,
                ref_type: Some("migration".into()),
                ref_id: None,
                transfer_group_id: None,
                idempotency_key: Some(format!("migrate-opening-{id}")),
                actor_user_id: owner,
                note: Some("Opening balance migrated from legacy saving ledger".into()),
                occurred_on: today.clone(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("{id}: {e:?}"))?;
        }
        migrated += 1;
    }
    println!("done: {migrated} migrated, {skipped} skipped");
    Ok(())
}
