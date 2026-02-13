use axum::Router;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::config::{AppConfig, CleanupConfig};
use mekhan_service::nats::MekhanNats;
use mekhan_service::petri::client::PetriClient;
use mekhan_service::{build_router, AppState};

/// Default test database URL. Uses the docker-compose postgres at localhost:5432
/// with the `mekhan` user. Each test gets its own database for isolation.
const BASE_DATABASE_URL: &str = "postgres://mekhan:mekhan@localhost:5432";

/// Create an isolated test database with a unique name, run migrations, and return the pool.
pub async fn create_test_db() -> PgPool {
    let db_name = format!("mekhan_test_{}", Uuid::new_v4().simple());

    // Connect to the default `mekhan` database to create the test database
    let admin_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&format!("{}/mekhan", BASE_DATABASE_URL))
        .await
        .expect("failed to connect to admin database — is docker-compose running?");

    sqlx::query(&format!("CREATE DATABASE \"{}\"", db_name))
        .execute(&admin_pool)
        .await
        .expect("failed to create test database");

    admin_pool.close().await;

    // Connect to the new test database and run migrations
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&format!("{}/{}", BASE_DATABASE_URL, db_name))
        .await
        .expect("failed to connect to test database");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("failed to run migrations");

    pool
}

/// Build a test AppConfig.
pub fn test_config() -> AppConfig {
    AppConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        database_url: String::new(), // unused — pool is created directly
        petri_lab_url: "http://localhost:3030".to_string(),
        nats_url: "nats://localhost:4222".to_string(),
        cleanup: CleanupConfig::default(),
    }
}

/// Build the full Axum Router wired to a test database.
/// Requires docker-compose postgres and NATS to be running.
///
/// Returns `(Router, PgPool)` — callers can use the pool for direct DB assertions.
pub async fn test_app() -> (Router, PgPool) {
    let db = create_test_db().await;
    let config = test_config();

    let petri = PetriClient::new(&config.petri_lab_url);

    // Try to connect to NATS; if unavailable, tests that need NATS should be skipped.
    let nats = MekhanNats::connect(&config.nats_url)
        .await
        .expect("failed to connect to NATS — is docker-compose running?");

    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config,
    };

    let router = build_router(state);
    (router, db)
}
