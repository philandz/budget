//! Synthesize missing _sqlx_migrations rows so future `cargo sqlx migrate run`
//! skips cleanly.
//!
//! Reads each migration file under `./migrations`, computes its SHA-384
//! checksum (matches sqlx format), and INSERTs a row into `_sqlx_migrations`
//! for any version NOT already present.
//!
//! Run with: DATABASE_URL=... cargo test --test synthesize_history -- --ignored --nocapture
use sqlx::{MySqlPool, Row};
use std::path::Path;

#[tokio::test]
#[ignore = "manual probe; requires DATABASE_URL"]
async fn synthesize_history() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = MySqlPool::connect(&url).await.expect("connect");

    let migrations_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut entries: Vec<(i64, String, Vec<u8>)> = Vec::new();

    for entry in std::fs::read_dir(&migrations_dir).expect("read migrations dir") {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sql") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        // Files look like <version>_<description>.sql — version is the
        // leading numeric segment up to the first underscore.
        let version_str: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(version) = version_str.parse::<i64>() else { continue };
        let description = stem[version_str.len() + 1..].replace('_', " ");
        let body = std::fs::read(&path).expect("read sql");
        let mut hasher = <sha2::Sha384 as sha2::Digest>::new();
        sha2::Digest::update(&mut hasher, &body);
        let checksum = sha2::Digest::finalize(hasher).to_vec();
        entries.push((version, description, checksum));
    }

    // Skip if no migrations
    if entries.is_empty() {
        eprintln!("no migrations found");
        return;
    }

    let mut inserted = 0usize;
    let mut skipped = 0usize;
    for (version, description, checksum) in entries {
        let row_exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM _sqlx_migrations WHERE version = ?",
        )
        .bind(version)
        .fetch_one(&pool)
        .await
        .unwrap_or(false);

        if row_exists {
            skipped += 1;
            continue;
        }

        let checksum_hex = hex::encode(&checksum);
        let res = sqlx::query(
            "INSERT IGNORE INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time)
             VALUES (?, ?, NOW(), true, UNHEX(?), 0)",
        )
        .bind(version)
        .bind(&description)
        .bind(&checksum_hex)
        .execute(&pool)
        .await;
        match res {
            Ok(r) => {
                if r.rows_affected() > 0 {
                    inserted += 1;
                    eprintln!("inserted version={} ({})", version, description);
                } else {
                    skipped += 1;
                }
            }
            Err(e) => eprintln!("failed version={}: {}", version, e),
        }
    }
    eprintln!("synthesize_history done: inserted={}, skipped={}", inserted, skipped);
}
