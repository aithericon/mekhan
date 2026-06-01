//! End-to-end coverage for the Decision node — live engine routes the token
//! through one of two branches based on a guard reading the Start token.
//!
//! Decision lowering is well-covered by compiler unit tests, but until this
//! file there was zero runtime proof that the synthesized guard transitions
//! actually fire under the live engine. We publish a `Start(amount) → Decision
//! → (HighEnd | LowEnd)` template, seed two instances with different amounts,
//! and assert each instance completes with the End-specific `resultMapping`
//! (high vs. low) — i.e. the engine took the expected branch.
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
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::nats::MekhanNats;

// ── Harness helpers ───────────────────────────────────────────────────────

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

struct TaskHandle(tokio::task::AbortHandle);
impl Drop for TaskHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn spawn_lifecycle(nats: MekhanNats, db: sqlx::PgPool) -> TaskHandle {
    let kv = nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("create KV");
    let sub_mgr = Arc::new(SubscriptionManager::new(kv, nats.jetstream().clone()));
    let handle = tokio::spawn(async move {
        start_lifecycle_listener(
            nats,
            db,
            sub_mgr,
            None,
            mekhan_service::triggers::ResultWaiters::new(),
        )
        .await;
    });
    // Give the listener a moment to subscribe before we kick off an instance.
    tokio::time::sleep(Duration::from_millis(200)).await;
    TaskHandle(handle.abort_handle())
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

async fn fetch_result(db: &sqlx::PgPool, id: Uuid) -> Value {
    sqlx::query_scalar::<_, Option<Value>>("SELECT result FROM workflow_instances WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
        .expect("result column was null — Decision branch produced no End envelope")
}

/// Publish a template + seed an instance with the given start token. Returns
/// the live `app`, db, and the new instance id. The lifecycle listener must
/// already be running on `db`.
async fn publish_and_start(app: &axum::Router, graph: Value, start_token: Value) -> Uuid {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Decision E2E",
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
                        "metadata": { "e2e": "decision" },
                        "start_tokens": [{
                            "start_block_id": "start",
                            "token": start_token,
                        }],
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
    Uuid::parse_str(inst["id"].as_str().unwrap()).unwrap()
}

/// `Start(amount: number) → Decision(amount > 100 → high, default → low)
///   → EndHigh / EndLow` — each End stamps a distinct `route` field so the
/// persisted result envelope records which branch fired.
fn decision_graph() -> Value {
    json!({
        "nodes": [
            {
                "id": "start", "type": "start", "position": { "x": 0, "y": 0 },
                "data": {
                    "type": "start", "label": "Start",
                    "initial": {
                        "id": "in", "label": "Input",
                        "fields": [
                            { "name": "amount", "label": "Amount",
                              "kind": "number", "required": true }
                        ]
                    }
                }
            },
            {
                "id": "dec", "type": "decision", "position": { "x": 240, "y": 0 },
                "data": {
                    "type": "decision", "label": "Route by Amount",
                    "conditions": [{
                        "edgeId": "cond_high", "label": "High",
                        "guard": "input.amount > 100"
                    }],
                    "defaultBranch": "default"
                }
            },
            {
                "id": "end_high", "type": "end", "position": { "x": 480, "y": -80 },
                "data": {
                    "type": "end", "label": "Done (High)",
                    "resultMapping": [
                        { "targetField": "route", "expression": "\"high\"" },
                        { "targetField": "amount", "expression": "input.amount" }
                    ]
                }
            },
            {
                "id": "end_low", "type": "end", "position": { "x": 480, "y": 80 },
                "data": {
                    "type": "end", "label": "Done (Low)",
                    "resultMapping": [
                        { "targetField": "route", "expression": "\"low\"" },
                        { "targetField": "amount", "expression": "input.amount" }
                    ]
                }
            }
        ],
        "edges": [
            { "id": "e_in", "source": "start", "target": "dec",
              "targetHandle": "in", "type": "sequence" },
            { "id": "cond_high", "source": "dec", "target": "end_high",
              "sourceHandle": "cond_high", "targetHandle": "in", "type": "sequence" },
            { "id": "cond_low", "source": "dec", "target": "end_low",
              "sourceHandle": "default", "targetHandle": "in", "type": "sequence" }
        ]
    })
}

// ── Test ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn decision_routes_to_branch_based_on_start_token() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }

    let nats_url = engine_nats_url();
    let (app, db) = common::test_app_with_petri_url(&nats_url, &engine_url()).await;
    let nats = MekhanNats::connect(&nats_url, None).await.expect("nats");
    let _lifecycle = spawn_lifecycle(nats, db.clone()).await;

    // High branch — `amount > 100` → end_high → result.route == "high"
    let id_high = publish_and_start(&app, decision_graph(), json!({ "amount": 250 })).await;
    wait_for_completion(&db, id_high, Duration::from_secs(30)).await;
    let result = fetch_result(&db, id_high).await;
    assert_eq!(
        result["ok"],
        json!(true),
        "expected success envelope on high branch, got {result}"
    );
    assert_eq!(
        result["value"]["route"],
        json!("high"),
        "amount=250 should have taken the high branch: {result}"
    );
    assert_eq!(result["value"]["amount"], json!(250));

    // Low branch — `amount <= 100` falls through to the default → end_low.
    let id_low = publish_and_start(&app, decision_graph(), json!({ "amount": 5 })).await;
    wait_for_completion(&db, id_low, Duration::from_secs(30)).await;
    let result = fetch_result(&db, id_low).await;
    assert_eq!(
        result["value"]["route"],
        json!("low"),
        "amount=5 should have taken the low (default) branch: {result}"
    );
    assert_eq!(result["value"]["amount"], json!(5));
}
