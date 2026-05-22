//! Cycle-closure integration test for the trigger-driven workflow pattern.
//!
//! Proves the missing link between (a) "a real workflow writes a catalogue
//! entry via the engine's `catalogue_register` effect" and (c) "a downstream
//! workflow spawns from a Catalog trigger". The link is the causality
//! ingest's call to `catalog::evaluate(dispatcher, &entry)` — every
//! production trigger fire goes through that one line, but until this test
//! nothing exercised it on the live path (coalesce_backfill_e2e seeded the
//! catalogue via direct SQL and relied on backfill, which bypasses ingest).
//!
//! Test shape — synthetic-NATS approach:
//!
//!   1. Publish a template: `Catalog trigger (category=<unique>) → Start → End`
//!      (`backfill=false`, `instance_concurrency=Unlimited`).
//!   2. Publish a synthetic `PersistedEvent::EffectCompleted` to NATS on
//!      `petri.events.{fake_net_id}.effect_completed` carrying
//!      `effect_handler_id = "catalogue_register"` and an `effect_result`
//!      shaped like the real `CatalogueRegisterCommand` (category matches
//!      our trigger's filter).
//!   3. Causality ingest picks the event off the PETRI_GLOBAL stream,
//!      inserts the catalogue row, and dispatches `catalog::evaluate` →
//!      fires our trigger → spawns instance.
//!   4. Verify (a) the catalogue row exists in `catalogue_entries`, (b) an
//!      instance was created for the template and completed.
//!
//! Why synthetic NATS instead of a real workflow? A real workflow writes a
//! catalogue entry only via the `catalogue_register` effect, which in turn
//! is reachable today only through (1) executor jobs with file outputs and
//! (2) the Start node's file-input lowering (which itself runs a `file_ops`
//! `probe` job through the executor). Both paths require the executor /
//! scheduler stack. The synthetic-event approach skips the executor while
//! still exercising the exact ingest line BO depends on — production
//! workflows just produce the same NATS event the test publishes.
//!
//! Requires `just dev up` (engine :3030 sharing the dev NATS broker). Run
//! serially (`--test-threads=1`).

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
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

// ── Harness ──────────────────────────────────────────────────────────────

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
    let c_triggers = triggers.clone();
    let causality = tokio::spawn(async move {
        start_causality_ingest(c_nats, c_db, c_sub, c_live, Some(c_triggers)).await;
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

    tokio::time::sleep(Duration::from_millis(300)).await;
    (
        TaskHandle(causality.abort_handle()),
        TaskHandle(lifecycle.abort_handle()),
    )
}

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

/// Build a `PersistedEvent` JSON envelope wrapping an `EffectCompleted`
/// event whose `effect_handler_id == "catalogue_register"`. Sequence /
/// hash are placeholders — ingest doesn't verify the hash chain on the
/// catalogue projection path.
fn catalogue_register_event(sequence: u64, cmd: Value) -> Value {
    json!({
        "sequence": sequence,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": {
            "type": "EffectCompleted",
            "transition_id": "t_test_catalogue_register",
            "transition_name": "test catalogue register",
            "consumed_tokens": [],
            "produced_tokens": [],
            "effect_handler_id": "catalogue_register",
            "effect_result": cmd,
            "read_tokens": []
        },
        "hash": format!("test-hash-{sequence}")
    })
}

async fn publish_event(
    js: &jetstream::Context,
    net_id: &str,
    suffix: &str,
    payload: &Value,
) {
    let subject = format!("petri.events.{net_id}.{suffix}");
    let bytes = serde_json::to_vec(payload).unwrap();
    js.publish(subject, bytes.into())
        .await
        .expect("publish event")
        .await
        .expect("event ACK");
}

async fn wait_for_instance_count(
    db: &sqlx::PgPool,
    template_id: Uuid,
    target: i64,
    timeout: Duration,
) -> Vec<(Uuid, String)> {
    let start = std::time::Instant::now();
    loop {
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, status FROM workflow_instances WHERE template_id = $1 ORDER BY created_at",
        )
        .bind(template_id)
        .fetch_all(db)
        .await
        .unwrap();
        if rows.len() as i64 >= target {
            return rows;
        }
        if start.elapsed() > timeout {
            panic!(
                "expected {target} instances for template {template_id} within {timeout:?}, \
                 got {} ({rows:?})",
                rows.len()
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
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

// ── Graph ────────────────────────────────────────────────────────────────

/// Catalog Trigger ─► Start ─► End. `backfill=false` so publish doesn't
/// race the synthetic NATS event. Filter on a per-run category to isolate.
fn template_graph(category: &str) -> Value {
    json!({
        "nodes": [
            { "id": "trig", "type": "trigger", "position": { "x": 0, "y": 0 },
              "data": { "type": "trigger", "label": "On Catalogue",
                        "enabled": true,
                        "source": {
                            "kind": "catalog",
                            "filters": { "category": { "eq": category } },
                            "backfill": false
                        },
                        "payloadMapping": [] } },
            { "id": "start", "type": "start", "position": { "x": 200, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": { "id": "in", "label": "Input", "fields": [] } } },
            { "id": "end", "type": "end", "position": { "x": 400, "y": 0 },
              "data": { "type": "end", "label": "Done",
                        "resultMapping": [] } }
        ],
        "edges": [
            { "id": "e_trig_start", "source": "trig", "target": "start",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e_start_end", "source": "start", "target": "end",
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
async fn live_catalogue_register_event_fires_catalog_trigger() {
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
    let _consumers = spawn_consumers(nats.clone(), db.clone(), triggers).await;

    let category = format!("test_live_ingest_{}", Uuid::new_v4().simple());
    let template = create_template(&app, "Live Ingest Cycle", template_graph(&category)).await;
    publish(&app, template).await;

    // Synthetic CatalogueRegisterCommand — same shape the engine emits via
    // `effects::CATALOGUE_REGISTER`. Carry the per-run category in
    // `detail.category` so it matches the trigger filter.
    let execution_id = format!("test-exec-{}", Uuid::new_v4());
    let artifact_id = format!("test-art-{}", Uuid::new_v4());
    let net_id = format!("mekhan-fake-{}", Uuid::new_v4());
    let cmd = json!({
        "execution_id": execution_id,
        "job_id": "test-job",
        "artifact_id": artifact_id,
        "name": "Synthetic Observation",
        "category": category,
        "filename": "obs.json",
        "mime_type": "application/json",
        "size_bytes": 0,
        "storage_path": "test/path/obs.json",
        // CatalogueRegisterCommand requires `created_at`; without it the
        // ingest projector silently drops the event (warns but returns Ok)
        // and the trigger never fires.
        "created_at": chrono::Utc::now().to_rfc3339()
    });
    let event = catalogue_register_event(1, cmd);

    let js = nats.jetstream().clone();
    publish_event(&js, &net_id, "effect_completed", &event).await;

    // The chain: ingest consumes the message → register_catalogue_entry
    // inserts a row → catalog::evaluate(dispatcher, &entry) fires the
    // Catalog trigger → dispatcher.fire spawns an instance of the
    // template. We wait for the workflow_instances row.
    let instances = wait_for_instance_count(
        &db,
        template,
        1,
        Duration::from_secs(15),
    )
    .await;
    let (instance_id, _) = instances[0].clone();
    wait_for_status(&db, instance_id, "completed", Duration::from_secs(15)).await;

    // Cross-check: the catalogue row exists with our category — proves
    // ingest's projection path ran (not just the trigger fire).
    let cat_row: Option<(String, String)> = sqlx::query_as(
        "SELECT id, category FROM catalogue_entries WHERE category = $1",
    )
    .bind(&category)
    .fetch_optional(&db)
    .await
    .unwrap();
    let (_, got_category) =
        cat_row.expect("catalogue_entries should hold the row ingest projected");
    assert_eq!(got_category, category);
}
