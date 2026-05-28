//! End-to-end coverage for **Part B — caller return paths** of rich return
//! values: the `POST /api/v1/triggers/{id}/fire` reply-mode selection, the
//! WaitForResult success / timeout mechanics, the SSE stream endpoint, and the
//! FireAndForget byte-compatibility guarantee.
//!
//! These exercise the seam the `rrv_*` tests in `causality_e2e.rs`
//! deliberately do *not*: a lifecycle consumer and the fire handler sharing
//! **one** `Arc<ResultWaiters>` (via `common::test_app_waiters`), so the
//! in-process oneshot actually resolves. Part A (the persisted envelope) is
//! covered there; this file is the Part B counterpart.
//!
//! Requires the dev/test infra + engine: `just dev up` (or the regression
//! infra). Each test builds a `MekhanNats` with a per-test
//! `with_consumer_prefix` so the lifecycle + causality durables are
//! scoped to this run and don't fight the live dev daemon's cursors.

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
use mekhan_service::triggers::ResultWaiters;

// ── Infra helpers (mirrors causality_e2e.rs — test crates don't share privates) ──

fn engine_nats_url() -> String {
    std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url())
}

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:13030".to_string())
}

async fn engine_available() -> bool {
    reqwest::get(format!("{}/api/nets/metadata", engine_url()))
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).to_string()
}

/// Minimal SSE parser for tests: collects (event-name, data) pairs from an
/// `text/event-stream` payload. Joins multi-line `data:` into a single string
/// per the SSE spec; drops comment lines and unrecognized fields. Defaults
/// `event` to `"message"` when omitted, matching browsers.
fn parse_sse(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut name = String::from("message");
    let mut data = String::new();
    for line in body.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !data.is_empty() {
                out.push((std::mem::take(&mut name), std::mem::take(&mut data)));
                name = String::from("message");
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if !data.is_empty() {
        out.push((name, data));
    }
    out
}

/// Abort-on-drop handle — the caller MUST keep these alive for the test's
/// duration or the spawned consumers die mid-test.
struct TaskHandle(tokio::task::AbortHandle);
impl Drop for TaskHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn spawn_consumer<F, Fut>(f: F) -> TaskHandle
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let handle = tokio::spawn(f());
    tokio::time::sleep(Duration::from_millis(200)).await;
    TaskHandle(handle.abort_handle())
}

/// Build a unique consumer prefix for this test invocation. Lets parallel
/// runs (and the live dev daemon) keep independent lifecycle/causality
/// cursors on the shared streams without any purge ritual.
fn test_prefix() -> String {
    format!("test_{}", Uuid::new_v4().simple())
}

async fn wait_for_instance_status(
    db: &sqlx::PgPool,
    instance_id: Uuid,
    target: &str,
    timeout: Duration,
) {
    let start = std::time::Instant::now();
    loop {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(instance_id)
                .fetch_optional(db)
                .await
                .unwrap()
                .flatten();
        if status.as_deref() == Some(target) {
            return;
        }
        if start.elapsed() > timeout {
            panic!(
                "instance {instance_id} did not reach '{target}' within {timeout:?} (current: {status:?})"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn fetch_result(db: &sqlx::PgPool, id: Uuid) -> Option<Value> {
    sqlx::query_scalar::<_, Option<Value>>("SELECT result FROM workflow_instances WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
}

// ── Graph builders ──────────────────────────────────────────────────────────

/// Manual Trigger → `start`, mapping the fired `amount` onto the Start port.
fn trigger_node() -> Value {
    json!({
        "id": "trig", "type": "trigger", "position": { "x": -200, "y": 0 },
        "data": {
            "type": "trigger", "label": "API",
            "source": { "kind": "manual", "form": [] },
            "concurrency": "allow",
            "payloadMapping": [ { "targetField": "amount", "expression": "amount" } ],
            "enabled": true
        }
    })
}

fn start_node() -> Value {
    json!({
        "id": "start", "type": "start", "position": { "x": 0, "y": 0 },
        "data": { "type": "start", "label": "Start",
                  "initial": { "id": "in", "label": "In", "fields": [
                      { "name": "amount", "label": "Amount", "kind": "number", "required": true }
                  ] } }
    })
}

fn trigger_edge() -> Value {
    json!({ "id": "et", "source": "trig", "target": "start",
            "targetHandle": "in", "type": "sequence" })
}

/// Trigger → Start → End(resultMapping total = input.amount). Completes fast.
fn success_graph() -> Value {
    json!({
        "nodes": [
            trigger_node(), start_node(),
            { "id": "end", "type": "end", "position": { "x": 240, "y": 0 },
              "data": { "type": "end", "label": "Done",
                        "resultMapping": [
                            { "targetField": "total", "expression": "input.amount" }
                        ] } }
        ],
        "edges": [
            trigger_edge(),
            { "id": "e1", "source": "start", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

/// Trigger → Start → HumanTask → End. The human task suspends the net
/// forever (nobody completes it), so the instance never reaches a terminal
/// state — a deterministic WaitForResult timeout.
fn never_terminates_graph() -> Value {
    json!({
        "nodes": [
            trigger_node(), start_node(),
            { "id": "hold", "type": "human_task", "position": { "x": 240, "y": 0 },
              "data": { "type": "human_task", "label": "Hold",
                        "taskTitle": "Hold", "instructionsMdsvex": "wait",
                        "steps": [ { "id": "s1", "title": "S", "blocks": [
                            { "type": "input", "field": {
                                "name": "ok", "label": "OK",
                                "kind": "checkbox", "required": true } } ] } ] } },
            { "id": "end", "type": "end", "position": { "x": 480, "y": 0 },
              "data": { "type": "end", "label": "Done" } }
        ],
        "edges": [
            trigger_edge(),
            { "id": "e1", "source": "start", "target": "hold",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e2", "source": "hold", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

// ── Harness ─────────────────────────────────────────────────────────────────

/// Build the app with a **shared** `ResultWaiters` Arc, spawn the causality +
/// lifecycle consumers (the lifecycle one resolving on that same Arc), publish
/// `graph`, and return everything the caller needs. The trigger node id is the
/// constant `"trig"`.
async fn setup(
    graph: Value,
    wait_timeout_secs: u64,
) -> (
    axum::Router,
    sqlx::PgPool,
    Arc<ResultWaiters>,
    TaskHandle,
    TaskHandle,
) {
    let engine_nats = engine_nats_url();
    let (app, db, waiters) =
        common::test_app_waiters(&engine_nats, &engine_url(), wait_timeout_secs).await;

    let nats = MekhanNats::connect(&engine_nats, None)
        .await
        .expect("connect to NATS")
        .with_consumer_prefix(test_prefix());
    let kv = nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("create KV");
    let sub_mgr = Arc::new(SubscriptionManager::new(kv, nats.jetstream().clone()));

    let c_nats = nats.clone();
    let c_db = db.clone();
    let c_sub = sub_mgr.clone();
    let c_live = LiveBroadcasts::new();
    let causality =
        spawn_consumer(move || start_causality_ingest(c_nats, c_db, c_sub, c_live, None)).await;

    let l_nats = nats.clone();
    let l_db = db.clone();
    let l_sub = sub_mgr.clone();
    let l_waiters = waiters.clone(); // SAME Arc as AppState.result_waiters
    let lifecycle = spawn_consumer(move || {
        start_lifecycle_listener(l_nats, l_db, l_sub, None, l_waiters)
    })
    .await;

    // Create + publish the template (publish registers the trigger live).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "name": "Reply Modes E2E",
                        "graph": graph,
                        "author_id": Uuid::new_v4(),
                    }))
                    .unwrap(),
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
    assert_eq!(resp.status(), StatusCode::OK, "publish template");

    (app, db, waiters, causality, lifecycle)
}

/// `POST /api/v1/triggers/trig/fire{query}` with `{ "payload": { "amount": 42 } }`.
fn fire_req(query: &str, accept: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/triggers/trig/fire{query}"))
        .header("content-type", "application/json");
    if let Some(a) = accept {
        b = b.header("accept", a);
    }
    b.body(Body::from(
        serde_json::to_string(&json!({ "payload": { "amount": 42 } })).unwrap(),
    ))
    .unwrap()
}

const SUCCESS_ENVELOPE: fn() -> Value = || json!({ "ok": true, "value": { "total": 42 } });

// ── Tests ───────────────────────────────────────────────────────────────────

/// FireAndForget (no selector) is byte-identical to pre-feature: a `result`
/// object and **no** `outcome` key (serde absence, not `null`). The poll path
/// still lands the persisted envelope.
#[tokio::test]
#[serial_test::serial]
async fn faf_default_has_no_outcome_and_polls() {
    if !engine_available().await {
        eprintln!("SKIP: engine not available — just dev up");
        return;
    }
    let (app, db, waiters, _c, _l) = setup(success_graph(), 30).await;

    let resp = app.clone().oneshot(fire_req("", None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "FAF fire is 200");
    let body = body_json(resp.into_body()).await;

    assert!(
        body.get("outcome").is_none(),
        "FAF response must omit `outcome` entirely (byte-compat): {body}"
    );
    assert_eq!(
        body["result"]["outcome"]["outcome"], "spawned",
        "FAF still spawns an instance: {body}"
    );
    let iid = Uuid::parse_str(body["result"]["outcome"]["instance_id"].as_str().unwrap())
        .unwrap();

    // Poll path: the shared lifecycle consumer drives it to terminal + persists
    // the same envelope the WaitForResult caller would have received.
    wait_for_instance_status(&db, iid, "completed", Duration::from_secs(30)).await;
    assert_eq!(fetch_result(&db, iid).await, Some(SUCCESS_ENVELOPE()));
    assert!(
        waiters.is_empty(),
        "FAF never registers a waiter — registry stays empty"
    );
    eprintln!("  ✓ faf_default_has_no_outcome_and_polls");
}

/// `?reply=wait` blocks until terminal, returns `200` with the
/// `outcome:{status,result}` superset; the envelope equals the persisted row;
/// the registry is empty afterward (resolve removed the entry — no leak).
/// Also asserts SSE-on-fire returns `text/event-stream` inline (same publish,
/// cheap): leading `fire` event carries the FireResult, then the instance's
/// domain events flow through to a terminal `result` envelope.
#[tokio::test]
#[serial_test::serial]
async fn wait_for_result_returns_envelope_no_leak() {
    if !engine_available().await {
        eprintln!("SKIP: engine not available — just dev up");
        return;
    }
    let (app, db, waiters, _c, _l) = setup(success_graph(), 30).await;

    // SSE on /fire is delivered inline: response is text/event-stream, the
    // first event (`fire`) carries the FireResult, then the JetStream-backed
    // instance events flow through to a terminal `result`. Same semantics
    // regardless of whether SSE was selected via Accept or ?reply=stream.
    for (q, accept, label) in [
        ("", Some("text/event-stream"), "Accept: text/event-stream"),
        ("?reply=stream", None, "?reply=stream"),
    ] {
        let resp = app.clone().oneshot(fire_req(q, accept)).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "{label}: SSE on /fire is 200"
        );
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(
            ct.starts_with("text/event-stream"),
            "{label}: content-type is SSE, got {ct:?}"
        );
        let body = body_string(resp.into_body()).await;
        let events = parse_sse(&body);
        // 1) Leading `fire` event with the FireResult (locator + outcome).
        let (kind, data) = events
            .first()
            .unwrap_or_else(|| panic!("{label}: empty SSE body: {body:?}"));
        assert_eq!(kind, "fire", "{label}: first SSE event is `fire`: {events:?}");
        let fire_v: Value = serde_json::from_str(data)
            .unwrap_or_else(|e| panic!("{label}: fire data not JSON: {e}: {data:?}"));
        let iid = fire_v["outcome"]["instance_id"]
            .as_str()
            .unwrap_or_else(|| panic!("{label}: fire event missing instance_id: {fire_v}"));
        Uuid::parse_str(iid).expect("instance_id is a uuid");
        // 2) Domain events appear (NetInitialized at minimum).
        assert!(
            events.iter().any(|(k, _)| k == "NetInitialized"),
            "{label}: stream replayed NetInitialized: {events:?}"
        );
        // 3) Stream closed on a terminal `result` carrying the success envelope.
        let (rkind, rdata) = events
            .last()
            .unwrap_or_else(|| panic!("{label}: no terminal event: {events:?}"));
        assert_eq!(rkind, "result", "{label}: stream ends with `result`: {events:?}");
        let envelope: Value = serde_json::from_str(rdata)
            .unwrap_or_else(|e| panic!("{label}: result data not JSON: {e}: {rdata:?}"));
        assert_eq!(
            envelope,
            SUCCESS_ENVELOPE(),
            "{label}: terminal envelope matches the persisted row"
        );
    }

    // WaitForResult.
    let resp = app
        .clone()
        .oneshot(fire_req("?reply=wait", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "WaitForResult success is 200");
    let body = body_json(resp.into_body()).await;

    assert_eq!(body["outcome"]["status"], "completed", "terminal status: {body}");
    assert_eq!(
        body["outcome"]["result"],
        SUCCESS_ENVELOPE(),
        "WaitForResult returns the structured envelope: {body}"
    );

    let iid = Uuid::parse_str(body["result"]["outcome"]["instance_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        fetch_result(&db, iid).await,
        Some(SUCCESS_ENVELOPE()),
        "wire envelope == persisted row (single source of truth)"
    );
    assert!(
        waiters.is_empty(),
        "resolve() removed the waiter — no registry leak"
    );
    eprintln!("  ✓ wait_for_result_returns_envelope_no_leak");
}

/// `?reply=wait` on a workflow that never terminates, with a 1s server cap,
/// degrades to `202 { instance_id }` and **deregisters** the waiter (no leak);
/// a later resolve is then a harmless no-op.
#[tokio::test]
#[serial_test::serial]
async fn wait_for_result_times_out_202_and_deregisters() {
    if !engine_available().await {
        eprintln!("SKIP: engine not available — just dev up");
        return;
    }
    let (app, _db, waiters, _c, _l) = setup(never_terminates_graph(), 1).await;

    let resp = app
        .clone()
        .oneshot(fire_req("?reply=wait", None))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::ACCEPTED,
        "WaitForResult timeout degrades to 202"
    );
    let body = body_json(resp.into_body()).await;
    let iid = Uuid::parse_str(body["instance_id"].as_str().unwrap())
        .expect("202 body carries the instance id to poll/stream");

    assert!(
        waiters.is_empty(),
        "handler deregistered the waiter on timeout — no leak"
    );
    // A late terminal resolve (were the human task ever completed) is a no-op
    // against the now-absent entry: idempotent, first-writer-wins.
    waiters.resolve(
        &iid,
        mekhan_service::triggers::TerminalOutcome {
            status: "completed".into(),
            result: None,
        },
    );
    assert!(waiters.is_empty(), "resolve on a deregistered id stays a no-op");
    eprintln!("  ✓ wait_for_result_times_out_202_and_deregisters");
}

/// SSE: fire FaF, let it finish, then open the stream. The already-terminal
/// fast path emits `connected` then a `result` event carrying the persisted
/// envelope, and closes (finite body). An unknown id is a real `404`.
#[tokio::test]
#[serial_test::serial]
async fn sse_already_terminal_emits_result_and_404() {
    if !engine_available().await {
        eprintln!("SKIP: engine not available — just dev up");
        return;
    }
    let (app, db, _w, _c, _l) = setup(success_graph(), 30).await;

    let resp = app.clone().oneshot(fire_req("", None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let iid = Uuid::parse_str(
        body_json(resp.into_body()).await["result"]["outcome"]["instance_id"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    wait_for_instance_status(&db, iid, "completed", Duration::from_secs(30)).await;

    // Unknown instance ⇒ real 404 HTTP status (not an SSE error frame).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/instances/{}/stream", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "unknown instance ⇒ 404");

    // Already-terminal stream: connected → result(envelope) → close.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/instances/{iid}/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream"),
        "SSE content type"
    );
    // Finite because the instance is terminal — the stream returns after the
    // `result` frame instead of holding the connection open.
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("event: connected"), "connected frame: {body}");
    assert!(body.contains("event: result"), "result frame: {body}");
    assert!(
        body.contains("\"ok\":true") && body.contains("\"total\":42"),
        "result frame carries the persisted envelope: {body}"
    );
    eprintln!("  ✓ sse_already_terminal_emits_result_and_404");
}
