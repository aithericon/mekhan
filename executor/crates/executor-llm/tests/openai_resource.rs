//! End-to-end conformance for the `openai` LLM backend × `openai` resource flow.
//!
//! This test exercises what happens when an LLM step references an `openai`
//! resource whose `api_key` is sourced from Vault / a `SecretStore`:
//!
//! 1. The compiler (or any AIR producer) emits `spec.config.api_key =
//!    "{{secret:aithericon/resources/.../v1#api_key}}"`.
//! 2. `PlanSecretsHook` populates `resolved_config` with the resolved
//!    envelope, leaving `spec.config` itself with the unresolved template.
//! 3. `LlmBackend::prepare` reads `resolved_config` (NOT `spec.config`) so the
//!    deserialized `LlmConfig.api_key` is the plaintext value.
//! 4. The OpenAI adapter sends `Authorization: Bearer <plaintext>` to the
//!    upstream endpoint.
//!
//! Until commit `<this PR>`, the LLM backend read `spec.config` directly and
//! the placeholder string itself was used as the Bearer token — every call
//! would 401 (or worse, leak the structure of the Vault path in upstream
//! logs). This test pins down the contract so a future refactor can't
//! regress the resolution boundary again.
//!
//! We use a `wiremock` HTTP server pretending to be `api.openai.com`, so the
//! test is fully hermetic — no real API key, no outbound network.
//!
//! Run with:
//!   cargo test -p aithericon-executor-llm --test openai_resource

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

// ─── In-memory SecretStore standing in for the launcher-wrapped resource ────

/// Test-only `SecretStore` keyed on the same `vault_path#field` strings that
/// `service/src/petri/resource_resolver.rs:350-359` emits at publish time.
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
        "openai-resource-test-store"
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

const RESOURCE_VAULT_PATH: &str =
    "aithericon/resources/00000000-0000-0000-0000-000000000000/r-openai-prod/v1";
const SECRET_FIELD: &str = "api_key";
const OPENAI_PLAINTEXT_KEY: &str = "sk-PLAINTEXT-OPENAI-KEY-DO-NOT-LEAK";

fn secret_token() -> String {
    format!("{{{{secret:{RESOURCE_VAULT_PATH}#{SECRET_FIELD}}}}}")
}

/// Canned OpenAI `/v1/chat/completions` response.
fn fake_openai_response() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 0,
        "model": "gpt-4o-mini",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "hello from mock openai" },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 4,
            "total_tokens": 9
        }
    })
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
        run_dir: RunDirectory::new(tmp, eid),
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

// ─── Test 1: api_key via spec.config (the resource path) ────────────────────

/// The `openai` resource type emits `spec.config.api_key = "{{secret:...}}"`.
/// `PlanSecretsHook` must populate `resolved_config`, and `LlmBackend::prepare`
/// must consume it. The mock OpenAI server then receives the resolved
/// plaintext as the Bearer token.
#[tokio::test]
async fn openai_resource_secret_is_resolved_into_bearer_token() {
    // ── Mock OpenAI HTTP surface ─────────────────────────────────────
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_openai_response()))
        .mount(&mock_server)
        .await;

    // ── Configure pipeline with an in-memory secret store ────────────
    let store = Arc::new(InMemoryStore(HashMap::from([(
        format!("{RESOURCE_VAULT_PATH}#{SECRET_FIELD}"),
        OPENAI_PLAINTEXT_KEY.to_string(),
    )])));
    let tmp = std::env::temp_dir().join(format!(
        "openai-resource-test-{}-{}",
        std::process::id(),
        cheap_unique()
    ));
    let pipeline = default_pipeline(
        tmp.clone(),
        None,
        Some(store.clone() as Arc<dyn SecretStore>),
        None,
        None,
        None,
    );

    // ── Build a spec that mirrors the openai resource shape ──────────
    // Same field layout `shared/resources/src/types.rs` declares for the
    // `openai` type: `api_key` is the secret leaf, `organization` is public.
    let spec = ExecutionSpec {
        backend: "llm".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({
            "provider": "openai",
            "model": "gpt-4o-mini",
            "prompt": "Say hello.",
            "api_key": secret_token(),
            "organization": "acme-test",
            "base_url": mock_server.uri(),
        }),
        config_ref: None,
    };
    let eid = format!("openai-resource-{}", cheap_unique());
    let job = ExecutionJob {
        execution_id: eid.clone(),
        workspace_id: String::new(),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: Some(Duration::from_secs(30)),
        priority: JobPriority::Medium,
        stream_events: None,
        feed_chunks: false,
        channels: Vec::new(),
        wrapped_secrets: None,
    };

    // ── Run full staging pipeline → backend.prepare ──────────────────
    let backend = LlmBackend::new();
    let initial_ctx = make_initial_ctx(&spec, &tmp, &eid);
    let ctx = pipeline
        .prepare(&job, initial_ctx, &backend as &dyn ExecutionBackend)
        .await
        .expect("staging + prepare must succeed");

    // PlanSecretsHook should have populated resolved_config with the
    // plaintext envelope. Defense in depth: the on-disk spec stays templated.
    assert!(
        ctx.resolved_config.is_some(),
        "PlanSecretsHook must populate resolved_config when spec.config has a secret template"
    );
    let on_disk_context = std::fs::read_to_string(&ctx.run_dir.context_file).unwrap();
    assert!(
        on_disk_context.contains("{{secret:"),
        "context.json must keep the unresolved template, got:\n{on_disk_context}"
    );
    assert!(
        !on_disk_context.contains(OPENAI_PLAINTEXT_KEY),
        "context.json must NOT contain the plaintext api_key"
    );

    // ── Execute against the wiremock OpenAI ───────────────────────────
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("backend.execute must succeed against the mock");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}. stderr: {:?}",
        result.outcome,
        result.stderr_tail
    );

    // ── Inspect what the mock saw ─────────────────────────────────────
    let received = mock_server
        .received_requests()
        .await
        .expect("mock should record requests");
    assert_eq!(
        received.len(),
        1,
        "expected exactly one POST to /v1/chat/completions"
    );
    let req = &received[0];
    let auth_header = req
        .headers
        .get("authorization")
        .map(|v| v.to_str().unwrap().to_string())
        .expect("Authorization header must be present");
    assert_eq!(
        auth_header,
        format!("Bearer {OPENAI_PLAINTEXT_KEY}"),
        "Bearer token must carry the RESOLVED api_key, not the template — got: {auth_header}"
    );
    assert!(
        !auth_header.contains("{{secret:"),
        "Authorization header must NOT contain the unresolved template"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// ─── Test 2: api_key via env (OPENAI_API_KEY) ───────────────────────────────

/// Same contract, env-routed: when `OPENAI_API_KEY` is set to a `{{secret:KEY}}`
/// template (e.g. by the AIR setting `env: { OPENAI_API_KEY: "{{secret:...}}" }`
/// on the LLM node), the resolved plaintext must overlay `env` before the
/// adapter looks up `OPENAI_API_KEY` to build the Bearer token.
#[tokio::test]
async fn openai_env_secret_is_resolved_into_bearer_token() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_openai_response()))
        .mount(&mock_server)
        .await;

    let store = Arc::new(InMemoryStore(HashMap::from([(
        format!("{RESOURCE_VAULT_PATH}#{SECRET_FIELD}"),
        OPENAI_PLAINTEXT_KEY.to_string(),
    )])));
    let tmp = std::env::temp_dir().join(format!(
        "openai-env-test-{}-{}",
        std::process::id(),
        cheap_unique()
    ));
    let pipeline = default_pipeline(
        tmp.clone(),
        None,
        Some(store.clone() as Arc<dyn SecretStore>),
        None,
        None,
        None,
    );

    // No `api_key` on the config this time — the adapter must pick it up
    // from env (which carries a template, resolved into ctx.resolved_env).
    let spec = ExecutionSpec {
        backend: "llm".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({
            "provider": "openai",
            "model": "gpt-4o-mini",
            "prompt": "Say hello.",
            "base_url": mock_server.uri(),
        }),
        config_ref: None,
    };
    let eid = format!("openai-env-{}", cheap_unique());
    let job = ExecutionJob {
        execution_id: eid.clone(),
        workspace_id: String::new(),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: Some(Duration::from_secs(30)),
        priority: JobPriority::Medium,
        stream_events: None,
        feed_chunks: false,
        channels: Vec::new(),
        wrapped_secrets: None,
    };

    let backend = LlmBackend::new();
    let mut initial_ctx = make_initial_ctx(&spec, &tmp, &eid);
    initial_ctx
        .env
        .insert("OPENAI_API_KEY".into(), secret_token());

    let ctx = pipeline
        .prepare(&job, initial_ctx, &backend as &dyn ExecutionBackend)
        .await
        .expect("staging + prepare must succeed");

    assert_eq!(
        ctx.resolved_env.get("OPENAI_API_KEY").map(String::as_str),
        Some(OPENAI_PLAINTEXT_KEY),
        "PlanSecretsHook must populate resolved_env for env-routed secrets"
    );
    assert_eq!(
        ctx.env.get("OPENAI_API_KEY").map(String::as_str),
        Some(secret_token().as_str()),
        "ctx.env must keep the unresolved template (defense in depth)"
    );

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("backend.execute must succeed against the mock");
    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}. stderr: {:?}",
        result.outcome,
        result.stderr_tail
    );

    let received = mock_server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let auth = received[0]
        .headers
        .get("authorization")
        .map(|v| v.to_str().unwrap().to_string())
        .expect("Authorization header must be present");
    assert_eq!(
        auth,
        format!("Bearer {OPENAI_PLAINTEXT_KEY}"),
        "env-routed Bearer token must be resolved, got: {auth}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// ─── Test 3: missing secret → SecretResolutionFailed, never a 401 leak ──────

/// If the `SecretStore` doesn't carry the referenced key, the pipeline must
/// fail at the staging boundary — NOT silently send the template to OpenAI
/// (which would either 401 or, in a debug log, expose the vault path).
#[tokio::test]
async fn missing_openai_secret_fails_at_staging_not_at_adapter() {
    let mock_server = MockServer::start().await;
    // Mount a "must never be called" expectation: if the executor falls
    // through and sends the request, it will fail with "expected 0, got 1".
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("should never see this"))
        .expect(0)
        .mount(&mock_server)
        .await;

    let store = Arc::new(InMemoryStore(HashMap::new())); // empty
    let tmp = std::env::temp_dir().join(format!(
        "openai-missing-test-{}-{}",
        std::process::id(),
        cheap_unique()
    ));
    let pipeline = default_pipeline(
        tmp.clone(),
        None,
        Some(store.clone() as Arc<dyn SecretStore>),
        None,
        None,
        None,
    );

    let spec = ExecutionSpec {
        backend: "llm".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({
            "provider": "openai",
            "model": "gpt-4o-mini",
            "prompt": "Say hello.",
            "api_key": secret_token(),
            "base_url": mock_server.uri(),
        }),
        config_ref: None,
    };
    let eid = format!("openai-missing-{}", cheap_unique());
    let job = ExecutionJob {
        execution_id: eid.clone(),
        workspace_id: String::new(),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: Some(Duration::from_secs(30)),
        priority: JobPriority::Medium,
        stream_events: None,
        feed_chunks: false,
        channels: Vec::new(),
        wrapped_secrets: None,
    };

    let backend = LlmBackend::new();
    let initial_ctx = make_initial_ctx(&spec, &tmp, &eid);
    let staging_result = pipeline
        .prepare(&job, initial_ctx, &backend as &dyn ExecutionBackend)
        .await;
    assert!(
        staging_result.is_err(),
        "PlanSecretsHook must fail closed when the referenced secret is unknown"
    );
    let err = staging_result.unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains("secret") || err.contains(SECRET_FIELD),
        "error must mention the secret/key, got: {err}"
    );

    // Mock dropped here; its `expect(0)` would scream if anyone hit it.
    let _ = std::fs::remove_dir_all(&tmp);
}
