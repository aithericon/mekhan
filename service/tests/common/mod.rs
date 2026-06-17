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

pub mod fake_upstream;
pub mod mock_auth;
pub mod model_runner_fixture;
pub mod nats_spy;
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

/// Per-process-unique durable-consumer prefix for every `MekhanNats` the test
/// harness builds.
///
/// Every helper below connects a `MekhanNats` and stuffs it into `AppState`.
/// That handle is publish-only today (the in-router `TriggerDispatcher`
/// publishes trigger signals to `PETRI_GLOBAL`; nothing here binds a durable
/// consumer off `state.nats`). But a test's in-process mekhan that DID pull a
/// consumer off a bare handle would bind the SAME durable (`mekhan-lifecycle`,
/// `mekhan-causality-ingest`, …) as a running `just dev` daemon — JetStream
/// then load-balances completion events across both, and the test misses
/// `NetCompleted` (the "instance did not complete/finish" timeouts).
///
/// `with_consumer_prefix` renames only the durable (see
/// `MekhanNats::durable_name`) and flips fresh durables to
/// `DeliverPolicy::New`; publishing is by subject and is unaffected, and the
/// engine/executor never reference mekhan's durable names. So scoping every
/// harness handle to a unique prefix is safe and lets the suite run alongside
/// a live stack. The suffix is per-test-binary-process (one `OnceLock` per
/// test crate) so all handles a single test builds share one cursor namespace
/// while different processes stay isolated.
fn harness_consumer_prefix() -> &'static str {
    use std::sync::OnceLock;
    static PREFIX: OnceLock<String> = OnceLock::new();
    PREFIX.get_or_init(|| format!("test_harness_{}", uuid::Uuid::new_v4().simple()))
}

/// Connect a `MekhanNats` for the test harness, scoped to a unique
/// per-process durable-consumer prefix so the in-process mekhan never shares a
/// durable cursor on `PETRI_GLOBAL` with a live `just dev` daemon. Use this in
/// place of a bare `MekhanNats::connect(url, None)` in every harness helper.
async fn connect_harness_nats(
    nats_url: &str,
) -> Result<MekhanNats, mekhan_service::nats::NatsError> {
    Ok(MekhanNats::connect(nats_url, None)
        .await?
        .with_consumer_prefix(harness_consumer_prefix()))
}

/// Re-point the nil "Default Workspace" `default` worker group to the partition
/// the live dev executor is already bound to — the OTHER half of running the
/// executor-completion e2e suite against the shared `just dev` stack.
///
/// Every executor job routes through a *group partition* whose token is the
/// workspace's `default` worker-group capacity-resource id (the compiler stamps
/// `executor_namespace = executor-<wire>-grp/<id>`). The dev executor enrolls
/// ONCE against the dev DB and binds that DB's id. A fresh `create_test_db()`
/// runs migration `20240144_default_worker_group`, which seeds a `default`
/// capacity with a fresh **`gen_random_uuid()`** id — a partition NO running
/// worker is bound to. So the compiler stamps that random id and the job lands
/// on a subject nobody drains → the instance hangs at `running`.
///
/// We can't just create-if-missing (the migration already created it, so the
/// idempotent seeder no-ops). Instead DELETE the migration's nil-workspace
/// `default` capacity (FKs cascade to its version/acl/audit rows) and re-create
/// it through the normal seed path pinned to the executor's `routing_partition`
/// — so the test's compiler stamps the SAME partition the running executor
/// drains. Templates with no workspace land in the nil workspace
/// (`user.workspace_id.unwrap_or_else(Uuid::nil)`), the one the dev executor
/// enrolled against, so the nil workspace is the only one we need to fix.
///
/// The partition comes from `TEST_WORKER_DEFAULT_PARTITION` — set it to the
/// slot's `.dev/.../executor/worker/identity.json` `routing_partition`
/// (`just dev::e2e-partition` does this). UNSET → no-op (offline/unit runs are
/// untouched; the completion tests simply remain unrunnable, as before).
pub async fn seed_dev_worker_partition(state: &AppState) {
    let Some(partition) = std::env::var("TEST_WORKER_DEFAULT_PARTITION")
        .ok()
        .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
    else {
        return;
    };
    let ws = uuid::Uuid::nil();
    // Drop the migration-seeded random-id default capacity (cascade removes its
    // resource_versions / resource_acl / resource_audit rows). A fresh test DB
    // has no enrolled workers, so nothing references its id.
    if let Err(e) = sqlx::query(
        "DELETE FROM resources \
         WHERE workspace_id = $1 AND path = 'default' AND resource_type = 'capacity'",
    )
    .bind(ws)
    .execute(&state.db)
    .await
    {
        eprintln!("seed_dev_worker_partition: delete migration default failed: {e:?}");
        return;
    }
    if let Err(e) =
        mekhan_service::worker_groups::ensure_default_worker_group(state, ws, Some(partition)).await
    {
        // Best-effort: a failure here just leaves the test to surface the hang
        // with its own (clearer) "instance did not complete" timeout.
        eprintln!("seed_dev_worker_partition({partition}) failed: {e:?}");
    }
}

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
        analytics: Default::default(),
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
        // Tests don't mint LiveKit viewer tokens; the endpoint returns 503 unset.
        livekit: None,
        // Default to presigned-302 serving (tests don't exercise the proxy path).
        proxy_s3_reads: false,
        auth: AuthConfig::default(),
        // No config-seeded platform bootstrap tokens in tests.
        bootstrap: mekhan_service::config::BootstrapConfig::default(),
        // Tests publish demos explicitly through the API; the startup
        // seeder is off so each test owns its template ids.
        demos: mekhan_service::config::DemosConfig::default(),
        // Invite email defaults to log-mode (no SMTP); accept-link base is the
        // dev SPA origin.
        email: mekhan_service::config::EmailConfig::default(),
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

/// READ-side secret store for tests — empty in-memory unless Vault env is set.
/// Mirrors `main.rs`'s selection for `secret_store`.
fn default_secret_store() -> Arc<dyn aithericon_secrets::SecretStore> {
    match aithericon_secrets::VaultSecretStore::from_env() {
        Some(vss) => Arc::new(vss),
        None => Arc::new(aithericon_secrets::InMemorySecretStore::new(
            std::collections::HashMap::new(),
        )),
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
    let nats = connect_harness_nats(&config.nats_url)
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
        secret_store: default_secret_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        runner_nats_signer: std::sync::Arc::new(
            mekhan_service::runners_nats::RunnerNatsSigner::generate_ephemeral(),
        ),
        runner_presence: mekhan_service::presence::RunnerPresence::new(),
        human_presence: mekhan_service::presence::HumanPresence::new(),
        fleet: mekhan_service::fleet::FleetLiveness::new(),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
        email: mekhan_service::notify::email::log_mailer(),
        user_provisioner: None,
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
    let nats = connect_harness_nats(&config.nats_url)
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
        secret_store: default_secret_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        runner_nats_signer: std::sync::Arc::new(
            mekhan_service::runners_nats::RunnerNatsSigner::generate_ephemeral(),
        ),
        runner_presence: mekhan_service::presence::RunnerPresence::new(),
        human_presence: mekhan_service::presence::HumanPresence::new(),
        fleet: mekhan_service::fleet::FleetLiveness::new(),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
        email: mekhan_service::notify::email::log_mailer(),
        user_provisioner: None,
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
    let nats = connect_harness_nats(&config.nats_url)
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
        secret_store: default_secret_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        runner_nats_signer: std::sync::Arc::new(
            mekhan_service::runners_nats::RunnerNatsSigner::generate_ephemeral(),
        ),
        runner_presence: mekhan_service::presence::RunnerPresence::new(),
        human_presence: mekhan_service::presence::HumanPresence::new(),
        fleet: mekhan_service::fleet::FleetLiveness::new(),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
        email: mekhan_service::notify::email::log_mailer(),
        user_provisioner: None,
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

    let nats = connect_harness_nats(&config.nats_url)
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
        secret_store: default_secret_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        runner_nats_signer: std::sync::Arc::new(
            mekhan_service::runners_nats::RunnerNatsSigner::generate_ephemeral(),
        ),
        runner_presence: mekhan_service::presence::RunnerPresence::new(),
        human_presence: mekhan_service::presence::HumanPresence::new(),
        fleet: mekhan_service::fleet::FleetLiveness::new(),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
        email: mekhan_service::notify::email::log_mailer(),
        user_provisioner: None,
    };

    seed_dev_worker_partition(&state).await;
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

    let nats = connect_harness_nats(nats_url)
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
        secret_store: default_secret_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        runner_nats_signer: std::sync::Arc::new(
            mekhan_service::runners_nats::RunnerNatsSigner::generate_ephemeral(),
        ),
        runner_presence: mekhan_service::presence::RunnerPresence::new(),
        human_presence: mekhan_service::presence::HumanPresence::new(),
        fleet: mekhan_service::fleet::FleetLiveness::new(),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
        email: mekhan_service::notify::email::log_mailer(),
        user_provisioner: None,
    };

    seed_dev_worker_partition(&state).await;
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

    let nats = connect_harness_nats(nats_url)
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
        secret_store: default_secret_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        runner_nats_signer: std::sync::Arc::new(
            mekhan_service::runners_nats::RunnerNatsSigner::generate_ephemeral(),
        ),
        runner_presence: mekhan_service::presence::RunnerPresence::new(),
        human_presence: mekhan_service::presence::HumanPresence::new(),
        fleet: mekhan_service::fleet::FleetLiveness::new(),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
        email: mekhan_service::notify::email::log_mailer(),
        user_provisioner: None,
    };

    seed_dev_worker_partition(&state).await;
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

    let nats = connect_harness_nats(nats_url)
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
        secret_store: default_secret_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        runner_nats_signer: std::sync::Arc::new(
            mekhan_service::runners_nats::RunnerNatsSigner::generate_ephemeral(),
        ),
        runner_presence: mekhan_service::presence::RunnerPresence::new(),
        human_presence: mekhan_service::presence::HumanPresence::new(),
        fleet: mekhan_service::fleet::FleetLiveness::new(),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
        email: mekhan_service::notify::email::log_mailer(),
        user_provisioner: None,
    };

    seed_dev_worker_partition(&state).await;
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

    let nats = connect_harness_nats(nats_url)
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
        secret_store: default_secret_store(),
        resource_resolver: std::sync::Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        runner_nats_signer: std::sync::Arc::new(
            mekhan_service::runners_nats::RunnerNatsSigner::generate_ephemeral(),
        ),
        runner_presence: mekhan_service::presence::RunnerPresence::new(),
        human_presence: mekhan_service::presence::HumanPresence::new(),
        fleet: mekhan_service::fleet::FleetLiveness::new(),
        asset_resolver: std::sync::Arc::new(
            mekhan_service::petri::asset_resolver::AssetResolver::new(db.clone()),
        ),
        email: mekhan_service::notify::email::log_mailer(),
        user_provisioner: None,
    };

    seed_dev_worker_partition(&state).await;
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
