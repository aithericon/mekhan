//! End-to-end coverage for ParallelSplit/Join — live engine forks the token,
//! each branch resolves, the join fires once, the net completes.
//!
//! Requires `just dev up` (engine :3030 sharing the dev NATS broker). Run
//! serially (`--test-threads=1`) — the lifecycle listener writes back to the
//! shared `workflow_instances` table.

mod common;

use std::sync::Arc;
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
use mekhan_service::nats::MekhanNats;

fn engine_nats_url() -> String {
    std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url())
}

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:3030".to_string())
}

async fn engine_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/nets/metadata", engine_url())).await,
        Ok(r) if r.status().is_success()
    )
}

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Best-effort delete of this test's per-prefix durables on the shared
/// streams. Each prefix is uniquely UUID-derived so a panicked test only
/// leaks its own durables until `just dev reset`.
async fn cleanup_durables(nats: &MekhanNats) {
    let prefix = match nats.consumer_prefix() {
        Some(p) => p,
        None => return,
    };
    for (stream_name, base) in [
        ("PETRI_GLOBAL", "mekhan-causality-ingest"),
        ("PETRI_GLOBAL", "mekhan-lifecycle"),
        ("HUMAN_REQUESTS", "mekhan-human-task-ingest"),
    ] {
        if let Ok(stream) = nats.jetstream().get_stream(stream_name).await {
            let _ = stream.delete_consumer(&format!("{prefix}_{base}")).await;
        }
    }
}

struct TaskHandle(tokio::task::AbortHandle);
impl Drop for TaskHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Spawn the two consumers a HumanTask-bearing e2e needs: lifecycle (status
/// updates) + causality ingest (projects HumanTaskRequest signals into the
/// `hpi_tasks` table the test polls).
async fn spawn_consumers(nats: MekhanNats, db: sqlx::PgPool) -> (TaskHandle, TaskHandle) {
    let kv = nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("create KV");
    let sub_mgr = Arc::new(SubscriptionManager::new(kv, nats.jetstream().clone()));

    let c_nats = nats.clone();
    let c_db = db.clone();
    let c_sub = sub_mgr.clone();
    let c_live = LiveBroadcasts::new();
    let causality = tokio::spawn(async move {
        start_causality_ingest(
            c_nats,
            c_db,
            c_sub,
            c_live,
            None,
            "mekhan-artifacts".to_string(),
        )
        .await;
    });

    let l_nats = nats.clone();
    let l_db = db.clone();
    let l_sub = sub_mgr.clone();
    let lifecycle = tokio::spawn(async move {
        start_lifecycle_listener(
            l_nats,
            l_db,
            l_sub,
            None,
            mekhan_service::triggers::ResultWaiters::new(),
        )
        .await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    (
        TaskHandle(causality.abort_handle()),
        TaskHandle(lifecycle.abort_handle()),
    )
}

async fn wait_for_completion(db: &sqlx::PgPool, id: Uuid, timeout: Duration) {
    let start = std::time::Instant::now();
    loop {
        let st: String = sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap();
        if st == "completed" {
            return;
        }
        if st == "failed" {
            let result: Option<Value> =
                sqlx::query_scalar("SELECT result FROM workflow_instances WHERE id = $1")
                    .bind(id)
                    .fetch_one(db)
                    .await
                    .unwrap();
            panic!("instance {id} reached `failed` (result: {result:?})");
        }
        if start.elapsed() > timeout {
            panic!("instance {id} did not complete within {timeout:?} (last: {st})");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// `Start → ParallelSplit → (taskA, taskB) → Join (mode: all) → End`.
/// Branch nodes are HumanTasks whose `task_id` and place are picked up from
/// the spawned hpi_tasks row and completed via `POST /api/v1/tasks/{id}/complete`.
fn parallel_graph() -> Value {
    json!({
        "nodes": [
            { "id": "s", "type": "start", "position": { "x": 0, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": { "id": "in", "label": "In", "fields": [] } } },
            { "id": "split", "type": "parallel_split", "position": { "x": 200, "y": 0 },
              "data": { "type": "parallel_split", "label": "Fork" } },
            // HumanTask requires at least one step with at least one block —
            // the engine's HumanTaskHandler rejects empty `steps` (see
            // engine/core-engine/.../human_handlers.rs:78). Without a block
            // here the effect fails immediately and no hpi_tasks row appears.
            { "id": "ta", "type": "human_task", "position": { "x": 400, "y": -80 },
              "data": { "type": "human_task", "label": "Task A",
                        "taskTitle": "Do A",
                        "steps": [{
                            "id": "s1", "title": "Approve",
                            "blocks": [{ "type": "input", "field": {
                                "name": "ok", "label": "OK",
                                "kind": "checkbox", "required": true } }]
                        }] } },
            { "id": "tb", "type": "human_task", "position": { "x": 400, "y": 80 },
              "data": { "type": "human_task", "label": "Task B",
                        "taskTitle": "Do B",
                        "steps": [{
                            "id": "s1", "title": "Approve",
                            "blocks": [{ "type": "input", "field": {
                                "name": "ok", "label": "OK",
                                "kind": "checkbox", "required": true } }]
                        }] } },
            { "id": "join", "type": "join", "position": { "x": 600, "y": 0 },
              "data": { "type": "join", "label": "Join", "mode": "all" } },
            { "id": "e", "type": "end", "position": { "x": 800, "y": 0 },
              "data": { "type": "end", "label": "Done",
                        "resultMapping": [
                            { "targetField": "joined", "expression": "true" }
                        ] } }
        ],
        "edges": [
            { "id": "e_in",  "source": "s",     "target": "split",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e_fa",  "source": "split", "target": "ta",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e_fb",  "source": "split", "target": "tb",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e_ja",  "source": "ta",    "target": "join",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e_jb",  "source": "tb",    "target": "join",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e_out", "source": "join",  "target": "e",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

async fn publish_and_start(app: &axum::Router, graph: Value) -> (Uuid, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Parallel E2E",
                        "graph": graph,
                        "author_id": Uuid::new_v4(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create template");
    let template_id = body_json(resp.into_body()).await["id"]
        .as_str()
        .unwrap()
        .to_string();

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
    let body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "publish: {body}");

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
                        "metadata": { "e2e": "parallel" }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let s = resp.status();
    let inst = body_json(resp.into_body()).await;
    assert_eq!(s, StatusCode::CREATED, "create instance: {inst}");
    let id = Uuid::parse_str(inst["id"].as_str().unwrap()).unwrap();
    let net_id = inst["net_id"].as_str().expect("net_id").to_string();
    (id, net_id)
}

/// Wait until BOTH human tasks for this net appear in `hpi_tasks`, then return
/// their ids. Times out with a diagnostic if only one shows up (would indicate
/// the ParallelSplit only forked one branch).
async fn wait_for_two_tasks(db: &sqlx::PgPool, net_id: &str, timeout: Duration) -> Vec<String> {
    let start = std::time::Instant::now();
    loop {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM hpi_tasks WHERE detail->>'net_id' = $1 AND status = 'pending' \
             ORDER BY created_at",
        )
        .bind(net_id)
        .fetch_all(db)
        .await
        .unwrap();
        if ids.len() >= 2 {
            return ids;
        }
        if start.elapsed() > timeout {
            panic!(
                "expected 2 pending tasks for net {net_id} within {timeout:?}, got {}",
                ids.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn complete_task(app: &axum::Router, task_id: &str) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/tasks/{task_id}/complete"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "data": {} }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "complete task {task_id}: status {}",
        resp.status()
    );
}

#[tokio::test]
async fn parallel_forks_joins_and_completes() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }
    let nats_url = engine_nats_url();
    let (app, db) = common::test_app_with_petri_url(&nats_url, &engine_url()).await;
    let prefix = format!("test_{}", Uuid::new_v4().simple());
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("nats")
        .with_consumer_prefix(prefix);
    let cleanup_nats = nats.clone();
    let (_causality, _lifecycle) = spawn_consumers(nats, db.clone()).await;

    let (id, net_id) = publish_and_start(&app, parallel_graph()).await;

    // ParallelSplit must fan out to BOTH human-task branches simultaneously.
    let task_ids = wait_for_two_tasks(&db, &net_id, Duration::from_secs(20)).await;
    assert_eq!(task_ids.len(), 2, "split must fork into 2 branches");

    // Drive both branches to completion — the Join transition then merges.
    for t in &task_ids {
        complete_task(&app, t).await;
    }

    wait_for_completion(&db, id, Duration::from_secs(30)).await;
    cleanup_durables(&cleanup_nats).await;
}
