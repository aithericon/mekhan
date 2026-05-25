//! End-to-end coverage for an Inline AutomatedStep — the most common
//! production node, previously with zero runtime proof.
//!
//! Publishes a `Start → AutomatedStep(python) → End` template (the inline
//! `main.py` is staged to rustfs S3 at publish), creates an instance, and
//! asserts the real executor runs the Python job and the net completes.
//!
//! Requires the full `just dev up` stack (engine :3030, executor, rustfs S3
//! :9005 sharing the dev NATS broker). Run serially (`--test-threads=1`) —
//! it shares the live engine/executor.

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

/// `Start → AutomatedStep(python, main.py) → End`.
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
            end("e"),
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
        viewport: None, instance_concurrency: Default::default(), resources: Default::default(),
    }
}

/// Minimal Aithericon-SDK Python step: the runner injects `set_output` /
/// `log_info` / `token` (the inbound control-token) as globals.
const MAIN_PY: &str = r#"# `token` is injected; just exercise it lightly here.
log_info("automated-step e2e ran", task_id=token.get("task_id"))
set_output("ran", True)
set_output("answer", 42)
"#;

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:3030".to_string())
}

async fn engine_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/nets/metadata", engine_url())).await,
        Ok(resp) if resp.status().is_success()
    )
}

#[tokio::test]
async fn automated_step_python_runs_through_executor() {
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

    let listener_nats = MekhanNats::connect(&engine_nats_url, None).await.expect("nats");
    let kv = listener_nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("kv");
    let sub_mgr = std::sync::Arc::new(SubscriptionManager::new(
        kv,
        listener_nats.jetstream().clone(),
    ));
    let listener_db = db.clone();
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

    // Create with the inline script attached to the step node, then publish
    // (stages main.py + the generated _aithericon_io to rustfs S3).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "AutomatedStep E2E",
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
                .uri(format!("/api/templates/{template_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let pub_body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "publish: {pub_body}");

    // Create an instance — deploys + Running.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "template_id": template_id,
                        "created_by": Uuid::new_v4(),
                        "metadata": { "e2e": "automated_step" }
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
    assert_eq!(instance["status"], "running");

    // The real executor must pull main.py from S3, run python3, emit the
    // output token, and the net must run through to End.
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
        assert_ne!(st, "failed", "instance failed — executor job did not succeed");
        if started.elapsed() > deadline {
            panic!("instance did not complete within {deadline:?} (status: {st})");
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// `Start → AutomatedStep(python) → End` where the step writes its output
/// via **native top-level assignment** (`result = "swept"`), not
/// `set_output`. The runner's post-exec sweep (commit `c61bb8c`) must
/// promote the declared output from globals into the executor's terminal
/// status, the engine must thread it onto the AutomatedStep's data port,
/// and the step-executions projector must materialize it into
/// `step_execution.outputs`.
///
/// Covers the regression caught in `f3145be` where `lower.rs` had been
/// hardcoding `"outputs": []` in the prepare-transition Rhai — with that
/// bug, the runner would never know `result` was declared, the sweep
/// would skip it, and `outputs` would land empty.
const NATIVE_OUTPUT_MAIN_PY: &str = r#"# Native output assignment — declared port field is "result".
log_info("native-output e2e ran", task_id=token.get("task_id"))
result = "swept-by-implicit-output-writes"
"#;

#[tokio::test]
async fn automated_step_python_native_assignment_reaches_step_executions() {
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

    let listener_nats = MekhanNats::connect(&engine_nats_url, None).await.expect("nats");
    let kv = listener_nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("kv");
    let sub_mgr = std::sync::Arc::new(SubscriptionManager::new(
        kv,
        listener_nats.jetstream().clone(),
    ));
    let listener_db = db.clone();
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

    // Spawn a prefixed step-executions consumer so the projector writes
    // `step_execution` rows we can query — without colliding with the
    // live dev daemon's durable `mekhan-step-executions`.
    let step_prefix = format!("test_auto_native_{}", Uuid::new_v4().simple());
    let step_nats = MekhanNats::connect(&engine_nats_url, None)
        .await
        .expect("nats")
        .with_consumer_prefix(step_prefix);
    {
        let step_db = db.clone();
        tokio::spawn(async move {
            start_step_executions_ingest(step_nats, step_db).await;
        });
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let step_id = "auto-native";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "AutomatedStep Native-Output E2E",
                        "graph": python_graph(step_id),
                        "files": { step_id: { "main.py": NATIVE_OUTPUT_MAIN_PY } },
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
                .uri(format!("/api/templates/{template_id}/publish"))
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
                .uri("/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "template_id": template_id,
                        "created_by": Uuid::new_v4(),
                        "metadata": { "e2e": "automated_step_native_output" }
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
    assert_eq!(instance["status"], "running");

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
        assert_ne!(st, "failed", "instance failed — executor job did not succeed");
        if started.elapsed() > deadline {
            panic!("instance did not complete within {deadline:?} (status: {st})");
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    // The step-executions projector folds the executor's terminal-status
    // outputs into `step_execution.outputs`. With the implicit sweep
    // wired correctly, the declared `result` field must surface here —
    // not as null, and not as the raw user code text.
    //
    // The lifecycle consumer (which flips `workflow_instances.status` to
    // `completed`) and the step-executions consumer pull independently
    // from `petri.events.>`, so the row + outputs land eventually after
    // the instance shows completed — poll instead of fetch_one.
    let projection_deadline = Duration::from_secs(30);
    let projection_started = std::time::Instant::now();
    let outputs = loop {
        let row: Option<Option<serde_json::Value>> = sqlx::query_scalar(
            "SELECT outputs FROM step_execution WHERE instance_id = $1 AND node_id = $2",
        )
        .bind(instance_id)
        .bind(step_id)
        .fetch_optional(&db)
        .await
        .unwrap();
        if let Some(Some(outputs)) = row {
            break outputs;
        }
        if projection_started.elapsed() > projection_deadline {
            panic!(
                "step_execution.outputs for node {step_id} did not materialize within \
                 {projection_deadline:?} after instance completed (row present: {})",
                row.is_some(),
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    assert_eq!(
        outputs.get("result").and_then(|v| v.as_str()),
        Some("swept-by-implicit-output-writes"),
        "expected the natively-assigned `result` to reach step_execution.outputs, got: {outputs}"
    );
}
