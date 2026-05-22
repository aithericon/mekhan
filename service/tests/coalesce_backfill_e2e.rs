//! Integration test for Catalog-trigger backfill + template-level
//! `SingleActiveCoalesce` (the two primitives shipped in `a023b51` to
//! unblock the BO authoring shape).
//!
//! Tests both features in a single workflow because they're meant to
//! compose: a published BO retrain template should both backfill against
//! historical observations AND coalesce overlapping fires so the
//! read/train/update cycle isn't broken by parallel instances.
//!
//! Shape:
//!
//! ```text
//!  Catalog Trigger (category=<unique>, backfill=true)
//!    │
//!    └─► Start ──► HumanTask ──► End
//!
//!  instance_concurrency: SingleActiveCoalesce
//! ```
//!
//! Sequence the test drives:
//!
//! 1. Pre-seed 3 catalogue entries with a unique `category` so the test
//!    is isolated from anything else in the dev catalogue.
//! 2. Publish the template. The dispatcher schedules backfill which
//!    fires the trigger 3 times in catalogued_at order:
//!    - Fire #1: no active sibling → spawns instance A, parks at
//!      HumanTask
//!    - Fire #2: instance A active → `Coalesced`, sets dirty,
//!      stores entry #2 as last_skipped
//!    - Fire #3: instance A still active → `Coalesced`, overwrites
//!      last_skipped with entry #3
//! 3. Complete instance A's HumanTask. Instance A terminates →
//!    `lifecycle.rs` calls `on_instance_terminal` → dispatcher sees
//!    dirty=true, fires a follow-up with entry #3's payload →
//!    spawns instance B, parks at HumanTask.
//! 4. Complete instance B's HumanTask. Instance B terminates →
//!    `on_instance_terminal` sees dirty=false → no further fire.
//!
//! Final state proves both features together:
//! - 3 backfill matches collapsed to **2 instances** (1 + 1 follow-up)
//!   — that's coalesce
//! - Backfill ran at all — that's the wiring of `CatalogTrigger.backfill`
//! - Both instances reached `completed` — no spurious lockout
//!
//! Requires `just dev up` (engine :3030 sharing the dev NATS broker).
//! Run serially (`--test-threads=1`) — the lifecycle listener writes
//! back to the shared `workflow_instances` table.

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

// ── Harness helpers (mirrors loop_e2e / composition_e2e) ─────────────────

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

/// Spawn the two consumers a HumanTask-bearing trigger-coalesce test needs:
///   - causality ingest (projects HumanTaskRequest signals into `hpi_tasks`
///     so the test can find and complete the pending task)
///   - lifecycle (status updates + the `on_instance_terminal` hook the
///     dispatcher uses for `SingleActiveCoalesce` follow-up fires)
///
/// The lifecycle listener gets the SAME `Arc<TriggerDispatcher>` the app's
/// fire handler uses — without that, the coalesce follow-up would target
/// a separate dispatcher with no state.
async fn spawn_consumers(
    nats: MekhanNats,
    db: sqlx::PgPool,
    triggers: Arc<mekhan_service::triggers::TriggerDispatcher>,
) -> (TaskHandle, TaskHandle) {
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
        start_causality_ingest(c_nats, c_db, c_sub, c_live, None).await;
    });

    let l_nats = nats.clone();
    let l_db = db.clone();
    let l_sub = sub_mgr.clone();
    let lifecycle = tokio::spawn(async move {
        start_lifecycle_listener(
            l_nats,
            l_db,
            l_sub,
            Some(triggers),
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

/// Purge the JetStream consumers + streams a previous test run may have
/// left dirty so a re-run on the same dev broker starts from a clean slate.
async fn clean_slate(nats: &MekhanNats) {
    for (stream_name, consumer_name) in [
        ("PETRI_GLOBAL", "mekhan-causality-ingest"),
        ("PETRI_GLOBAL", "mekhan-lifecycle"),
        ("HUMAN_REQUESTS", "mekhan-human-task-ingest"),
        ("PROCESS", "mekhan-process-event-ingest"),
    ] {
        if let Ok(stream) = nats.jetstream().get_stream(stream_name).await {
            let _ = stream.delete_consumer(consumer_name).await;
        }
    }
    for stream_name in ["PETRI_GLOBAL", "HUMAN_REQUESTS", "PROCESS"] {
        if let Ok(stream) = nats.jetstream().get_stream(stream_name).await {
            let _ = stream.purge().await;
        }
    }
}

/// Pre-seed a catalogue entry. The Catalog trigger's filter is
/// `category=<unique>` so each test run is isolated.
async fn seed_catalogue_entry(db: &sqlx::PgPool, category: &str, name: &str, ordinal: i32) {
    let id = format!("test-{name}-{}", Uuid::new_v4());
    let execution_id = format!("test-exec-{}", Uuid::new_v4());
    // `catalogued_at` must be ordered so backfill replays oldest-first; bump
    // by `ordinal` seconds so the test can be sure of order.
    sqlx::query(
        r#"
        INSERT INTO catalogue_entries (
            id, execution_id, name, category, filename,
            catalogued_at, created_at
        ) VALUES ($1, $2, $3, $4, $5, NOW() + ($6 || ' seconds')::interval, NOW())
        "#,
    )
    .bind(&id)
    .bind(&execution_id)
    .bind(name)
    .bind(category)
    .bind(format!("{name}.bin"))
    .bind(ordinal.to_string())
    .execute(db)
    .await
    .expect("seed catalogue entry");
}

/// Wait for *exactly N* workflow_instances rows to exist for `template_id`.
/// Returns once the count stabilises at N (we poll for `stable_for` after
/// reaching the target so the test can also assert "no extra instance
/// spawned" — the BO regression for single-active concurrency).
async fn wait_for_instance_count(
    db: &sqlx::PgPool,
    template_id: Uuid,
    target: i64,
    timeout: Duration,
    stable_for: Duration,
) -> Vec<(Uuid, String)> {
    let start = std::time::Instant::now();
    let mut reached_at: Option<std::time::Instant> = None;
    loop {
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, status FROM workflow_instances WHERE template_id = $1 ORDER BY created_at",
        )
        .bind(template_id)
        .fetch_all(db)
        .await
        .unwrap();
        let count = rows.len() as i64;
        if count == target {
            if let Some(at) = reached_at {
                if at.elapsed() >= stable_for {
                    return rows;
                }
            } else {
                reached_at = Some(std::time::Instant::now());
            }
        } else if count > target {
            panic!(
                "expected exactly {target} instances for template {template_id}, got {count}: \
                 {rows:?}"
            );
        } else {
            reached_at = None;
        }
        if start.elapsed() > timeout {
            panic!(
                "did not see {target} instances for template {template_id} within {timeout:?} \
                 (last count: {count})"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_status(db: &sqlx::PgPool, id: Uuid, expected: &str, timeout: Duration) {
    let start = std::time::Instant::now();
    loop {
        let st: String =
            sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(id)
                .fetch_one(db)
                .await
                .unwrap();
        if st == expected {
            return;
        }
        if start.elapsed() > timeout {
            panic!("instance {id} did not reach `{expected}` within {timeout:?} (last: {st})");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Find the single pending HumanTask for `net_id` and return its id.
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

async fn complete_task(app: &axum::Router, task_id: &str) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/tasks/{task_id}/complete"))
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

async fn net_id_for(db: &sqlx::PgPool, instance_id: Uuid) -> String {
    sqlx::query_scalar("SELECT net_id FROM workflow_instances WHERE id = $1")
        .bind(instance_id)
        .fetch_one(db)
        .await
        .unwrap()
}

// ── Graph ────────────────────────────────────────────────────────────────

/// Catalog Trigger ─► Start ─► HumanTask ─► End, with
/// `instance_concurrency = single_active_coalesce`.
///
/// Catalog trigger filters on a per-test-run `category` to isolate from
/// any unrelated entries in the dev catalogue. The HumanTask body parks
/// the instance until the test completes it via `POST /api/tasks/{id}/
/// complete`, giving a deterministic window for coalescing fire #2 and
/// fire #3 while fire #1's instance is still active.
fn template_graph(category: &str) -> Value {
    json!({
        "instance_concurrency": { "mode": "single_active_coalesce" },
        "nodes": [
            { "id": "trig", "type": "trigger", "position": { "x": 0, "y": 0 },
              "data": { "type": "trigger", "label": "On New Observation",
                        "enabled": true,
                        "source": {
                            "kind": "catalog",
                            "filters": { "category": { "eq": category } },
                            "backfill": true
                        },
                        "payloadMapping": [] } },
            { "id": "start", "type": "start", "position": { "x": 200, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": { "id": "in", "label": "Input", "fields": [] } } },
            { "id": "task", "type": "human_task", "position": { "x": 400, "y": 0 },
              "data": { "type": "human_task", "label": "Hold",
                        "taskTitle": "Hold",
                        "steps": [{
                            "id": "review", "title": "Review",
                            "blocks": [{ "type": "input", "field": {
                                "name": "ack", "label": "Ack",
                                "kind": "checkbox", "required": true } }]
                        }] } },
            { "id": "end", "type": "end", "position": { "x": 600, "y": 0 },
              "data": { "type": "end", "label": "Done",
                        "resultMapping": [] } }
        ],
        "edges": [
            { "id": "e_trig_start", "source": "trig", "target": "start",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e_start_task", "source": "start", "target": "task",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e_task_end", "source": "task", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

async fn create_template(app: &axum::Router, name: &str, graph: Value) -> Uuid {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/templates")
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
                .uri(format!("/api/templates/{id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    if status != StatusCode::OK {
        let body_str = String::from_utf8_lossy(&bytes);
        panic!("publish {id}: HTTP {status}: {body_str}");
    }
}

// ── Test ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn backfill_with_single_active_coalesces_overlapping_fires() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }
    let nats_url = engine_nats_url();
    let (app, db, triggers) =
        common::test_app_with_petri_url_and_triggers(&nats_url, &engine_url()).await;
    let nats = MekhanNats::connect(&nats_url, None).await.expect("nats");
    clean_slate(&nats).await;
    // Reset so the projection-failure assertion at the end is about THIS
    // test, not residual drift from a prior run.
    mekhan_service::observability::reset_silent_drops();
    let _consumers = spawn_consumers(nats, db.clone(), triggers).await;

    // Unique per-run category so the trigger's filter selects only the
    // entries we seed — the dev catalogue may hold rows from other tests.
    let category = format!("test_coalesce_backfill_{}", Uuid::new_v4().simple());

    // Pre-seed 3 entries. catalogued_at is staggered so the order is
    // deterministic when backfill_one queries `catalogued_at ASC`.
    seed_catalogue_entry(&db, &category, "obs1", 1).await;
    seed_catalogue_entry(&db, &category, "obs2", 2).await;
    seed_catalogue_entry(&db, &category, "obs3", 3).await;

    // Publish — register_template fires the backfill task in a tokio::spawn
    // for the Catalog+backfill=true trigger we added. The backfill walks the
    // 3 entries in catalogued_at order and calls dispatcher.fire 3x:
    //   - fire 1 spawns instance A (parks at HumanTask)
    //   - fires 2 & 3 see A active → Coalesced; last_skipped = entry 3.
    let template = create_template(&app, "BO Retrain Mock", template_graph(&category)).await;
    publish(&app, template).await;

    // Wait for instance A to spawn and the backfill to complete (fires 2 & 3
    // arrive serially after fire 1 returns; A is parked the whole time).
    // Stability window confirms backfill has finished and no fourth fire
    // sneaked through.
    let instances_after_backfill = wait_for_instance_count(
        &db,
        template,
        1,
        Duration::from_secs(15),
        Duration::from_secs(2),
    )
    .await;
    let (instance_a, status_a) = instances_after_backfill[0].clone();
    assert_eq!(
        status_a, "running",
        "instance A should still be running (parked at HumanTask)"
    );

    // Complete instance A's HumanTask. Lifecycle terminal hook fires the
    // coalesced follow-up with entry 3's payload → spawns instance B.
    let net_a = net_id_for(&db, instance_a).await;
    let task_a = wait_for_pending_task(&db, &net_a, Duration::from_secs(10)).await;
    complete_task(&app, &task_a).await;
    wait_for_status(&db, instance_a, "completed", Duration::from_secs(15)).await;

    // Instance B must appear (the coalesced follow-up). Reaching count=2
    // and staying there proves the coalesce semantic: 3 backfill fires
    // collapsed to 2 instances, NOT 3.
    let instances_after_follow_up = wait_for_instance_count(
        &db,
        template,
        2,
        Duration::from_secs(15),
        Duration::from_secs(2),
    )
    .await;
    let (instance_b, _) = instances_after_follow_up
        .iter()
        .find(|(id, _)| *id != instance_a)
        .expect("follow-up instance B must exist")
        .clone();

    // Drive instance B to completion. After it terminates, on_instance_terminal
    // sees dirty=false (cleared when the follow-up was dispatched) → no third
    // fire. The next stability window confirms it.
    let net_b = net_id_for(&db, instance_b).await;
    let task_b = wait_for_pending_task(&db, &net_b, Duration::from_secs(10)).await;
    complete_task(&app, &task_b).await;
    wait_for_status(&db, instance_b, "completed", Duration::from_secs(15)).await;

    // Final assertion: still exactly 2 instances, both completed.
    let final_rows = wait_for_instance_count(
        &db,
        template,
        2,
        Duration::from_secs(5),
        Duration::from_secs(2),
    )
    .await;
    for (id, status) in &final_rows {
        assert_eq!(
            status, "completed",
            "instance {id} should be completed, was {status}"
        );
    }

    // Regression guard: no silent projection drops during the test. The
    // HumanTask completions, lifecycle terminals, and trigger fires all
    // flow through the ingest pipeline; a malformed shape anywhere would
    // bump this counter (with an `error!`-level structured log).
    assert_eq!(
        mekhan_service::observability::silent_drops(),
        0,
        "silent drops occurred during this test — \
         check error logs targeted at `mekhan_service::observability::silent_drop`"
    );
}
