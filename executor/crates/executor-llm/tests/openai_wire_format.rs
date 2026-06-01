//! Wire-format conformance for the `openai` adapter.
//!
//! These tests assert the EXACT shape of `/v1/chat/completions` request
//! bodies the adapter sends, plus the model-capability fallback that
//! transparently downgrades `json_schema` → `json_object` when the
//! upstream tells us it doesn't support strict structured outputs (the
//! deepseek-v4-flash / older-OpenAI / proxy case).
//!
//! Run with:
//!   cargo test -p aithericon-executor-llm --test openai_wire_format
//!
//! We use a `wiremock` server pretending to be OpenAI so the tests are
//! fully hermetic. The adapter's internal capability cache is process-
//! global; each test gets a unique mock server URL (free port per
//! `MockServer::start()`), so cache keys never collide across tests.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionSpec, JobPriority, RunContext, RunDirectory,
};
use aithericon_executor_llm::LlmBackend;
use aithericon_executor_worker::staging::default_pipeline;
use aithericon_secrets::{SecretError, SecretStore};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ─── Test infra ────────────────────────────────────────────────────────────

struct InMemoryStore(HashMap<String, String>);

#[async_trait]
impl SecretStore for InMemoryStore {
    async fn get(&self, key: &str) -> Result<String, SecretError> {
        self.0
            .get(key)
            .cloned()
            .ok_or_else(|| SecretError::NotFound(key.to_string()))
    }
    fn name(&self) -> &str {
        "openai-wire-test-store"
    }
}

fn cheap_unique() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{n:x}")
}

fn make_initial_ctx(spec: &ExecutionSpec, tmp: &std::path::Path, eid: &str) -> RunContext {
    RunContext {
        execution_id: eid.to_string(),
        spec: spec.clone(),
        run_dir: RunDirectory::new(&tmp.to_path_buf(), eid),
        timeout: Duration::from_secs(30),
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    }
}

fn noop_callback() -> aithericon_executor_backend::StatusCallback {
    Box::new(|_status, _detail| Box::pin(async {}))
}

fn ok_chat_response(content: &str, model: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-wire-test",
        "object": "chat.completion",
        "created": 0,
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 4, "total_tokens": 9 }
    })
}

/// Mimics the deepseek-v4-flash / older-OpenAI 400 that triggers the
/// json_schema → json_object capability downgrade. The exact phrasing
/// here is the user-reported one; `is_json_schema_unsupported` in the
/// adapter matches on both this and the OpenAI-3.5 variant.
fn capability_400_body() -> serde_json::Value {
    serde_json::json!({
        "code": 400,
        "reason": "INVALID_REQUEST_BODY",
        "message": "Model 'deepseek/deepseek-v4-flash' does not support 'json_schema' response \
                    format. Supported formats: json_object.",
        "metadata": {}
    })
}

async fn run_spec(spec: ExecutionSpec, tmp: std::path::PathBuf, eid: &str) -> ExecutionOutcome {
    let store = Arc::new(InMemoryStore(HashMap::new()));
    let pipeline = default_pipeline(
        tmp.clone(),
        None,
        Some(store as Arc<dyn SecretStore>),
        None,
        None,
    );
    let job = ExecutionJob {
        execution_id: eid.to_string(),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: Some(Duration::from_secs(30)),
        priority: JobPriority::Medium,
        stream_events: None,
        feed_chunks: false,
        wrapped_secrets: None,
    };
    let backend = LlmBackend::new();
    let mut initial_ctx = make_initial_ctx(&spec, &tmp, eid);
    initial_ctx
        .env
        .insert("OPENAI_API_KEY".into(), "sk-test".into());
    let ctx = pipeline
        .prepare(&job, initial_ctx, &backend as &dyn ExecutionBackend)
        .await
        .expect("staging must succeed");
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute must succeed against the mock");
    result.outcome
}

// ─── Tests ─────────────────────────────────────────────────────────────────

/// Pins the strict-mode `response_format` wire shape — `{type:"json_schema",
/// json_schema:{name:"extract", strict:true, schema:{...}}}`. Mirrors
/// OpenAI's documented Structured Outputs envelope. A drift here would
/// break every gpt-4o / gpt-4-turbo / gpt-4o-mini caller.
#[tokio::test]
async fn json_schema_request_has_canonical_openai_wire_shape() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ok_chat_response("{\"foo\":\"bar\"}", "gpt-4o-mini")),
        )
        .mount(&mock_server)
        .await;

    let tmp = std::env::temp_dir().join(format!("openai-wire-strict-{}", cheap_unique()));
    let outcome = run_spec(
        ExecutionSpec {
            backend: "llm".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({
                "provider": "openai",
                "model": "gpt-4o-mini",
                "prompt": "Extract some fields.",
                "base_url": mock_server.uri(),
                "response_format": {
                    "type": "json_schema",
                    "schema": {
                        "type": "object",
                        "properties": { "foo": { "type": "string" } },
                        "required": ["foo"]
                    }
                }
            }),
            config_ref: None,
        },
        tmp.clone(),
        &format!("wire-strict-{}", cheap_unique()),
    )
    .await;
    assert!(
        matches!(outcome, ExecutionOutcome::Success),
        "expected Success in strict mode, got {outcome:?}"
    );

    let received = mock_server.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        1,
        "exactly one request in strict-mode happy path"
    );
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();

    // The Structured Outputs envelope.
    let rf = &body["response_format"];
    assert_eq!(rf["type"], "json_schema", "outer type must be json_schema");
    assert_eq!(rf["json_schema"]["name"], "extract");
    assert_eq!(rf["json_schema"]["strict"], true);
    let inner_schema = &rf["json_schema"]["schema"];
    assert_eq!(inner_schema["type"], "object");
    assert_eq!(inner_schema["properties"]["foo"]["type"], "string");
    assert_eq!(
        inner_schema["required"].as_array().unwrap(),
        &vec![serde_json::Value::String("foo".into())]
    );

    // Messages must NOT have a synthetic schema-as-system-prompt — the
    // strict mode carries the schema in the envelope, injecting it would
    // be redundant and confuse instruction-tuned models.
    let msgs = body["messages"].as_array().unwrap();
    assert!(
        !msgs.iter().any(|m| m["role"] == "system"
            && m["content"]
                .as_str()
                .is_some_and(|s| s.contains("conforms to this JSON schema"))),
        "strict mode must not inject the schema-as-system-prompt; got messages {msgs:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The user's actual bug: model behind an OpenAI-compatible proxy rejects
/// `json_schema` with a deterministic 400. Adapter must:
///   1. Detect the capability error (not return it to the user).
///   2. Re-issue the same request with `response_format = {type:"json_object"}`.
///   3. Inject a leading system message carrying the original schema so the
///      model still gets shape guidance, AND the literal word "JSON" is
///      present (OpenAI's `json_object` mode mandates it).
///   4. Surface the model's reply as a successful structured output.
#[tokio::test]
async fn json_object_fallback_kicks_in_on_capability_400() {
    let mock_server = MockServer::start().await;

    // First call (json_schema) → 400 capability error.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(wiremock::matchers::body_partial_json(
            serde_json::json!({ "response_format": { "type": "json_schema" } }),
        ))
        .respond_with(ResponseTemplate::new(400).set_body_json(capability_400_body()))
        .expect(1)
        .mount(&mock_server)
        .await;

    // Retry (json_object) → 200 with structured payload.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(wiremock::matchers::body_partial_json(
            serde_json::json!({ "response_format": { "type": "json_object" } }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_chat_response(
            "{\"sentiment\":\"positive\"}",
            "deepseek-v4-flash",
        )))
        .expect(1)
        .mount(&mock_server)
        .await;

    let tmp = std::env::temp_dir().join(format!("openai-wire-fallback-{}", cheap_unique()));
    let outcome = run_spec(
        ExecutionSpec {
            backend: "llm".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({
                "provider": "openai",
                "model": "deepseek/deepseek-v4-flash",
                "prompt": "Classify the sentiment.",
                "base_url": mock_server.uri(),
                "response_format": {
                    "type": "json_schema",
                    "schema": {
                        "type": "object",
                        "properties": { "sentiment": { "type": "string" } },
                        "required": ["sentiment"]
                    }
                }
            }),
            config_ref: None,
        },
        tmp.clone(),
        &format!("wire-fallback-{}", cheap_unique()),
    )
    .await;
    assert!(
        matches!(outcome, ExecutionOutcome::Success),
        "fallback must surface as Success to the caller, got {outcome:?}"
    );

    // Pin the SHAPE of the retry — schema must reach the model via system
    // message, and the word "JSON" must appear (OpenAI requirement).
    let received = mock_server.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        2,
        "exactly two upstream calls: schema attempt + object retry"
    );

    let retry_body: serde_json::Value = serde_json::from_slice(&received[1].body).unwrap();
    assert_eq!(retry_body["response_format"]["type"], "json_object");
    assert!(
        retry_body["response_format"].get("json_schema").is_none(),
        "json_object mode must not include the json_schema envelope"
    );
    let msgs = retry_body["messages"].as_array().unwrap();
    let sys = msgs
        .iter()
        .find(|m| m["role"] == "system")
        .expect("must inject a system message with the schema");
    let sys_content = sys["content"].as_str().unwrap();
    assert!(
        sys_content.contains("JSON"),
        "system message must mention JSON literally (OpenAI json_object requirement)"
    );
    assert!(
        sys_content.contains("\"sentiment\""),
        "system message must inline the original schema, got:\n{sys_content}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Second call to the SAME `(base_url, model)` after the capability cache
/// has learned the downgrade must skip the dead first attempt entirely —
/// otherwise every call burns a wasted round trip + a noisy upstream 400.
#[tokio::test]
async fn capability_cache_short_circuits_subsequent_calls_for_same_model() {
    let mock_server = MockServer::start().await;
    // Prime: one capability 400 + one successful retry.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(wiremock::matchers::body_partial_json(
            serde_json::json!({ "response_format": { "type": "json_schema" } }),
        ))
        .respond_with(ResponseTemplate::new(400).set_body_json(capability_400_body()))
        .expect(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(wiremock::matchers::body_partial_json(
            serde_json::json!({ "response_format": { "type": "json_object" } }),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ok_chat_response("{\"ok\":true}", "deepseek/cache-test")),
        )
        .mount(&mock_server)
        .await;

    let tmp = std::env::temp_dir().join(format!("openai-wire-cache-{}", cheap_unique()));
    let spec_template = |eid: &str| ExecutionSpec {
        backend: "llm".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({
            "provider": "openai",
            "model": format!("deepseek/cache-test-{eid}"),
            "prompt": "x",
            "base_url": mock_server.uri(),
            "response_format": {
                "type": "json_schema",
                "schema": { "type": "object", "properties": { "ok": { "type": "boolean" } } }
            }
        }),
        config_ref: None,
    };
    // Same model string across both calls so the cache key matches.
    let model_suffix = cheap_unique();
    let mut spec = spec_template(&model_suffix);
    spec.config["model"] = serde_json::Value::String(format!("deepseek/cache-test-{model_suffix}"));

    let _ = run_spec(
        spec.clone(),
        tmp.clone(),
        &format!("call-1-{}", cheap_unique()),
    )
    .await;
    let _ = run_spec(spec, tmp.clone(), &format!("call-2-{}", cheap_unique())).await;

    let received = mock_server.received_requests().await.unwrap();
    // Call 1: schema attempt (400) + object retry (200) → 2 requests.
    // Call 2: cache hits → goes straight to json_object → 1 request.
    // Total: 3 requests, only ONE of which had `type: "json_schema"`.
    assert_eq!(
        received.len(),
        3,
        "expected schema-attempt + retry on call 1, cached-direct on call 2"
    );
    let schema_attempts = received
        .iter()
        .filter(|r| {
            let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap_or_default();
            body["response_format"]["type"] == "json_schema"
        })
        .count();
    assert_eq!(
        schema_attempts, 1,
        "capability cache must prevent re-asking for json_schema once it's known unsupported"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
