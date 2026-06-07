use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub async fn create_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;

    // NOTE: `sqlx::migrate!` embeds the ./migrations directory at COMPILE time.
    // Touch this line whenever a migration is added/renamed so the macro re-embeds
    // it — sccache content-hashes this file but NOT the external migrations dir, so
    // a real edit here is required to bust the cache (a bare `touch` won't).
    // (latest: 20240152000000_node_replicas.sql — de-collided from a duplicate
    //  20240151000000; see 20240151000000_model_states_policy.sql).
    // (latest: 20240135000000_capability_types.sql — Phase 4 capability registry)
    // (latest: 20240134000000_runners.sql — Lab Runner Fleet).
    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}
