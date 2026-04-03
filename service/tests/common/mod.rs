use std::future::IntoFuture;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use sqlx::PgPool;
use tokio::net::TcpListener;

use aithericon_test_infra::TestDb;
use mekhan_service::config::{AppConfig, CleanupConfig, S3Config};
use mekhan_service::nats::MekhanNats;
use mekhan_service::petri::client::PetriClient;
use mekhan_service::s3::ArtifactStore;
use mekhan_service::yjs::manager::YjsManager;
use mekhan_service::yjs::persistence::YjsPersistence;
use mekhan_service::{build_router, AppState};

/// Create an isolated test database with migrations applied.
/// Uses the shared test infrastructure at localhost:5599.
///
/// Returns a `PgPool` for backward compat with existing tests.
/// The `TestDb` is leaked to prevent the destructor from dropping the database
/// before the test completes. Since the infra is tmpfs-backed, leaked DBs
/// disappear on `just down`.
pub async fn create_test_db() -> PgPool {
    let db = TestDb::create("./migrations").await;
    let pool = db.pool.clone();
    // Leak the TestDb to prevent Drop from deleting the database mid-test.
    // The tmpfs-backed Postgres container handles cleanup on shutdown.
    std::mem::forget(db);
    pool
}

/// Default S3 URL for test infrastructure.
/// Override with `TEST_S3_ENDPOINT` env var.
const DEFAULT_TEST_S3_ENDPOINT: &str = "http://localhost:9099";

/// Build a test AppConfig pointing to the shared test infrastructure.
pub fn test_config() -> AppConfig {
    let s3_endpoint = std::env::var("TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_TEST_S3_ENDPOINT.to_string());

    AppConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        database_url: String::new(),
        petri_lab_url: "http://localhost:3030".to_string(),
        nats_url: aithericon_test_infra::nats_url(),
        cleanup: CleanupConfig::default(),
        s3: S3Config {
            endpoint: s3_endpoint,
            bucket: "mekhan-test".to_string(),
            access_key: "testadmin".to_string(),
            secret_key: "testadmin".to_string(),
            region: "us-east-1".to_string(),
        },
        artifact_s3: None,
    }
}

/// Build the full Axum Router wired to a test database.
/// Requires `just -f aithericon-test-infra/justfile up` to be running.
///
/// Returns `(Router, PgPool)` — callers can use the pool for direct DB assertions.
pub async fn test_app() -> (Router, PgPool) {
    let db = create_test_db().await;
    let config = test_config();

    let petri = PetriClient::new(&config.petri_lab_url);

    let nats = MekhanNats::connect(&config.nats_url)
        .await
        .expect("failed to connect to NATS — run: just -f aithericon-test-infra/justfile up");

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));

    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
    };

    let router = build_router(state);
    (router, db)
}

/// Build test app with a specific NATS URL.
/// Used by E2E tests that need to share NATS with the petri-lab engine.
pub async fn test_app_with_nats(nats_url: &str) -> (Router, PgPool) {
    let db = create_test_db().await;
    let mut config = test_config();
    config.nats_url = nats_url.to_string();

    let petri = PetriClient::new(&config.petri_lab_url);

    let nats = MekhanNats::connect(nats_url)
        .await
        .unwrap_or_else(|e| panic!("failed to connect to NATS at {nats_url}: {e}"));

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));

    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
    };

    let router = build_router(state);
    (router, db)
}

/// Build test app with a specific NATS URL and petri-lab URL.
/// Used for error path tests where we want the engine to be "unavailable"
/// by pointing PetriClient at a bogus URL.
pub async fn test_app_with_petri_url(nats_url: &str, petri_url: &str) -> (Router, PgPool) {
    let db = create_test_db().await;
    let mut config = test_config();
    config.nats_url = nats_url.to_string();
    config.petri_lab_url = petri_url.to_string();

    let petri = PetriClient::new(petri_url);

    let nats = MekhanNats::connect(nats_url)
        .await
        .unwrap_or_else(|e| panic!("failed to connect to NATS at {nats_url}: {e}"));

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));

    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
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
