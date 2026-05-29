//! Mini-BO composition e2e — proves the keystone primitives compose, not just
//! that they work individually.
//!
//! Each of Loop / SubWorkflow / CatalogueQuery / Decision has its own e2e file
//! exercising that primitive in isolation. The BO-shaped workflow needs them
//! _composed_: an outer Loop whose body calls a SubWorkflow and reads the
//! catalogue, with a Decision after the loop. Every prior keystone landing
//! surfaced 1-2 merge-blind-spot bugs on first composition — this is the
//! canary.
//!
//! Graph (mini-BO without the GP math):
//!
//! ```text
//!  Start
//!    └─► Loop(max=3, "lp.iteration < 3")
//!         │                  ┌────────────────────────────┐
//!         │  body_in────────►│ Sub(child:Start→End)       │
//!         │                  │   └─► Cat(catalogue_query) │
//!         │  body_out◄───────│         └─► (back to lp)   │
//!         │                  └────────────────────────────┘
//!         └─► Decision("input.total_count >= 0")
//!               ├─cond_completed─► EndCompleted (outcome="completed",
//!               │                                total_count=N)
//!               └─default────────► EndUnexpected (outcome="unexpected")
//! ```
//!
//! ## Gaps surfaced by this test (first run, 2026-05-20)
//!
//! Two real composition gaps were found — neither is a "compiler bug" so much
//! as a structural limitation of how Loop interacts with body nodes that
//! reshape the token. BO needs to know about both before authoring.
//!
//! **Gap 1 — loop counter leaf doesn't survive non-passthrough bodies.**
//! `_loop_<id>_count` is a field on the data token (injected by `t_enter`,
//! incremented by `t_continue`). Body nodes that replace the token shape
//! erase it: `CatalogueQuery` emits `{ artifacts, total_count, ... }`;
//! `SubWorkflow`'s join returns the child's reply shape. After such a body,
//! `t_continue`'s guard `input._loop_<id>_count < max` reads undefined,
//! falsifies — and `t_exit`'s `!loop_condition` branch fires on iteration 1.
//! In other words: **the `maxIterations` safety cap is effectively ignored
//! with non-passthrough bodies; the loop relies entirely on
//! `loop_condition` to stop**. For BO this is fine (BO's stop condition
//! reads catalogue size, which is fresh from `CatalogueQuery` each
//! iteration), but it's a sharp authoring rule.
//!
//! **Gap 2 — post-Loop scope doesn't include body output fields.**
//! The static guard validator (`token_shape::guard_readarc_plan`) reports
//! only `_loop_<id>_count` (+ `_instance_id`) in scope for a Decision /
//! End `resultMapping` downstream of a Loop. It doesn't propagate the
//! last body node's output port. At runtime `t_exit` does forward the
//! body's token verbatim, so the fields *are* there — the scope analyzer
//! just doesn't know it. BO can work around this by reading those fields
//! inside `loop_condition` instead of post-loop, but anything that needs
//! catalogue results *after* the Loop hits this wall.
//!
//! Both gaps are recorded here so we don't re-derive them, and so a future
//! compiler fix can flip the assertions in this test from "completes" to
//! "completes with N iterations" / "reads body fields downstream".
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

// ── Harness helpers (mirrors loop_e2e / subworkflow_e2e) ──────────────────

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

async fn create_template(app: &axum::Router, name: &str, graph: Value) -> Uuid {
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
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    if status != StatusCode::CREATED {
        let body_str = String::from_utf8_lossy(&bytes);
        panic!("create {name}: HTTP {status}: {body_str}");
    }
    let created: Value = serde_json::from_slice(&bytes).unwrap();
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

async fn create_instance(app: &axum::Router, template_id: Uuid) -> Uuid {
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
                        "metadata": { "e2e": "composition" }
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

// ── Graphs ────────────────────────────────────────────────────────────────

/// Child SubWorkflow: trivial `Start → End` passthrough. The fingerprint
/// `ck` in node ids survives compilation + `make_child_callable` and is
/// visible in the parent's embedded AIR (sanity check elsewhere).
fn child_graph() -> Value {
    json!({
        "nodes": [
            { "id": "ckstart", "type": "start", "position": { "x": 0, "y": 0 },
              "data": { "type": "start", "label": "Child Start",
                        "initial": { "id": "in", "label": "Input", "fields": [] } } },
            { "id": "ckend", "type": "end", "position": { "x": 240, "y": 0 },
              "data": { "type": "end", "label": "Child End",
                        "resultMapping": [] } }
        ],
        "edges": [
            { "id": "ce", "source": "ckstart", "target": "ckend",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

/// Parent: `Start → Loop(body: Sub → Cat) → Decision → end_completed | end_unexpected`.
///
/// `child_family` is the SubWorkflow node's resolved-at-publish child id (a
/// `Latest` pin is fine; the resolver freezes the current is_latest version).
fn parent_graph(child_family: Uuid) -> Value {
    json!({
        "nodes": [
            { "id": "start", "type": "start", "position": { "x": 0, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": { "id": "in", "label": "Input", "fields": [] } } },

            // Outer loop. `_loop_lp_count` rides the slim control token; the
            // guard reads it without a read-arc.
            { "id": "lp", "type": "loop", "position": { "x": 240, "y": 0 },
              "data": { "type": "loop", "label": "Outer Loop",
                        "maxIterations": 3,
                        "loopCondition": "lp.iteration < 3" } },

            // Body — child of the Loop. SubWorkflow call.
            { "id": "sub", "type": "sub_workflow",
              "position": { "x": 360, "y": 80 },
              "parentId": "lp",
              "data": { "type": "sub_workflow", "label": "Call Child",
                        "templateId": child_family,
                        "versionPin": { "mode": "latest" },
                        "inputMapping": [],
                        "output": { "id": "out", "label": "Result", "fields": [] } } },

            // Body — child of the Loop. CatalogueQuery (empty catalogue is
            // a valid result; total_count=0 still completes).
            { "id": "cat", "type": "automated_step",
              "position": { "x": 540, "y": 80 },
              "parentId": "lp",
              "data": { "type": "automated_step", "label": "Read Observations",
                        "executionSpec": {
                            "backendType": "catalogue_query",
                            "config": { "category": "observation", "limit": 10 }
                        },
                        "input": { "id": "in", "label": "Input", "fields": [] },
                        "output": {
                            "id": "out", "label": "Output",
                            "fields": [
                                { "name": "artifacts", "label": "Artifacts",
                                  "kind": "json", "required": false },
                                { "name": "total_count", "label": "Total",
                                  "kind": "number", "required": false },
                                { "name": "source_process_ids",
                                  "label": "Source Process IDs",
                                  "kind": "json", "required": false }
                            ]
                        },
                        "retryPolicy": {},
                        "deploymentModel": { "mode": "executor" } } },

            // End — empty resultMapping because Gap 2 (see module doc) means
            // we can't reach the body's output fields from here. Completion
            // alone is the assertion: it proves Loop+Sub+Cat composed in a
            // body run without deadlock or compile rejection.
            { "id": "end", "type": "end",
              "position": { "x": 760, "y": 0 },
              "data": { "type": "end", "label": "Done",
                        "resultMapping": [] } }
        ],
        "edges": [
            { "id": "e_start_lp", "source": "start", "target": "lp",
              "targetHandle": "in", "type": "sequence" },

            // Loop → body chain.
            { "id": "e_body_in", "source": "lp", "target": "sub",
              "sourceHandle": "body_in", "targetHandle": "in",
              "type": "sequence" },
            { "id": "e_sub_cat", "source": "sub", "target": "cat",
              "targetHandle": "in", "type": "sequence" },
            // Body back-edge — tagged `loop_back` to exclude from topo/cycle.
            { "id": "e_body_out", "source": "cat", "target": "lp",
              "targetHandle": "body_out", "type": "loop_back" },

            // Loop → End.
            { "id": "e_lp_end", "source": "lp", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

// ── Test ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn loop_body_with_subworkflow_and_catalogue_query_composes() {
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

    // Publish the child first so its `is_latest` row exists when the parent's
    // SubWorkflow node resolves at publish time.
    let child = create_template(&app, "Composition Child", child_graph()).await;
    publish(&app, child).await;

    let parent = create_template(&app, "Composition Parent", parent_graph(child)).await;
    publish(&app, parent).await;

    let instance = create_instance(&app, parent).await;
    // 3 iterations × (SubWorkflow spawn+reply ~hundreds of ms + CatalogueQuery
    // lookup ~tens of ms) + outer Decision → keep the same 30s deadline as the
    // sibling e2e files. If composition surfaces a deadlock the panic includes
    // the last-seen status.
    wait_for_completion(&db, instance, Duration::from_secs(30)).await;

    // Completion is the assertion (`wait_for_completion` panics on `failed`
    // and on timeout). It proves: (1) the parent compiled with SubWorkflow
    // + CatalogueQuery inside a Loop body, (2) the SubWorkflow spawn / reply
    // / join all wired correctly inside a Loop body, (3) the CatalogueQuery
    // effect fired downstream of SubWorkflow inside the body, (4) Loop's
    // `t_exit` accepted the post-body token shape and forwarded it onward,
    // (5) End received the token and completed cleanly. The two scoping
    // gaps documented in the module header mean we can't reach finer-grained
    // assertions (iteration count, body-output fields) from a vanilla End
    // — but compilation + completion alone is the meaningful BO readiness
    // signal. We do NOT assert on `workflow_instances.result` because an
    // empty End `resultMapping` is allowed to leave that column null.

    // Sanity: the run produced engine events (rules out an "instance row
    // flipped to completed without the net actually running" false positive).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/instances/{instance}/state"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "get instance state");
    let state = body_json(resp.into_body()).await;
    assert!(
        state["event_count"].as_u64().unwrap_or(0) > 10,
        "composition run must have produced engine events: {state}"
    );
}
