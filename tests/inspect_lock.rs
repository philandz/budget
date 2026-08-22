//! Inspect who holds the sqlx::connect GET_LOCK on Aiven.
use sqlx::{MySqlPool, Row};
#[tokio::test]
#[ignore = "manual diagnostic"]
async fn inspect_lock() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = MySqlPool::connect(&url).await.expect("connect");
    // query who holds GET_LOCK
    let rows = sqlx::query(
        "SELECT IS_USED_LOCK('sqlx::connect') AS lock_holder,
         IS_FREE_LOCK('sqlx::connect') AS is_free"
    ).fetch_one(&pool).await.expect("query");
    let holder: Option<i64> = rows.try_get("lock_holder").ok();
    let is_free: i64 = rows.try_get("is_free").ok().unwrap_or(0);
    eprintln!("is_free_lock={} lock_holder_thread={:?}", is_free, holder);
    // Also list current MySQL sessions
    let procs = sqlx::query(
        "SELECT id, user, command, time, state, info FROM information_schema.processlist WHERE db IS NOT NULL AND command LIKE '%%LOCK%%' ORDER BY id"
    ).fetch_all(&pool).await.expect("ps");
    for p in procs {
        let id: u64 = p.get("id");
        let user: String = p.get("user");
        let state: Option<String> = p.try_get("state").ok();
        let info: Option<String> = p.try_get("info").ok();
        eprintln!("  id={} user={} state={:?} info={:?}", id, user, state, info);
    }
}
