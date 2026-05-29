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
//!   - `just dev::up` (engine :13030, executor with `llm`+`python` features,
//!     NATS :14333, postgres :15439)
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
use mekhan_service::models::template::WorkflowNodeData;
use mekhan_service::nats::MekhanNats;
use mekhan_service::projections::step_executions::start_step_executions_ingest;

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:13030".to_string())
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
        ("PETRI_GLOBAL", "mekhan-step-executions"),
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

async fn spawn_consumers(
    nats: MekhanNats,
    db: sqlx::PgPool,
) -> (TaskHandle, TaskHandle, TaskHandle) {
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

    // Step-executions projector: materializes one `step_execution` row per
    // (instance, node, iteration) from the engine event log. The
    // Python-tool-contract test reads the `lookup_order` row to prove the
    // tool actually ran (vs. failing with an AttributeError when the LLM
    // emits the wrong arg key). `main.rs` spawns this in prod; tests must
    // spawn it explicitly like the causality/lifecycle consumers above.
    let s_nats = nats.clone();
    let s_db = db.clone();
    let step_exec = tokio::spawn(async move {
        start_step_executions_ingest(s_nats, s_db).await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    (
        TaskHandle(causality.abort_handle()),
        TaskHandle(lifecycle.abort_handle()),
        TaskHandle(step_exec.abort_handle()),
    )
}

fn demo_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("service crate has a parent")
        .join("demos/09-agent-tool-loop")
}

fn hello_world_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("service crate has a parent")
        .join("demos/01-hello-world")
}

fn order_lookup_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("service crate has a parent")
        .join("demos/08a-order-lookup")
}

fn feedback_child_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("service crate has a parent")
        .join("demos/09b-collect-feedback")
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

/// Publish the shipped 09 demo + its tool child and fire an instance with
/// `customer_message`. Returns the new (parent) instance id.
///
/// 09's `lookup_order` tool is a SubWorkflow referencing the 08a-order-lookup
/// child (Python). To stay self-contained (independent of the demo seeder),
/// we publish the 08a child here, rewrite 09's SubWorkflow node to point at
/// the freshly-published child id, then publish + fire 09. Shared by both
/// 09-based live tests so the heavyweight dance stays in one place; each test
/// still drives its own instance (and LLM round-trips) under its prefix.
async fn publish_and_fire(app: &axum::Router, customer_message: &str) -> Uuid {
    // Tool child first — the SubWorkflow resolver needs it published.
    let child = demos::load_demo(&order_lookup_dir()).expect("load demos/08a-order-lookup");
    let child_id = create_and_publish(
        app,
        &format!("Order Lookup Child E2E {}", Uuid::new_v4().simple()),
        &child.graph,
        &child.files,
    )
    .await;

    let mut demo = demos::load_demo(&demo_dir()).expect("load demos/09-agent-tool-loop");
    assert_eq!(demo.metadata.name, "09 · Agent + Tool Loop");
    // Point the SubWorkflow tool node at the child we just published (the
    // on-disk demo references the fixed seeded id; the test owns its child).
    for node in &mut demo.graph.nodes {
        if let WorkflowNodeData::SubWorkflow { template_id, .. } = &mut node.data {
            *template_id = child_id;
        }
    }

    let parent_id = create_and_publish(
        app,
        &format!("Agent Loop E2E {}", Uuid::new_v4().simple()),
        &demo.graph,
        &demo.files,
    )
    .await;

    fire_customer_message(app, parent_id, customer_message).await
}

/// Poll `step_execution` until `node_id` has at least one row, returning all
/// its rows (one per loop iteration the LLM dispatched it). The
/// step-executions projector is an async NATS consumer, so the row can lag
/// the instance reaching `completed` by a few hundred ms — poll rather than
/// read once. Panics on timeout so a missing projection (consumer not
/// spawned, attribution regression) is caught loudly.
async fn wait_for_step_execution(
    db: &sqlx::PgPool,
    instance_id: Uuid,
    node_id: &str,
    timeout: Duration,
) -> Vec<(String, Option<Value>, Option<Value>)> {
    let start = std::time::Instant::now();
    loop {
        let rows: Vec<(String, Option<Value>, Option<Value>)> = sqlx::query_as(
            "SELECT status, outputs, error FROM step_execution \
             WHERE instance_id = $1 AND node_id = $2 \
             ORDER BY iteration_index",
        )
        .bind(instance_id)
        .bind(node_id)
        .fetch_all(db)
        .await
        .unwrap();
        if !rows.is_empty() {
            return rows;
        }
        if start.elapsed() > timeout {
            panic!(
                "no step_execution row for node '{node_id}' on instance \
                 {instance_id} within {timeout:?} — the step-executions \
                 projector saw no events attributed to that node (was the \
                 tool ever dispatched? is the projector spawned?)"
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
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
    let (_causality, _lifecycle, _step_exec) = spawn_consumers(nats, db.clone()).await;

    // Create→publish→fire the shipped demo (drift between the disk
    // fixture and the platform contract surfaces here as a publish
    // failure or a `failed` instance, never on the user's first click).
    // The customer message names a known order id; the Triage Agent's
    // system prompt instructs the LLM to call lookup_order when it sees
    // one — the signal that drives the loop into its tool-dispatch path.
    let instance_id = publish_and_fire(&app, "Hi, where is my order ORD-42?").await;

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
    // text; `turns_used` is the agent's loop turn counter. The tool's
    // Python body lives in the child template
    // (demos/08a-order-lookup/nodes/lookup_order/main.py) and returns the
    // literal strings the LLM has access to via the sub-workflow result.
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

    // Tool was dispatched at least once. The agent's `turn` counter (mapped
    // to `turns_used`) is bumped ONLY by `t_route_dispatch_<tn>` — NOT by
    // `t_route_final` — so `turns_used == N` means exactly N tool calls
    // before the final reply. One `lookup_order` call is the happy path →
    // `turns_used >= 1`. (Note: the old `>= 2` threshold was inadvertently
    // calibrated to the *buggy* collect: an empty tool result made the model
    // re-ask / re-dispatch, inflating the count. With the parked-data fix the
    // agent gets the result on the first call and finishes in one turn.)
    let turns_used = payload
        .get("turns_used")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        turns_used >= 1,
        "agent loop completed in {turns_used} turn(s) — the tool was never \
         called. Either the Ollama adapter dropped `message.tool_calls`, \
         the LLM ignored the tool (model too small / wrong system prompt), \
         or the agent compiler skipped t_route_dispatch_lookup_order. \
         Full result: {result}"
    );

    // Deterministic proof the SubWorkflow tool actually ran (vs. the model
    // hallucinating a status): a `lookup_order` step-execution row only
    // exists if `t_agent_invoke_lookup_order` deposited the args at the
    // child and the child net ran.
    let lookup_rows =
        wait_for_step_execution(&db, instance_id, "lookup_order", Duration::from_secs(20)).await;
    assert!(
        !lookup_rows.is_empty(),
        "lookup_order tool never produced a step row — dispatch/collect \
         plumbing did not fire"
    );

    // Reply must be non-empty text. A blank reply means the agent exited
    // through an unintended path (e.g. an error envelope on the success port).
    let reply = payload
        .get("reply")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        !reply.trim().is_empty(),
        "agent reply was empty — final response did not propagate through \
         the End node's `agent.response` resultMapping. Result: {result}"
    );

    // Integration signal for the COLLECT path (parked-data fix): 08a's Python
    // returns ("In transit", "tomorrow") for ORD-42 and PARKS that write-once
    // in `p_<child>_data` — NOT on the slim control token. The deterministic
    // regression guard for the wiring lives in `agent_loop_e2e.rs`
    // (`agent_tool_collect_reads_child_parked_data_not_control_token`); here
    // we corroborate at runtime that the parked result round-tripped into the
    // model's next turn by surfacing in the final reply.
    let reply_lc = reply.to_lowercase();
    assert!(
        reply_lc.contains("transit") || reply_lc.contains("tomorrow"),
        "agent reply does not reflect the looked-up order status (expected \
         'transit'/'tomorrow' for ORD-42) — the tool result may not have \
         reached the model. Reply: {reply:?}; full result: {result}"
    );

    cleanup_durables(&cleanup_nats).await;
}

/// Publish BOTH tool children — 08a (lookup_order, Python) and 09b
/// (collect_feedback, a HumanTask form) — rewire demo 09's two SubWorkflow
/// tool nodes to point at them BY NODE ID, then publish 09 and fire
/// `message`. Returns the parent instance id.
///
/// `publish_and_fire` (above) rewires *every* SubWorkflow node to the single
/// 08a child — fine for the ORD-42 test that only calls lookup_order, but it
/// would make collect_feedback run Python instead of rendering a form. This
/// keeps the two tools distinct.
async fn publish_both_and_fire(app: &axum::Router, message: &str) -> Uuid {
    let lookup = demos::load_demo(&order_lookup_dir()).expect("load demos/08a-order-lookup");
    let lookup_id = create_and_publish(
        app,
        &format!("Order Lookup Child E2E {}", Uuid::new_v4().simple()),
        &lookup.graph,
        &lookup.files,
    )
    .await;

    let feedback =
        demos::load_demo(&feedback_child_dir()).expect("load demos/09b-collect-feedback");
    let feedback_id = create_and_publish(
        app,
        &format!("Collect Feedback Child E2E {}", Uuid::new_v4().simple()),
        &feedback.graph,
        &feedback.files,
    )
    .await;

    let mut demo = demos::load_demo(&demo_dir()).expect("load demos/09-agent-tool-loop");
    assert_eq!(demo.metadata.name, "09 · Agent + Tool Loop");
    for node in &mut demo.graph.nodes {
        let nid = node.id.clone();
        if let WorkflowNodeData::SubWorkflow { template_id, .. } = &mut node.data {
            match nid.as_str() {
                "lookup_order" => *template_id = lookup_id,
                "collect_feedback" => *template_id = feedback_id,
                other => panic!(
                    "demo 09 has an unexpected SubWorkflow node '{other}' — \
                     update publish_both_and_fire to map it to a child"
                ),
            }
        }
    }

    let parent_id = create_and_publish(
        app,
        &format!("Agent Loop Form E2E {}", Uuid::new_v4().simple()),
        &demo.graph,
        &demo.files,
    )
    .await;

    fire_customer_message(app, parent_id, message).await
}

/// Poll `hpi_tasks` for the pending human task the `collect_feedback` child
/// (09b) spawned — its `taskTitle` is "Agent-requested input". Scoped to
/// tasks created at/after the parent fired so a concurrent run on the shared
/// dev stack can't hand us the wrong task. Returns (task_id, detail).
async fn wait_for_agent_form_task(
    db: &sqlx::PgPool,
    parent_id: Uuid,
    timeout: Duration,
) -> (String, Value) {
    let start = std::time::Instant::now();
    loop {
        let row: Option<(String, Value)> = sqlx::query_as(
            "SELECT id, detail FROM hpi_tasks \
             WHERE status = 'pending' AND title = 'Agent-requested input' \
               AND created_at >= (SELECT created_at FROM workflow_instances WHERE id = $1) \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(parent_id)
        .fetch_optional(db)
        .await
        .unwrap();
        if let Some((id, detail)) = row {
            return (id, detail);
        }
        if start.elapsed() > timeout {
            panic!(
                "collect_feedback child never surfaced a pending \
                 'Agent-requested input' task for parent {parent_id} within \
                 {timeout:?} — did the LLM call collect_feedback (check \
                 .dev/log/ollama.log + executor.log), and is the child's \
                 human task projected into hpi_tasks?"
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Recursively collect every form-field `name` from a task-detail blob. The
/// agent DESIGNS the form (field names are LLM-chosen), so we discover them
/// rather than hard-code one. A field object looks like
/// `{name, label, kind, required}`; step/mdsvex blocks have no `name`+`kind`.
fn collect_field_names(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if let Some(Value::String(name)) = map.get("name") {
                if (map.contains_key("kind") || map.contains_key("label"))
                    && !out.contains(name)
                {
                    out.push(name.clone());
                }
            }
            for val in map.values() {
                collect_field_names(val, out);
            }
        }
        Value::Array(arr) => {
            for val in arr {
                collect_field_names(val, out);
            }
        }
        _ => {}
    }
}

/// Submit `order_id` for every input field the agent's form declared (the
/// operator's answer). POSTs to `/api/v1/tasks/{id}/complete` exactly as the
/// UI would.
async fn complete_form_with_order_id(
    app: &axum::Router,
    task_id: &str,
    detail: &Value,
    order_id: &str,
) {
    let mut names = Vec::new();
    collect_field_names(detail, &mut names);
    let mut data = serde_json::Map::new();
    for n in &names {
        data.insert(n.clone(), json!(order_id));
    }
    if data.is_empty() {
        // Couldn't discover the LLM's field name from the detail blob — fall
        // back to the conventional key the system prompt suggests.
        data.insert("order_id".to_string(), json!(order_id));
    }
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
        "complete form task {task_id} (fields {names:?}): status {}",
        resp.status()
    );
}

/// Full human-in-the-loop chain — the regression guard for the parked-data
/// collect bug on a HUMAN-TASK-bearing SubWorkflow tool. A customer message
/// with NO order id must drive the agent to call `collect_feedback` (09b,
/// which renders an agent-designed form); once the operator submits ORD-42,
/// the agent must RECEIVE those answers as the tool result, extract the id,
/// call `lookup_order` (08a), and reply with the looked-up status. With the
/// pre-fix wiring the agent collected the form child's slim control token
/// (empty), never saw the order id, and could never reach lookup_order — so
/// `lookup_order` would have no step row and the reply could not mention the
/// looked-up status ("transit"/"tomorrow"), which exist only inside 08a's
/// parked Python output.
#[tokio::test]
async fn agent_human_form_tool_feeds_collected_answer_into_lookup() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev::up`",
            engine_url()
        );
    }
    if !ollama_available().await {
        panic!(
            "ollama not available at {} — start it with `just dev::up-ollama`",
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
    let (_causality, _lifecycle, _step_exec) = spawn_consumers(nats, db.clone()).await;

    // No order id → the agent must ASK via collect_feedback before it can
    // look anything up.
    let instance_id = publish_both_and_fire(&app, "Hi, where is my order?").await;

    // The agent designs + dispatches the form; once the child human task is
    // pending, submit ORD-42 as the operator would.
    let (task_id, detail) =
        wait_for_agent_form_task(&db, instance_id, Duration::from_secs(180)).await;
    eprintln!("--- agent-designed form task {task_id} ---");
    complete_form_with_order_id(&app, &task_id, &detail, "ORD-42").await;

    let terminal = wait_for_terminal_status(&db, instance_id, Duration::from_secs(240)).await;
    assert_eq!(
        terminal, "completed",
        "instance ended `{terminal}` — the human-form → lookup chain did not \
         round-trip; check ollama + executor logs"
    );

    // The collected answer must have driven a real lookup_order dispatch —
    // a step row for `lookup_order` only exists if the agent chained the
    // form's answer into the second tool. (With the bug it never could.)
    let lookup_rows =
        wait_for_step_execution(&db, instance_id, "lookup_order", Duration::from_secs(20)).await;
    assert!(
        !lookup_rows.is_empty(),
        "lookup_order never ran — the agent did not chain the collected order \
         id into the lookup tool (the parked-data collect bug)"
    );

    let result: Value =
        sqlx::query_scalar("SELECT result FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .expect("instance result must be present after `completed`");
    eprintln!(
        "\n--- human-form chain final result ---\n{}\n---\n",
        serde_json::to_string_pretty(&result).unwrap_or_default()
    );
    let payload = result.get("value").unwrap_or(&result);
    let reply = payload.get("reply").and_then(|v| v.as_str()).unwrap_or("");
    let reply_lc = reply.to_lowercase();
    assert!(
        reply_lc.contains("transit") || reply_lc.contains("tomorrow"),
        "reply does not reflect the looked-up status for ORD-42 — the answer \
         the operator submitted never round-tripped through collect_feedback's \
         tool result into lookup_order. Reply: {reply:?}; result: {result}"
    );

    cleanup_durables(&cleanup_nats).await;
}

/// Create a template from a graph + files and publish it; return its id.
/// Publishing compiles the AIR and — for a SubWorkflow parent — resolves
/// each referenced child's AIR + Start contract (`resolve_subworkflow_air`).
async fn create_and_publish<G: serde::Serialize, F: serde::Serialize>(
    app: &axum::Router,
    name: &str,
    graph: &G,
    files: &F,
) -> Uuid {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": name,
                        "graph": graph,
                        "files": files,
                        "author_id": Uuid::new_v4(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create template '{name}'");
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
    assert_eq!(status, StatusCode::OK, "publish '{name}': {pub_body}");
    template_id
}

async fn fire_customer_message(app: &axum::Router, template_id: Uuid, message: &str) -> Uuid {
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
                            "token": { "customer_message": message }
                        }],
                        "metadata": { "e2e": "agent_subworkflow_tool" }
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
    inst["id"].as_str().unwrap().parse().unwrap()
}

/// Parent agent graph whose `tools` handle targets a SubWorkflow node
/// referencing `child_template_id` (a published `Start{name} → … → End`
/// child). The agent has no per-node input declaration of its own — the
/// tool's `input_schema` is derived entirely from the child's Start
/// `initial` contract (`{ name }`), the whole point of this path.
fn parent_agent_with_subworkflow_tool(child_template_id: Uuid) -> Value {
    json!({
        "nodes": [
            {
                "id": "start", "type": "start", "position": { "x": 0, "y": 80 },
                "data": {
                    "type": "start", "label": "Start",
                    "initial": { "id": "in", "label": "Customer message", "fields": [
                        { "name": "customer_message", "label": "Customer message",
                          "kind": "textarea", "required": true }
                    ] }
                }
            },
            {
                "id": "agent", "type": "agent",
                "position": { "x": 320, "y": 80 }, "width": 440, "height": 360,
                "data": {
                    "type": "agent", "label": "Greeter Agent",
                    "description": "Calls the greet sub-workflow tool with a name.",
                    "model": {
                        "provider": "ollama", "model": "qwen3.5:9b",
                        "baseUrl": "http://localhost:11434", "temperature": 0
                    },
                    "systemPrompt": "You are a helpful assistant. To greet a person, \
                        call the `greet` tool with their name. After the tool returns \
                        a greeting, reply to the user with that greeting in one short \
                        sentence.",
                    "userPrompt": "{{ start.customer_message }}",
                    "maxTurns": 4,
                    "onToolError": "feedback"
                }
            },
            {
                "id": "greet_tool", "type": "sub_workflow",
                "position": { "x": 320, "y": 520 },
                "data": {
                    "type": "sub_workflow", "label": "greet",
                    "description": "Greets a person by name. Embeds the hello-world \
                        child; the child's Start declares `name`, which becomes this \
                        tool's input schema.",
                    "templateId": child_template_id,
                    "versionPin": { "mode": "latest" },
                    "inputMapping": [],
                    "output": { "id": "out", "label": "Greeting result", "fields": [
                        { "name": "greeting", "label": "Greeting", "kind": "text",
                          "required": true }
                    ] }
                }
            },
            {
                "id": "end", "type": "end", "position": { "x": 820, "y": 80 },
                "data": {
                    "type": "end", "label": "Done",
                    "resultMapping": [
                        { "targetField": "reply", "expression": "agent.response" },
                        { "targetField": "turns_used", "expression": "agent.turn" }
                    ]
                }
            }
        ],
        "edges": [
            { "id": "e1", "source": "start", "target": "agent",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e2", "source": "agent", "target": "end",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e3", "source": "agent", "target": "greet_tool",
              "sourceHandle": "tools", "targetHandle": "in", "type": "tools" }
        ]
    })
}

/// SubWorkflow-as-tool proof: an agent whose `tools` handle targets a
/// SubWorkflow node (a referenced `Start{name} → greet → End{greeting}`
/// child). The LLM-facing tool schema comes from the *child's Start*
/// `initial` contract — there is no per-node input declaration on the
/// SubWorkflow reference. This is the runtime counterpart to
/// `agent_loop_e2e::subworkflow_tool_input_schema_reflects_child_start`.
///
/// The chain under test:
///   child Start{name} (user-declared)
///     → resolve_subworkflow_air extracts it into ResolvedChild.input_contract
///       → agent tool schema `{ name }` → LLM tool_call greet({name: …})
///         → t_agent_invoke_greet deposits args at the SubWorkflow input
///           → spawn_net spawns the hello-world child net, which greets
///             → child reply → t_agent_collect_greet feeds it back into p_agent_state
///               → turn 2: the LLM produces a final reply
///
/// Assertion mirrors the loop test: `completed` + `turns_used >= 2`. Two
/// turns means the LLM emitted a tool call (turn 1), the child sub-workflow
/// spawned + ran + replied (otherwise collect never fires and the loop
/// stalls → caught by the timeout), and the LLM produced a final answer
/// (turn 2). A subworkflow that failed to spawn or never replied would
/// hang the instance, not complete it.
#[tokio::test]
async fn agent_subworkflow_tool_loop_completes() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev::up`",
            engine_url()
        );
    }
    if !ollama_available().await {
        panic!(
            "ollama not available at {} — start it with `just dev::up-ollama`",
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
    let (_causality, _lifecycle, _step_exec) = spawn_consumers(nats, db.clone()).await;

    // Publish the tool child first (Start{name} → greet → End{greeting}), so
    // the parent's publish-time SubWorkflow resolution can find it + read its
    // Start contract. Reuses the shipped 01-hello-world fixture as the child.
    let child = demos::load_demo(&hello_world_dir()).expect("load demos/01-hello-world");
    let child_id = create_and_publish(
        &app,
        &format!("Greet Child E2E {}", Uuid::new_v4().simple()),
        &child.graph,
        &child.files,
    )
    .await;

    // Publish the parent agent that calls the child as a tool, then fire it.
    let parent_graph = parent_agent_with_subworkflow_tool(child_id);
    let parent_id = create_and_publish(
        &app,
        &format!("Greet Agent E2E {}", Uuid::new_v4().simple()),
        &parent_graph,
        &json!({}),
    )
    .await;
    let instance_id = fire_customer_message(&app, parent_id, "Please greet Alice.").await;

    let terminal = wait_for_terminal_status(&db, instance_id, Duration::from_secs(300)).await;
    assert_eq!(
        terminal, "completed",
        "instance ended `{terminal}` — the agent did not round-trip through the \
         SubWorkflow tool. Check ollama (.dev/log/ollama.log), the executor \
         (.dev/log/executor.log: greet child spawned + ran?), and that the \
         child sub-workflow published cleanly (Start/End boundary present)"
    );

    let result: Value =
        sqlx::query_scalar("SELECT result FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .expect("instance result must be present after `completed`");
    eprintln!(
        "\n--- agent subworkflow-tool final result ---\n{}\n---\n",
        serde_json::to_string_pretty(&result).unwrap_or_default()
    );
    let payload = result.get("value").unwrap_or(&result);

    let turns_used = payload
        .get("turns_used")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        turns_used >= 2,
        "agent completed in {turns_used} turn(s) — the SubWorkflow tool was never \
         called. Either the Ollama adapter dropped the tool plumbing, the LLM \
         ignored the tool, or the agent compiler did not route a tools edge to a \
         SubWorkflow callee. Full result: {result}"
    );

    let reply = payload.get("reply").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        !reply.trim().is_empty(),
        "agent reply was empty — the final response did not propagate through the \
         End node's `agent.response` resultMapping. Result: {result}"
    );

    cleanup_durables(&cleanup_nats).await;
}

/// Tool-contract proof: the deterministic-on-the-tool-side companion to the
/// loop test above, for the SubWorkflow tool.
///
/// The agent's `tools` handle targets a *SubWorkflow* (`lookup_order` → the
/// 08a-order-lookup child). The compiler derives that tool's LLM-facing
/// `input_schema` from the *child's Start* `initial` contract (`{ order_id:
/// string }`) — `agent_loop_e2e::subworkflow_tool_input_schema_reflects_child_start`
/// pins that derivation offline. This test pins the *other half*, which only
/// a live run can prove: that the LLM-emitted args reach the child's Python
/// body as `input.order_id` and the result flows back as the tool result.
///
/// The chain under test:
///   schema (order_id, from child Start) → LLM tool_call args {order_id: …}
///     → t_agent_invoke_lookup_order deposits args at the SubWorkflow input
///       → spawn_net spawns the child net; its Start passes order_id through
///         → child Python `oid = input.order_id` (08a/nodes/lookup_order/main.py)
///           → child End maps {status, eta} → reply → SubWorkflow join → data_port
///
/// The child net has no `workflow_instances` row, so its Python step isn't
/// projected directly. We assert instead on the *SubWorkflow node's* row on
/// the parent (it has a `data_port`, so the projector records it). Signals:
///   - `status = 'completed'` on the `lookup_order` SubWorkflow row proves the
///     child spawned, ran, and replied — which requires the child Python to
///     have read `input.order_id` and returned. A mis-keyed arg
///     (`AttributeError`) fails the child net; since 08a routes no explicit
///     `fail_out`, that stalls the loop → the instance times out (caught by
///     `wait_for_terminal_status`) rather than completing.
///   - `outputs.status` present proves the child's declared `status` output
///     was mapped back through the SubWorkflow result, not just that it ran.
///
/// Deterministic on the tool side regardless of which order id the LLM passes
/// (ORD-42 → "In transit"; unknown id → "Unknown order id" — both successful
/// child runs that read `input.order_id`).
#[tokio::test]
async fn python_tool_reads_llm_args_as_input_field() {
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
    let (_causality, _lifecycle, _step_exec) = spawn_consumers(nats, db.clone()).await;

    let instance_id = publish_and_fire(&app, "Hi, where is my order ORD-42?").await;

    let terminal = wait_for_terminal_status(&db, instance_id, Duration::from_secs(300)).await;
    assert_eq!(
        terminal, "completed",
        "instance ended `{terminal}` — the agent loop did not round-trip; \
         the Python tool contract can't be evaluated. Check ollama + executor \
         logs (.dev/log/{{ollama,executor}}.log)"
    );

    // The projector is an async consumer; allow it to catch up past the
    // instance reaching `completed`.
    let rows = wait_for_step_execution(
        &db,
        instance_id,
        "lookup_order",
        Duration::from_secs(30),
    )
    .await;

    eprintln!("\n--- lookup_order step_execution rows ---\n{rows:#?}\n---\n");

    // No iteration of the tool may have failed — a child-net failure (e.g.
    // an AttributeError from a mis-keyed arg) that explicitly routed to the
    // SubWorkflow's error path surfaces as status 'failed' here.
    if let Some((_, _, error)) = rows.iter().find(|(st, _, _)| st == "failed") {
        panic!(
            "lookup_order SubWorkflow tool ran but FAILED — the child net \
             errored. The likely cause is the LLM-emitted args not matching \
             the child Python's `input.<field>` reads (the \
             `AttributeError: '_AccessibleDict' object has no attribute \
             'order_id'` mode), i.e. the derived tool input_schema drifted \
             from the child's Start contract. error: {error:?}"
        );
    }

    // At least one iteration must have completed, proving the LLM args reached
    // the child net, its Python read `input.order_id`, and the result flowed
    // back through the SubWorkflow's data_port.
    let completed = rows.iter().find(|(st, _, _)| st == "completed");
    let (_, outputs, _) = completed.unwrap_or_else(|| {
        panic!(
            "lookup_order SubWorkflow has step_execution rows but none \
             `completed` (statuses: {:?}) — the tool was dispatched but the \
             child never replied cleanly; the `input.<field>` contract is \
             unproven",
            rows.iter().map(|(s, _, _)| s.as_str()).collect::<Vec<_>>()
        )
    });

    // The declared `status` output must be present in the SubWorkflow result
    // — proves the child's Python read `input.order_id` and its declared
    // `status` output was mapped back, not merely that the child ran.
    let outputs = outputs
        .as_ref()
        .expect("completed lookup_order row must carry its sub-workflow result envelope");
    let status_out = outputs.get("status").and_then(Value::as_str);
    assert!(
        status_out.is_some_and(|s| !s.trim().is_empty()),
        "lookup_order completed but its `status` output is missing/empty — \
         the child ran but the declared `status` didn't map back through the \
         SubWorkflow result. outputs: {outputs}"
    );

    cleanup_durables(&cleanup_nats).await;
}
