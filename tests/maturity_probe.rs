//! Temporary probe: verify the maturity_scan JOIN SELECT works against the
//! live philandz schema. Run with:
//!   DATABASE_URL=... cargo test --test maturity_probe -- --ignored --nocapture
//!
//! Delete once `fix(budget) maturity scan` is verified.

use sqlx::{MySqlPool, Row};

#[tokio::test]
#[ignore = "manual probe; require DATABASE_URL"]
async fn probe_maturity_select() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for `--ignored` runs");
    let pool = MySqlPool::connect(&url).await.expect("connect");

    let rows = sqlx::query(
        "SELECT pa.budget_id, pfd.asset_id
         FROM portfolio_fixed_deposits pfd
         JOIN portfolio_assets pa ON pa.id = pfd.asset_id
         WHERE pfd.maturity_date <= ? AND pa.status = 'ACTIVE'",
    )
    .bind(0_i64)
    .fetch_all(&pool)
    .await
    .expect("select should succeed even with 0 rows");

    eprintln!(
        "maturity_probe: {} rows (0 expected for any test DB)",
        rows.len()
    );
    for r in &rows {
        let b: String = r.get("budget_id");
        let a: String = r.get("asset_id");
        eprintln!("  budget={} asset={}", b, a);
    }
}
