// `mod common` is included by many test binaries; each binary only references
// a subset of these helpers, so unused items appear "dead" per-binary even
// though they're load-bearing in others.
#![allow(dead_code, unused_imports)]

use std::future::IntoFuture;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use sqlx::PgPool;
use tokio::net::TcpListener;

pub mod mock_auth;
pub mod test_infra;
pub mod workspace_fixtures;
pub mod zitadel_live;
pub mod zitadel_mock;
pub use test_infra::{nats_url, postgres_url, wait_for_nats, wait_for_postgres, TestDb, TestNats};

use mekhan_service::auth::authenticator::{Authenticator, NoopAuthenticator};
use mekhan_service::auth::bff::session::{PgSessionStore, SessionStore};
use mekhan_service::auth::dev::NoopTokenVerifier;
use mekhan_service::auth::resolver::StaticPrincipalResolver;
use mekhan_service::auth::{IntrospectionVerifier, ZitadelMgmt};
use mekhan_service::auth::{PrincipalResolver, TokenVerifier};
use mekhan_service::catalogue::repository::PgCatalogueRepository;
use mekhan_service::causality::live::LiveBroadcasts;
use mekhan_service::config::{AppConfig, AuthConfig, CleanupConfig, S3Config};
use mekhan_service::nats::MekhanNats;
use mekhan_service::petri::client::PetriClient;
use mekhan_service::s3::ArtifactStore;
use mekhan_service::triggers::TriggerDispatcher;
use mekhan_service::yjs::manager::YjsManager;
use mekhan_service::yjs::persistence::YjsPersistence;
use mekhan_service::{build_router, AppState};

/// Build a `TriggerDispatcher` for tests. The dispatcher's `hydrate()` is
/// skipped here — tests that exercise trigger behavior should call it
/// explicitly after seeding template rows.
fn test_triggers(db: PgPool, petri: PetriClient, nats: MekhanNats) -> Arc<TriggerDispatcher> {
    Arc::new(TriggerDispatcher::new(db, petri, nats))
}

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
const DEFAULT_TEST_S3_ENDPOINT: &str = "http://localhost:19005";

/// Build a test AppConfig pointing to the shared test infrastructure.
///
/// For executor-backed e2e against a live `just dev` stack the published
/// node-file bucket/creds MUST match what the running executor reads
/// (`mekhan-artifacts` + the rustfs creds), or staging 404s and the net
/// hangs. Override via `TEST_S3_{ENDPOINT,BUCKET,ACCESS_KEY,SECRET_KEY}`.
pub fn test_config() -> AppConfig {
    let s3_endpoint =
        std::env::var("TEST_S3_ENDPOINT").unwrap_or_else(|_| DEFAULT_TEST_S3_ENDPOINT.to_string());
    let s3_bucket =
        std::env::var("TEST_S3_BUCKET").unwrap_or_else(|_| "mekhan-artifacts".to_string());
    let s3_access_key =
        std::env::var("TEST_S3_ACCESS_KEY").unwrap_or_else(|_| "rustfsadmin".to_string());
    let s3_secret_key =
        std::env::var("TEST_S3_SECRET_KEY").unwrap_or_else(|_| "rustfsadmin".to_string());

    AppConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        database_url: String::new(),
        petri_lab_url: std::env::var("TEST_PETRI_URL")
            .unwrap_or_else(|_| "http://localhost:13030".to_string()),
        nats_url: nats_url(),
        nats_creds: None,
        cleanup: CleanupConfig::default(),
        wait_timeout_secs: 30,
        s3: S3Config {
            endpoint: s3_endpoint,
            bucket: s3_bucket,
            access_key: s3_access_key,
            secret_key: s3_secret_key,
            region: "us-east-1".to_string(),
        },
        artifact_s3: None,
        frontend_dir: None,
        auth: AuthConfig::default(),
        // Tests publish demos explicitly through the API; the startup
        // seeder is off so each test owns its template ids.
        demos: mekhan_service::config::DemosConfig::default(),
    }
}

/// Resource secret store for the test `AppState`. When `VAULT_ADDR`/`VAULT_TOKEN`
/// are set — e.g. an executor-backed e2e driven against a live `just dev` stack
/// whose ENGINE reads the SAME Vault — use the Vault-backed store so resource
/// SECRET fields (a datacenter's inline `ssh_key` PEM, an `nomad_token`, …) land
/// exactly where the engine resolves `{{secret:<vault_path>#<field>}}` at fire
/// time. Without this, secrets would go to a process-local in-memory store and
/// the engine's secret-template resolution would come up empty (a slurm lease
/// then fails with "ssh: failed to connect" — a malformed/empty key). Offline
/// unit/integration tests leave the env unset and get the in-memory fallback.
/// Mirrors `main.rs`'s selection.
fn default_resource_store() -> Arc<dyn aithericon_resources::ResourceSecretStore> {
    match aithericon_resources::VaultResourceStore::from_env() {
        Some(vrs) => Arc::new(vrs),
        None => Arc::new(aithericon_resources::InMemoryResourceStore::new()),
    }
}

/// Default test auth adapters: NoopTokenVerifier + StaticPrincipalResolver.
/// Tests that exercise auth behavior should swap these via direct `AppState`
/// construction or by using their own helpers.
pub fn default_test_auth() -> (Arc<dyn TokenVerifier>, Arc<dyn PrincipalResolver>) {
    (
        Arc::new(NoopTokenVerifier::default()),
        Arc::new(StaticPrincipalResolver),
    )
}

/// Build the full Axum Router wired to a test database, using a caller-supplied
/// [`Authenticator`]. Lets a single test exercise the per-request auth seam
/// (cookie present/absent/expired → 200/401) while keeping the rest of
/// `AppState` identical to production. The token verifier / resolver stay the
/// defaults (only the BFF callback path uses them).
pub async fn test_app_with_authenticator(
    authenticator: Arc<dyn Authenticator>,
) -> (Router, PgPool) {
    let db = create_test_db().await;
    let config = test_config();
    let petri = PetriClient::new(&config.petri_lab_url);
    let nats = MekhanNats::connect(&config.nats_url, None)
        .await
        .expect("failed to connect to NATS — run test infra");
    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));

    let triggers = test_triggers(db.clone(), petri.clone(), nats.clone());
    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
        catalogue_repo: Arc::new(PgCatalogueRepository::new(db.clone())),
        live: LiveBroadcasts::new(),
        authenticator,
        session_store,
        oidc: None,
        token_verifier: Arc::new(NoopTokenVerifier::default()),
        principal_resolver: Arc::new(StaticPrincipalResolver),
        introspection: None,
        zitadel_mgmt: None,
        triggers,
        result_waiters: mekhan_service::triggers::ResultWaiters::new(),
        resource_store: default_resource_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
    };

    let router = build_router(state);
    (router, db)
}

/// Build the full Axum Router with a caller-supplied [`IntrospectionVerifier`]
/// wired into `AppState.introspection` (the machine-PAT Bearer path). The
/// cookie `Authenticator` is a mock that requires a cookie, so a request with
/// no valid Bearer falls through and 401s — letting tests prove both the
/// introspection success path and the fall-through.
pub async fn test_app_with_introspection(
    introspection: Arc<IntrospectionVerifier>,
) -> (Router, PgPool) {
    let db = create_test_db().await;
    let config = test_config();
    let petri = PetriClient::new(&config.petri_lab_url);
    let nats = MekhanNats::connect(&config.nats_url, None)
        .await
        .expect("failed to connect to NATS — run test infra");
    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));

    let triggers = test_triggers(db.clone(), petri.clone(), nats.clone());
    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
        catalogue_repo: Arc::new(PgCatalogueRepository::new(db.clone())),
        live: LiveBroadcasts::new(),
        authenticator: Arc::new(mock_auth::MockAuthenticator::cookie_required("cookie-user")),
        session_store,
        oidc: None,
        token_verifier: Arc::new(NoopTokenVerifier::default()),
        principal_resolver: Arc::new(StaticPrincipalResolver),
        introspection: Some(introspection),
        zitadel_mgmt: None,
        triggers,
        result_waiters: mekhan_service::triggers::ResultWaiters::new(),
        resource_store: default_resource_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
    };

    let router = build_router(state);
    (router, db)
}

/// Build the full Axum Router with a caller-supplied [`ZitadelMgmt`] wired
/// into `AppState.zitadel_mgmt` (the embedded `/api/v1/auth/tokens` broker). The
/// cookie `Authenticator` is a mock that *requires* a cookie, so a request
/// with no cookie (e.g. a Bearer PAT) 401s — letting tests prove the
/// cookie-only privilege boundary as well as the happy path.
pub async fn test_app_with_mgmt(mgmt: Arc<ZitadelMgmt>) -> (Router, PgPool) {
    let db = create_test_db().await;
    let config = test_config();
    let petri = PetriClient::new(&config.petri_lab_url);
    let nats = MekhanNats::connect(&config.nats_url, None)
        .await
        .expect("failed to connect to NATS — run test infra");
    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));

    let triggers = test_triggers(db.clone(), petri.clone(), nats.clone());
    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
        catalogue_repo: Arc::new(PgCatalogueRepository::new(db.clone())),
        live: LiveBroadcasts::new(),
        authenticator: Arc::new(mock_auth::MockAuthenticator::cookie_required("cookie-user")),
        session_store,
        oidc: None,
        token_verifier: Arc::new(NoopTokenVerifier::default()),
        principal_resolver: Arc::new(StaticPrincipalResolver),
        introspection: None,
        zitadel_mgmt: Some(mgmt),
        triggers,
        result_waiters: mekhan_service::triggers::ResultWaiters::new(),
        resource_store: default_resource_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
    };

    let router = build_router(state);
    (router, db)
}

/// Build the full Axum Router wired to a test database.
/// Requires `just -f aithericon-test-infra/justfile up` to be running.
///
/// Returns `(Router, PgPool)` — callers can use the pool for direct DB assertions.
pub async fn test_app() -> (Router, PgPool) {
    let db = create_test_db().await;
    let config = test_config();

    let petri = PetriClient::new(&config.petri_lab_url);

    let nats = MekhanNats::connect(&config.nats_url, None)
        .await
        .expect("failed to connect to NATS — run: just -f aithericon-test-infra/justfile up");

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));

    let triggers = test_triggers(db.clone(), petri.clone(), nats.clone());
    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
        catalogue_repo: Arc::new(PgCatalogueRepository::new(db.clone())),
        live: LiveBroadcasts::new(),
        authenticator: Arc::new(NoopAuthenticator::default()),
        session_store,
        oidc: None,
        token_verifier: Arc::new(NoopTokenVerifier::default()),
        principal_resolver: Arc::new(StaticPrincipalResolver),
        introspection: None,
        zitadel_mgmt: None,
        triggers,
        result_waiters: mekhan_service::triggers::ResultWaiters::new(),
        resource_store: default_resource_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
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

    let nats = MekhanNats::connect(nats_url, None)
        .await
        .unwrap_or_else(|e| panic!("failed to connect to NATS at {nats_url}: {e}"));

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));

    let triggers = test_triggers(db.clone(), petri.clone(), nats.clone());
    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
        catalogue_repo: Arc::new(PgCatalogueRepository::new(db.clone())),
        live: LiveBroadcasts::new(),
        authenticator: Arc::new(NoopAuthenticator::default()),
        session_store,
        oidc: None,
        token_verifier: Arc::new(NoopTokenVerifier::default()),
        principal_resolver: Arc::new(StaticPrincipalResolver),
        introspection: None,
        zitadel_mgmt: None,
        triggers,
        result_waiters: mekhan_service::triggers::ResultWaiters::new(),
        resource_store: default_resource_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
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

    let nats = MekhanNats::connect(nats_url, None)
        .await
        .unwrap_or_else(|e| panic!("failed to connect to NATS at {nats_url}: {e}"));

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));

    let triggers = test_triggers(db.clone(), petri.clone(), nats.clone());
    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
        catalogue_repo: Arc::new(PgCatalogueRepository::new(db.clone())),
        live: LiveBroadcasts::new(),
        authenticator: Arc::new(NoopAuthenticator::default()),
        session_store,
        oidc: None,
        token_verifier: Arc::new(NoopTokenVerifier::default()),
        principal_resolver: Arc::new(StaticPrincipalResolver),
        introspection: None,
        zitadel_mgmt: None,
        triggers,
        result_waiters: mekhan_service::triggers::ResultWaiters::new(),
        resource_store: default_resource_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
    };

    let router = build_router(state);
    (router, db)
}

/// Like [`test_app_with_petri_url`], but returns the `AppState.result_waiters`
/// `Arc` so a test can hand the **same** registry to a spawned
/// `start_lifecycle_listener`. That shared `Arc` is the seam WaitForResult
/// rides: the fire handler registers on `state.result_waiters`, the lifecycle
/// consumer resolves on the listener's `waiters` — they must be one and the
/// same. `wait_timeout_secs` is threaded into the config so a test can force a
/// fast WaitForResult timeout.
pub async fn test_app_waiters(
    nats_url: &str,
    petri_url: &str,
    wait_timeout_secs: u64,
) -> (Router, PgPool, Arc<mekhan_service::triggers::ResultWaiters>) {
    let db = create_test_db().await;
    let mut config = test_config();
    config.nats_url = nats_url.to_string();
    config.petri_lab_url = petri_url.to_string();
    config.wait_timeout_secs = wait_timeout_secs;

    let petri = PetriClient::new(petri_url);

    let nats = MekhanNats::connect(nats_url, None)
        .await
        .unwrap_or_else(|e| panic!("failed to connect to NATS at {nats_url}: {e}"));

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));

    let triggers = test_triggers(db.clone(), petri.clone(), nats.clone());
    let result_waiters = mekhan_service::triggers::ResultWaiters::new();
    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
        catalogue_repo: Arc::new(PgCatalogueRepository::new(db.clone())),
        live: LiveBroadcasts::new(),
        authenticator: Arc::new(NoopAuthenticator::default()),
        session_store,
        oidc: None,
        token_verifier: Arc::new(NoopTokenVerifier::default()),
        principal_resolver: Arc::new(StaticPrincipalResolver),
        introspection: None,
        zitadel_mgmt: None,
        triggers,
        result_waiters: result_waiters.clone(),
        resource_store: default_resource_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
    };

    let router = build_router(state);
    (router, db, result_waiters)
}

/// Like [`test_app_with_petri_url`] but also returns the `Arc<TriggerDispatcher>`
/// from the constructed `AppState`. The same `Arc` must be handed to the
/// `start_lifecycle_listener` task so the `on_instance_terminal` hook (used
/// for `SingleActiveCoalesce` follow-up fires) talks to the same dispatcher
/// the fire handler uses — two separate dispatchers would each hold their
/// own `concurrency` DashMap and never converge.
pub async fn test_app_with_petri_url_and_triggers(
    nats_url: &str,
    petri_url: &str,
) -> (
    Router,
    PgPool,
    Arc<mekhan_service::triggers::TriggerDispatcher>,
) {
    let db = create_test_db().await;
    let mut config = test_config();
    config.nats_url = nats_url.to_string();
    config.petri_lab_url = petri_url.to_string();

    let petri = PetriClient::new(petri_url);

    let nats = MekhanNats::connect(nats_url, None)
        .await
        .unwrap_or_else(|e| panic!("failed to connect to NATS at {nats_url}: {e}"));

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));

    let triggers = test_triggers(db.clone(), petri.clone(), nats.clone());
    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
        catalogue_repo: Arc::new(PgCatalogueRepository::new(db.clone())),
        live: LiveBroadcasts::new(),
        authenticator: Arc::new(NoopAuthenticator::default()),
        session_store,
        oidc: None,
        token_verifier: Arc::new(NoopTokenVerifier::default()),
        principal_resolver: Arc::new(StaticPrincipalResolver),
        introspection: None,
        zitadel_mgmt: None,
        triggers: triggers.clone(),
        result_waiters: mekhan_service::triggers::ResultWaiters::new(),
        resource_store: default_resource_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
    };

    let router = build_router(state);
    (router, db, triggers)
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

/// Like `start_test_server` but with a caller-supplied `Authenticator` —
/// lets a WS test exercise the per-request gate (e.g. workspace
/// membership) with the header-driven mock from `mock_auth.rs`.
pub async fn start_test_server_with_authenticator(
    authenticator: Arc<dyn Authenticator>,
) -> (SocketAddr, PgPool) {
    let (app, db) = test_app_with_authenticator(authenticator).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, app).into_future());
    (addr, db)
}
