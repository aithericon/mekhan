//! Launcher × resources integration after the alias-layer drop.
//!
//! The launcher is now resource-unaware — it parameterizes, INSERTs, and
//! deploys, full stop. Resources are resolved + spliced at publish time
//! by the publish handler, and the persisted AIR already carries the
//! `let __resources = #{ ... };` declarations on every prepare transition
//! that needs them.
//!
//! These tests cover the publish-time half of the contract:
//!
//! 1. `ResourceResolver::resolve_known` projects a [`KnownResources`] map
//!    into the per-name JSON envelope (`{ name: { ...inline..., ...secret
//!    refs... } }`), writes an audit row per name, and returns the same
//!    shape the legacy `resolve` returned. Verified end-to-end against a
//!    seeded Postgres resource + ACL grant.
//! 2. `splice_resources_into_air` inserts a single `let __resources` at the
//!    top of every prepare transition whose Rhai logic references one of
//!    the resource names. Idempotent and skips non-prepare transitions.
//!
//! There is no longer a "launch missing binding" path to assert — the
//! launcher cannot fail on resources because it does not touch them. The
//! publish path raises bad-request when a known resource can't be resolved
//! (resolver error → ApiError::bad_request); that surface is exercised by
//! the existing resource_resolver.rs suite.

mod common;

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use mekhan_service::compiler::resource_refs::{KnownResource, KnownResources};
use mekhan_service::petri::resource_resolver::{splice_resources_into_air, ResourceResolver};

// ── Seeding helpers (mirrors resource_resolver.rs) ────────────────────────

async fn seed_resource(
    db: &PgPool,
    workspace_id: Uuid,
    creator: Uuid,
    resource_type: &str,
    path: &str,
    public_config: serde_json::Value,
) -> Uuid {
    let resource_id = Uuid::new_v4();
    let vault_path = format!(
        "aithericon/resources/{}/{}/v1",
        workspace_id, resource_id
    );

    sqlx::query(
        "INSERT INTO resources \
            (id, workspace_id, path, resource_type, display_name, latest_version, created_by) \
         VALUES ($1, $2, $3, $4, $5, 1, $6)",
    )
    .bind(resource_id)
    .bind(workspace_id)
    .bind(path)
    .bind(resource_type)
    .bind(path)
    .bind(creator)
    .execute(db)
    .await
    .expect("insert resources row");

    sqlx::query(
        "INSERT INTO resource_versions \
            (resource_id, version, vault_path, public_config, created_by) \
         VALUES ($1, 1, $2, $3, $4)",
    )
    .bind(resource_id)
    .bind(&vault_path)
    .bind(&public_config)
    .bind(creator)
    .execute(db)
    .await
    .expect("insert resource_versions row");

    resource_id
}

async fn grant_acl(
    db: &PgPool,
    resource_id: Uuid,
    principal_id: Uuid,
    permission: &str,
    granted_by: Uuid,
) {
    sqlx::query(
        "INSERT INTO resource_acl \
            (resource_id, principal_id, principal_kind, permission, granted_by) \
         VALUES ($1, $2, 'user', $3, $4)",
    )
    .bind(resource_id)
    .bind(principal_id)
    .bind(permission)
    .bind(granted_by)
    .execute(db)
    .await
    .expect("insert resource_acl row");
}

// ── AIR fixture ───────────────────────────────────────────────────────────

/// Minimal AIR with one prepare transition referencing `__resources["local_pg"]`
/// so the splice has something to operate on. Mirrors what the compiler's
/// resource-borrow apply emits for a Python step.
fn air_with_local_pg_prepare_transition() -> Value {
    json!({
        "name": "publish-resources-test",
        "places": [
            { "id": "p_start_ready", "name": "Start", "initial_tokens": [] }
        ],
        "transitions": [
            {
                "id": "t_step_prepare",
                "name": "Prepare",
                "logic": {
                    "type": "rhai",
                    "source": "let job_inputs = []; job_inputs.push(#{ \"name\": \"local_pg.json\", \"source\": #{ \"type\": \"inline\", \"value\": __resources[\"local_pg\"] } }); job_inputs"
                }
            }
        ]
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// Happy path: `resolve_known` projects a workspace-known resource into an
/// envelope carrying inline public fields + a secret template; the spliced
/// AIR carries one `let __resources` declaration and the host/secret are
/// both visible inside it.
#[tokio::test]
async fn publish_resolves_and_splices_known_resources() {
    let db = common::create_test_db().await;
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();

    let resource_id = seed_resource(
        &db,
        workspace_id,
        principal_id,
        "postgres",
        "local_pg",
        json!({
            "host": "db.example.internal",
            "port": 5432,
            "database": "app",
            "username": "app_rw",
            "sslmode": "require"
        }),
    )
    .await;
    grant_acl(&db, resource_id, principal_id, "read", principal_id).await;

    let mut known = KnownResources::new();
    known.insert(
        "local_pg".to_string(),
        KnownResource {
            id: resource_id,
            type_name: "postgres".to_string(),
            latest_version: 1,
        },
    );

    let resolver = ResourceResolver::new(db.clone());
    let envelope = resolver
        .resolve_known(workspace_id, principal_id, &known, None)
        .await
        .expect("resolve_known must succeed when ACL is present");

    // Splice into the AIR.
    let names: Vec<&str> = known.keys().map(String::as_str).collect();
    let air = air_with_local_pg_prepare_transition();
    let spliced = splice_resources_into_air(air, &envelope, &names);

    let spliced_source = spliced["transitions"][0]["logic"]["source"]
        .as_str()
        .expect("spliced transition logic source");
    assert!(
        spliced_source.contains("let __resources = #{"),
        "spliced AIR must declare __resources, got: {spliced_source}"
    );
    assert!(
        spliced_source.contains("\"host\": \"db.example.internal\""),
        "spliced AIR must inline the public host field, got: {spliced_source}"
    );
    assert!(
        spliced_source.contains("{{secret:aithericon/resources/"),
        "spliced AIR must carry the secret template, got: {spliced_source}"
    );

    // Exactly one audit row per resolved name with site="publish".
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT resource_id, action, site FROM resource_audit ORDER BY id ASC",
    )
    .fetch_all(&db)
    .await
    .expect("read audit rows");
    assert_eq!(rows.len(), 1, "expected one audit row for one resolved name");
    assert_eq!(rows[0].0, resource_id);
    assert_eq!(rows[0].1, "resolve");
    assert_eq!(rows[0].2, "publish");
}

/// Empty known map round-trips an untouched AIR. Guards against accidental
/// splicing when a workflow doesn't reference any workspace resources.
#[tokio::test]
async fn publish_with_empty_known_map_skips_splice() {
    let db = common::create_test_db().await;
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();
    let resolver = ResourceResolver::new(db.clone());

    let known = KnownResources::new();
    let envelope = resolver
        .resolve_known(workspace_id, principal_id, &known, None)
        .await
        .expect("empty known must resolve to an empty envelope");
    assert!(envelope.as_object().map(|o| o.is_empty()).unwrap_or(false));

    let air = air_with_local_pg_prepare_transition();
    let names: Vec<&str> = known.keys().map(String::as_str).collect();
    let spliced = splice_resources_into_air(air.clone(), &envelope, &names);
    // No splicing happens on an empty name list.
    let source = spliced["transitions"][0]["logic"]["source"]
        .as_str()
        .unwrap();
    assert!(!source.contains("let __resources"));
}
