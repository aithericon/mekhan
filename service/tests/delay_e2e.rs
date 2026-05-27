//! End-to-end coverage for the Delay node — live engine schedules a real
//! timer, waits the configured duration, then forwards the inbound token
//! on the single default output.
//!
//! Compiler unit tests in `compiler_tests.rs` pin the AIR shape (prep →
//! schedule → forward triple + signal place); this test proves the engine
//! actually executes that shape: timer_schedule effect fires, the
//! clockmaster signals the sig place after the delay, the forward
//! transition consumes the scheduled token + signal, and the End sees the
//! original payload.
//!
//! Requires `just dev up` (engine :3030 sharing the dev NATS broker). Run
//! serially (`--test-threads=1`).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
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
    tokio::time::sleep(Duration::from_millis(200)).await;
    TaskHandle(handle.abort_handle())
}

async fn wait_for_completion(db: &sqlx::PgPool, id: Uuid, timeout: Duration) {
    let start = Instant::now();
    loop {
        let st: String =
            sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(id)
                .fetch_one(db)
                .await
                .unwrap();
        if st == "completed" {
            return;
        }
        if st == "failed" {
            let result: Option<Value> = sqlx::query_scalar(
                "SELECT result FROM workflow_instances WHERE id = $1",
            )
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap();
            panic!("instance {id} reached `failed` (result: {result:?})");
        }
        if start.elapsed() > timeout {
            panic!("instance {id} did not complete within {timeout:?} (last: {st})");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn fetch_result(db: &sqlx::PgPool, id: Uuid) -> Value {
    sqlx::query_scalar::<_, Option<Value>>(
        "SELECT result FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_one(db)
    .await
    .unwrap()
    .expect("result column was null — Delay produced no End envelope")
}

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
                        "name": "Delay E2E",
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
                        "metadata": { "e2e": "delay" },
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

/// `Start(marker) → Delay(500ms) → End{marker}`.
///
/// The End's `resultMapping` copies the start-token `marker` through —
/// proving the inbound payload survived the prep → schedule → forward
/// triple intact (the forward transition's `#{ out: scheduled.payload }`
/// Rhai expression has to reconstruct the control token from the parked
/// TimerScheduled envelope; a regression there shows up as a missing or
/// wrong `marker` in the End result).
fn delay_graph(duration_ms_expr: &str) -> Value {
    json!({
        "nodes": [
            { "id": "start", "type": "start", "position": { "x": 0, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": { "id": "in", "label": "Input",
                                     "fields": [
                                         { "name": "marker", "label": "Marker",
                                           "kind": "text", "required": true }
                                     ] } } },
            { "id": "d", "type": "delay", "position": { "x": 240, "y": 0 },
              "data": { "type": "delay", "label": "Pause",
                        "durationMsExpr": duration_ms_expr } },
            { "id": "end", "type": "end", "position": { "x": 480, "y": 0 },
              "data": { "type": "end", "label": "Done",
                        "resultMapping": [
                            { "targetField": "marker",
                              "expression": "input.marker" }
                        ] } }
        ],
        "edges": [
            { "id": "e1", "source": "start", "target": "d",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e2", "source": "d", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

/// Smallest possible Delay smoke: timer is scheduled, fires after the
/// configured delay, the forward transition re-emits the original token.
/// Asserts both correctness (token survives) AND that the engine actually
/// waited (elapsed >= delay).
#[tokio::test]
async fn delay_waits_then_forwards_payload() {
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

    let started_at = Instant::now();
    let id =
        publish_and_start(&app, delay_graph("500"), json!({ "marker": "delay-payload" })).await;
    wait_for_completion(&db, id, Duration::from_secs(20)).await;
    let elapsed = started_at.elapsed();

    let result = fetch_result(&db, id).await;
    assert_eq!(
        result["value"]["marker"],
        json!("delay-payload"),
        "Delay must forward the inbound payload unchanged: {result}"
    );
    // The engine had to wait at least the configured delay before the
    // forward transition could fire. Allow a 100ms grace below the floor
    // for clock skew + the lifecycle listener's drain interval. A regression
    // that fires `t_forward` without waiting on the signal place would
    // complete in <100ms.
    assert!(
        elapsed >= Duration::from_millis(400),
        "Delay must wait at least ~500ms; ran in {elapsed:?}"
    );
}
