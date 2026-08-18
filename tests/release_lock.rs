//! Release the sqlx migrator advisory lock. Run with:
//!   DATABASE_URL=... cargo test --test release_lock -- --ignored --nocapture
use sqlx::MySqlPool;

#[tokio::test]
#[ignore = "manual maintenance"]
async fn release_migrator_lock() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = MySqlPool::connect(&url).await.expect("connect");
    // sqlx-mysql uses 'sqlx::connect' name for GET_LOCK(?, -1). Force-release.
    let r = sqlx::query("SELECT RELEASE_LOCK('sqlx::connect')")
        .fetch_one(&pool)
        .await
        .expect("release");
    let v: Option<i64> = sqlx::Row::get(&r, 0);
    eprintln!("RELEASE_LOCK returned: {:?}", v);
}
