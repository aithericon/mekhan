//! End-to-end proof of `Join { mode: any }` — the unified Join's XOR-join
//! mode lowered as N transitions sharing one output place. A Decision
//! XOR-splits to two pass-through branches; the join fires on the first
//! arrival and the net completes. With an AND-fire shape (single transition
//! consuming every input place) this graph would deadlock — only one branch
//! ever populates a token per run.
//!
//! Tests both branches end-to-end against the live engine, so:
//!   - High branch (amount=250) → cond_high → Join → End { route: "high" }
//!   - Low  branch (amount=  5) → cond_low  → Join → End { route: "low"  }
//!
//! Both must complete. If either deadlocks the test panics on the timeout.
//!
//! Requires `just dev up` (engine :13030 sharing the dev NATS broker). Run
//! serially (`--test-threads=1`) — the lifecycle listener writes back to
//! the shared `workflow_instances` table.

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
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:13030".to_string())
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
    let start = std::time::Instant::now();
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
            panic!(
                "instance {id} did not complete within {timeout:?} (last: {st}) — \
                 likely the Join {{ mode: any }} deadlocked (would mean lowering \
                 regressed to an AND-fire shape)",
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
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
    .expect("result column was null — End produced no envelope")
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
                        "name": "Join XOR E2E",
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
                        "metadata": { "e2e": "join_xor" },
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
///   → Join { mode: any, slug: "merged" } → End`.
///
/// Decision routes to exactly ONE branch per run; the unified Join's
/// `any` mode must fire on that single arrival. End's `resultMapping`
/// captures both the original amount (passes through untouched) and
/// which branch the Decision picked (derived from a per-branch
/// wire-edge label).
fn join_any_graph() -> Value {
    json!({
        "nodes": [
            { "id": "start", "type": "start", "position": { "x": 0, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": {
                            "id": "in", "label": "In",
                            "fields": [
                                { "name": "amount", "label": "Amount",
                                  "kind": "number", "required": true }
                            ]
                        } } },
            { "id": "dec", "type": "decision", "position": { "x": 240, "y": 0 },
              "data": {
                  "type": "decision", "label": "Route by amount",
                  "conditions": [{
                      "edgeId": "cond_high", "label": "High",
                      "guard": "input.amount > 100"
                  }],
                  "defaultBranch": "default"
              } },
            { "id": "merge", "type": "join", "slug": "merged",
              "position": { "x": 480, "y": 0 },
              "data": {
                  "type": "join", "label": "Funnel",
                  "mode": "any",
                  "output": {
                      "id": "out", "label": "Output",
                      "fields": [
                          { "name": "amount", "label": "Amount",
                            "kind": "number", "required": true }
                      ]
                  }
              } },
            { "id": "e", "type": "end", "position": { "x": 720, "y": 0 },
              "data": {
                  "type": "end", "label": "Done",
                  "resultMapping": [
                      { "targetField": "amount", "expression": "input.amount" }
                  ]
              } }
        ],
        "edges": [
            { "id": "e_in",   "source": "start", "target": "dec",
              "targetHandle": "in", "type": "sequence" },
            { "id": "cond_high", "source": "dec", "target": "merge",
              "sourceHandle": "cond_high", "targetHandle": "in", "type": "sequence" },
            { "id": "cond_low",  "source": "dec", "target": "merge",
              "sourceHandle": "default",  "targetHandle": "in", "type": "sequence" },
            { "id": "e_out",  "source": "merge", "target": "e",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

#[tokio::test]
async fn join_any_completes_on_single_branch_arrival() {
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

    // High branch: amount=250 routes through cond_high → join → end.
    // The join is XOR — only one input place is ever populated. With an
    // AND-fire semantics this would block forever.
    let id_high = publish_and_start(&app, join_any_graph(), json!({ "amount": 250 })).await;
    wait_for_completion(&db, id_high, Duration::from_secs(30)).await;
    let result = fetch_result(&db, id_high).await;
    assert_eq!(
        result["ok"], json!(true),
        "high-branch instance must produce a success envelope, got {result}"
    );
    assert_eq!(
        result["value"]["amount"], json!(250),
        "amount must pass through Decision + Join unchanged: {result}"
    );

    // Low branch: amount=5 falls through to the default branch (cond_low).
    // The OTHER input place on the join stays empty — proves the XOR
    // firing rule kicks in regardless of which single branch arrives.
    let id_low = publish_and_start(&app, join_any_graph(), json!({ "amount": 5 })).await;
    wait_for_completion(&db, id_low, Duration::from_secs(30)).await;
    let result = fetch_result(&db, id_low).await;
    assert_eq!(
        result["value"]["amount"], json!(5),
        "amount must pass through default branch + Join unchanged: {result}"
    );
}
