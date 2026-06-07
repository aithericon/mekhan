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
    // (latest: 20240154000000_reconcile_views.sql — legacy-migration reconcile
    //  views. Merge de-collided a duplicate 20240152000000: node_replicas → 151,
    //  model_states_policy → 152, catalog_content_addressed → 153, reconcile → 154.)
    // (latest: 20240135000000_capability_types.sql — Phase 4 capability registry)
    // (latest: 20240134000000_runners.sql — Lab Runner Fleet).
    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}
