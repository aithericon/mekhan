//! End-to-end proof that HTTP nodes resolve `{{ slug.field }}` borrows.
//!
//! Publishes `Start → AutomatedStep(python, "pyprod") → AutomatedStep(http) →
//! End`. The Python step parks `result = "borrowed-path-ok"`; the HTTP step's
//! URL is `<mock>/check/{{ pyprod.result }}`. A wiremock server (reachable by
//! the live executor daemon on localhost) only answers 200 at the *resolved*
//! path `/check/borrowed-path-ok`. So the net completing — and the mock having
//! received that exact path — is runtime proof that the compiler synthesized
//! the read-arc, staged `pyprod.json`, and the executor Tera-rendered the URL.
//!
//! Requires the full `just dev up` stack (engine :3030, executor, rustfs S3),
//! with mekhan-service AND the executor rebuilt from this branch. Run serially.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::models::template::{
    default_output_port, ExecutionBackendType, ExecutionSpecConfig, Port, Position, WorkflowEdge,
    WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;

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

/// Python producer that natively parks `result` (the declared Python output
/// field). Its parked envelope is what the HTTP step borrows as `pyprod`.
const PRODUCER_PY: &str = r#"log_info("http-borrow producer ran", task_id=token.get("task_id"))
result = "borrowed-path-ok"
"#;

/// `Start → python("pyprod") → http(url has {{ pyprod.result }}) → End`.
fn graph(http_url: &str) -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![
            start("s"),
            WorkflowNode {
                id: "pyprod".to_string(),
                node_type: "automated_step".to_string(),
                slug: Some("pyprod".to_string()),
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Produce".to_string(),
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
                    channels: Vec::new(),
                    requirements: None,
                    asset_bindings: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "httpcall".to_string(),
                node_type: "automated_step".to_string(),
                slug: Some("httpcall".to_string()),
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Call".to_string(),
                    description: None,
                    execution_spec: ExecutionSpecConfig {
                        backend_type: ExecutionBackendType::Http,
                        entrypoint: None,
                        config: json!({
                            "url": http_url,
                            "method": "GET",
                        }),
                    },
                    input: Port::empty_input(),
                    output: default_output_port(ExecutionBackendType::Http),
                    retry_policy: Default::default(),
                    deployment_model: Default::default(),
                    channels: Vec::new(),
                    requirements: None,
                    asset_bindings: Vec::new(),
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
                target: "pyprod".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "e2".to_string(),
                source: "pyprod".to_string(),
                target: "httpcall".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "e3".to_string(),
                source: "httpcall".to_string(),
                target: "e".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    }
}

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
async fn http_step_resolves_slug_field_borrow_in_url() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }

    // Mock endpoint the live executor will call. Only the *resolved* path
    // answers 200; anything else falls through to wiremock's default 404.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/check/borrowed-path-ok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    let http_url = format!("{}/check/{{{{ pyprod.result }}}}", server.uri());

    let engine_nats_url = std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) = common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

    let listener_nats = MekhanNats::connect(&engine_nats_url, None)
        .await
        .expect("nats");
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

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "HTTP Borrow E2E",
                        "graph": graph(&http_url),
                        "files": { "pyprod": { "main.py": PRODUCER_PY } },
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
                        "metadata": { "e2e": "http_borrow" }
                    })
                    .to_string(),
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
    let instance_id: Uuid = instance["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(instance["status"], "running");

    let deadline = Duration::from_secs(90);
    let started = std::time::Instant::now();
    let final_status = loop {
        let st: String = sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .unwrap();
        if st == "completed" || st == "failed" {
            break st;
        }
        if started.elapsed() > deadline {
            panic!("instance did not finish within {deadline:?} (status: {st})");
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    };

    // Inspect what the executor actually requested — the strongest assertion:
    // the resolved path must have arrived at the mock.
    let received = server.received_requests().await.unwrap_or_default();
    let paths: Vec<String> = received.iter().map(|r| r.url.path().to_string()).collect();
    assert!(
        paths.iter().any(|p| p == "/check/borrowed-path-ok"),
        "executor never requested the resolved path; mock saw: {paths:?} \
         (a literal '/check/{{{{ pyprod.result }}}}' here means the borrow did not render)"
    );

    assert_eq!(
        final_status, "completed",
        "instance did not complete (HTTP step should have gotten 200 at the resolved path)"
    );
}
