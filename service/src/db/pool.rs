use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub async fn create_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;

    // NOTE: `sqlx::migrate!` embeds the ./migrations directory at COMPILE time.
    // Touch this line whenever a migration is added so the macro re-embeds it.
    // (latest: 20240135000000_capability_types.sql — Phase 4 capability registry)
    // (latest: 20240134000000_runners.sql — Lab Runner Fleet).
    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}
