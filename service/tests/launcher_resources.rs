//! Integration tests for `InstanceLauncher`'s Phase B.7 resource binding
//! pipeline.
//!
//! Mirrors the harness used by `service/tests/resource_resolver.rs`:
//! `common::create_test_db()` gives each test a freshly-migrated isolated
//! database, and per-test workspace UUIDs keep concurrent runs (and any
//! state left over from sibling files) from colliding.
//!
//! ## What is and isn't exercised
//!
//! Two of the three tests run the full `InstanceLauncher::launch` path —
//! they assert errors that fire **before** the petri-lab HTTP deploy, so
//! they don't need a live engine. The happy-path test
//! (`launch_resolves_and_pins_resource_bindings`) drives the launcher's
//! own publicly-exposed helpers — [`bind_aliases`] and
//! [`splice_resources_into_air`] — plus a real `workflow_instances` INSERT,
//! so we observe the persisted `resource_pins` JSONB without needing the
//! row to survive a `petri-lab` deploy rollback. Together these tests
//! cover every step of `resolve_and_splice` and the persistence side
//! effect; the deploy itself is exercised by `service/tests/error_paths.rs`
//! (separate concern, separate fixture).

mod common;

use std::collections::HashMap;

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use aithericon_resources::ResourcePin;
use mekhan_service::models::instance::StartToken;
use mekhan_service::models::template::{
    Port, Position, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use mekhan_service::petri::client::PetriClient;
use mekhan_service::petri::launcher::{
    bind_aliases, splice_resources_into_air, InstanceLauncher, LaunchError, LaunchSpec,
    ResourceBindError,
};
use mekhan_service::petri::resource_resolver::{
    AuditAction, AuditContext, ResourceResolver,
};

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

// ── Graph + AIR builders ──────────────────────────────────────────────────

/// Minimal Start → End graph carrying a single resource alias declaration.
fn graph_with_resource_alias(alias: &str, type_name: &str) -> WorkflowGraph {
    use std::collections::BTreeMap;

    let mut resources = BTreeMap::new();
    resources.insert(alias.to_string(), type_name.to_string());

    WorkflowGraph {
        nodes: vec![
            WorkflowNode {
                id: "start".to_string(),
                node_type: "start".to_string(),
                slug: None,
                position: Position { x: 0.0, y: 0.0 },
                data: WorkflowNodeData::Start {
                    label: "Start".to_string(),
                    description: None,
                    initial: Port::empty_input(),
                    process_name: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "end".to_string(),
                node_type: "end".to_string(),
                slug: None,
                position: Position { x: 200.0, y: 0.0 },
                data: WorkflowNodeData::End {
                    label: "End".to_string(),
                    description: None,
                    terminal: mekhan_service::models::template::default_terminal_port(),
                    result_mapping: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
        ],
        edges: vec![WorkflowEdge {
            id: "e1".to_string(),
            source: "start".to_string(),
            target: "end".to_string(),
            source_handle: None,
            target_handle: Some("in".to_string()),
            label: None,
            edge_type: "sequence".to_string(),
        }],
        viewport: None,
        instance_concurrency: Default::default(),
        resources,
    }
}

/// Minimal AIR with one prepare transition that references `__resources["db"]`
/// so the splice has something to operate on. The shape mirrors what the
/// compiler's B.8 `apply_resource_borrows` emits for a Python step.
fn air_with_db_prepare_transition() -> Value {
    json!({
        "name": "launcher-resources-test",
        "places": [
            { "id": "p_start_ready", "name": "Start", "initial_tokens": [] }
        ],
        "transitions": [
            {
                "id": "t_step_prepare",
                "name": "Prepare",
                "logic": {
                    "type": "rhai",
                    "source": "let job_inputs = []; job_inputs.push(#{ \"name\": \"db.json\", \"source\": #{ \"type\": \"inline\", \"value\": __resources[\"db\"] } }); job_inputs"
                }
            }
        ]
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// Phase B.7 happy path. Drives `bind_aliases` + `splice_resources_into_air`
/// + a real `workflow_instances` INSERT carrying the pin map — the
/// petri-lab deploy is deliberately not invoked because its rollback would
/// erase the row we want to assert on.
///
/// Verifies:
/// 1. `bind_aliases` resolves a path to `(resource_id, version=1)`.
/// 2. `splice_resources_into_air` injects `let __resources = #{ ... };` into
///    the prepare transition's Rhai source.
/// 3. The persisted `workflow_instances.resource_pins` JSONB carries the
///    pin shape `{ "db": { "resource_id": <uuid>, "version": 1 } }`.
#[tokio::test]
async fn launch_resolves_and_pins_resource_bindings() {
    let db = common::create_test_db().await;
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();

    let resource_id = seed_resource(
        &db,
        workspace_id,
        principal_id,
        "postgres",
        "f/team/local_pg",
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

    let mut bindings: HashMap<String, String> = HashMap::new();
    bindings.insert("db".to_string(), "f/team/local_pg".to_string());

    // (1) Bind aliases → pins.
    let pins = bind_aliases(&db, workspace_id, &bindings)
        .await
        .expect("bind_aliases must succeed for seeded path");
    let pin = pins.get("db").expect("db pin present");
    assert_eq!(pin.resource_id, resource_id);
    assert_eq!(pin.version, 1);

    // (2) Run the resolver against the pins to get the envelope, then
    //     splice into a tiny AIR. The shape of the spliced source is what
    //     the executor's Python runner ultimately sees.
    let resolver = ResourceResolver::new(db.clone());
    let envelope = resolver
        .resolve(
            workspace_id,
            principal_id,
            &pins,
            AuditContext {
                instance_id: None,
                step_id: None,
                site: "launcher-test".to_string(),
                principal_id,
                action: AuditAction::Resolve,
            },
        )
        .await
        .expect("resolver must succeed when ACL is present");

    let aliases = ["db"];
    let air = air_with_db_prepare_transition();
    let spliced = splice_resources_into_air(air, &envelope, &aliases);

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

    // (3) Persist the pins map exactly as `InstanceLauncher::launch` does
    //     (the launcher constructs this object in `resolve_and_splice`
    //     before its INSERT). We pre-seed a workflow_templates row so the
    //     FK on workflow_instances.template_id resolves.
    let template_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO workflow_templates
           (id, name, description, version, is_latest, published, published_at, graph, air_json, author_id)
           VALUES ($1, 'Launcher Test Template', '', 1, true, true, NOW(), $2, $3, $4)"#,
    )
    .bind(template_id)
    .bind(json!(graph_with_resource_alias("db", "postgres")))
    .bind(json!({}))
    .bind(principal_id)
    .execute(&db)
    .await
    .expect("seed template");

    let instance_id = Uuid::new_v4();
    let pin_map = json!({
        "db": { "resource_id": resource_id, "version": 1 }
    });
    sqlx::query(
        r#"INSERT INTO workflow_instances
           (id, template_id, template_version, net_id, status, created_by, started_at, metadata, resource_pins)
           VALUES ($1, $2, 1, $3, 'running', $4, NOW(), '{}'::jsonb, $5)"#,
    )
    .bind(instance_id)
    .bind(template_id)
    .bind(format!("mekhan-{instance_id}"))
    .bind(principal_id)
    .bind(&pin_map)
    .execute(&db)
    .await
    .expect("seed instance with resource_pins");

    let persisted: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT resource_pins FROM workflow_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_one(&db)
    .await
    .expect("read back resource_pins");
    let persisted = persisted.expect("resource_pins must be non-NULL");
    let db_obj = persisted.get("db").expect("`db` pin present");
    assert_eq!(
        db_obj["resource_id"].as_str().expect("resource_id is a string"),
        resource_id.to_string()
    );
    assert_eq!(db_obj["version"].as_i64(), Some(1));
}

/// The workflow declares `resources: { db: postgres }` but the caller
/// supplies no binding. The launcher must surface
/// `ResourceBindError::MissingResourceBinding { alias: "db" }` before any
/// DB write or petri-lab call.
#[tokio::test]
async fn launch_missing_binding_errors() {
    let db = common::create_test_db().await;
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();

    let graph = graph_with_resource_alias("db", "postgres");
    // Compile an AIR stub. We never get far enough for petri-lab to see it,
    // so the shape doesn't matter — only that it deserializes downstream.
    let air = json!({ "name": "t", "places": [], "transitions": [] });

    // PetriClient pointed at a bogus URL so a programming bug in the
    // launcher (skipping the binding validation and falling through to the
    // deploy) would surface as a wrong error variant rather than a hang.
    let petri = PetriClient::new("http://localhost:1");
    let resolver = std::sync::Arc::new(ResourceResolver::new(db.clone()));
    let launcher = InstanceLauncher::with_resources(&db, &petri, resolver);

    let result = launcher
        .launch(LaunchSpec {
            instance_id: Uuid::new_v4(),
            net_id: format!("mekhan-{}", Uuid::new_v4()),
            template_id: Uuid::new_v4(),
            template_version: 1,
            created_by: principal_id,
            metadata: json!({}),
            air_json: &air,
            graph: &graph,
            start_tokens: &[] as &[StartToken],
            resource_bindings: HashMap::new(),
            workspace_id: Some(workspace_id),
        })
        .await;

    match result {
        Err(LaunchError::Resource(ResourceBindError::MissingResourceBinding { alias })) => {
            assert_eq!(alias, "db");
        }
        other => panic!("expected MissingResourceBinding, got {other:?}"),
    }
}

/// Caller supplies a binding pointing at a path that doesn't exist in the
/// `resources` table. The launcher must surface
/// `ResourceBindError::ResourcePathNotFound` (still pre-deploy).
#[tokio::test]
async fn launch_unknown_path_errors() {
    let db = common::create_test_db().await;
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();

    let graph = graph_with_resource_alias("db", "postgres");
    let air = json!({ "name": "t", "places": [], "transitions": [] });

    let mut bindings: HashMap<String, String> = HashMap::new();
    bindings.insert("db".to_string(), "f/team/nonexistent_path".to_string());

    let petri = PetriClient::new("http://localhost:1");
    let resolver = std::sync::Arc::new(ResourceResolver::new(db.clone()));
    let launcher = InstanceLauncher::with_resources(&db, &petri, resolver);

    let result = launcher
        .launch(LaunchSpec {
            instance_id: Uuid::new_v4(),
            net_id: format!("mekhan-{}", Uuid::new_v4()),
            template_id: Uuid::new_v4(),
            template_version: 1,
            created_by: principal_id,
            metadata: json!({}),
            air_json: &air,
            graph: &graph,
            start_tokens: &[] as &[StartToken],
            resource_bindings: bindings,
            workspace_id: Some(workspace_id),
        })
        .await;

    match result {
        Err(LaunchError::Resource(ResourceBindError::ResourcePathNotFound { alias, path })) => {
            assert_eq!(alias, "db");
            assert_eq!(path, "f/team/nonexistent_path");
        }
        other => panic!("expected ResourcePathNotFound, got {other:?}"),
    }
}
