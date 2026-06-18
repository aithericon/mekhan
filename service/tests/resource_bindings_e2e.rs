//! Integration tests for resources/pools as first-class RUN-TIME parameters
//! (the resource-params feature).
//!
//! These drive the live Axum router (`common::test_app`, `NoopAuthenticator` →
//! the fixed `dev-user` in `DEV_USER_WORKSPACE_ID`) against the shared test
//! Postgres. They cover the launch-time binding precedence chain, the launcher
//! run-gate, and the fork binding-default behaviour — the DB-dependent half of
//! the feature that the pure-logic unit tests (inline in
//! `service/src/compiler/requirements.rs`, `service/src/petri/binding.rs`,
//! `service/src/petri/launcher.rs`) cannot reach.
//!
//! GATING: like the rest of the `service/tests/` integration suite, these need a
//! live local stack (`just dev` — Postgres at `localhost:15439`, NATS). They
//! will fail to even build a test app offline. Run with:
//!   cargo test -p mekhan-service --test resource_bindings_e2e
//!
//! What's covered:
//!  1. precedence: per-instance override > per-workspace default > platform
//!     auto-bind, surfaced via `GET /templates/{id}/requirements` readiness +
//!     `PUT /templates/{id}/bindings`.
//!  2. run-gate: `POST /api/v1/instances` returns 422 for an unbound REQUIRED
//!     slot (no deploy attempted), and a per-instance override clears the gate
//!     (then fails downstream on the absent engine, NOT on the run-gate).
//!  3. fork: a forked template drops the source's per-workspace default bindings
//!     and path-matches a same-named/typed resource in the target workspace.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::auth::authenticator::DEV_USER_WORKSPACE_ID;
use mekhan_service::auth::model::SUBJECT_UUID_NAMESPACE;
use mekhan_service::compiler::{
    RequirementSlot, RequirementsManifest, SlotAirAddresses, SlotRole,
};

/// The `dev-user` principal id `NoopAuthenticator` resolves to (mirrors
/// `AuthUser::subject_as_uuid()`).
fn dev_user_id() -> Uuid {
    Uuid::new_v5(&SUBJECT_UUID_NAMESPACE, b"dev-user")
}

// ── HTTP helpers ────────────────────────────────────────────────────────────

async fn get_json(app: &Router, path: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn send_json(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let out = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, out)
}

use tower::ServiceExt;

// ── Seeding ─────────────────────────────────────────────────────────────────

async fn ensure_dev_workspace_membership(db: &PgPool) {
    sqlx::query(
        "INSERT INTO workspaces (id, slug, display_name) \
            VALUES ($1, 'dev-ws', 'Dev Workspace') ON CONFLICT (id) DO NOTHING",
    )
    .bind(DEV_USER_WORKSPACE_ID)
    .execute(db)
    .await
    .expect("seed dev workspace");
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
            VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING",
    )
    .bind(DEV_USER_WORKSPACE_ID)
    .bind(dev_user_id())
    .execute(db)
    .await
    .expect("seed dev membership");
}

/// Seed a resource (+ v1) of `resource_type` at `path` in a workspace or
/// (when `platform`) the platform tier. Returns the resource id.
async fn seed_resource(
    db: &PgPool,
    workspace_id: Uuid,
    resource_type: &str,
    path: &str,
    platform: bool,
) -> Uuid {
    let id = Uuid::new_v4();
    let creator = dev_user_id();
    let (scope_kind, scope_id) = if platform {
        // Platform tier — scope_id is the well-known PLATFORM_SCOPE_ID.
        ("platform", mekhan_service::models::asset::PLATFORM_SCOPE_ID)
    } else {
        ("workspace", workspace_id)
    };

    sqlx::query("INSERT INTO workspaces (id, slug, display_name) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING")
        .bind(workspace_id)
        .bind(format!("ws-{workspace_id}"))
        .bind("Test WS")
        .execute(db)
        .await
        .expect("seed ws for resource fk");

    sqlx::query(
        "INSERT INTO resources \
            (id, workspace_id, path, resource_type, display_name, latest_version, created_by, scope_kind, scope_id) \
         VALUES ($1, $2, $3, $4, $5, 1, $6, $7, $8)",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(path)
    .bind(resource_type)
    .bind(path)
    .bind(creator)
    .bind(scope_kind)
    .bind(scope_id)
    .execute(db)
    .await
    .expect("insert resource");

    sqlx::query(
        "INSERT INTO resource_versions (resource_id, version, vault_path, public_config, created_by) \
         VALUES ($1, 1, $2, $3, $4)",
    )
    .bind(id)
    .bind(format!("secret/data/test/{id}/v1"))
    .bind(json!({}))
    .bind(creator)
    .execute(db)
    .await
    .expect("insert resource_version");

    id
}

/// A requirements manifest with one REQUIRED slot whose home-baseline AIR baked
/// NOTHING (empty addresses) — so the slot is unsatisfied unless a higher tier
/// binds it. This is the shape that exercises the precedence chain + run-gate.
fn manifest_one_unbaked_slot(slot_key: &str, resource_type: &str, role: SlotRole) -> Value {
    let mut m = RequirementsManifest::default();
    m.slots.push(RequirementSlot {
        key: slot_key.to_string(),
        resource_type: resource_type.to_string(),
        role,
        required: true,
        request_shape: None,
        used_by: vec!["step1".to_string()],
    });
    // Empty address ⇒ baseline_satisfies == false (genuinely unbound at tier 4).
    m.air_addresses
        .insert(slot_key.to_string(), SlotAirAddresses::default());
    serde_json::to_value(&m).unwrap()
}

/// Insert a published template into `workspace_id` carrying `requirements_json`.
async fn seed_template(
    db: &PgPool,
    workspace_id: Uuid,
    requirements_json: Option<Value>,
    air_json: Value,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO workflow_templates
            (id, name, description, base_template_id, version, is_latest, published,
             graph, air_json, requirements_json, author_id, workspace_id, visibility,
             template_kind, lifecycle_status)
           VALUES ($1, 'Bindings T', 'requirements test', $1, 1, TRUE, TRUE,
                   $2, $3, $4, $5, $6, 'workspace', 'workflow', 'active')"#,
    )
    .bind(id)
    .bind(json!({ "nodes": [], "edges": [] }))
    .bind(&air_json)
    .bind(&requirements_json)
    .bind(dev_user_id())
    .bind(workspace_id)
    .execute(db)
    .await
    .expect("insert template");
    id
}

fn readiness_for<'a>(body: &'a Value, slot_key: &str) -> &'a Value {
    body["readiness"]
        .as_array()
        .expect("readiness array")
        .iter()
        .find(|r| r["slot"]["key"] == slot_key)
        .unwrap_or_else(|| panic!("no readiness for slot '{slot_key}': {body}"))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn requirements_null_manifest_is_launchable_with_empty_readiness() {
    // BACK-COMPAT: a template with NULL requirements_json surfaces no slots and
    // is launchable — the legacy path, byte-for-byte.
    let (app, db) = common::test_app().await;
    ensure_dev_workspace_membership(&db).await;
    let tmpl = seed_template(&db, DEV_USER_WORKSPACE_ID, None, json!({ "places": [] })).await;

    let (status, body) = get_json(&app, &format!("/api/v1/templates/{tmpl}/requirements")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body["slots"].as_array().unwrap().is_empty());
    assert!(body["readiness"].as_array().unwrap().is_empty());
    assert_eq!(body["launchable"], json!(true));
}

#[tokio::test]
async fn precedence_workspace_default_then_platform_auto_bind() {
    let (app, db) = common::test_app().await;
    ensure_dev_workspace_membership(&db).await;

    // A required DataResource slot the baseline never baked.
    let manifest = manifest_one_unbaked_slot("main_db", "postgres", SlotRole::DataResource);
    let tmpl = seed_template(
        &db,
        DEV_USER_WORKSPACE_ID,
        Some(manifest),
        json!({ "places": [] }),
    )
    .await;

    // Initially: no binding of any tier → unsatisfied, not launchable.
    let (status, body) = get_json(&app, &format!("/api/v1/templates/{tmpl}/requirements")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let r = readiness_for(&body, "main_db");
    assert_eq!(r["satisfied"], json!(false), "no tier yet: {body}");
    assert_eq!(body["launchable"], json!(false));

    // Tier 3: a single platform postgres of matching type → platform auto-bind.
    let platform_pg = seed_resource(&db, DEV_USER_WORKSPACE_ID, "postgres", "platform_db", true).await;
    let (status, body) = get_json(&app, &format!("/api/v1/templates/{tmpl}/requirements")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let r = readiness_for(&body, "main_db");
    assert_eq!(r["satisfied"], json!(true), "platform auto-bind: {body}");
    assert_eq!(r["tier"], json!("platform_auto_bind"));
    assert_eq!(r["resource_id"], json!(platform_pg.to_string()));
    assert_eq!(body["launchable"], json!(true));

    // Tier 2: a per-workspace default OUTRANKS the platform auto-bind.
    let tenant_pg = seed_resource(&db, DEV_USER_WORKSPACE_ID, "postgres", "tenant_db", false).await;
    let (status, body) = send_json(
        &app,
        "PUT",
        &format!("/api/v1/templates/{tmpl}/bindings"),
        json!({ "bindings": [{ "slot_key": "main_db", "resource_id": tenant_pg }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "PUT bindings: {body}");
    let r = readiness_for(&body, "main_db");
    assert_eq!(r["tier"], json!("workspace_default"), "ws default wins over platform: {body}");
    assert_eq!(r["resource_id"], json!(tenant_pg.to_string()));
}

#[tokio::test]
async fn run_gate_rejects_unbound_required_slot_then_override_clears_it() {
    let (app, db) = common::test_app().await;
    ensure_dev_workspace_membership(&db).await;

    let manifest = manifest_one_unbaked_slot("main_db", "postgres", SlotRole::DataResource);
    let tmpl = seed_template(
        &db,
        DEV_USER_WORKSPACE_ID,
        Some(manifest),
        json!({ "places": [], "transitions": [] }),
    )
    .await;

    // No binding of any tier → the launcher run-gate rejects with 422 BEFORE any
    // deploy is attempted.
    let (status, body) = send_json(
        &app,
        "POST",
        "/api/v1/instances",
        json!({ "template_id": tmpl, "start_tokens": [] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unbound required slot must run-gate (422): {body}"
    );

    // A per-instance override of the right type clears the run-gate. The launch
    // then fails DOWNSTREAM (no engine in the test app), which is NOT a 422 — the
    // important assertion is that we no longer hit the run-gate.
    let pg = seed_resource(&db, DEV_USER_WORKSPACE_ID, "postgres", "override_db", false).await;
    let (status, body) = send_json(
        &app,
        "POST",
        "/api/v1/instances",
        json!({
            "template_id": tmpl,
            "start_tokens": [],
            "bindings": { "main_db": pg }
        }),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an override of matching type must clear the run-gate: {status} {body}"
    );
}

#[tokio::test]
async fn override_type_mismatch_is_bad_request() {
    let (app, db) = common::test_app().await;
    ensure_dev_workspace_membership(&db).await;

    let manifest = manifest_one_unbaked_slot("main_db", "postgres", SlotRole::DataResource);
    let tmpl = seed_template(
        &db,
        DEV_USER_WORKSPACE_ID,
        Some(manifest),
        json!({ "places": [] }),
    )
    .await;

    // Bind a non-postgres resource → SlotTypeMismatch → 400 (not 422).
    let wrong = seed_resource(&db, DEV_USER_WORKSPACE_ID, "openai", "some_llm", false).await;
    let (status, body) = send_json(
        &app,
        "POST",
        "/api/v1/instances",
        json!({
            "template_id": tmpl,
            "start_tokens": [],
            "bindings": { "main_db": wrong }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "type-mismatched override must be 400: {body}"
    );
}

#[tokio::test]
async fn fork_drops_workspace_defaults_and_path_matches_target_resource() {
    use mekhan_service::auth::authenticator::DEV_ORG2_WORKSPACE_ID;

    let (app, db) = common::test_app().await;
    ensure_dev_workspace_membership(&db).await;
    // dev-user owns both dev workspaces in the noop roster — make the membership
    // explicit so the fork-target + readiness gates pass.
    sqlx::query("INSERT INTO workspaces (id, slug, display_name) VALUES ($1, 'acme', 'Acme') ON CONFLICT (id) DO NOTHING")
        .bind(DEV_ORG2_WORKSPACE_ID)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING")
        .bind(DEV_ORG2_WORKSPACE_ID)
        .bind(dev_user_id())
        .execute(&db)
        .await
        .unwrap();

    // SOURCE template in the dev workspace with a required slot + a per-workspace
    // default binding (tier 2) in the SOURCE workspace.
    let manifest = manifest_one_unbaked_slot("main_db", "postgres", SlotRole::DataResource);
    let src = seed_template(
        &db,
        DEV_USER_WORKSPACE_ID,
        Some(manifest),
        json!({ "places": [] }),
    )
    .await;
    let src_pg = seed_resource(&db, DEV_USER_WORKSPACE_ID, "postgres", "main_db", false).await;
    let (status, _) = send_json(
        &app,
        "PUT",
        &format!("/api/v1/templates/{src}/bindings"),
        json!({ "bindings": [{ "slot_key": "main_db", "resource_id": src_pg }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The TARGET workspace has its OWN resource at the same path/type — the
    // fork's path-match should pick this up, NOT the source's binding.
    let target_pg =
        seed_resource(&db, DEV_ORG2_WORKSPACE_ID, "postgres", "main_db", false).await;

    // Fork INTO the target workspace.
    let (status, forked) = send_json(
        &app,
        "POST",
        &format!("/api/v1/templates/{src}/fork"),
        json!({ "target_workspace_id": DEV_ORG2_WORKSPACE_ID }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "fork: {forked}");
    let forked_id = forked["id"].as_str().expect("forked id").to_string();
    let forked_chain = forked["base_template_id"]
        .as_str()
        .unwrap_or(&forked_id)
        .to_string();

    // (a) The source's per-workspace default did NOT ride along: there is no
    //     template_resource_bindings row for the FORKED chain in the SOURCE ws.
    let src_default_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM template_resource_bindings \
         WHERE chain_root_id = $1 AND workspace_id = $2",
    )
    .bind(Uuid::parse_str(&forked_chain).unwrap())
    .bind(DEV_USER_WORKSPACE_ID)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(src_default_rows, 0, "fork must not copy source-ws defaults");

    // (b) The path-match seeded a per-workspace default in the TARGET ws pointing
    //     at the target's own same-name/type resource.
    let seeded: Option<Uuid> = sqlx::query_scalar(
        "SELECT resource_id FROM template_resource_bindings \
         WHERE chain_root_id = $1 AND workspace_id = $2 AND slot_key = 'main_db'",
    )
    .bind(Uuid::parse_str(&forked_chain).unwrap())
    .bind(DEV_ORG2_WORKSPACE_ID)
    .fetch_optional(&db)
    .await
    .unwrap();
    assert_eq!(
        seeded,
        Some(target_pg),
        "fork should path-match the TARGET workspace's resource"
    );
}
