//! End-to-end coverage of the shipped `vllm-smoke` demo against the live
//! dev stack + a local vllm-metal server.
//!
//! Loads `demos/vllm-smoke/` via [`mekhan_service::demos::load_demo`] — the
//! same disk fixture the runtime seeder publishes at service startup — and
//! drives it through:
//!
//! 1. POST `/api/v1/templates` with the loaded `(graph, files)`.
//! 2. POST `/api/v1/templates/{id}/publish` (compiles AIR; no S3 files since
//!    the demo has no node-attached scripts).
//! 3. POST `/api/v1/instances` with no `start_tokens` (Start's `initial.fields`
//!    is `[]` — the workflow needs no inputs at all).
//! 4. Wait for the AutomatedStep `ask` to dispatch via NATS → executor →
//!    LlmBackend → HTTP `/v1/chat/completions` against vllm-metal at
//!    `http://localhost:8000` → response → output port → End node.
//! 5. Assert the instance reaches `completed`. Reaching terminal `completed`
//!    is a sufficient end-to-end proof: any LLM-side failure (network error,
//!    auth, model-id mismatch, vllm-metal crash, timeout) surfaces as the
//!    AutomatedStep going `failed`, which the lifecycle listener propagates
//!    to the instance as `failed` — NOT `completed`.
//!
//! Companion to `showcase_demo_e2e.rs` (HumanTask + Python branch). This
//! test catches drift between the shipped vllm-smoke demo and the platform:
//! a new required field on `WorkflowNodeData`, an LLM compiler-validation
//! tightening, an executor `llm` feature regression, a vllm-metal protocol
//! shift — all surface as a publish-time compile error or a `failed`
//! instance here, never on the user's first try.
//!
//! Requires:
//!   - `just dev::up` (engine :3030, executor with `llm` feature, NATS :4333,
//!     postgres :5439)
//!   - `just dev::up-vllm` (vllm-metal at :8000 serving the model the demo
//!     references — default `mlx-community/Qwen3.5-9B-MLX-4bit`)
//!
//! Skipped (with a clear panic) if either is unreachable.
//!
//! Parallel-safe: per-test consumer prefix scopes lifecycle + causality
//! durables uniquely; the fresh template id avoids colliding with the
//! seeded singleton at `00000000-0000-0000-0000-000000000020`.

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

fn vllm_url() -> String {
    std::env::var("TEST_VLLM_URL").unwrap_or_else(|_| "http://localhost:8000".to_string())
}

async fn engine_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/nets/metadata", engine_url())).await,
        Ok(r) if r.status().is_success()
    )
}

async fn vllm_available() -> bool {
    matches!(
        reqwest::get(format!("{}/v1/models", vllm_url())).await,
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

fn demo_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("service crate has a parent")
        .join("demos/vllm-smoke")
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
async fn vllm_smoke_demo_completes_against_local_vllm() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev::up`",
            engine_url()
        );
    }
    if !vllm_available().await {
        panic!(
            "vllm-metal not available at {} — start it with `just dev::up-vllm` \
             (model must be loaded; check `.dev/log/vllm.log`)",
            vllm_url()
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

    // Load the literal demo. The test runs against an isolated DB+template
    // (we POST a fresh copy under a new id) so the seeded singleton at
    // `00000000-0000-0000-0000-000000000020` is unaffected. The shape of
    // the graph comes straight from disk — drift between this fixture
    // and any platform contract surfaces here.
    let demo = demos::load_demo(&demo_dir()).expect("load demos/vllm-smoke");
    assert_eq!(demo.metadata.name, "vLLM Smoke Test");

    // POST a fresh copy. Name is uniqued so successive runs can't collide
    // on any name constraint.
    let unique_name = format!("vLLM Smoke E2E {}", Uuid::new_v4().simple());
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

    // Publish: compiles AIR. The demo has no node files, so the staged
    // inputs for the LLM step are empty. Catches drift between the disk
    // fixture and the LLM-config compiler validation.
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

    // Create an instance. Start.initial.fields is `[]`, so no `start_tokens`
    // content is needed — the workflow has no runtime inputs.
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
                        "metadata": { "e2e": "vllm_smoke_demo" }
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

    // Timeout budget: a 9B 4-bit model on M-series Apple Silicon typically
    // produces ~30-60 tokens/sec; max_tokens=256 with the demo's prompt is
    // a few seconds at most, but the first-ever request on a cold KV cache
    // can take longer. 180s leaves headroom for a slow cold start without
    // masking real regressions.
    let terminal = wait_for_terminal_status(&db, instance_id, Duration::from_secs(180)).await;
    assert_eq!(
        terminal, "completed",
        "instance ended `{terminal}` — the vllm-smoke demo did not round-trip; \
         check vllm-metal (.dev/log/vllm.log: model loaded? port 8000 listening?), \
         the executor (.dev/log/executor.log: LLM backend dispatched? auth error?), \
         and that the demo's model id matches what vllm-metal is serving"
    );

    cleanup_durables(&cleanup_nats).await;
}
