use std::future::IntoFuture;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio::net::TcpListener;
use uuid::Uuid;

use mekhan_service::config::{AppConfig, CleanupConfig, S3Config};
use mekhan_service::nats::MekhanNats;
use mekhan_service::petri::client::PetriClient;
use mekhan_service::s3::ArtifactStore;
use mekhan_service::yjs::manager::YjsManager;
use mekhan_service::yjs::persistence::YjsPersistence;
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
        s3: S3Config::default(),
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

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));

    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config,
        yjs: yjs_manager,
        s3: artifact_store,
    };

    let router = build_router(state);
    (router, db)
}

/// Start the full Axum server on a random port for WebSocket tests.
/// Returns `(SocketAddr, PgPool)` — the address to connect to and the pool for assertions.
pub async fn start_test_server() -> (SocketAddr, PgPool) {
    let (app, db) = test_app().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    (addr, db)
}
