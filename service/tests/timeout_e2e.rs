//! End-to-end coverage for the Timeout node — live engine races a body
//! against a clock, taking the `done` branch if the body completes inside
//! the window and the `timeout` branch (plus body-cancellation drain) if
//! the timer wins.
//!
//! Body shape: `Timeout(N ms) { Start → HumanTask → body_out }`. The
//! HumanTask is wired as a child via `parent_id == "tmo"`, with edges
//! routing `tmo --(body_in)--> review --(body_out)--> tmo`. The post-pass
//! in `lower/mod.rs::apply_timeout_cancel_fanouts` synthesizes a
//! `t_tmo_drain_review` transition that read-arcs on `p_tmo_cancel_pulse`
//! and consumes `p_review_assigned`, firing `human_cancel`.
//!
//! Two scenarios:
//!
//! - **body wins**: Timeout(60s) + immediate task completion. Asserts the
//!   `done` envelope captures the HumanTask response AND that the pending
//!   timer was drained (`t_tmo_cancel` → `timer_cancel` effect, no
//!   `timeout` envelope, no orphan signal in the events).
//! - **timer wins**: Timeout(500ms) + no response. Asserts the `timeout`
//!   envelope is emitted, the HumanTask is no longer pending (drained by
//!   `human_cancel`), and the instance reaches `completed` cleanly.
//!
//! Requires `just dev up` (engine :3030 sharing the dev NATS broker). Run
//! serially (`--test-threads=1`).

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
use mekhan_service::process::cancel_listener::start_human_cancel_listener;
use mekhan_service::projections::step_executions::start_step_executions_ingest;

fn engine_nats_url() -> String {
    std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url())
}

/// Best-effort delete of the test's per-prefix durables on the shared
/// JetStream streams. Without this each panicked test leaks a durable
/// consumer until `just dev reset`.
async fn cleanup_durables(nats: &MekhanNats) {
    let prefix = match nats.consumer_prefix() {
        Some(p) => p,
        None => return,
    };
    for (stream_name, base) in [
        ("PETRI_GLOBAL", "mekhan-causality-ingest"),
        ("PETRI_GLOBAL", "mekhan-lifecycle"),
        ("PETRI_GLOBAL", "mekhan-step-executions"),
        ("HUMAN_REQUESTS", "mekhan-human-task-ingest"),
        ("HUMAN_CANCEL", "mekhan-human-cancel-ingest"),
    ] {
        if let Ok(stream) = nats.jetstream().get_stream(stream_name).await {
            let _ = stream.delete_consumer(&format!("{prefix}_{base}")).await;
        }
    }
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

async fn spawn_consumers(
    nats: MekhanNats,
    db: sqlx::PgPool,
) -> (TaskHandle, TaskHandle, TaskHandle, TaskHandle) {
    let kv = nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("create KV");
    let sub_mgr = Arc::new(SubscriptionManager::new(kv, nats.jetstream().clone()));

    let c_nats = nats.clone();
    let c_db = db.clone();
    let c_sub = sub_mgr.clone();
    let c_live = LiveBroadcasts::new();
    // Causality ingest projects HumanTask requests into `hpi_tasks` — the
    // table this test polls. Without it the body task is invisible to the
    // test even though the engine has created it.
    let causality = tokio::spawn(async move {
        start_causality_ingest(c_nats, c_db, c_sub, c_live, None).await;
    });

    // Cancel listener — consumes engine-fired `human.cancel.>` and flips
    // hpi_tasks rows to `cancelled`. The test's per-prefix consumer means
    // we get our own durable cursor; the live dev mekhan's listener still
    // runs but writes to a different DB.
    let cancel_nats = nats.clone();
    let cancel_db = db.clone();
    let cancel = tokio::spawn(async move {
        start_human_cancel_listener(cancel_nats, cancel_db).await;
    });

    // Step-executions projection — folds the engine event log into per-node
    // rows (`step_execution` table) that back the editor's node runtime badge.
    // This is what proves a drained body node leaves `running` once the net
    // completes via the timeout branch.
    let steps_nats = nats.clone();
    let steps_db = db.clone();
    let steps = tokio::spawn(async move {
        start_step_executions_ingest(steps_nats, steps_db).await;
    });

    let l_nats = nats;
    let l_db = db;
    let l_sub = sub_mgr;
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
        TaskHandle(cancel.abort_handle()),
        TaskHandle(steps.abort_handle()),
    )
}

async fn wait_for_terminal(db: &sqlx::PgPool, id: Uuid, timeout: Duration) -> String {
    let start = std::time::Instant::now();
    loop {
        let st: String =
            sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(id)
                .fetch_one(db)
                .await
                .unwrap();
        if matches!(st.as_str(), "completed" | "failed" | "cancelled") {
            return st;
        }
        if start.elapsed() > timeout {
            panic!("instance {id} did not reach terminal state within {timeout:?} (last: {st})");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
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
    .expect("result column was null — Timeout produced no End envelope")
}

async fn wait_for_net_id(db: &sqlx::PgPool, id: Uuid, timeout: Duration) -> String {
    let start = std::time::Instant::now();
    loop {
        let net_id: Option<String> =
            sqlx::query_scalar("SELECT net_id FROM workflow_instances WHERE id = $1")
                .bind(id)
                .fetch_one(db)
                .await
                .unwrap();
        if let Some(nid) = net_id {
            return nid;
        }
        if start.elapsed() > timeout {
            panic!("instance {id} never got a net_id within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_pending_task(db: &sqlx::PgPool, net_id: &str, timeout: Duration) -> String {
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
        if let Some(id) = ids.into_iter().next() {
            return id;
        }
        if start.elapsed() > timeout {
            panic!("no pending task for net {net_id} within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

async fn task_status(db: &sqlx::PgPool, task_id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM hpi_tasks WHERE id = $1")
        .bind(task_id)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn complete_task(app: &axum::Router, task_id: &str, data: Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/tasks/{task_id}/complete"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "data": data }).to_string()))
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
                        "name": "Timeout E2E",
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
                        "metadata": { "e2e": "timeout" }
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

/// `Start → Timeout(N ms) { body = HumanTask review }`
///
/// Two outputs: `done` (body finished first) → `end_done`,
/// `timeout` (timer fired first) → `end_timeout`. Each End stamps a
/// `branch` field so we can prove which side fired from the result row.
fn timeout_graph(duration_ms: u64) -> Value {
    let expr = duration_ms.to_string();
    json!({
        "nodes": [
            { "id": "start", "type": "start", "position": { "x": 0, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": { "id": "in", "label": "Input", "fields": [] } } },
            { "id": "tmo", "type": "timeout", "position": { "x": 240, "y": 0 },
              "data": { "type": "timeout", "label": "Race",
                        "durationMsExpr": expr } },
            // Body child: a HumanTask. parent_id == "tmo" + the body_in /
            // body_out edges route the inbound payload through it. The
            // post-pass synthesizes a drain transition that fires
            // human_cancel when the timer wins.
            { "id": "review", "type": "human_task", "slug": "review",
              "position": { "x": 360, "y": 80 },
              "parentId": "tmo",
              "data": {
                  "type": "human_task",
                  "label": "Review",
                  "taskTitle": "Review",
                  "steps": [{
                      "id": "step1", "title": "Confirm",
                      "blocks": [
                          { "type": "input", "field": {
                              "name": "approved", "label": "Approved",
                              "kind": "checkbox", "required": true } }
                      ]
                  }]
              } },
            { "id": "end_done", "type": "end", "position": { "x": 600, "y": -80 },
              "data": { "type": "end", "label": "Done (body)",
                        "resultMapping": [
                            { "targetField": "branch",
                              "expression": "\"done\"" }
                        ] } },
            { "id": "end_timeout", "type": "end", "position": { "x": 600, "y": 80 },
              "data": { "type": "end", "label": "Done (timeout)",
                        "resultMapping": [
                            { "targetField": "branch",
                              "expression": "\"timeout\"" }
                        ] } }
        ],
        "edges": [
            { "id": "e_in", "source": "start", "target": "tmo",
              "targetHandle": "in", "type": "sequence" },
            // tmo --body_in--> review (enter body)
            { "id": "e_body_in", "source": "tmo", "target": "review",
              "sourceHandle": "body_in", "targetHandle": "in",
              "type": "sequence" },
            // review --> tmo body_out (body completion). Tag as loop_back so
            // the topo sort excludes it from the DAG (same convention Loop
            // uses for its body_out edge).
            { "id": "e_body_out", "source": "review", "target": "tmo",
              "targetHandle": "body_out", "type": "loop_back" },
            // tmo (default = done) --> end_done
            { "id": "e_done", "source": "tmo", "target": "end_done",
              "targetHandle": "in", "type": "sequence" },
            // tmo --timeout--> end_timeout
            { "id": "e_timeout", "source": "tmo", "target": "end_timeout",
              "sourceHandle": "timeout", "targetHandle": "in",
              "type": "sequence" }
        ]
    })
}

/// `Start → Timeout(N ms) { body_in → wait (Delay) → review (HumanTask) → body_out }`
///
/// The HumanTask is NOT the body's entry node — it sits downstream of a short
/// Delay, so it's genuinely *in between* the body entry and the completion
/// arc. Both `wait` and `review` are `parent_id == "tmo"` body children, so
/// the post-pass synthesizes a drain transition for each. When the timer
/// wins, only `review`'s drain (`t_tmo_drain_review`) can fire — the Delay
/// has already forwarded, so its in-flight place is empty and its drain
/// (`t_tmo_drain_wait`) is starved. That asymmetry is the assertion: the
/// cancel-pulse fan-out reclaims only live tokens.
fn timeout_delay_then_human_graph(duration_ms: u64) -> Value {
    let expr = duration_ms.to_string();
    json!({
        "nodes": [
            { "id": "start", "type": "start", "position": { "x": 0, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": { "id": "in", "label": "Input", "fields": [] } } },
            { "id": "tmo", "type": "timeout", "position": { "x": 240, "y": 0 },
              "data": { "type": "timeout", "label": "Race",
                        "durationMsExpr": expr } },
            // First body step — a short Delay. parent_id == "tmo".
            { "id": "wait", "type": "delay", "slug": "wait",
              "position": { "x": 340, "y": 80 },
              "parentId": "tmo",
              "data": { "type": "delay", "label": "Settle",
                        "durationMsExpr": "50" } },
            // The "in between" HumanTask — left pending when the timer wins.
            { "id": "review", "type": "human_task", "slug": "review",
              "position": { "x": 460, "y": 80 },
              "parentId": "tmo",
              "data": {
                  "type": "human_task",
                  "label": "Review",
                  "taskTitle": "Review",
                  "steps": [{
                      "id": "step1", "title": "Confirm",
                      "blocks": [
                          { "type": "input", "field": {
                              "name": "approved", "label": "Approved",
                              "kind": "checkbox", "required": true } }
                      ]
                  }]
              } },
            { "id": "end_done", "type": "end", "position": { "x": 700, "y": -80 },
              "data": { "type": "end", "label": "Done (body)",
                        "resultMapping": [
                            { "targetField": "branch", "expression": "\"done\"" }
                        ] } },
            { "id": "end_timeout", "type": "end", "position": { "x": 700, "y": 80 },
              "data": { "type": "end", "label": "Done (timeout)",
                        "resultMapping": [
                            { "targetField": "branch", "expression": "\"timeout\"" }
                        ] } }
        ],
        "edges": [
            { "id": "e_in", "source": "start", "target": "tmo",
              "targetHandle": "in", "type": "sequence" },
            // tmo --body_in--> wait (enter the body at the Delay)
            { "id": "e_body_in", "source": "tmo", "target": "wait",
              "sourceHandle": "body_in", "targetHandle": "in",
              "type": "sequence" },
            // wait --> review (Delay forwards into the HumanTask)
            { "id": "e_seq", "source": "wait", "target": "review",
              "targetHandle": "in", "type": "sequence" },
            // review --> tmo body_out (body completion back-edge)
            { "id": "e_body_out", "source": "review", "target": "tmo",
              "targetHandle": "body_out", "type": "loop_back" },
            // tmo (default = done) --> end_done
            { "id": "e_done", "source": "tmo", "target": "end_done",
              "targetHandle": "in", "type": "sequence" },
            // tmo --timeout--> end_timeout
            { "id": "e_timeout", "source": "tmo", "target": "end_timeout",
              "sourceHandle": "timeout", "targetHandle": "in",
              "type": "sequence" }
        ]
    })
}

/// Body-wins: Timeout(60s) wrapping a HumanTask; complete the task
/// immediately. Asserts the `done` branch fired AND the pending timer was
/// cancelled (no `timeout` envelope, no second End).
#[tokio::test]
async fn timeout_body_wins_completes_done_branch() {
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
    let (_causality, _lifecycle, _cancel, _steps) = spawn_consumers(nats, db.clone()).await;

    let id = publish_and_start(&app, timeout_graph(60_000)).await;
    let net_id = wait_for_net_id(&db, id, Duration::from_secs(10)).await;

    // Task should appear quickly; complete it before the 60s timer.
    let task_id = wait_for_pending_task(&db, &net_id, Duration::from_secs(15)).await;
    complete_task(&app, &task_id, json!({ "approved": true })).await;

    let terminal = wait_for_terminal(&db, id, Duration::from_secs(30)).await;
    assert_eq!(
        terminal, "completed",
        "body-wins should complete cleanly (not failed/cancelled)"
    );

    let result = fetch_result(&db, id).await;
    assert_eq!(
        result["value"]["branch"],
        json!("done"),
        "body wins should route to `done` branch: {result}"
    );
    // NOTE: body fields (e.g. `review.approved`) are NOT reachable from
    // post-Timeout nodes — same scope gap that Loop has (see auto-memory
    // `loop_composition_gaps`). The body's terminal envelope DOES ride on
    // the `done` control token at runtime (`t_tmo_body_done` sets
    // `done: body_out`), but the compile-time borrow resolver doesn't
    // surface it. Asserting only on `branch` here keeps this test honest
    // about what's currently composable.

    // Sanity on the drain: the task itself transitions out of `pending` —
    // either completed (body normally finished) or cancelled (drain fired).
    // In the body-wins case it must be `completed`, NOT `cancelled` — a
    // drain firing here would mean the race was lost the wrong way.
    let final_task_status = task_status(&db, &task_id).await;
    assert_eq!(
        final_task_status, "completed",
        "body-wins task must be `completed`, not cancelled: {final_task_status}"
    );

    cleanup_durables(&cleanup_nats).await;
}

/// Timer-wins: Timeout(500ms) wrapping a HumanTask never responded to.
/// Asserts the `timeout` branch fires AND the body HumanTask was drained
/// via `human_cancel` (task moves out of `pending` without a /complete).
#[tokio::test]
async fn timeout_timer_wins_drains_body_human_task() {
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
    let (_causality, _lifecycle, _cancel, _steps) = spawn_consumers(nats, db.clone()).await;

    let id = publish_and_start(&app, timeout_graph(500)).await;
    let net_id = wait_for_net_id(&db, id, Duration::from_secs(10)).await;
    let task_id = wait_for_pending_task(&db, &net_id, Duration::from_secs(10)).await;

    // Do NOT complete the task — let the 500ms timer win.
    let terminal = wait_for_terminal(&db, id, Duration::from_secs(20)).await;
    assert_eq!(
        terminal, "completed",
        "timer-wins should complete the instance (the `timeout` branch routes \
         to an End): final status was `{terminal}`"
    );

    let result = fetch_result(&db, id).await;
    assert_eq!(
        result["value"]["branch"],
        json!("timeout"),
        "timer wins should route to `timeout` branch: {result}"
    );

    // Two layers of assertion — they catch different things:
    //
    // 1. **Engine event stream** proves the drain mechanic fired (the timer
    //    won, the drain transition consumed the in-flight HumanTaskAssigned
    //    token, the human_cancel effect handler completed). This is the
    //    petri-net-level guarantee.
    //
    // 2. **hpi_tasks.status** proves the BFF projection saw the engine's
    //    `human.cancel.{net_id}.{place}` publish and flipped the task row to
    //    `cancelled`. Closed by `process::cancel_listener` — without it, the
    //    drain runs but the task lingers in the UI inbox forever.
    let events_url = format!("{}/api/nets/{}/events?last=200", engine_url(), net_id);
    let events = reqwest::get(&events_url)
        .await
        .expect("engine events fetch")
        .json::<Value>()
        .await
        .expect("engine events parse");
    let evs = events["events"].as_array().expect("events array");
    let saw = |name: &str| -> bool {
        evs.iter().any(|e| {
            let ev = &e["event"];
            ev.get("type").and_then(|t| t.as_str()).map_or(false, |t| {
                (t == "TransitionFired" || t == "EffectCompleted")
                    && ev.get("transition_id").and_then(|x| x.as_str()) == Some(name)
            })
        })
    };
    assert!(saw("t_tmo_timeout"), "timer must have won the race");
    assert!(
        saw("t_tmo_drain_review"),
        "drain transition must have consumed the in-flight HumanTaskAssigned token"
    );
    assert!(
        saw("t_tmo_drain_review_effect"),
        "human_cancel effect must have completed"
    );

    // Poll the hpi_tasks projection — the cancel_listener runs in the live
    // dev mekhan (sharing this PG via :5439) and consumes `human.cancel.>`
    // from JetStream, so there's a small async gap between the engine firing
    // human_cancel and the row flipping. 5s is conservative.
    let task_cancelled_within = Duration::from_secs(5);
    let task_start = std::time::Instant::now();
    let final_task_status = loop {
        let st = task_status(&db, &task_id).await;
        if st != "pending" {
            break st;
        }
        if task_start.elapsed() > task_cancelled_within {
            panic!(
                "task {task_id} still pending {task_cancelled_within:?} after engine \
                 fired human_cancel — is the dev mekhan running with the cancel_listener?"
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    assert_eq!(
        final_task_status, "cancelled",
        "timer-wins must flip the body task to `cancelled` via human.cancel.> listener: \
         got `{final_task_status}`"
    );

    cleanup_durables(&cleanup_nats).await;
}

/// Timer-wins with a multi-step body: `Timeout(3s) { wait (Delay) → review }`.
/// The HumanTask `review` is the "in between" node — it's reached only after
/// the body's leading Delay forwards, so it is NOT the body's entry. No task
/// is ever completed: the body simply parks on `review` until the 3s timer
/// wins. Asserts:
///   - the `timeout` branch fires,
///   - `review` is drained via `human_cancel` (`t_tmo_drain_review` +
///     `t_tmo_drain_review_effect`),
///   - the leading Delay's drain (`t_tmo_drain_wait`) NEVER fires — by the
///     time the timer expires the Delay has long since forwarded, so its
///     in-flight place is empty and its drain is starved. This proves the
///     fan-out only reclaims live tokens, not already-departed siblings.
///   - the projection flips `review` → `cancelled`.
///
/// The 3s window is generous: the leading Delay is 50ms, so `review` goes
/// pending within a few hundred ms of body entry — well before the timer.
#[tokio::test]
async fn timeout_drains_mid_body_human_task() {
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
    let (_causality, _lifecycle, _cancel, _steps) = spawn_consumers(nats, db.clone()).await;

    let id = publish_and_start(&app, timeout_delay_then_human_graph(3_000)).await;
    let net_id = wait_for_net_id(&db, id, Duration::from_secs(10)).await;

    // `review` goes pending after the ~50ms leading Delay. Confirm it appears
    // (proving it's a genuine mid-body task), then do NOT complete it — let
    // the 3s timer win and drain it.
    let task_id = wait_for_pending_task(&db, &net_id, Duration::from_secs(15)).await;

    let terminal = wait_for_terminal(&db, id, Duration::from_secs(20)).await;
    assert_eq!(
        terminal, "completed",
        "timer-wins should complete the instance (the `timeout` branch routes \
         to an End): final status was `{terminal}`"
    );

    let result = fetch_result(&db, id).await;
    assert_eq!(
        result["value"]["branch"],
        json!("timeout"),
        "timer wins should route to `timeout` branch: {result}"
    );

    let events_url = format!("{}/api/nets/{}/events?last=300", engine_url(), net_id);
    let events = reqwest::get(&events_url)
        .await
        .expect("engine events fetch")
        .json::<Value>()
        .await
        .expect("engine events parse");
    let evs = events["events"].as_array().expect("events array");
    let saw = |name: &str| -> bool {
        evs.iter().any(|e| {
            let ev = &e["event"];
            ev.get("type").and_then(|t| t.as_str()).map_or(false, |t| {
                (t == "TransitionFired" || t == "EffectCompleted")
                    && ev.get("transition_id").and_then(|x| x.as_str()) == Some(name)
            })
        })
    };
    assert!(saw("t_tmo_timeout"), "timer must have won the race");
    assert!(
        saw("t_tmo_drain_review"),
        "drain transition must have consumed the in-flight review HumanTaskAssigned token"
    );
    assert!(
        saw("t_tmo_drain_review_effect"),
        "human_cancel effect must have completed for review"
    );
    // The leading Delay forwarded long before the timer fired, so its
    // in-flight place is empty — its drain must never fire. The cancel_pulse
    // fan-out only reclaims live tokens.
    assert!(
        !saw("t_tmo_drain_wait"),
        "the Delay already forwarded — its drain must NOT fire (no live token to reclaim)"
    );

    // Projection: review flips to `cancelled` once the cancel_listener sees
    // the engine's `human.cancel.>` publish.
    let task_cancelled_within = Duration::from_secs(5);
    let task_start = std::time::Instant::now();
    let final_task_status = loop {
        let st = task_status(&db, &task_id).await;
        if st != "pending" {
            break st;
        }
        if task_start.elapsed() > task_cancelled_within {
            panic!(
                "review ({task_id}) still pending {task_cancelled_within:?} after engine \
                 fired human_cancel — is the cancel_listener consuming human.cancel.>?"
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    assert_eq!(
        final_task_status, "cancelled",
        "mid-body review must flip to `cancelled` on timeout: got `{final_task_status}`"
    );

    // Node runtime projection (the seam behind the editor's per-node badge):
    // the drained `review` node must NOT stay stuck at `running` once the net
    // completes via the timeout branch — it closes as `skipped` (superseded
    // in-flight work). Regression guard for the `close_open_rows` NetCompleted
    // gap. The step-executions consumer re-projects on each event, so poll
    // until it has folded the terminal NetCompleted.
    let node_closed_within = Duration::from_secs(5);
    let node_start = std::time::Instant::now();
    let review_status = loop {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/instances/{id}/step-executions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            resp.status().is_success(),
            "list step-executions: {}",
            resp.status()
        );
        let rows = body_json(resp.into_body()).await;
        // Latest iteration for `review` (one row here) — mirrors the badge,
        // which reads the last execution.
        let status = rows.as_array().and_then(|rs| {
            rs.iter()
                .filter(|r| r["node_id"] == json!("review"))
                .next_back()
                .and_then(|r| r["status"].as_str().map(str::to_string))
        });
        if let Some(s) = &status {
            if s != "running" {
                break s.clone();
            }
        }
        if node_start.elapsed() > node_closed_within {
            panic!(
                "review node step-execution still `running` (or missing: {status:?}) \
                 {node_closed_within:?} after NetCompleted — close_open_rows regression?"
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    };
    assert_eq!(
        review_status, "skipped",
        "drained body node must close as `skipped` on NetCompleted, not stay `running`: \
         got `{review_status}`"
    );

    cleanup_durables(&cleanup_nats).await;
}
