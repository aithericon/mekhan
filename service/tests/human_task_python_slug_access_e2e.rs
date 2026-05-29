//! End-to-end proof of the HumanTask → Python direct-slug-access *staging
//! contract*. Not a literal demo test — see TODO below.
//!
//! Builds a minimal `Start → review(HumanTask) → extract(Python) → End`
//! graph whose form-field shape matches the showcase's Review step
//! (`vendor_name`, `invoice_amount`, `verified`). The Python source uses
//! direct slug access (`review.vendor_name` etc. — exactly what the
//! editor's picker offers) and `assert`s the values match what the test
//! POSTs to `/api/v1/tasks/.../complete`.
//!
//!   * Without the compiler hoist (`apply_control_data_foundation` in
//!     `compile.rs`) → `<slug>.json` is the raw HumanTask envelope (form
//!     fields nested under `.data`), `review.vendor_name` is an
//!     `AttributeError`, the executor returns non-zero, the instance ends
//!     `failed`, this test fails.
//!   * With the hoist → `<slug>.json` is the *flattened* envelope (form
//!     fields hoisted out of `.data` to the top level), asserts pass, net
//!     completes.
//!
//! TODO: hand-rolled Rust, not the literal `showcaseGraph` from
//! `app/src/lib/templates/showcase.ts`. A follow-up should move the demos
//! to disk (real `.py` + `workflow.yaml`, loaded via the existing
//! `fs_ops::import_from_dir`) and exercise THAT here — so trigger node,
//! decision branches, scope group, parallel join, and Python source
//! drift are all caught too.
//!
//! Requires `just dev::up` (engine :13030, executor, rustfs S3 :9005,
//! NATS :14333). Set `TEST_POSTGRES_URL=postgres://mekhan:mekhan@localhost:15439/mekhan`
//! and `TEST_S3_{ENDPOINT,BUCKET,ACCESS_KEY,SECRET_KEY}` per
//! `reference_executor_e2e_s3_bucket`. Single-threaded
//! (`--test-threads=1`) — shares the live engine + executor.

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

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:13030".to_string())
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

/// Best-effort delete of this test's per-prefix durables on the shared
/// streams. Does NOT purge the streams themselves — they're shared with
/// the live dev daemon. Each prefix is uniquely UUID-derived so a
/// panicked test only leaks its own durables until `just dev reset`.
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
            let _ = stream
                .delete_consumer(&format!("{prefix}_{base}"))
                .await;
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
    (TaskHandle(causality.abort_handle()), TaskHandle(lifecycle.abort_handle()))
}

/// `Start → review(HumanTask) → extract(Python) → End` — the showcase's
/// HumanTask → Python sub-flow with the same form fields as `review` in
/// `app/src/lib/templates/showcase.ts`.
fn demo_graph() -> Value {
    json!({
        "nodes": [
            { "id": "s", "type": "start", "position": { "x": 0, "y": 0 },
              "data": { "type": "start", "label": "Start",
                        "initial": { "id": "in", "label": "In", "fields": [] } } },
            { "id": "review", "type": "human_task", "slug": "review",
              "position": { "x": 200, "y": 0 },
              "data": {
                  "type": "human_task",
                  "label": "Review Invoice",
                  "taskTitle": "Review",
                  "steps": [{
                      "id": "step1", "title": "Verify",
                      "blocks": [
                          { "type": "input", "field": {
                              "name": "vendor_name", "label": "Vendor",
                              "kind": "text", "required": true } },
                          { "type": "input", "field": {
                              "name": "invoice_amount", "label": "Amount",
                              "kind": "number", "required": true } },
                          { "type": "input", "field": {
                              "name": "verified", "label": "Verified",
                              "kind": "checkbox", "required": true } }
                      ]
                  }]
              } },
            { "id": "extract", "type": "automated_step", "slug": "extract",
              "position": { "x": 400, "y": 0 },
              "data": {
                  "type": "automated_step",
                  "label": "Extract",
                  "executionSpec": {
                      "backendType": "python",
                      "entrypoint": "main.py",
                      "config": {
                          "python": "python3",
                          "requirements": [],
                          "virtualenv": false,
                          "sdk": true,
                          "inherit_env": true,
                          "env": {}
                      }
                  },
                  "retryPolicy": { "maxRetries": 0, "strategy": { "type": "immediate" } },
                  "deploymentModel": { "mode": "executor" },
                  "output": {
                      "id": "out", "label": "Out", "fields": [
                          { "name": "vendor", "label": "Vendor", "kind": "text", "required": true },
                          { "name": "amount", "label": "Amount", "kind": "number", "required": true }
                      ]
                  }
              } },
            { "id": "e", "type": "end", "position": { "x": 600, "y": 0 },
              "data": { "type": "end", "label": "Done" } }
        ],
        "edges": [
            { "id": "e_in",  "source": "s",      "target": "review",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e_rx",  "source": "review", "target": "extract",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e_out", "source": "extract", "target": "e",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

/// Python source mirroring the showcase's `extract/main.py`. The asserts
/// pin the contract: if the staging hoist regresses, `review.vendor_name`
/// either AttributeErrors (no fix) or returns the wrong value (different
/// regression) — both bubble up as a non-zero exit and a `failed` instance.
const EXTRACT_PY: &str = r#"# Test fixture: prove HumanTask form fields are reachable via direct slug access.
vendor = review.vendor_name
amount = review.invoice_amount
verified = review.verified

assert vendor == "ACME-E2E", f"expected vendor ACME-E2E, got {vendor!r}"
assert amount == 1234.5, f"expected amount 1234.5, got {amount!r}"
assert verified is True, f"expected verified True, got {verified!r}"

# Envelope meta should still be reachable (kept at top level by the hoist).
assert isinstance(review.task_id, str) and review.task_id, "task_id must be a non-empty string"

# Direct slug access must NOT see the legacy `data` wrapper — it would mean
# the hoist didn't happen and the test only "worked" because the runner's
# AccessibleDict falls through. The post-hoist envelope drops the nested key.
assert not hasattr(review, "data") or review.data is None or not isinstance(review.data, dict), \
    "post-hoist envelope must not carry a nested `data` dict"

set_output("vendor", vendor)
set_output("amount", amount)
log_info("showcase demo extract OK", vendor=vendor, amount=amount)
"#;

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

async fn complete_review_task(app: &axum::Router, task_id: &str) {
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
                            "vendor_name": "ACME-E2E",
                            "invoice_amount": 1234.5,
                            "verified": true
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
        "complete task {task_id}: status {}",
        resp.status()
    );
}

async fn wait_for_terminal_status(
    db: &sqlx::PgPool,
    id: Uuid,
    timeout: Duration,
) -> String {
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
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

#[tokio::test]
async fn showcase_human_task_to_python_direct_slug_access() {
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
    let (_causality, _lifecycle) = spawn_consumers(nats, db.clone()).await;

    // Create + publish the demo sub-flow with the inline Python source.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Showcase Demo E2E",
                        "graph": demo_graph(),
                        "files": { "extract": { "main.py": EXTRACT_PY } },
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
                        "metadata": { "e2e": "showcase_demo" }
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

    // Wait for the Review task to appear, then complete it with the form
    // data the Python step asserts on.
    let task_id = wait_for_pending_task(&db, &net_id, Duration::from_secs(20)).await;
    complete_review_task(&app, &task_id).await;

    // The extract step must consume `review.vendor_name` / `review.invoice_amount`
    // via direct slug access, the asserts must pass, the executor must return
    // success, and the net must run through to End.
    let terminal = wait_for_terminal_status(&db, instance_id, Duration::from_secs(60)).await;
    assert_eq!(
        terminal, "completed",
        "instance ended as `{terminal}` — Python direct slug access against the HumanTask envelope failed; \
         most likely the form-field hoist regressed (compile.rs apply_control_data_foundation)"
    );

    cleanup_durables(&cleanup_nats).await;
}
