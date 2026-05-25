//! Integration tests for the executor-llm backend against Ollama.
//!
//! Uses the shared Ollama testcontainer (`qwen2.5:3b`) — fully self-contained.
//!
//! For standard backend contract tests, see `tests/conformance.rs` which uses
//! the `llm_conformance_tests!` macro.
//!
//! Run with:
//!   cargo test -p aithericon-executor-llm --test ollama -- --nocapture

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionSpec, JobPriority, OutputDeclaration, RunContext,
    RunDirectory,
};
use aithericon_executor_llm::LlmBackend;
use aithericon_executor_test_harness::ollama::{ollama_model, shared_ollama_base_url};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn noop_callback() -> StatusCallback {
    Box::new(|_status, _detail| Box::pin(async {}))
}

fn make_llm_spec(config: serde_json::Value) -> ExecutionSpec {
    ExecutionSpec {
        backend: "llm".into(),
        inputs: vec![],
        outputs: vec![],
        config,
        config_ref: None,
    }
}

fn make_job(spec: ExecutionSpec) -> ExecutionJob {
    ExecutionJob {
        execution_id: "ollama-test".into(),
        spec,
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

fn make_run_context(spec: ExecutionSpec, timeout: Duration) -> RunContext {
    let execution_id = "ollama-test".to_string();
    RunContext {
        execution_id: execution_id.clone(),
        spec,
        run_dir: RunDirectory::new(&PathBuf::from("/tmp"), &execution_id),
        timeout,
        env: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    }
}

fn structured_blocks_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "title": { "type": "string" },
                        "blocks": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "type": {
                                        "type": "string",
                                        "enum": ["mdsvex", "table", "image", "callout", "divider", "input", "download"]
                                    },
                                    "content": { "type": "string" },
                                    "headers": { "type": "array", "items": { "type": "string" } },
                                    "rows": { "type": "array", "items": { "type": "array", "items": { "type": "string" } } },
                                    "alignments": { "type": "array", "items": { "type": "string", "enum": ["left", "center", "right"] } },
                                    "caption": { "type": "string" },
                                    "url": { "type": "string" },
                                    "alt": { "type": "string" },
                                    "severity": { "type": "string", "enum": ["info", "warning", "error", "success"] },
                                    "title": { "type": "string" },
                                    "field": {
                                        "type": "object",
                                        "properties": {
                                            "name": { "type": "string" },
                                            "label": { "type": "string" },
                                            "kind": { "type": "string", "enum": ["text", "textarea", "number", "select", "checkbox"] }
                                        },
                                        "required": ["name", "label", "kind"]
                                    }
                                },
                                "required": ["type"]
                            }
                        }
                    },
                    "required": ["id", "title", "blocks"]
                }
            }
        },
        "required": ["title", "steps"]
    })
}

fn collect_blocks(response: &Value) -> Vec<&serde_json::Map<String, Value>> {
    let steps = response
        .as_object()
        .and_then(|o| o.get("steps"))
        .and_then(|v| v.as_array())
        .expect("response should have a steps array");

    let mut blocks = Vec::new();
    for step in steps {
        if let Some(step_blocks) = step.get("blocks").and_then(|v| v.as_array()) {
            for block in step_blocks {
                if let Some(obj) = block.as_object() {
                    if obj.contains_key("type") {
                        blocks.push(obj);
                    }
                }
            }
        }
    }
    blocks
}

/// Probe the testcontainer Ollama by issuing a one-token chat. Returns true
/// when the model can serve inferences; false (with stderr note) when the
/// container starts but the loader fails (CPU-only Docker on Apple Silicon,
/// low-memory CI runners, etc).
async fn ollama_model_usable(base_url: &str, model: &str) -> bool {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base_url}/api/chat"))
        .json(&serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": false,
            "options": {"num_predict": 1},
        }))
        .timeout(Duration::from_secs(60))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => true,
        Ok(r) => {
            eprintln!(
                "SKIPPED: Ollama testcontainer can't run {model}: {}",
                r.text().await.unwrap_or_default()
            );
            false
        }
        Err(e) => {
            eprintln!("SKIPPED: Ollama probe failed: {e}");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Structured output tests
// ---------------------------------------------------------------------------

/// Validates structured block extraction via Ollama's native `/api/chat` + `format` path.
#[tokio::test]
async fn ollama_extract_structured_blocks() {
    let base_url = shared_ollama_base_url().await;
    let model = ollama_model();

    if !ollama_model_usable(base_url, model).await {
        return;
    }

    let system = "You are a task designer. You MUST use structured block types.\n\n\
        Block types:\n\
        - table: { type: \"table\", headers: [...], rows: [[...]], alignments?: [...], caption?: \"...\" }\n\
        - callout: { type: \"callout\", severity: \"info\"|\"warning\"|\"error\"|\"success\", content: \"...\", title?: \"...\" }\n\
        - divider: { type: \"divider\" }\n\
        - image: { type: \"image\", url: \"...\", alt?: \"...\", caption?: \"...\" }\n\
        - mdsvex: { type: \"mdsvex\", content: \"...\" }\n\
        - input: { type: \"input\", field: { name, label, kind } }\n\n\
        IMPORTANT: You MUST include at least one \"table\" block AND one \"callout\" block in your response. \
        Also include a \"divider\" block between sections.";

    let prompt = "Create a task to review quarterly server metrics. \
        Include a table with CPU, memory, and disk usage. \
        Add a warning callout about high memory usage. \
        Use a divider between the data and the review form. \
        Add one input field for reviewer notes.";

    let backend = LlmBackend::new();
    let spec = make_llm_spec(serde_json::json!({
        "provider": "ollama",
        "model": model,
        "prompt": prompt,
        "system_prompt": system,
        "temperature": 0.3,
        "base_url": base_url,
        "response_format": {
            "type": "json_schema",
            "schema": structured_blocks_schema()
        }
    }));
    let job = make_job(spec.clone());
    let mut ctx = make_run_context(spec, Duration::from_secs(300));

    ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}. stderr: {:?}",
        result.outcome,
        result.stderr_tail
    );

    let response = result.outputs.get("response").expect("missing 'response' output");
    assert!(response.is_object(), "expected JSON object, got: {response}");

    eprintln!(
        "\n--- Ollama LLM output ---\n{}\n---\n",
        serde_json::to_string_pretty(response).unwrap()
    );

    let obj = response.as_object().unwrap();
    assert!(obj.contains_key("title"), "response should have a title");

    let steps = obj.get("steps").and_then(|v| v.as_array()).unwrap();
    assert!(!steps.is_empty(), "steps array should not be empty");
    for (i, step) in steps.iter().enumerate() {
        let s = step
            .as_object()
            .unwrap_or_else(|| panic!("step[{i}] should be an object"));
        assert!(s.contains_key("id"), "step[{i}] should have an id");
        assert!(s.contains_key("title"), "step[{i}] should have a title");
        assert!(
            s.get("blocks").and_then(|v| v.as_array()).is_some(),
            "step[{i}] should have blocks"
        );
    }

    let all_blocks = collect_blocks(response);
    let block_types: Vec<&str> = all_blocks
        .iter()
        .filter_map(|b| b.get("type")?.as_str())
        .collect();
    eprintln!("Block types found: {:?}", block_types);

    // With constrained decoding the structure is guaranteed valid.
    // The 3b model may not always produce every requested block type,
    // so we just check that blocks exist with valid types.
    assert!(
        !all_blocks.is_empty(),
        "expected at least one block, got none"
    );
    for block_type in &block_types {
        assert!(
            ["mdsvex", "table", "image", "callout", "divider", "input", "download"]
                .contains(block_type),
            "unexpected block type: {block_type}"
        );
    }
}

/// Tests OpenAI adapter's structured output path by hitting Ollama's
/// OpenAI-compatible endpoint (`/v1/chat/completions` with `response_format`).
#[tokio::test]
async fn openai_extract_structured_blocks() {
    let base_url = shared_ollama_base_url().await;
    let model = ollama_model();

    if !ollama_model_usable(base_url, model).await {
        return;
    }

    let system = "You are a task designer. You MUST use structured block types.\n\n\
        Block types:\n\
        - table: { type: \"table\", headers: [...], rows: [[...]], alignments?: [...], caption?: \"...\" }\n\
        - callout: { type: \"callout\", severity: \"info\"|\"warning\"|\"error\"|\"success\", content: \"...\", title?: \"...\" }\n\
        - divider: { type: \"divider\" }\n\
        - mdsvex: { type: \"mdsvex\", content: \"...\" }\n\
        - input: { type: \"input\", field: { name, label, kind } }\n\n\
        IMPORTANT: You MUST include at least one \"table\" block AND one \"callout\" block.";

    let prompt = "Create a task to compare cloud provider pricing. \
        Include a table with AWS, GCP, and Azure prices for compute instances. \
        Add an info callout about pricing variability by region. \
        Use a divider before the feedback section. \
        Add one input field for reviewer comments.";

    let backend = LlmBackend::new();
    let spec = make_llm_spec(serde_json::json!({
        "provider": "open_ai",
        "model": model,
        "prompt": prompt,
        "system_prompt": system,
        "temperature": 0.3,
        "api_key": "not-needed",
        "base_url": base_url,
        "response_format": {
            "type": "json_schema",
            "schema": structured_blocks_schema()
        }
    }));
    let job = make_job(spec.clone());
    let mut ctx = make_run_context(spec, Duration::from_secs(300));

    ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}. stderr: {:?}",
        result.outcome,
        result.stderr_tail
    );

    let response = result.outputs.get("response").expect("missing 'response' output");
    assert!(response.is_object(), "expected JSON object, got: {response}");

    eprintln!(
        "\n--- OpenAI-compat LLM output ---\n{}\n---\n",
        serde_json::to_string_pretty(response).unwrap()
    );

    let all_blocks = collect_blocks(response);
    let block_types: Vec<&str> = all_blocks
        .iter()
        .filter_map(|b| b.get("type")?.as_str())
        .collect();
    eprintln!("Block types found (OpenAI path): {:?}", block_types);

    assert!(
        !all_blocks.is_empty(),
        "expected at least one block, got none"
    );
    for block_type in &block_types {
        assert!(
            ["mdsvex", "table", "image", "callout", "divider", "input", "download"]
                .contains(block_type),
            "unexpected block type: {block_type}"
        );
    }
}

/// When the LLM returns a JSON object and the spec declares multiple output
/// ports whose names match top-level schema keys, each port should receive its
/// own key's value — not the whole response envelope. Mirrors the Python
/// backend's name-based output sweep.
#[tokio::test]
async fn ollama_per_key_structured_unpack() {
    let base_url = shared_ollama_base_url().await;
    let model = ollama_model();

    if !ollama_model_usable(base_url, model).await {
        return;
    }

    let backend = LlmBackend::new();
    let mut spec = make_llm_spec(serde_json::json!({
        "provider": "ollama",
        "model": model,
        "prompt": "Return x=1 and y=2 as JSON.",
        "system_prompt": "You output structured JSON. Always set x to 1 and y to 2.",
        "temperature": 0.0,
        "base_url": base_url,
        "response_format": {
            "type": "json_schema",
            "schema": {
                "type": "object",
                "properties": {
                    "x": { "type": "integer" },
                    "y": { "type": "integer" }
                },
                "required": ["x", "y"]
            }
        }
    }));
    spec.outputs = vec![
        OutputDeclaration {
            name: "x".into(),
            path: None,
            required: true,
            kind: Some("number".into()),
            upload_to: None,
        },
        OutputDeclaration {
            name: "y".into(),
            path: None,
            required: true,
            kind: Some("number".into()),
            upload_to: None,
        },
    ];
    let job = make_job(spec.clone());
    let mut ctx = make_run_context(spec, Duration::from_secs(300));

    ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}. stderr: {:?}",
        result.outcome,
        result.stderr_tail
    );

    let x = result.outputs.get("x").expect("missing 'x' output");
    let y = result.outputs.get("y").expect("missing 'y' output");

    // The key assertion: each port carries its own key's scalar value,
    // not the whole {"x":1,"y":2} envelope.
    assert!(
        x.is_number(),
        "outputs[\"x\"] should be a number, got: {x}"
    );
    assert!(
        y.is_number(),
        "outputs[\"y\"] should be a number, got: {y}"
    );
    assert_eq!(x.as_i64(), Some(1), "outputs[\"x\"] should be 1, got: {x}");
    assert_eq!(y.as_i64(), Some(2), "outputs[\"y\"] should be 2, got: {y}");

    // The unmapped built-in 'response' still carries the full envelope.
    let response = result.outputs.get("response").expect("missing 'response' output");
    assert!(response.is_object(), "response should still be the full object: {response}");
}

// ---------------------------------------------------------------------------
// Chat / metrics tests (qwen2.5:3b — small model, fast)
// ---------------------------------------------------------------------------

/// Validates chat mode with conversation history.
/// Verifies that outputs["usage"] is always populated.
#[tokio::test]
async fn ollama_chat_with_history() {
    let base_url = shared_ollama_base_url().await;
    let model = ollama_model();

    if !ollama_model_usable(base_url, model).await {
        return;
    }

    let backend = LlmBackend::new();
    let spec = make_llm_spec(serde_json::json!({
        "provider": "ollama",
        "model": model,
        "prompt": "What was the city I mentioned?",
        "system_prompt": "You are a helpful assistant. Answer concisely.",
        "base_url": base_url,
        "history": [
            { "role": "user", "content": "I live in Paris." },
            { "role": "assistant", "content": "That's a beautiful city!" }
        ]
    }));
    let job = make_job(spec.clone());
    let mut ctx = make_run_context(spec, Duration::from_secs(300));

    ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}. stderr: {:?}",
        result.outcome,
        result.stderr_tail
    );

    // Verify the model understood the history context
    let response_text = result.stdout_tail.as_deref().unwrap_or("");
    eprintln!("\n--- Chat with history response ---\n{response_text}\n---\n");
    assert!(!response_text.is_empty(), "response should not be empty");

    // Verify usage is populated (key advantage over rig chat mode)
    let usage = result.outputs.get("usage").expect("missing 'usage' output");
    assert!(
        usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) > 0,
        "input_tokens should be > 0, got: {usage}"
    );
    assert!(
        usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) > 0,
        "output_tokens should be > 0, got: {usage}"
    );
}

/// Validates that metrics are always populated — even in chat mode.
#[tokio::test]
async fn ollama_metrics_always_populated() {
    let base_url = shared_ollama_base_url().await;
    let model = ollama_model();

    if !ollama_model_usable(base_url, model).await {
        return;
    }

    let backend = LlmBackend::new();
    let spec = make_llm_spec(serde_json::json!({
        "provider": "ollama",
        "model": model,
        "prompt": "Say hello.",
        "base_url": base_url,
    }));
    let job = make_job(spec.clone());
    let mut ctx = make_run_context(spec, Duration::from_secs(300));

    ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );

    // Metrics must be Some even for chat mode
    let metrics = result
        .metrics
        .expect("metrics should always be populated for chat mode");
    assert_eq!(metrics.total_points, 3);
    assert!(
        metrics
            .metric_names
            .contains(&"llm/input_tokens".to_string()),
        "should have llm/input_tokens metric"
    );
    assert!(
        metrics
            .metric_names
            .contains(&"llm/output_tokens".to_string()),
        "should have llm/output_tokens metric"
    );
    assert!(
        metrics
            .metric_names
            .contains(&"llm/total_tokens".to_string()),
        "should have llm/total_tokens metric"
    );

    // Values should be non-zero
    let input_tokens = metrics
        .latest_values
        .get("llm/input_tokens")
        .copied()
        .unwrap_or(0.0);
    let output_tokens = metrics
        .latest_values
        .get("llm/output_tokens")
        .copied()
        .unwrap_or(0.0);
    assert!(
        input_tokens > 0.0,
        "input_tokens should be > 0, got {input_tokens}"
    );
    assert!(
        output_tokens > 0.0,
        "output_tokens should be > 0, got {output_tokens}"
    );

    eprintln!(
        "Metrics: input={input_tokens}, output={output_tokens}, total={}",
        metrics
            .latest_values
            .get("llm/total_tokens")
            .copied()
            .unwrap_or(0.0)
    );
}
