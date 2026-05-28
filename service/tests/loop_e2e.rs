//! End-to-end coverage for the Loop node — live engine iterates a real body
//! node N times then exits, capturing `lp.iteration` into the End result.
//!
//! The body is a 1-step HumanTask that gets auto-completed each iteration —
//! proves the token actually flows through user code each pass (not just that
//! the counter cascade fires). After the empty-loop noop semantic was retired
//! (Loop now requires a body via `parent_id == loop.id`), this is the
//! canonical Loop runtime test.
//!
//! Requires `just dev up` (engine :13030 sharing the dev NATS broker). Run
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
            panic!("instance {id} did not complete within {timeout:?} (last: {st})");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// `Start → Loop(max=3, "lp.iteration < 3", body=PhaseUpdate) → End`.
///
/// The PhaseUpdate body sits inside the loop (`parent_id == "lp"`) and is
/// wired via the `body_in`/`body_out` handles so each iteration routes
/// through it. PhaseUpdate is a pass-through for the data token (the `lp`
/// namespace declared on the control token is preserved); its phase signal
/// is a no-op outside a registered process, so the test doesn't need a
/// process to be running — just proves the loop iterates user code three
/// times then exits.
///
/// Topology per iteration: enter → body → continue/exit, where body =
/// PhaseUpdate. Counter increments at `t_lp_continue`; after the 3rd pass
/// the guard `count < 3` flips false and `t_lp_exit` fires. The End's
/// `resultMapping` captures `final_count` so the test asserts iteration
/// (not just completion).
fn loop_graph() -> Value {
    json!({
        "nodes": [
            { "id": "start", "type": "start", "position": { "x": 0, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": { "id": "in", "label": "Input", "fields": [] } } },
            { "id": "lp", "type": "loop", "position": { "x": 240, "y": 0 },
              "data": { "type": "loop", "label": "Retry",
                        "maxIterations": 3,
                        // Loop declares `iteration: number` as a producer field
                        // (see service/src/compiler/validate.rs::node_output_fields).
                        // The counter is parked in `p_lp_data` by `lower_loop`;
                        // the standard read-arc synthesis pass rewrites
                        // `lp.iteration` to `d_lp.iteration` on the loop's own
                        // pre-wired continue/exit transitions.
                        "loopCondition": "lp.iteration < 3" } },
            // Loop body — a PhaseUpdate passthrough. `parent_id == "lp"`
            // satisfies the LoopEmpty check; the body_in/body_out handle
            // edges route the iteration through it. PhaseUpdate's `out` is
            // its `input` verbatim, so the counter rides through unchanged.
            { "id": "body", "type": "phase_update",
              "position": { "x": 360, "y": 80 },
              "parentId": "lp",
              "data": { "type": "phase_update", "label": "Body",
                        "phaseName": "iteration",
                        "status": "running" } },
            { "id": "end", "type": "end", "position": { "x": 480, "y": 0 },
              "data": { "type": "end", "label": "Done",
                        "resultMapping": [
                            { "targetField": "final_count",
                              "expression": "lp.iteration" }
                        ] } }
        ],
        "edges": [
            { "id": "e1", "source": "start", "target": "lp",
              "targetHandle": "in", "type": "sequence" },
            // Loop → body via the body_in source handle.
            { "id": "e_body_in", "source": "lp", "target": "body",
              "sourceHandle": "body_in", "targetHandle": "in",
              "type": "sequence" },
            // body → Loop via the body_out target handle. Tagged `loop_back`
            // so topo sort/cycle detection excludes it from the DAG.
            { "id": "e_body_out", "source": "body", "target": "lp",
              "targetHandle": "body_out", "type": "loop_back" },
            { "id": "e2", "source": "lp", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

async fn fetch_result(db: &sqlx::PgPool, id: Uuid) -> Value {
    sqlx::query_scalar::<_, Option<Value>>(
        "SELECT result FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_one(db)
    .await
    .unwrap()
    .expect("result column was null — loop produced no End envelope")
}

async fn publish_and_start(app: &axum::Router, graph: Value) -> Uuid {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Loop E2E",
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
                        "metadata": { "e2e": "loop" }
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

#[tokio::test]
async fn loop_iterates_and_exits() {
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

    let id = publish_and_start(&app, loop_graph()).await;
    wait_for_completion(&db, id, Duration::from_secs(30)).await;

    // Prove the loop actually iterated. max_iterations=3 + `count < 3` guard
    // means: enter (count=0) → 3× continue (count=1,2,3) → exit when count>=3.
    // The continue branch fires before exit alphabetically (`continue` < `exit`),
    // and the guards are mutually exclusive so the cascade is deterministic.
    let result = fetch_result(&db, id).await;
    assert_eq!(
        result["value"]["final_count"], json!(3),
        "loop should have iterated 3 times before exiting: {result}"
    );
}
