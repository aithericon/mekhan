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
        start_causality_ingest(
            c_nats,
            c_db,
            c_sub,
            c_live,
            Some(c_triggers),
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

/// Build a unique consumer prefix for this test invocation. With it set
/// on `MekhanNats`, the lifecycle + causality durables are uniquely named
/// so parallel runs (and the live dev daemon) keep independent cursors
/// on the shared streams.
fn test_prefix() -> String {
    format!("test_{}", Uuid::new_v4().simple())
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

async fn publish_event(js: &jetstream::Context, net_id: &str, suffix: &str, payload: &Value) {
    // Post-multi-tenancy subject scheme: petri.{ws}.{net}.events.{suffix}.
    let ws = "00000000-0000-0000-0000-000000000000";
    let subject = format!("petri.{ws}.{net_id}.events.{suffix}");
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
        let st: String = sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
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

/// Catalog Trigger filtering on `category=metric` AND a `user_metadata.kind`
/// sentinel, with a non-empty `payloadMapping` projecting two user_metadata
/// fields onto kind:json Start fields. This is the shape the (fixed) 12a demo
/// uses — the bare `template_graph` above proves spawn but never exercises
/// user_metadata filtering or payloadMapping projection, which is the actual
/// Phase-4 gap.
fn template_graph_with_metadata(kind_sentinel: &str) -> Value {
    json!({
        "nodes": [
            { "id": "trig", "type": "trigger", "position": { "x": 0, "y": 0 },
              "data": { "type": "trigger", "label": "On BO Observation",
                        "enabled": true,
                        "source": {
                            "kind": "catalog",
                            "filters": {
                                "category": { "eq": "metric" },
                                "user_metadata.kind": { "eq": kind_sentinel }
                            },
                            "backfill": false
                        },
                        "payloadMapping": [
                            { "targetField": "observations", "expression": "user_metadata.observations" },
                            { "targetField": "last_z", "expression": "user_metadata.z" }
                        ] } },
            { "id": "start", "type": "start", "position": { "x": 200, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": { "id": "in", "label": "Input", "fields": [
                            { "name": "observations", "label": "Observations", "kind": "json", "required": true },
                            { "name": "last_z", "label": "Latest z", "kind": "json", "required": true }
                        ] } } },
            { "id": "end", "type": "end", "position": { "x": 400, "y": 0 },
              "data": { "type": "end", "label": "Done", "resultMapping": [] } }
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
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("nats")
        .with_consumer_prefix(test_prefix());
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
    // The projector now content-addresses the artifact and refuses to write a
    // half catalogue/inventory row, so the synthetic command must carry a
    // content_hash (unique per run to avoid ON CONFLICT (content_hash) clashing
    // with prior runs on the shared dev DB) and a storage_path.
    let content_hash = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let cmd = json!({
        "execution_id": execution_id,
        "job_id": "test-job",
        "artifact_id": artifact_id,
        "name": "Synthetic Observation",
        "category": category,
        "filename": "obs.json",
        "mime_type": "application/json",
        "size_bytes": 0,
        "storage_path": format!("test/path/{artifact_id}.json"),
        "content_hash": content_hash,
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
    let instances = wait_for_instance_count(&db, template, 1, Duration::from_secs(15)).await;
    let (instance_id, _) = instances[0].clone();
    wait_for_status(&db, instance_id, "completed", Duration::from_secs(15)).await;

    // Cross-check: the catalogue row exists with our category — proves
    // ingest's projection path ran (not just the trigger fire).
    let cat_row: Option<(String, String)> =
        sqlx::query_as("SELECT id, category FROM catalogue_entries WHERE category = $1")
            .bind(&category)
            .fetch_optional(&db)
            .await
            .unwrap();
    let (_, got_category) =
        cat_row.expect("catalogue_entries should hold the row ingest projected");
    assert_eq!(got_category, category);

    // (The original `silent_drops == 0` invariant guarded against
    // malformed synthetic events sneaking through. It only worked because
    // the old `clean_slate` purged PETRI_GLOBAL first, which we can no
    // longer do without destroying the live dev daemon's in-flight state.
    // Background bridge messages on the shared stream now hit our
    // prefixed causality consumer and bump silent_drops legitimately.
    // The catalogue row assertion above already catches the
    // missing-`created_at` bug this test was originally written for, and
    // the sibling `malformed_catalogue_register_bumps_silent_drops` test
    // proves the loud-failure wiring still works.)
}

/// Phase-4 cycle-closure with a REAL trigger shape: prove that a catalogue
/// entry carrying `category=metric` + a `user_metadata.kind` sentinel fires a
/// trigger that filters on BOTH the category and the sentinel, projects two
/// user_metadata fields through a non-empty payloadMapping onto kind:json
/// Start fields, passes the strict Start-contract gate (no number-field
/// reject), and spawns + completes an instance. Executor-free: the synthetic
/// catalogue_register event stands in for the 12b producer's real LogArtifact.
#[tokio::test]
async fn metric_with_kind_sentinel_fires_payload_mapping_trigger() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }
    let nats_url = engine_nats_url();
    let (app, db, triggers) =
        common::test_app_with_petri_url_and_triggers(&nats_url, &engine_url()).await;
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("nats")
        .with_consumer_prefix(test_prefix());
    let _consumers = spawn_consumers(nats.clone(), db.clone(), triggers).await;

    // Per-run sentinel so this test isolates from any concurrent fires.
    let kind_sentinel = format!("bo_observation_{}", Uuid::new_v4().simple());
    let template = create_template(
        &app,
        "BO Observation Cycle",
        template_graph_with_metadata(&kind_sentinel),
    )
    .await;
    publish(&app, template).await;

    // Synthetic CatalogueRegisterCommand mirroring what the 12b producer's
    // log_artifact(category=metric, metadata={kind, observations, z}) becomes
    // after the engine's catalogue_register effect: category=metric and a
    // user_metadata map of STRING values (proto map<string,string>).
    let execution_id = format!("test-exec-{}", Uuid::new_v4());
    let artifact_id = format!("test-art-{}", Uuid::new_v4());
    let net_id = format!("mekhan-fake-{}", Uuid::new_v4());
    // Content-address the synthetic artifact (see sibling test) so the projector
    // couples it into both catalogue + inventory rather than failing closed.
    let content_hash = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let cmd = json!({
        "execution_id": execution_id,
        "job_id": "test-job",
        "artifact_id": artifact_id,
        "name": "bo_obs",
        "category": "metric",
        "filename": "obs.json",
        "mime_type": "application/json",
        "size_bytes": 0,
        "storage_path": format!("test/path/{artifact_id}.json"),
        "content_hash": content_hash,
        "user_metadata": {
            "kind": kind_sentinel,
            "observations": "[{\"a\":0.3,\"d\":0.7,\"z\":1.42}]",
            "z": "1.42"
        },
        "created_at": chrono::Utc::now().to_rfc3339()
    });
    let event = catalogue_register_event(1, cmd);

    let js = nats.jetstream().clone();
    publish_event(&js, &net_id, "effect_completed", &event).await;

    // Trigger filter must match BOTH category=metric and the kind sentinel;
    // payloadMapping must project user_metadata.observations / user_metadata.z
    // onto the kind:json Start fields and pass the strict Start gate.
    let instances = wait_for_instance_count(&db, template, 1, Duration::from_secs(15)).await;
    let (instance_id, _) = instances[0].clone();
    wait_for_status(&db, instance_id, "completed", Duration::from_secs(15)).await;
}

/// Proof-of-loudness: publish a deliberately malformed `catalogue_register`
/// event (missing the required `created_at`) and verify that
/// `silent_drops()` bumps. Catches future regressions where someone
/// "helpfully" adds a fallback or removes the error-level log — anything
/// that silently absorbs the drop without bumping the counter.
#[tokio::test]
async fn malformed_catalogue_register_bumps_silent_drops() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }
    let nats_url = engine_nats_url();
    let (_app, _db, triggers) =
        common::test_app_with_petri_url_and_triggers(&nats_url, &engine_url()).await;
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("nats")
        .with_consumer_prefix(test_prefix());
    mekhan_service::observability::reset_silent_drops();
    let db = _db;
    let _consumers = spawn_consumers(nats.clone(), db.clone(), triggers).await;

    let baseline = mekhan_service::observability::silent_drops();
    assert_eq!(baseline, 0, "reset should leave counter at 0");

    // Build an EffectCompleted event whose effect_result is missing the
    // required `created_at` field on CatalogueRegisterCommand.
    let net_id = format!("mekhan-fake-malformed-{}", Uuid::new_v4());
    let bad_cmd = json!({
        "execution_id": "x",
        "job_id": "y",
        "artifact_id": "z",
        "name": "Bad",
        "category": "wont_fire_anyway",
        "filename": "x.bin"
        // created_at intentionally omitted
    });
    let event = catalogue_register_event(1, bad_cmd);
    publish_event(nats.jetstream(), &net_id, "effect_completed", &event).await;

    // Wait for ingest to consume and the counter to bump.
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if mekhan_service::observability::silent_drops() > baseline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let after = mekhan_service::observability::silent_drops();
    assert!(
        after > baseline,
        "malformed catalogue_register event should bump silent_drops \
         (baseline={baseline}, after={after}) — if this fails, either the \
         loud-failure wiring at `record_silent_drop` was removed or \
         the projector grew a silent fallback that swallows malformed shapes"
    );
}

/// Full DLQ round-trip: publish a malformed catalogue_register event, verify
/// the record reaches the `MEKHAN_SILENT_DROPS` stream via the drainer, and
/// the `GET /api/v1/observability/silent-drops` endpoint returns it with the
/// payload + per-site context intact.
///
/// This is the test that proves the dead-letter queue actually does what it
/// promises: when something goes silently sideways, an operator can later
/// query exactly *what* sideways and inspect the original bytes.
#[tokio::test]
async fn dlq_endpoint_returns_malformed_payload() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }
    let nats_url = engine_nats_url();
    let (app, db, triggers) =
        common::test_app_with_petri_url_and_triggers(&nats_url, &engine_url()).await;
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("nats")
        .with_consumer_prefix(test_prefix());
    nats.ensure_silent_drops_stream()
        .await
        .expect("ensure MEKHAN_SILENT_DROPS");
    mekhan_service::observability::reset_silent_drops();

    // Install the drainer (idempotent — first caller wins). If a previous
    // test in the same process already installed it, the existing drainer
    // continues to receive our records; either way the stream gets fed.
    if let Some(rx) = mekhan_service::observability::install_drainer() {
        let drainer_nats = nats.clone();
        tokio::spawn(async move {
            mekhan_service::observability::drain_silent_drops(drainer_nats, rx).await;
        });
        // Tiny pause so the drainer's loop is actually awaiting the channel
        // by the time we send the first record.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let _consumers = spawn_consumers(nats.clone(), db.clone(), triggers).await;

    // Same malformed event as the proof-of-loudness test — missing
    // `created_at`. Carries a unique sentinel string in `category` so we
    // can match the DLQ record back to this test without picking up
    // residual records from other tests in the same process.
    let sentinel = format!("dlq_sentinel_{}", Uuid::new_v4().simple());
    let net_id = format!("mekhan-fake-dlq-{}", Uuid::new_v4());
    let bad_cmd = json!({
        "execution_id": "x",
        "job_id": "y",
        "artifact_id": "z",
        "name": "Bad",
        "category": sentinel,
        "filename": "x.bin"
        // created_at intentionally omitted
    });
    let event = catalogue_register_event(1, bad_cmd);
    publish_event(nats.jetstream(), &net_id, "effect_completed", &event).await;

    // Wait for the silent_drops counter to bump (proves ingest consumed
    // the event and the projector called record_silent_drop_with).
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if mekhan_service::observability::silent_drops() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        mekhan_service::observability::silent_drops() > 0,
        "silent_drops counter never bumped — projector didn't fire"
    );

    // Now poll the DLQ endpoint until our sentinel appears. The drainer is
    // async (channel → NATS publish), so there's a small race window
    // between the counter bump and the stream having the record.
    let resp_records = poll_dlq_for_sentinel(
        &app,
        &sentinel,
        "catalogue_register",
        Duration::from_secs(10),
    )
    .await;

    // Find the record carrying our sentinel — the same drainer feeds every
    // test in this process so the stream may have unrelated records too.
    let ours = resp_records
        .iter()
        .find(|r| {
            r.kind == "catalogue_register"
                && r.payload
                    .as_deref()
                    .map(|p| p.contains(sentinel.as_str()))
                    .unwrap_or(false)
        })
        .expect("DLQ should contain a record carrying our sentinel category");

    // Verify the record's shape carries what we need for forensics.
    assert!(
        ours.error.to_lowercase().contains("created_at")
            || ours.error.to_lowercase().contains("missing"),
        "error string should name the missing field, was: {}",
        ours.error
    );
    assert_eq!(
        ours.context["net_id"].as_str(),
        Some(net_id.as_str()),
        "context.net_id should match the publisher: {:?}",
        ours.context
    );
    assert_eq!(
        ours.context["event_seq"].as_i64(),
        Some(1),
        "context.event_seq should match the synthetic envelope's seq: {:?}",
        ours.context
    );
    let payload = ours.payload.as_deref().expect("payload should be captured");
    assert!(
        payload.contains(&sentinel),
        "captured payload should include the malformed input verbatim"
    );
}

/// Helper: poll `GET /api/v1/observability/silent-drops` until a record
/// carrying `sentinel` shows up (or timeout). Returns the records array
/// of the matching response.
async fn poll_dlq_for_sentinel(
    app: &axum::Router,
    sentinel: &str,
    kind: &str,
    timeout: Duration,
) -> Vec<mekhan_service::observability::SilentDropRecord> {
    let start = std::time::Instant::now();
    loop {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/v1/observability/silent-drops?kind={kind}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(
            status.is_success(),
            "GET silent-drops: HTTP {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let records: Vec<mekhan_service::observability::SilentDropRecord> =
            serde_json::from_value(body["records"].clone()).unwrap();
        if records.iter().any(|r| {
            r.payload
                .as_deref()
                .map(|p| p.contains(sentinel))
                .unwrap_or(false)
        }) {
            return records;
        }
        if start.elapsed() > timeout {
            panic!(
                "DLQ endpoint never returned a record carrying `{sentinel}` within {timeout:?}; \
                 got {} records — drainer might not be running, or the stream consumer \
                 didn't pick up the message",
                records.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
