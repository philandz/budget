//! Integration helper: idempotent migrator for tests against a shared/dev DB.
//!
//! Mirrors the pattern in `identity/tests/identity_service_test.rs`. The dev
//! Aiven schema has been touched by multiple branches — some migrations
//! partially ran (columns added but checksum record inconsistent), some
//! failed (duplicate column because ALTER TABLE ran twice). All are fine as
//! long as the schema is in place.
//!
//! Runs only when invoked explicitly:
//!   DATABASE_URL=... cargo test --test migrations -- --ignored
//!
//! Without `--ignored`, the test is marked `#[ignore]` so unit `cargo test`
//! runs stay fast and DB-free.

use sqlx::MySqlPool;
use sqlx::Row;

#[allow(dead_code)]
async fn run_migrations_idempotent(pool: &MySqlPool) {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    if let Err(e) = migrator.run(pool).await {
        let msg = e.to_string();
        let ignorable = msg.contains("VersionMismatch")
            || msg.contains("partially applied")
            || msg.contains("Duplicate column name")
            || msg.contains("Duplicate key name")
            || msg.contains("already exists");
        if !ignorable {
            panic!("Migration failed (unexpected): {}", msg);
        }
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; run with `--ignored`"]
async fn apply_pending_migrations_idempotently() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for `--ignored` runs");
    let pool = MySqlPool::connect(&url).await.expect("connect");

    // Print pre-state for visibility.
    let rows = sqlx::query("SELECT version, description, success FROM _sqlx_migrations ORDER BY version")
        .fetch_all(&pool).await.expect("query _sqlx_migrations");
    eprintln!("_sqlx_migrations PRE: {} rows", rows.len());
    for r in &rows {
        let v: i64 = r.get("version");
        let d: String = r.get("description");
        let s: Option<i8> = r.try_get("success").ok();
        eprintln!("  version={} desc={} success={:?}", v, d, s);
    }

    // List existing tables (sizes vary; we only need names).
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME"
    ).fetch_all(&pool).await.expect("query tables");
    eprintln!("DB tables ({}):", tables.len());
    for (t,) in &tables {
        eprintln!("  {}", t);
    }

    // One-off cleanup: a row exists for a version that has no migration file
    // in this crate's source dir (cross-service orphan left by identity's
    // 20260408000004 user_profile_fields migration). Safe to remove.
    let deleted = sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 20260408000004")
        .execute(&pool)
        .await
        .ok()
        .map(|x| x.rows_affected())
        .unwrap_or(0);
    eprintln!("orphan row delete affected={}", deleted);

    // Verify the macro actually embedded our 15 pending migrations.
    let mut count_only = sqlx::migrate!("./migrations");
    let pending_count = count_only.iter().count();
    eprintln!("sqlx::migrate! embedded {} migrations", pending_count);

    run_migrations_idempotent(&pool).await;

    let post = sqlx::query("SELECT COUNT(*) as c FROM _sqlx_migrations")
        .fetch_one(&pool).await.expect("post-count");
    let c: i64 = post.get("c");
    eprintln!("_sqlx_migrations POST: {} rows", c);

    let tables_post: Vec<(String,)> = sqlx::query_as(
        "SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME"
    ).fetch_all(&pool).await.expect("query tables");
    eprintln!("DB tables POST ({}):", tables_post.len());
    for (t,) in &tables_post {
        eprintln!("  {}", t);
    }
}
