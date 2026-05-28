//! Live end-to-end coverage of the agent loop against the dev stack +
//! a local Ollama daemon.
//!
//! Companion to `agent_loop_e2e.rs` (compile-only — pins AIR shape,
//! Rhai parsability, route guards, terminal scoping). This file is the
//! *runtime* counterpart: load the shipped `demos/09-agent-tool-loop/`
//! fixture, publish + fire it, and prove the loop fires end-to-end —
//! LLM → tool dispatch → Python tool → tool result fed back → final
//! reply. Until this test existed, the agent's full multi-turn path
//! was only structurally verified; an undetected runtime regression
//! (e.g. the Ollama adapter stripping `tool_calls`, the agent compiler
//! mis-wiring `t_invoke_<tn>`, the executor's `agent_node_id` metadata
//! key not propagating) would only surface on the user's first try.
//!
//! What this catches that the compile-only file can't:
//!   - Ollama adapter regressions on the native `/api/chat` tool path
//!     (request body must carry `tools: […]`; response `message.tool_calls`
//!     must round-trip through `LlmTurnResult` with `stop_reason: ToolUse`).
//!   - Agent route transition mis-fires (a stale Rhai guard could pass
//!     `cargo test --test agent_loop_e2e` but skip dispatch at runtime
//!     because of a token-shape mismatch the engine surfaces only when
//!     actually firing).
//!   - Tool-child wiring drift: `t_agent_invoke_lookup_order` must deposit
//!     the LLM's `arguments` map at the child's input place, and the
//!     child's output must flow back through `t_agent_collect_lookup_order`
//!     into `p_agent_state`.
//!   - Loop termination: `t_agent_route_final` must eventually fire when
//!     the LLM stops emitting tool calls — a livelock would surface as a
//!     hung instance, caught by the test timeout.
//!
//! Requires:
//!   - `just dev::up` (engine :3030, executor with `llm`+`python` features,
//!     NATS :4333, postgres :5439)
//!   - `just dev::up-ollama` (Ollama at :11434; the demo references
//!     `qwen3.5:9b`, which is `up-ollama`'s default. Any tool-capable
//!     model — qwen2.5+/qwen3+/llama3.1+ — works; override by editing
//!     the demo's `data.model.model` field).
//!
//! Skipped (with a clear panic) if either is unreachable.

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

fn ollama_url() -> String {
    std::env::var("TEST_OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".to_string())
}

async fn engine_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/nets/metadata", engine_url())).await,
        Ok(r) if r.status().is_success()
    )
}

async fn ollama_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/tags", ollama_url())).await,
        Ok(r) if r.status().is_success()
    )
}

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Best-effort: clean up the per-test consumer durables we created so
/// the shared `PETRI_GLOBAL` stream doesn't accumulate them across
/// test runs.
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
        .join("demos/09-agent-tool-loop")
}

/// Poll the instance's persisted status until terminal. Panics on
/// timeout so a hung loop is caught loudly.
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
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Loop-path proof: a customer message that mentions a known order id
/// (ORD-42) should drive the agent to call `lookup_order`, ingest its
/// mock status ("In transit", "tomorrow"), then return a one-sentence
/// reply. The instance must reach `completed` AND the End-mapped
/// `turns_used` must be ≥ 2 — a single-turn run would mean the LLM
/// never called the tool, which would mean either the Ollama adapter
/// dropped the tool plumbing or the agent compiler emitted a topology
/// that never reaches `t_route_dispatch_<tn>`.
#[tokio::test]
async fn agent_tool_loop_demo_completes_with_tool_call() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev::up`",
            engine_url()
        );
    }
    if !ollama_available().await {
        panic!(
            "ollama not available at {} — start it with `just dev::up-ollama` \
             (tool-capable model must be pulled; check `.dev/log/ollama.log`)",
            ollama_url()
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

    // Load the demo from disk — same fixture the runtime seeder
    // publishes at service startup. Drift between the shipped demo
    // and the platform contract surfaces here as a publish failure or
    // a `failed` instance, never on the user's first click.
    let demo = demos::load_demo(&demo_dir()).expect("load demos/09-agent-tool-loop");
    assert_eq!(demo.metadata.name, "09 · Agent + Tool Loop");

    // POST a uniquely-named copy so the seeded singleton template
    // (00000000-…-019) isn't disturbed and successive runs don't
    // collide on the name index.
    let unique_name = format!("Agent Loop E2E {}", Uuid::new_v4().simple());
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

    // Publish: compiles the agent loop AIR (parked state, dispatch/
    // collect per tool, route guards). Stages the Python `lookup_order`
    // source to S3 in the process.
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

    // Fire with a customer message naming a known order id. The Triage
    // Agent's system prompt instructs the LLM to call lookup_order when
    // it sees an order id — this is the signal that drives the loop into
    // its tool-dispatch path.
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
                                "customer_message": "Hi, where is my order ORD-42?"
                            }
                        }],
                        "metadata": { "e2e": "agent_tool_loop" }
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

    // Timeout budget: a 9B 4-bit model on M-series Apple Silicon
    // produces ~30-60 tokens/sec. Two LLM round-trips + one Python
    // tool execution + envelope hops typically completes in <30s, but
    // the first-ever inference on a cold KV cache can take longer.
    // 300s leaves headroom for a slow cold start + a confused-LLM
    // retry-path through `t_route_unknown` (which feedbacks and tries
    // again) without masking real regressions.
    let terminal = wait_for_terminal_status(&db, instance_id, Duration::from_secs(300)).await;
    assert_eq!(
        terminal, "completed",
        "instance ended `{terminal}` — the agent loop did not round-trip; \
         check ollama (.dev/log/ollama.log: model pulled? port 11434 listening?), \
         the executor (.dev/log/executor.log: LLM + Python backends dispatched? \
         did t_invoke_lookup_order fire?), and that the model the demo references \
         actually supports tool calling (qwen2.5+/qwen3+/llama3.1+)"
    );

    // Read the End-mapped result. `reply` is the agent's final response
    // text; `turns_used` is the agent's loop turn counter. The Python
    // tool body lives in demos/09-agent-tool-loop/nodes/lookup_order/
    // main.py and returns the literal strings the LLM has access to.
    let result: Value =
        sqlx::query_scalar("SELECT result FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .expect("instance result must be present after `completed`");
    eprintln!("\n--- agent loop final result ---\n{}\n---\n",
        serde_json::to_string_pretty(&result).unwrap_or_default()
    );

    // End wraps the result_mapping output in a success envelope:
    // `{ok: true, value: {reply, turns_used}}`. Pre-envelope the
    // mappings sat at the top level; navigate through `value` now.
    // Fall back to top-level lookup so the test stays correct if a
    // future change inverts the envelope direction.
    let payload = result.get("value").unwrap_or(&result);

    // Strongest pin: at least 2 turns means the LLM emitted a tool
    // call on turn 1, the tool ran, the result was fed back, and the
    // LLM emitted a final response on turn 2+. Anything less and we
    // never exercised the dispatch/collect plumbing — regression.
    let turns_used = payload
        .get("turns_used")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        turns_used >= 2,
        "agent loop completed in {turns_used} turn(s) — the tool was never \
         called. Either the Ollama adapter dropped `message.tool_calls`, \
         the LLM ignored the tool (model too small / wrong system prompt), \
         or the agent compiler skipped t_route_dispatch_lookup_order. \
         Full result: {result}"
    );

    // Softer pin: the agent's reply should be non-empty text. The exact
    // wording is LLM-dependent so we don't grep for "transit" — but a
    // blank reply means the agent exited through an unintended path
    // (e.g. an error envelope leaking onto the success port).
    let reply = payload
        .get("reply")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        !reply.trim().is_empty(),
        "agent reply was empty — final response did not propagate through \
         the End node's `agent.response` resultMapping. Result: {result}"
    );

    cleanup_durables(&cleanup_nats).await;
}
