//! Integration tests for the Phase B.9 Resource CRUD endpoints.
//!
//! Drives every endpoint through the live Axum router with an
//! `InMemoryResourceStore` substituted for the Vault backend so we can
//! assert the secret-side-effect without standing up a real Vault. Talks
//! to the shared test Postgres (`localhost:5599`) via the same
//! `common::create_test_db()` helper the rest of the suite uses.
//!
//! Each test owns its workspace (`Uuid::nil()` is the v1 default; we
//! explicitly pass it everywhere so the assertion shape stays stable when
//! workspaces get real). Routes that need an authenticated principal are
//! exercised through the `NoopAuthenticator` test-app fixture, which
//! supplies a fixed dev user — sufficient because the handlers only read
//! `user.subject_as_uuid()` for `created_by` / audit fields.

mod common;

use std::sync::Arc;

use aithericon_resources::InMemoryResourceStore;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::auth::authenticator::NoopAuthenticator;
use mekhan_service::auth::bff::session::{PgSessionStore, SessionStore};
use mekhan_service::auth::dev::NoopTokenVerifier;
use mekhan_service::auth::resolver::StaticPrincipalResolver;
use mekhan_service::catalogue::repository::PgCatalogueRepository;
use mekhan_service::causality::live::LiveBroadcasts;
use mekhan_service::handlers::resources::vault_path_for;
use mekhan_service::nats::MekhanNats;
use mekhan_service::petri::client::PetriClient;
use mekhan_service::s3::ArtifactStore;
use mekhan_service::triggers::TriggerDispatcher;
use mekhan_service::yjs::manager::YjsManager;
use mekhan_service::yjs::persistence::YjsPersistence;
use mekhan_service::{build_router, AppState};

// ── Test fixture ──────────────────────────────────────────────────────────

/// Same shape as `common::test_app()` but exposes the `InMemoryResourceStore`
/// so tests can assert what the handlers wrote to it. Inlined rather than
/// adding another helper to `common/mod.rs` because the assertion side
/// channel only matters here.
async fn resources_test_app() -> (Router, PgPool, Arc<InMemoryResourceStore>) {
    let db = common::create_test_db().await;
    let config = common::test_config();
    let petri = PetriClient::new(&config.petri_lab_url);
    let nats = MekhanNats::connect(&config.nats_url, None)
        .await
        .expect("failed to connect to NATS — run test infra");
    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));
    let triggers = Arc::new(TriggerDispatcher::new(
        db.clone(),
        petri.clone(),
        nats.clone(),
    ));
    let resource_store = Arc::new(InMemoryResourceStore::new());

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
        triggers,
        result_waiters: mekhan_service::triggers::ResultWaiters::new(),
        resource_store: resource_store.clone(),
        secret_store: Arc::new(aithericon_secrets::InMemorySecretStore::new(
            std::collections::HashMap::new(),
        )),
        resource_resolver: Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
        runner_nats_signer: Arc::new(
            mekhan_service::runners_nats::RunnerNatsSigner::generate_ephemeral(),
        ),
        runner_presence: mekhan_service::presence::RunnerPresence::new(),
        human_presence: mekhan_service::presence::HumanPresence::new(),
        fleet: mekhan_service::fleet::FleetLiveness::new(),
        asset_resolver: Arc::new(mekhan_service::petri::asset_resolver::AssetResolver::new(
            db.clone(),
        )),
        email: mekhan_service::notify::email::log_mailer(),
    };

    (build_router(state), db, resource_store)
}

// ── Body helpers ──────────────────────────────────────────────────────────

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn pg_config_full() -> Value {
    json!({
        "host": "db.example.internal",
        "port": 5432,
        "database": "app",
        "username": "app_rw",
        "password": "hunter2",
        "sslmode": "require"
    })
}

async fn post_create(app: &Router, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/resources")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp.into_body()).await;
    (status, body)
}

async fn post_apply(app: &Router, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/resources/apply")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp.into_body()).await;
    (status, body)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_returns_only_the_seeded_default_group_for_fresh_workspace() {
    // A fresh workspace is not empty: migration 20240144 seeds the per-workspace
    // `default` worker-group capacity (doc 24 D1). It must be the ONLY item.
    let (app, _db, _store) = resources_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/resources")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(body["total"].as_i64(), Some(1));
    assert_eq!(items[0]["path"], "default");
    assert_eq!(items[0]["resource_type"], "capacity");
}

#[tokio::test]
async fn types_returns_registry_descriptors() {
    let (app, _db, _store) = resources_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/resources/types")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    for required in ["postgres", "openai", "slack", "s3", "google_oauth"] {
        assert!(
            names.contains(&required),
            "missing built-in type `{required}` in /types response: {names:?}"
        );
    }
    // Schema is present and non-empty.
    let pg = body
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "postgres")
        .unwrap();
    assert!(pg["schema"]["properties"]["host"].is_object());
    assert_eq!(pg["secret_fields"][0], "password");
}

#[tokio::test]
async fn create_persists_public_config_and_writes_secret_via_store() {
    let (app, db, store) = resources_test_app().await;
    let (status, body) = post_create(
        &app,
        json!({
            "path": "main_pg",
            "resource_type": "postgres",
            "display_name": "Main PG",
            "config": pg_config_full(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {body}");

    let id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();
    assert_eq!(body["latest_version"].as_i64(), Some(1));
    assert_eq!(body["resource_type"], "postgres");

    // GET returns the same record with `password` listed as redacted.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/resources/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let detail = body_json(resp.into_body()).await;
    assert_eq!(detail["public_config"]["host"], "db.example.internal");
    assert_eq!(detail["public_config"]["port"], 5432);
    // Public config must not leak the secret value.
    assert!(detail["public_config"].get("password").is_none());
    // Secret fields are surfaced as names so the UI can render a redacted
    // input — but never the value.
    let redacted: Vec<&str> = detail["redacted_secret_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(redacted, vec!["password"]);

    // Confirm the secret made it into the store at the launcher-deterministic
    // vault path.
    let workspace_id =
        sqlx::query_scalar::<_, Uuid>("SELECT workspace_id FROM resources WHERE id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    let vault_path = vault_path_for(workspace_id, id, 1);
    let secrets = store
        .get_version(&vault_path)
        .await
        .expect("vault path must be populated");
    assert_eq!(secrets["password"], "hunter2");
}

#[tokio::test]
async fn create_rejects_unknown_type() {
    let (app, _db, _store) = resources_test_app().await;
    let (status, body) = post_create(
        &app,
        json!({
            "path": "bogus",
            "resource_type": "definitely_not_real",
            "config": {},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap_or("")
        .contains("unknown resource_type"));
}

#[tokio::test]
async fn create_rejects_bad_path_format() {
    let (app, _db, _store) = resources_test_app().await;
    let (status, body) = post_create(
        &app,
        json!({
            "path": "team/main_pg", // missing namespace prefix
            "resource_type": "postgres",
            "config": pg_config_full(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap_or("")
        .contains("must be a snake_case identifier"));
}

#[tokio::test]
async fn create_rejects_schema_violation() {
    let (app, _db, _store) = resources_test_app().await;
    let (status, body) = post_create(
        &app,
        json!({
            "path": "partial_pg",
            "resource_type": "postgres",
            // `host` and `database` are required; deliberately omitted.
            "config": {
                "port": 5432,
                "username": "u",
                "password": "p"
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    let msg = body["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("required config field") && msg.contains("host"),
        "expected required-field error; got: {msg}"
    );
}

#[tokio::test]
async fn get_returns_404_for_soft_deleted() {
    let (app, db, _store) = resources_test_app().await;
    let (_, body) = post_create(
        &app,
        json!({
            "path": "soon_to_die",
            "resource_type": "postgres",
            "config": pg_config_full(),
        }),
    )
    .await;
    let id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();

    // Soft-delete directly in the DB — equivalent to DELETE /api/v1/resources/{id}
    // but bypassing the handler so we can assert GET's filter independent
    // of the delete handler.
    sqlx::query("UPDATE resources SET deleted_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/resources/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_bumps_version_and_writes_new_vault_path() {
    let (app, db, store) = resources_test_app().await;
    let (_, body) = post_create(
        &app,
        json!({
            "path": "rotates",
            "resource_type": "postgres",
            "config": pg_config_full(),
        }),
    )
    .await;
    let id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/v1/resources/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "config": {
                            "host": "db-new.example.internal",
                            "port": 5432,
                            "database": "app",
                            "username": "app_rw",
                            "password": "new-secret"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let summary = body_json(resp.into_body()).await;
    assert_eq!(summary["latest_version"].as_i64(), Some(2));

    // Both v1 and v2 vault paths must be populated; v1 is intact for any
    // pinned instance, v2 holds the new secret.
    let workspace_id =
        sqlx::query_scalar::<_, Uuid>("SELECT workspace_id FROM resources WHERE id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    let v1 = vault_path_for(workspace_id, id, 1);
    let v2 = vault_path_for(workspace_id, id, 2);
    assert!(store.get_version(&v1).await.is_some(), "v1 must remain");
    let v2_secrets = store.get_version(&v2).await.expect("v2 was written");
    assert_eq!(v2_secrets["password"], "new-secret");

    // Both `resource_versions` rows must exist.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM resource_versions WHERE resource_id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn delete_marks_row_and_preserves_versions() {
    let (app, db, _store) = resources_test_app().await;
    let (_, body) = post_create(
        &app,
        json!({
            "path": "del_me",
            "resource_type": "postgres",
            "config": pg_config_full(),
        }),
    )
    .await;
    let id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/resources/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let deleted_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM resources WHERE id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(deleted_at.is_some(), "deleted_at must be set");

    // `resource_versions` survive the soft delete — pinned instances need them.
    let versions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM resource_versions WHERE resource_id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(versions, 1);
}

#[tokio::test]
async fn rotate_bumps_version_without_changing_schema() {
    let (app, db, store) = resources_test_app().await;
    let (_, body) = post_create(
        &app,
        json!({
            "path": "rotate_me",
            "resource_type": "postgres",
            "config": pg_config_full(),
        }),
    )
    .await;
    let id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/resources/{id}/rotate"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "config": {
                            "host": "db.example.internal",
                            "port": 5432,
                            "database": "app",
                            "username": "app_rw",
                            "password": "rotated-secret"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let summary = body_json(resp.into_body()).await;
    assert_eq!(summary["latest_version"].as_i64(), Some(2));

    let workspace_id =
        sqlx::query_scalar::<_, Uuid>("SELECT workspace_id FROM resources WHERE id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    let v2 = vault_path_for(workspace_id, id, 2);
    let secrets = store.get_version(&v2).await.expect("v2 was written");
    assert_eq!(secrets["password"], "rotated-secret");
}

#[tokio::test]
async fn audit_returns_one_row_per_write_action() {
    let (app, _db, _store) = resources_test_app().await;
    let (_, created) = post_create(
        &app,
        json!({
            "path": "audited",
            "resource_type": "postgres",
            "config": pg_config_full(),
        }),
    )
    .await;
    let id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();

    // Rotate, then update display_name (no version bump, no audit row), then
    // soft-delete. Expected audit verbs: ["create", "rotate", "delete"].
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/resources/{id}/rotate"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "config": {
                            "host": "h",
                            "port": 5432,
                            "database": "d",
                            "username": "u",
                            "password": "rot"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/resources/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/resources/{id}/audit"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "got audit items: {items:?}");
    let mut actions: Vec<&str> = items
        .iter()
        .map(|i| i["action"].as_str().unwrap())
        .collect();
    actions.sort();
    assert_eq!(actions, vec!["create", "delete", "rotate"]);
}

#[tokio::test]
async fn apply_upserts_idempotently_by_content_hash() {
    let (app, db, store) = resources_test_app().await;

    // 1. First apply of a new path → CREATED at v1.
    let (status, body) = post_apply(
        &app,
        json!({
            "path": "ci_pg",
            "resource_type": "postgres",
            "config": pg_config_full(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "apply create failed: {body}");
    assert_eq!(body["action"], "created");
    assert_eq!(body["resource"]["latest_version"].as_i64(), Some(1));
    let id = Uuid::parse_str(body["resource"]["id"].as_str().unwrap()).unwrap();

    // 2. Re-apply the SAME config, but with object keys in a different order.
    //    The canonical hash must ignore key order → UNCHANGED, still v1.
    let (status, body) = post_apply(
        &app,
        json!({
            "path": "ci_pg",
            "resource_type": "postgres",
            "config": {
                "sslmode": "require",
                "password": "hunter2",
                "database": "app",
                "username": "app_rw",
                "port": 5432,
                "host": "db.example.internal"
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "re-apply failed: {body}");
    assert_eq!(
        body["action"], "unchanged",
        "reordered keys must hash equal"
    );
    assert_eq!(body["resource"]["latest_version"].as_i64(), Some(1));

    // 3. Apply a CHANGED config (rotated secret only) → UPDATED to v2, and the
    //    new secret lands at the new version's vault path. This proves a secret
    //    change is detected even though secrets never round-trip on a read.
    let (status, body) = post_apply(
        &app,
        json!({
            "path": "ci_pg",
            "resource_type": "postgres",
            "config": {
                "host": "db.example.internal",
                "port": 5432,
                "database": "app",
                "username": "app_rw",
                "password": "rotated-pw",
                "sslmode": "require"
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "apply update failed: {body}");
    assert_eq!(body["action"], "updated");
    assert_eq!(body["resource"]["latest_version"].as_i64(), Some(2));

    let workspace_id =
        sqlx::query_scalar::<_, Uuid>("SELECT workspace_id FROM resources WHERE id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
    let secrets = store
        .get_version(&vault_path_for(workspace_id, id, 2))
        .await
        .expect("v2 vault path must be populated");
    assert_eq!(secrets["password"], "rotated-pw");

    // 4. Re-apply the v2 config again → UNCHANGED, still v2 (no churn).
    let (status, body) = post_apply(
        &app,
        json!({
            "path": "ci_pg",
            "resource_type": "postgres",
            "config": {
                "host": "db.example.internal",
                "port": 5432,
                "database": "app",
                "username": "app_rw",
                "password": "rotated-pw",
                "sslmode": "require"
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["action"], "unchanged");
    assert_eq!(body["resource"]["latest_version"].as_i64(), Some(2));
}

#[tokio::test]
async fn apply_rejects_type_change_on_existing_path() {
    let (app, _db, _store) = resources_test_app().await;
    let (status, _) = post_apply(
        &app,
        json!({
            "path": "typed",
            "resource_type": "postgres",
            "config": pg_config_full(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Same path, different type → 409 (type is immutable across apply).
    let (status, body) = post_apply(
        &app,
        json!({
            "path": "typed",
            "resource_type": "openai",
            "config": { "base_url": "https://api.openai.com", "api_key": "sk-x" },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "got {body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("already exists with type"),
        "expected a type-immutability conflict message; got: {body}"
    );
}
