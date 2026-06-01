//! Live end-to-end of the full template-tests cycle against a real engine.
//!
//!   create template → publish → instance → human-task completion →
//!   promote-to-test → run (pass) → patch assertion → run (fail).
//!
//! Drives every seam the per-handler tests stub out: the engine launches the
//! AIR, the executor would handle Automated steps, the causality consumer
//! projects `hpi_tasks`, the `/api/v1/tasks/.../complete` path sends a
//! `human.completed.<net_id>.<place>` NATS message, and the runner's
//! auto-completer + scope builder run against real `causality_events` /
//! `step_execution` rows rather than a hand-seeded fixture.
//!
//! Skips silently when the engine isn't reachable so `cargo test --workspace`
//! stays green without `just dev up`. Single-threaded — shares the live engine
//! with other `*_live_e2e.rs` / `*_e2e.rs` tests.

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

struct TaskHandle(tokio::task::AbortHandle);
impl Drop for TaskHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Spawn the causality-ingest + lifecycle-listener tasks the test app needs
/// to project engine NATS events into Postgres rows (`hpi_tasks`,
/// `causality_events`, `workflow_instances.status`). Mirrors the shape of
/// `human_task_python_slug_access_e2e.rs::spawn_consumers`.
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

/// Best-effort delete of this test's per-prefix durables on the shared streams.
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

/// `Start(amount) → review(HumanTask) → End` — the minimum shape that proves
/// every promote-to-test extractor path: a Start field round-trips into the
/// stored `start_tokens`, the HumanTask completion shows up keyed by slug in
/// `human_answers`, and the End drives the instance to `completed` so the
/// runner can build the scope and evaluate assertions.
fn demo_graph() -> Value {
    json!({
        "nodes": [
            {
                "id": "start", "type": "start",
                "position": { "x": 0, "y": 0 },
                "data": {
                    "type": "start", "label": "Start",
                    "initial": {
                        "id": "in", "label": "In",
                        "fields": [
                            { "name": "amount", "label": "Amount",
                              "kind": "number", "required": true },
                            // `name` is here to back the interpolation
                            // assertion below: a `"Hello, {{ start.name }}!"`
                            // template compares against `result.value.greeting`
                            // built from the same field via End.resultMapping.
                            { "name": "name", "label": "Name",
                              "kind": "text", "required": true }
                        ]
                    }
                }
            },
            {
                "id": "review", "type": "human_task", "slug": "review",
                "position": { "x": 200, "y": 0 },
                "data": {
                    "type": "human_task",
                    "label": "Review",
                    "taskTitle": "Review",
                    "steps": [{
                        "id": "step1", "title": "Verify",
                        "blocks": [
                            { "type": "input", "field": {
                                "name": "approved", "label": "Approved",
                                "kind": "checkbox", "required": true } },
                            { "type": "input", "field": {
                                "name": "comment", "label": "Comment",
                                "kind": "text", "required": false } }
                        ]
                    }]
                }
            },
            {
                "id": "end", "type": "end",
                "position": { "x": 400, "y": 0 },
                "data": {
                    "type": "end", "label": "Done",
                    // marker — Rhai literal, used by the self-reference and
                    //   the "expected_resolved on failure" checks.
                    // greeting — built from the inbound terminal token's
                    //   `name` field (threaded through from Start), used by
                    //   the interpolation assertion below.
                    "resultMapping": [
                        { "targetField": "marker", "expression": "\"e2e\"" },
                        {
                            "targetField": "greeting",
                            // Literal — matches what the interpolation
                            // assertion below resolves to (`"Hello, {{ start.name }}!"`
                            // with start_tokens.name = "Alice"). Can't pull
                            // from `input.name` here: the End's `input` is
                            // the slim control token, not the parked Start
                            // data (control-data model — see
                            // docs/10-control-data-token-model.md).
                            "expression": "\"Hello, Alice!\""
                        }
                    ]
                }
            }
        ],
        "edges": [
            { "id": "e1", "source": "start", "target": "review",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e2", "source": "review", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    })
}

/// Poll `hpi_tasks` until the engine's `human.request` effect projects a
/// pending row for this net. Returns the task id the `/api/v1/tasks/.../complete`
/// endpoint takes.
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
async fn template_tests_live_full_cycle() {
    if !engine_available().await {
        eprintln!(
            "engine not available at {} — skipping (start `just dev up`)",
            engine_url()
        );
        return;
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

    // 1. Create + publish the demo template.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Template-Tests Live E2E",
                        "graph": demo_graph(),
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
    let body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "publish: {body}");

    // 2. Create an instance with a real start token. This is the source
    //    instance that promote-to-test will later scoop into a fixture.
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
                            "token": { "amount": 1234, "name": "Alice" }
                        }],
                        "created_by": Uuid::new_v4(),
                        "metadata": { "e2e": "template_tests_live" }
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

    // 3. Drive the human task → instance completion.
    let task_id = wait_for_pending_task(&db, &net_id, Duration::from_secs(20)).await;
    complete_task(
        &app,
        &task_id,
        json!({ "approved": true, "comment": "lgtm" }),
    )
    .await;
    let terminal = wait_for_terminal_status(&db, instance_id, Duration::from_secs(30)).await;
    assert_eq!(
        terminal, "completed",
        "source instance never reached `completed` — promote-to-test would scoop a partial fixture"
    );

    // 4. Promote to test. Asserts the extractors actually saw the live event
    //    stream: the start token's `amount` should be 1234, `human_answers`
    //    keyed by the `review` slug should carry the form data, and
    //    `reference_scope` should be populated.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/instances/{instance_id}/promote-to-test"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "name": "live-promoted" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "promote-to-test");
    let test_row = body_json(resp.into_body()).await;
    let test_id: Uuid = test_row["id"].as_str().unwrap().parse().unwrap();

    let start_tokens = test_row["start_tokens"]
        .as_array()
        .expect("start_tokens array");
    assert_eq!(start_tokens.len(), 1, "got: {test_row}");
    assert_eq!(start_tokens[0]["start_block_id"], "start");
    assert_eq!(
        start_tokens[0]["token"]["amount"], 1234,
        "live extractor must round-trip the start field; got: {test_row}"
    );
    assert!(
        start_tokens[0]["token"].get("_instance_id").is_none(),
        "system fields must be stripped; got: {}",
        start_tokens[0]["token"]
    );

    let answers = test_row["human_answers"]
        .as_object()
        .expect("human_answers object");
    let review = answers
        .get("review")
        .unwrap_or_else(|| panic!("missing 'review' in human_answers: {test_row}"));
    assert_eq!(review["approved"], true, "got: {review}");
    assert_eq!(review["comment"], "lgtm");
    assert!(
        review.get("task_id").is_none(),
        "engine envelope must be unwrapped to `.data`; got: {review}"
    );

    assert!(
        test_row["reference_scope"].is_object(),
        "reference_scope must be populated from the source instance; got: {}",
        test_row["reference_scope"]
    );

    // 5. PATCH a passing assertion. `result.this.does.not.exist` is guaranteed
    //    not to navigate, so NotExists holds — that proves the runner builds a
    //    scope and walks it without depending on what the demo End emits.
    let patch_body = json!({
        "assertions": [
            { "path": "result.this.does.not.exist", "op": "not_exists", "value": null }
        ]
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/templates/{template_id}/tests/{test_id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "patch passing assertion");

    // 6. Run the test — full runner cycle: launch test_run instance, auto-
    //    complete the human task from the stored `human_answers`, wait for
    //    completion, build scope, evaluate assertion. Expect `passed`.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/templates/{template_id}/tests/{test_id}/run"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let run = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "run-one: {run}");
    assert_eq!(
        run["status"], "passed",
        "runner did not pass with a tautological assertion — check failure_detail: {run}"
    );
    assert!(
        run["final_scope"].is_object(),
        "passing run must capture final_scope: {run}"
    );

    // 7. PATCH a failing assertion. Same path but Exists — guaranteed to miss
    //    so the runner surfaces a structured failure_detail.
    let patch_body = json!({
        "assertions": [
            { "path": "result.this.does.not.exist", "op": "exists", "value": null }
        ]
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/templates/{template_id}/tests/{test_id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "patch failing assertion");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/templates/{template_id}/tests/{test_id}/run"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let run = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "run-one (failure path): {run}");
    assert_eq!(
        run["status"], "failed",
        "expected failed status with an Exists assertion on a missing path; got: {run}"
    );
    let failure = run["failure_detail"]
        .as_object()
        .expect("failed run must carry failure_detail object");
    assert_eq!(failure["assertion_idx"], 0);
    assert_eq!(failure["op"], "exists");
    assert_eq!(failure["path"], "result.this.does.not.exist");

    // 7b. PATCH a Rhai-templated value that resolves against the live scope.
    //     Two assertions exercise the lifted picker scope:
    //       - `result.value.marker` self-reference (End resultMapping path)
    //       - `start.<id>.amount` cross-reference (Start-token path, NEW —
    //         proves `build_scope` actually exposes the start tokens)
    let patch_body = json!({
        "assertions": [
            {
                "path": "result.value.marker",
                "op": "eq",
                "value": "{{ result.value.marker }}"
            },
            {
                "path": "start.amount",
                "op": "eq",
                "value": "{{ start.amount }}"
            }
        ]
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/templates/{template_id}/tests/{test_id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "patch rhai-template assertion"
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/templates/{template_id}/tests/{test_id}/run"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let run = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "rhai-template run: {run}");
    assert_eq!(
        run["status"], "passed",
        "rhai self-reference must hold against live final_scope: {run}"
    );

    // 7c. PATCH a Mustache-style interpolated assertion:
    //     `result.value.greeting == "Hello, {{ start.name }}!"`. The End's
    //     resultMapping builds `greeting = "Hello, " + name + "!"` from the
    //     same `name` field the template references, so the comparison is
    //     a tautological "did the start token thread through to result?"
    //     check — passes iff the interpolation resolver works.
    let patch_body = json!({
        "assertions": [
            {
                "path": "result.value.greeting",
                "op": "eq",
                "value": "Hello, {{ start.name }}!"
            }
        ]
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/templates/{template_id}/tests/{test_id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "patch interpolation assertion"
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/templates/{template_id}/tests/{test_id}/run"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let run = body_json(resp.into_body()).await;
    assert_eq!(
        run["status"], "passed",
        "interpolation must resolve `Hello, {{{{ start.name }}}}!` to `Hello, Alice!` and match: {run}"
    );

    // 7d. PATCH a Rhai template that resolves but compares unequal — the
    let patch_body = json!({
        "assertions": [
            {
                "path": "result.value.marker",
                "op": "eq",
                "value": "{{ \"not_e2e\" }}"
            }
        ]
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/templates/{template_id}/tests/{test_id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "patch unequal rhai assertion"
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/templates/{template_id}/tests/{test_id}/run"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let run = body_json(resp.into_body()).await;
    assert_eq!(run["status"], "failed", "unequal rhai → failed: {run}");
    let failure = run["failure_detail"]
        .as_object()
        .expect("failure_detail object");
    assert_eq!(
        failure["expected"], "{{ \"not_e2e\" }}",
        "raw template preserved in `expected`: {run}"
    );
    assert_eq!(
        failure["expected_resolved"], "not_e2e",
        "resolved value must surface in `expected_resolved`: {run}"
    );
    assert_eq!(failure["actual"], "e2e", "actual is the live marker: {run}");

    // 8. The run-history endpoint must show every run newest-first.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/templates/{template_id}/tests/{test_id}/runs"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let runs = body_json(resp.into_body()).await;
    let arr = runs.as_array().expect("runs array");
    assert!(
        arr.len() >= 5,
        "expected at least 5 runs in history, got {}: {runs}",
        arr.len()
    );
    // Newest first: unequal-rhai (failed), interpolation (passed),
    // passing-rhai (passed), exists-on-missing (failed),
    // not-exists-on-missing (passed).
    assert_eq!(arr[0]["status"], "failed", "newest first: {runs}");
    assert_eq!(arr[1]["status"], "passed");
    assert_eq!(arr[2]["status"], "passed");
    assert_eq!(arr[3]["status"], "failed");
    assert_eq!(arr[4]["status"], "passed");

    cleanup_durables(&cleanup_nats).await;
}
