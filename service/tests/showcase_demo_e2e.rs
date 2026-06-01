//! End-to-end coverage of the *literal* shipped `Invoice Processing Demo`.
//!
//! Loads `demos/invoice-processing/` via [`mekhan_service::demos::load_demo`]
//! — the same disk fixture the runtime seeder publishes at service startup
//! — then drives it through the full pipeline:
//!
//! 1. POST `/api/v1/templates` with the loaded `(graph, files)`.
//! 2. POST `/api/v1/templates/{id}/publish` (stages Python files + generated
//!    `_aithericon_io.{py,pyi}` to S3, compiles AIR).
//! 3. POST `/api/v1/instances` with `start_tokens` seeding the Start block's
//!    `invoice_file` + `invoice_id` fields.
//! 4. Complete the Review human task with a *low-value* amount (< $5,000)
//!    so the Decision routes to the `Processed` end — exercises Start →
//!    Review → Extract (Python) → Decision → End without the high-value
//!    branch (Manager Approval + Compliance + Join) which would need a
//!    second human-task completion.
//! 5. Assert the instance reaches `completed`.
//!
//! Companion to `human_task_python_slug_access_e2e.rs` (which uses a
//! hand-rolled minimal graph and asserts on specific Python values). This
//! test catches drift between the shipped demo and the platform: a new
//! required field on `WorkflowNodeData`, a renamed variant, a token-shape
//! contract change — all surface as a publish-time compile error or a
//! runtime-time assert failure here, never on the user's first try.
//!
//! Requires `just dev::up` (engine :3030, executor, rustfs S3 :9005,
//! NATS :4333). Set
//! `TEST_POSTGRES_URL=postgres://mekhan:mekhan@localhost:5439/mekhan`
//! and the `TEST_S3_*` env per `reference_executor_e2e_s3_bucket`.
//!
//! Parallel-safe: builds `MekhanNats` with a per-test
//! [`MekhanNats::with_consumer_prefix`] so the lifecycle + causality
//! durables are uniquely named, and the test net_id scopes the events
//! that matter. No `clean_slate` ritual — durables die with the test.

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
use mekhan_service::demos;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::nats::MekhanNats;

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:3030".to_string())
}

fn engine_nats_url() -> String {
    std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url())
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

/// Delete the per-test durables this test created. Best-effort; the test
/// stream itself (`PETRI_GLOBAL` etc.) is shared with the live dev daemon
/// and must NOT be purged.
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

/// Locate `demos/invoice-processing` relative to this test crate. The
/// service tests run under `service/` (the binary crate's manifest dir),
/// so the demos directory is one level up.
fn demo_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("service crate has a parent")
        .join("demos/invoice-processing")
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
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_terminal_status(db: &sqlx::PgPool, id: Uuid, timeout: Duration) -> String {
    let start = std::time::Instant::now();
    loop {
        let st: String = sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
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
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

#[tokio::test]
async fn invoice_processing_demo_low_value_path_completes() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev::up`",
            engine_url()
        );
    }
    let nats_url = engine_nats_url();
    let (app, db) = common::test_app_with_petri_url(&nats_url, &engine_url()).await;
    // Per-test consumer prefix scopes the lifecycle + causality durables
    // away from any concurrent test or the live dev daemon — no purge,
    // no shared cursor.
    let prefix = format!("test_{}", Uuid::new_v4().simple());
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("nats")
        .with_consumer_prefix(prefix);
    let cleanup_nats = nats.clone();
    let (_causality, _lifecycle) = spawn_consumers(nats, db.clone()).await;

    // Load the literal demo. The test runs against an isolated DB+template
    // (we POST a fresh copy under a new id) so the seeded singleton at
    // `00000000-0000-0000-0000-000000000001` is unaffected. The shape of
    // the graph + files comes straight from disk — drift between this
    // fixture and any platform contract surfaces here.
    let demo = demos::load_demo(&demo_dir()).expect("load demos/invoice-processing");
    assert_eq!(demo.metadata.name, "Invoice Processing Demo");

    // POST a fresh copy. The name is suffixed so successive runs can't
    // collide on the (unique) name constraint should one exist.
    let unique_name = format!("Invoice Processing Demo E2E {}", Uuid::new_v4().simple());
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": unique_name,
                        "graph": demo.graph,
                        "files": demo.files,
                        "author_id": Uuid::new_v4(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create template");
    let template_id: Uuid = body_json(resp.into_body()).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Publish: stages files to S3 + compiles AIR. Catches drift between
    // the disk fixture and the compiler's expectations (e.g. a removed
    // node type, a renamed field).
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
    let pub_body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "publish: {pub_body}");

    // Seed the Start block. The showcase's Start has `invoice_file` (file)
    // + `invoice_id` (text); we provide a synthetic file ref + a test id.
    // The `key` doesn't need to point at a real S3 object — the Review
    // task's image/download blocks reference it for display only, and
    // the Python extract step doesn't read the file.
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
                        "start_tokens": [{
                            "start_block_id": "start",
                            "token": {
                                "invoice_file": {
                                    "key": "test-fixtures/synthetic-invoice.png",
                                    "filename": "synthetic-invoice.png",
                                    "content_type": "image/png",
                                    "size": 1024,
                                    "url": "/api/v1/files/test-fixtures/synthetic-invoice.png"
                                },
                                "invoice_id": "E2E-LOW-VALUE"
                            }
                        }],
                        "metadata": { "e2e": "invoice_processing_demo_low_value" }
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
    let instance_id: Uuid = inst["id"].as_str().unwrap().parse().unwrap();
    let net_id = inst["net_id"].as_str().expect("net_id").to_string();

    // Drive the Review task — low-value amount routes to the Processed
    // end without entering the Scope/Split/Manager-Approval branch.
    let task_id = wait_for_pending_task(&db, &net_id, Duration::from_secs(20)).await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/tasks/{task_id}/complete"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "data": {
                            "vendor_name": "ACME Demo",
                            "invoice_amount": 250.0,
                            "description": "low-value invoice for e2e"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "complete review task: {}",
        resp.status()
    );

    // Extract (Python) runs against the hoisted `review` envelope and
    // emits its declared outputs; Decision sees `review.invoice_amount =
    // 250` and routes to the `end-processed` Processed end. The whole net
    // must run through to `completed`.
    let terminal = wait_for_terminal_status(&db, instance_id, Duration::from_secs(90)).await;
    assert_eq!(
        terminal, "completed",
        "instance ended `{terminal}` — the literal Invoice Processing demo regressed; \
         check the executor + service logs (the failure can be in publish, in the \
         Python extract step, in the Decision guard, or in the lifecycle listener)"
    );

    // Best-effort cleanup of this test's per-test durables on the shared
    // streams. A test panic above leaks them until `just dev reset`; each
    // is uniquely prefixed so they don't collide.
    cleanup_durables(&cleanup_nats).await;
}
