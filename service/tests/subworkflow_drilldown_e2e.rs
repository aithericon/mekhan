//! End-to-end test for the SubWorkflow instance drill-in feature.
//!
//! Proves the full chain: a parent instance that spawns a sub-workflow child
//! net gets a first-class child `workflow_instances` row registered by the
//! causality ingest (with parent linkage), the net_id-keyed step-execution
//! projection then materializes the child's steps for free, and the
//! `GET /api/v1/instances/{id}/children` endpoint surfaces the child.
//!
//! Requires the full `just dev up` stack (engine on :13030 sharing the dev
//! NATS broker) + Postgres. Run serially with `--test-threads=1` (shared
//! engine + broker). Like its sibling `subworkflow_e2e.rs`, it PANICS rather
//! than skips when the engine is down.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::causality::ingest::start_causality_ingest;
use mekhan_service::causality::live::LiveBroadcasts;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::models::template::{
    default_subworkflow_output_port, Port, Position, VersionPin, WorkflowEdge, WorkflowGraph,
    WorkflowNode, WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;
use mekhan_service::projections::step_executions::start_step_executions_ingest;

// ---------------------------------------------------------------------------
// Helpers (self-contained, mirroring subworkflow_e2e.rs)
// ---------------------------------------------------------------------------

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn pos() -> Position {
    Position { x: 0.0, y: 0.0 }
}

fn start(id: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "start".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port::empty_input(),
            process_name: None,
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn end(id: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "end".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::End {
            label: "End".to_string(),
            description: None,
            terminal: mekhan_service::models::template::default_terminal_port(),
            result_mapping: Vec::new(),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn subworkflow(id: &str, child_family: Uuid, pin: VersionPin) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "sub_workflow".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::SubWorkflow {
            label: "Call Child".to_string(),
            description: None,
            template_id: child_family,
            version_pin: pin,
            input_mapping: Vec::new(),
            output: default_subworkflow_output_port(),
            input_contract: mekhan_service::models::template::default_subworkflow_input_contract(),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn edge(id: &str, source: &str, target: &str) -> WorkflowEdge {
    WorkflowEdge {
        id: id.to_string(),
        source: source.to_string(),
        target: target.to_string(),
        source_handle: None,
        target_handle: Some("in".to_string()),
        label: None,
        join: None,
        edge_type: "sequence".to_string(),
    }
}

fn child_graph(tag: &str) -> WorkflowGraph {
    let s = format!("{tag}start");
    let e = format!("{tag}end");
    WorkflowGraph {
        nodes: vec![start(&s), end(&e)],
        edges: vec![edge("ce", &s, &e)],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    }
}

/// `Start → SubWorkflow("sub") → End` — the parent workflow. The SubWorkflow
/// node id is `sub`, so the spawn transition is `t_sub_spawn` and the
/// registered child's `parent_node_id` must be `sub`.
fn parent_graph(child_family: Uuid, pin: VersionPin) -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![
            start("pstart"),
            subworkflow("sub", child_family, pin),
            end("pend"),
        ],
        edges: vec![edge("pe1", "pstart", "sub"), edge("pe2", "sub", "pend")],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    }
}

async fn create_with_graph(app: &axum::Router, name: &str, graph: &WorkflowGraph) -> Uuid {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": name, "graph": graph, "author_id": Uuid::new_v4() })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create {name}");
    let created = body_json(resp.into_body()).await;
    created["id"].as_str().unwrap().parse().unwrap()
}

async fn publish(app: &axum::Router, id: Uuid) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/templates/{id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "publish {id}: {body}");
}

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:13030".to_string())
}

async fn engine_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/nets/metadata", engine_url())).await,
        Ok(resp) if resp.status().is_success()
    )
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::type_complexity)]
async fn subworkflow_child_is_registered_and_drillable() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }

    let engine_nats_url = std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) = common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

    // Shared sub-manager / nats for the listener + consumers.
    let base_nats = MekhanNats::connect(&engine_nats_url, None)
        .await
        .expect("nats");
    let kv = base_nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("kv");
    let sub_mgr = std::sync::Arc::new(SubscriptionManager::new(kv, base_nats.jetstream().clone()));

    // Lifecycle listener — drives the parent (and child) instance status in the DB.
    {
        let n = base_nats.clone();
        let d = db.clone();
        let s = sub_mgr.clone();
        tokio::spawn(async move {
            start_lifecycle_listener(
                n,
                d,
                s,
                None,
                mekhan_service::triggers::ResultWaiters::new(),
            )
            .await;
        });
    }

    // Causality ingest (prefixed durable) — this is where child registration
    // runs. Without a prefix it would collide with the live dev daemon's
    // `mekhan-causality-ingest` durable.
    {
        let prefix = format!("test_caus_{}", Uuid::new_v4().simple());
        let n = MekhanNats::connect(&engine_nats_url, None)
            .await
            .expect("nats")
            .with_consumer_prefix(prefix);
        let d = db.clone();
        let s = sub_mgr.clone();
        tokio::spawn(async move {
            start_causality_ingest(
                n,
                d,
                s,
                LiveBroadcasts::new(),
                None,
                "mekhan-artifacts".to_string(),
            )
            .await;
        });
    }

    // Step-executions ingest (prefixed durable) — proves the child's steps
    // materialize for free once the child instance row exists.
    {
        let prefix = format!("test_se_{}", Uuid::new_v4().simple());
        let n = MekhanNats::connect(&engine_nats_url, None)
            .await
            .expect("nats")
            .with_consumer_prefix(prefix);
        let d = db.clone();
        tokio::spawn(async move {
            start_step_executions_ingest(n, d).await;
        });
    }

    // Let the consumers come up before publishing events.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Child + parent, both published.
    let child = create_with_graph(&app, "Drilldown Child", &child_graph("ddc")).await;
    publish(&app, child).await;
    let parent = create_with_graph(
        &app,
        "Drilldown Parent",
        &parent_graph(child, VersionPin::Latest),
    )
    .await;
    publish(&app, parent).await;

    // Create the parent instance (deploys + runs).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "template_id": parent, "created_by": Uuid::new_v4() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let inst_status = resp.status();
    let instance = body_json(resp.into_body()).await;
    assert_eq!(
        inst_status,
        StatusCode::CREATED,
        "create instance: {instance}"
    );
    let parent_id: Uuid = instance["id"].as_str().unwrap().parse().unwrap();

    // Parent runs to completion (spawns child, joins reply, reaches End).
    let deadline = Duration::from_secs(45);
    let started = std::time::Instant::now();
    loop {
        let status: String =
            sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(parent_id)
                .fetch_one(&db)
                .await
                .unwrap();
        if status == "completed" {
            break;
        }
        assert_ne!(status, "failed", "parent instance failed");
        if started.elapsed() > deadline {
            panic!("parent did not complete within {deadline:?} (status: {status})");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // The causality ingest must have registered exactly one child instance,
    // linked to the parent via the SubWorkflow node `sub`.
    let mut child_row: Option<(Uuid, Option<String>, Uuid, Option<Uuid>)> = None;
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(20) {
        let rows: Vec<(Uuid, Option<String>, Uuid, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, parent_node_id, template_id, root_instance_id \
             FROM workflow_instances WHERE parent_instance_id = $1",
        )
        .bind(parent_id)
        .fetch_all(&db)
        .await
        .unwrap();
        if !rows.is_empty() {
            assert_eq!(
                rows.len(),
                1,
                "exactly one child expected, got {}",
                rows.len()
            );
            child_row = Some(rows.into_iter().next().unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let (child_instance_id, parent_node_id, child_template_id, root_instance_id) =
        child_row.expect("child instance was never registered");
    assert_eq!(parent_node_id.as_deref(), Some("sub"), "parent_node_id");
    assert_eq!(
        child_template_id, child,
        "child template id resolved to the published child"
    );
    assert_eq!(
        root_instance_id,
        Some(parent_id),
        "root points at the top-level parent"
    );

    // The child's steps materialize for free via the net_id-keyed projection.
    let started = std::time::Instant::now();
    let mut child_step_count = 0i64;
    while started.elapsed() < Duration::from_secs(15) {
        child_step_count =
            sqlx::query_scalar("SELECT COUNT(*) FROM step_execution WHERE instance_id = $1")
                .bind(child_instance_id)
                .fetch_one(&db)
                .await
                .unwrap();
        if child_step_count > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(child_step_count > 0, "child steps never materialized");

    // The children endpoint surfaces the child for the drill-in UI.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/instances/{parent_id}/children"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "children endpoint");
    let children = body_json(resp.into_body()).await;
    let arr = children.as_array().expect("children is an array");
    assert_eq!(arr.len(), 1, "one child via endpoint: {children}");
    assert_eq!(arr[0]["parent_node_id"], "sub");
    assert_eq!(
        arr[0]["id"].as_str().unwrap(),
        child_instance_id.to_string()
    );
    assert_eq!(arr[0]["template_name"], "Drilldown Child");

    // The child must NOT appear in the top-level instances list — it is a
    // sub-run, reachable only via the parent's drill-in. The parent must.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/instances?per_page=100")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "list instances");
    let list = body_json(resp.into_body()).await;
    let ids: Vec<String> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ids.contains(&parent_id.to_string()),
        "parent must appear in the top-level list"
    );
    assert!(
        !ids.contains(&child_instance_id.to_string()),
        "child must be excluded from the top-level list"
    );
}
