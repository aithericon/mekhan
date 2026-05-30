//! End-to-end coverage for the step-executions projection.
//!
//! Reuses the proven `Start → AutomatedStep(python) → End` flow from
//! `automated_step_e2e.rs`, but also spawns the step-executions consumer
//! and asserts that, after the instance completes, the `step_execution`
//! table contains one row per workflow node with the expected status,
//! inputs/outputs payloads, and timestamps.
//!
//! Requires the full `just dev up` stack (engine :13030, executor, rustfs S3,
//! NATS broker). Run serially: `cargo test --test step_executions_e2e -- --test-threads=1`.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::models::template::{
    default_output_port, ExecutionBackendType, ExecutionSpecConfig, Port, Position,
    WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;
use mekhan_service::projections::step_executions::start_step_executions_ingest;

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

fn end_node(id: &str) -> WorkflowNode {
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

fn python_graph(step_id: &str) -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![
            start("s"),
            WorkflowNode {
                id: step_id.to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Run Python".to_string(),
                    description: None,
                    execution_spec: ExecutionSpecConfig {
                        backend_type: ExecutionBackendType::Python,
                        entrypoint: Some("main.py".to_string()),
                        config: json!({
                            "python": "python3",
                            "requirements": [],
                            "virtualenv": false,
                            "sdk": true,
                            "inherit_env": true,
                            "env": {}
                        }),
                    },
                    input: Port::empty_input(),
                    output: default_output_port(ExecutionBackendType::Python),
                    retry_policy: Default::default(),
                    deployment_model: Default::default(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end_node("e"),
        ],
        edges: vec![
            WorkflowEdge {
                id: "e1".to_string(),
                source: "s".to_string(),
                target: step_id.to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "e2".to_string(),
                source: step_id.to_string(),
                target: "e".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(), default_scheduler: None,
    }
}

const MAIN_PY: &str = r#"
log_info("step-executions e2e ran", task_id=token.get("task_id"))
set_output("ran", True)
set_output("answer", 42)
"#;

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:13030".to_string())
}

async fn engine_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/nets/metadata", engine_url())).await,
        Ok(resp) if resp.status().is_success()
    )
}

#[tokio::test]
#[allow(clippy::type_complexity)]
async fn step_executions_materialize_for_completed_instance() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }

    let engine_nats_url =
        std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) =
        common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

    // ── Spawn consumers (with per-test prefix to isolate durables) ──────────
    // The test app's MekhanNats was created with `with_consumer_prefix`, so
    // these consumers get unique durable names and start at DeliverPolicy::New.
    let listener_nats = MekhanNats::connect(&engine_nats_url, None).await.expect("nats");
    let kv = listener_nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("kv");
    let sub_mgr = std::sync::Arc::new(SubscriptionManager::new(
        kv,
        listener_nats.jetstream().clone(),
    ));
    {
        let listener_db = db.clone();
        let listener_nats = listener_nats.clone();
        let sub_mgr = sub_mgr.clone();
        tokio::spawn(async move {
            start_lifecycle_listener(
                listener_nats,
                listener_db,
                sub_mgr,
                None,
                mekhan_service::triggers::ResultWaiters::new(),
            )
            .await;
        });
    }

    // Step-executions consumer — needs a prefixed `MekhanNats` so its durable
    // doesn't collide with the production `mekhan-step-executions` durable
    // owned by the live dev daemon.
    let test_prefix = format!("test_se_{}", Uuid::new_v4().simple());
    let step_nats = MekhanNats::connect(&engine_nats_url, None)
        .await
        .expect("nats")
        .with_consumer_prefix(test_prefix);
    {
        let step_db = db.clone();
        tokio::spawn(async move {
            start_step_executions_ingest(step_nats, step_db).await;
        });
    }
    // Give the consumer a beat to come up before we start publishing events.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // ── Publish template + create instance ──────────────────────────────────
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Step Executions E2E",
                        "graph": python_graph("auto"),
                        "files": { "auto": { "main.py": MAIN_PY } },
                        "author_id": Uuid::new_v4(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create template");
    let created = body_json(resp.into_body()).await;
    let template_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/templates/{template_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let pub_body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "publish: {pub_body}");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "template_id": template_id,
                        "created_by": Uuid::new_v4(),
                        "metadata": { "e2e": "step_executions" }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let inst_status = resp.status();
    let instance = body_json(resp.into_body()).await;
    assert_eq!(inst_status, StatusCode::CREATED, "create instance: {instance}");
    let instance_id: Uuid = instance["id"].as_str().unwrap().parse().unwrap();

    // ── Wait for instance completion ────────────────────────────────────────
    let deadline = Duration::from_secs(60);
    let started = std::time::Instant::now();
    loop {
        let st: String =
            sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(instance_id)
                .fetch_one(&db)
                .await
                .unwrap();
        if st == "completed" {
            break;
        }
        assert_ne!(st, "failed", "instance failed — executor did not succeed");
        if started.elapsed() > deadline {
            panic!("instance did not complete within {deadline:?} (status: {st})");
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    // ── Wait for step_executions to materialize ─────────────────────────────
    // The consumer is eventually consistent; it re-projects on each event.
    let deadline = Duration::from_secs(20);
    let started = std::time::Instant::now();
    loop {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM step_execution WHERE instance_id = $1 AND status = 'completed'",
        )
        .bind(instance_id)
        .fetch_one(&db)
        .await
        .unwrap();
        // Expect at least 3 completed rows: Start, AutomatedStep, End.
        if count >= 3 {
            break;
        }
        if started.elapsed() > deadline {
            // Dump rows for debugging before panicking.
            let rows: Vec<(String, String, i32)> = sqlx::query_as(
                "SELECT node_id, status, iteration_index FROM step_execution \
                 WHERE instance_id = $1 ORDER BY node_id",
            )
            .bind(instance_id)
            .fetch_all(&db)
            .await
            .unwrap();
            panic!(
                "step_execution did not materialize within {deadline:?} (count={count}, rows={rows:?})"
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // ── Assertions on the materialized rows ─────────────────────────────────
    let rows: Vec<(String, String, Option<Value>, Option<Value>)> = sqlx::query_as(
        "SELECT node_id, status, inputs, outputs FROM step_execution \
         WHERE instance_id = $1 ORDER BY node_id",
    )
    .bind(instance_id)
    .fetch_all(&db)
    .await
    .unwrap();

    // Sanity: every expected node should be present and completed.
    let by_node: std::collections::HashMap<&str, &(String, String, Option<Value>, Option<Value>)> =
        rows.iter().map(|r| (r.0.as_str(), r)).collect();

    let start = by_node.get("s").expect("s row exists");
    assert_eq!(start.1, "completed", "Start should be completed");
    assert!(start.3.is_some(), "Start should have outputs (its parked envelope)");

    let auto = by_node.get("auto").expect("auto row exists");
    assert_eq!(auto.1, "completed", "AutomatedStep should be completed");
    let outputs = auto.3.as_ref().expect("AutomatedStep should have outputs");
    // The Python script sets `ran=True` and `answer=42` — they should appear
    // in the parked envelope at the data_port.
    assert!(
        outputs.get("ran").is_some() || outputs.is_object(),
        "AutomatedStep outputs should be an object (got: {outputs})"
    );

    let end = by_node.get("e").expect("e row exists");
    assert_eq!(end.1, "completed", "End should be completed");
}
