//! `POST /v1/execute` HTTP handler for the executor-llm pool_listener — the
//! generic inference-as-a-step surface.
//!
//! Stage 2 of the generic-execute-surface work. Where `/v1/inference` (see
//! [`crate::inference_handler`]) is the bespoke inference envelope, `/v1/execute`
//! is the SHARED generic envelope every executor pool serves: the engine POSTs
//! an [`ExecuteRequest`] and receives an [`ExecuteResponse`] whose `outputs`
//! map mirrors `InferenceResponse`'s fields (`output`, `model`, `finish_reason`,
//! `usage`, `structured_output`).
//!
//! ## Body-shaping moved POOL-SIDE
//!
//! Vision/Chat body-shaping previously lived ENGINE-side (`build_vision_body`
//! / `build_chat_body` in `application::http_executor_client`). Stage 2 ports
//! it here: the engine now hands the pool the opaque `input` + `config` and
//! the *pool* dispatches on `task_kind` to shape the `CompletionRequest`. This
//! keeps body-shaping co-located with the provider it targets.
//!
//! - `task_kind == "Vision"` → image turn: `images` built from
//!   `input.file_b64` + `input.mime_type` (default `image/png`); the
//!   DI-extraction prompt is authored from `input.document_id`.
//! - `task_kind ∈ {"Chat", "Agent", "StructuredOutput"}` → text turn: the
//!   user prompt is the JSON-serialized `input` with the `parameterize_*`
//!   system fields stripped; `config.system_prompt` → `system_prompt`;
//!   `config.tool_catalogue` → `tools` (forwarded verbatim to the provider).
//! - `task_kind ∈ {"Embeddings", "Asr"}` → 400 (not served by this pool).
//!
//! ## Model fail-closed AT THE POOL (`feedback_no_default_model`)
//!
//! `ExecuteRequest.model` is `Option<String>`. The handler 400s when it is
//! absent OR empty — the no-default-model rule is enforced here so the engine
//! can carry `model` as an `Option` without weakening it.
//!
//! ## Auth
//!
//! Validates a non-empty `Authorization: Bearer <token>` (lease proof);
//! lease *verification* is deferred, mirroring `/v1/inference`.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{Map, Value};

use aithericon_executor_domain::{ExecuteRequest, ExecuteResponse};

use crate::adapters::ollama::base_url_for_subprocess;
use crate::inference_handler::{
    extract_bearer, run_completion, InferenceImage, InferenceRequest, InferenceState,
};

/// System fields injected into every seeded token by mekhan-service's
/// `parameterize_*`. Stripped from the chat prompt so the LLM sees only the
/// clinical-domain payload. Keep in sync with `application::http_executor_client`.
const SYSTEM_FIELDS: &[&str] = &[
    "_instance_id",
    "_template_id",
    "_template_version",
    "_created_at",
    "_created_by",
];

/// `POST /v1/execute` handler for the LLM pool.
///
/// Pipeline:
///   1. Validate `Authorization: Bearer <token>` — 401 if absent/empty.
///   2. Translate [`ExecuteRequest`] → [`InferenceRequest`] by dispatching on
///      `task_kind` (ported engine body-shaping); model fail-closed (400 on
///      absent/empty).
///   3. Call the existing `run_completion` verbatim.
///   4. Project the `InferenceResponse` into the canonical `outputs` map.
pub async fn execute(
    State(state): State<InferenceState>,
    headers: HeaderMap,
    Json(req): Json<ExecuteRequest>,
) -> Result<Json<ExecuteResponse>, (StatusCode, String)> {
    let token = extract_bearer(&headers)?;
    if token.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authorization Bearer token must not be empty".to_string(),
        ));
    }

    let inference_req = build_inference_request(req)?;

    // Reuse the existing inference path verbatim — same env injection, same
    // run_completion (model/prompt re-validation, structured_output extraction).
    let (key, value) = base_url_for_subprocess(&state.ollama);
    let mut env = HashMap::new();
    env.insert(key, value);
    let resp = run_completion(&*state.port, inference_req, &env).await?;

    // Canonical outputs map — mirrors InferenceResponse's fields so a step
    // routed through /v1/execute and one routed through /v1/inference surface
    // identical `outputs`.
    let mut outputs: Map<String, Value> = Map::new();
    outputs.insert("output".into(), Value::String(resp.output));
    outputs.insert("model".into(), Value::String(resp.model));
    outputs.insert("finish_reason".into(), Value::String(resp.finish_reason));
    outputs.insert(
        "usage".into(),
        serde_json::json!({
            "input_tokens": resp.usage.input_tokens,
            "output_tokens": resp.usage.output_tokens,
            "total_tokens": resp.usage.total_tokens,
        }),
    );
    // structured_output only when present (parity with InferenceResponse's
    // skip_serializing_if).
    if let Some(so) = resp.structured_output {
        outputs.insert("structured_output".into(), so);
    }

    Ok(Json(ExecuteResponse { outputs }))
}

/// Translate an [`ExecuteRequest`] into an [`InferenceRequest`] by dispatching
/// on `task_kind`. Ports the engine's `build_vision_body` / `build_chat_body`.
fn build_inference_request(req: ExecuteRequest) -> Result<InferenceRequest, (StatusCode, String)> {
    // Model fail-closed AT THE POOL — no-default-model. (run_completion also
    // re-checks `model.is_empty()`, but we reject the absent/empty case up
    // front with a task-shaping-specific message.)
    let model = req.model.filter(|m| !m.is_empty()).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "ExecuteRequest.model is required and must be non-empty (no-default-model)".to_string(),
        )
    })?;

    let ExecuteRequest {
        task_kind,
        config,
        input,
        ..
    } = req;

    match task_kind.as_str() {
        "Vision" => Ok(build_vision_request(model, &config, &input)),
        "Chat" | "Agent" | "StructuredOutput" => build_chat_request(model, &config, &input),
        "Embeddings" | "Asr" => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "task_kind '{task_kind}' is not served by the LLM pool's /v1/execute; \
                 route to the appropriate executor pool"
            ),
        )),
        other => Err((
            StatusCode::BAD_REQUEST,
            format!("unrecognized task_kind '{other}'"),
        )),
    }
}

/// Build a Vision-style request: `images` from `input.file_b64` +
/// `input.mime_type` (default `image/png`); the DI-extraction prompt authored
/// from `input.document_id`. Ports `build_vision_body`.
fn build_vision_request(model: String, config: &Value, input: &Value) -> InferenceRequest {
    let images = match input.get("file_b64").and_then(Value::as_str) {
        Some(b64) => {
            let mime_type = input
                .get("mime_type")
                .and_then(Value::as_str)
                .unwrap_or("image/png")
                .to_string();
            vec![InferenceImage {
                base64: b64.to_string(),
                mime_type,
            }]
        }
        None => vec![],
    };

    let document_id = input
        .get("document_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let prompt = if document_id.is_empty() {
        "Extract structured fields from the attached image.".to_string()
    } else {
        format!("Extract structured fields from the attached image. document_id={document_id}")
    };

    InferenceRequest {
        model,
        system_prompt: config
            .get("system_prompt")
            .and_then(Value::as_str)
            .map(str::to_string),
        prompt,
        images,
        temperature: config.get("temperature").and_then(Value::as_f64),
        max_tokens: config.get("max_tokens").and_then(Value::as_u64),
        response_format: None,
        tools: vec![],
    }
}

/// Build a Chat-style request for `Chat` / `Agent` / `StructuredOutput`. The
/// user prompt is the JSON-serialized `input` with system fields stripped;
/// `config.system_prompt` → `system_prompt`; `config.tool_catalogue` →
/// `tools`. Ports `build_chat_body` + `strip_system_fields`.
fn build_chat_request(
    model: String,
    config: &Value,
    input: &Value,
) -> Result<InferenceRequest, (StatusCode, String)> {
    let stripped = strip_system_fields(input);
    let user_prompt = serde_json::to_string(&stripped).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("failed to serialize input token as prompt JSON: {e}"),
        )
    })?;
    if user_prompt.is_empty() || user_prompt == "{}" || user_prompt == "null" {
        return Err((
            StatusCode::BAD_REQUEST,
            "chat-style task_kind requires a non-empty input token \
             (system fields stripped — token had no domain payload)"
                .to_string(),
        ));
    }

    // tool_catalogue (engine effect_config key) → tools (InferenceRequest).
    // The handler is opaque to the contents; run_completion types each entry
    // into the domain ToolSchema.
    let tools = match config.get("tool_catalogue") {
        Some(Value::Array(arr)) => arr.clone(),
        Some(other) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "config.tool_catalogue must be an array of tool definitions, got {}",
                    json_type_name(other)
                ),
            ));
        }
        None => vec![],
    };

    Ok(InferenceRequest {
        model,
        system_prompt: config
            .get("system_prompt")
            .and_then(Value::as_str)
            .map(str::to_string),
        prompt: user_prompt,
        images: vec![],
        temperature: config.get("temperature").and_then(Value::as_f64),
        max_tokens: config.get("max_tokens").and_then(Value::as_u64),
        response_format: None,
        tools,
    })
}

/// Strip the `parameterize_*` system fields from a token object. Ports
/// `application::http_executor_client::strip_system_fields`.
fn strip_system_fields(token: &Value) -> Value {
    match token {
        Value::Object(map) => {
            let mut clean = Map::new();
            for (k, v) in map {
                if !SYSTEM_FIELDS.contains(&k.as_str()) {
                    clean.insert(k.clone(), v.clone());
                }
            }
            Value::Object(clean)
        }
        other => other.clone(),
    }
}

/// Human-readable JSON type name for error messages.
fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use async_trait::async_trait;

    use super::*;
    use crate::port::{CompletionPort, CompletionRequest, CompletionResponse, LlmError};
    use aithericon_executor_domain::{LlmStopReason, LlmUsage};

    fn exec_req(
        task_kind: &str,
        model: Option<&str>,
        config: Value,
        input: Value,
    ) -> ExecuteRequest {
        ExecuteRequest {
            backend: "llm".into(),
            task_kind: task_kind.into(),
            model: model.map(str::to_string),
            config,
            input,
        }
    }

    // ---- model fail-closed ----

    #[test]
    fn build_rejects_absent_model() {
        let req = exec_req(
            "Chat",
            None,
            serde_json::json!({}),
            serde_json::json!({"q": "hi"}),
        );
        let err = build_inference_request(req).expect_err("absent model must 400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("no-default-model"));
    }

    #[test]
    fn build_rejects_empty_model() {
        let req = exec_req(
            "Chat",
            Some(""),
            serde_json::json!({}),
            serde_json::json!({"q": "hi"}),
        );
        let err = build_inference_request(req).expect_err("empty model must 400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    // ---- Vision body-shaping ----

    #[test]
    fn vision_builds_image_turn_and_di_prompt() {
        let req = exec_req(
            "Vision",
            Some("qwen3.5:9b"),
            serde_json::json!({}),
            serde_json::json!({
                "file_b64": "QUJD",
                "mime_type": "image/jpeg",
                "document_id": "doc-42",
            }),
        );
        let ir = build_inference_request(req).expect("vision request builds");
        assert_eq!(ir.model, "qwen3.5:9b");
        assert_eq!(ir.images.len(), 1);
        assert_eq!(ir.images[0].base64, "QUJD");
        assert_eq!(ir.images[0].mime_type, "image/jpeg");
        assert!(ir.prompt.contains("document_id=doc-42"));
        assert!(ir.tools.is_empty());
    }

    #[test]
    fn vision_defaults_mime_and_omits_doc_id() {
        let req = exec_req(
            "Vision",
            Some("m"),
            serde_json::json!({}),
            serde_json::json!({ "file_b64": "QUJD" }),
        );
        let ir = build_inference_request(req).expect("builds");
        assert_eq!(ir.images[0].mime_type, "image/png");
        assert_eq!(
            ir.prompt,
            "Extract structured fields from the attached image."
        );
    }

    #[test]
    fn vision_without_image_yields_empty_images() {
        let req = exec_req(
            "Vision",
            Some("m"),
            serde_json::json!({}),
            serde_json::json!({}),
        );
        let ir = build_inference_request(req).expect("builds");
        assert!(ir.images.is_empty());
    }

    // ---- Chat / Agent / StructuredOutput body-shaping ----

    #[test]
    fn chat_serializes_stripped_input_as_prompt() {
        let req = exec_req(
            "Chat",
            Some("m"),
            serde_json::json!({ "system_prompt": "You are clinical." }),
            serde_json::json!({
                "_instance_id": "i-1",
                "_template_id": "t-1",
                "complaint": "headache",
            }),
        );
        let ir = build_inference_request(req).expect("chat builds");
        assert_eq!(ir.system_prompt.as_deref(), Some("You are clinical."));
        // System fields stripped; domain payload retained.
        assert!(ir.prompt.contains("complaint"));
        assert!(ir.prompt.contains("headache"));
        assert!(!ir.prompt.contains("_instance_id"));
        assert!(!ir.prompt.contains("_template_id"));
        assert!(ir.images.is_empty());
    }

    #[test]
    fn agent_forwards_tool_catalogue_as_tools() {
        let req = exec_req(
            "Agent",
            Some("m"),
            serde_json::json!({
                "tool_catalogue": [
                    {"name": "lookup", "description": "d", "parameters": {}}
                ]
            }),
            serde_json::json!({ "task": "find patient" }),
        );
        let ir = build_inference_request(req).expect("agent builds");
        assert_eq!(ir.tools.len(), 1);
        assert_eq!(ir.tools[0]["name"], "lookup");
    }

    #[test]
    fn structured_output_routes_through_chat_shape() {
        let req = exec_req(
            "StructuredOutput",
            Some("m"),
            serde_json::json!({}),
            serde_json::json!({ "field": "value" }),
        );
        let ir = build_inference_request(req).expect("structured builds");
        assert!(ir.prompt.contains("field"));
    }

    #[test]
    fn chat_rejects_empty_domain_payload() {
        // Only system fields → stripped to {} → 400.
        let req = exec_req(
            "Chat",
            Some("m"),
            serde_json::json!({}),
            serde_json::json!({ "_instance_id": "i-1", "_template_id": "t-1" }),
        );
        let err = build_inference_request(req).expect_err("empty payload must 400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn chat_rejects_non_array_tool_catalogue() {
        let req = exec_req(
            "Chat",
            Some("m"),
            serde_json::json!({ "tool_catalogue": "not-an-array" }),
            serde_json::json!({ "q": "hi" }),
        );
        let err = build_inference_request(req).expect_err("bad tool_catalogue must 400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    // ---- task_kind routing ----

    #[test]
    fn embeddings_and_asr_are_rejected() {
        for tk in ["Embeddings", "Asr"] {
            let req = exec_req(
                tk,
                Some("m"),
                serde_json::json!({}),
                serde_json::json!({"q": "x"}),
            );
            let err = build_inference_request(req).expect_err("must 400");
            assert_eq!(err.0, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn unrecognized_task_kind_is_rejected() {
        let req = exec_req(
            "Nonsense",
            Some("m"),
            serde_json::json!({}),
            serde_json::json!({"q": "x"}),
        );
        let err = build_inference_request(req).expect_err("must 400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("unrecognized task_kind"));
    }

    // ---- end-to-end handler against a stub CompletionPort ----

    struct StubPort;

    #[async_trait]
    impl CompletionPort for StubPort {
        async fn complete(
            &self,
            req: &CompletionRequest,
            _env: &HashMap<String, String>,
        ) -> Result<CompletionResponse, LlmError> {
            Ok(CompletionResponse {
                content: "Summary text.".to_string(),
                model: req.model.clone(),
                stop_reason: LlmStopReason::EndTurn,
                usage: LlmUsage {
                    input_tokens: 12,
                    output_tokens: 3,
                    total_tokens: 15,
                },
                structured_output: None,
                tool_calls: vec![],
            })
        }
        fn name(&self) -> &str {
            "stub"
        }
    }

    /// Drive the full handler via `run_completion` directly (no live Ollama):
    /// build the inference request, run the stub port, project the outputs map.
    /// Mirrors what `execute` does after env injection (which the stub ignores).
    /// The production `execute` wrapper is a thin orchestration of
    /// `extract_bearer` (covered by the inference_handler tests + the bearer
    /// guards below) + `build_inference_request` + `run_completion`; building
    /// a live `InferenceState` requires spawning Ollama, so the projection is
    /// exercised here at the piece level instead.
    #[tokio::test]
    async fn execute_projects_canonical_outputs_map() {
        let req = exec_req(
            "Chat",
            Some("qwen3.5:9b"),
            serde_json::json!({ "system_prompt": "You are a clinical assistant." }),
            serde_json::json!({ "prompt": "Summarize the chart." }),
        );
        let inference_req = build_inference_request(req).expect("builds");
        let resp = run_completion(&StubPort, inference_req, &HashMap::new())
            .await
            .expect("completion succeeds");

        let mut outputs: Map<String, Value> = Map::new();
        outputs.insert("output".into(), Value::String(resp.output));
        outputs.insert("model".into(), Value::String(resp.model));
        outputs.insert("finish_reason".into(), Value::String(resp.finish_reason));
        outputs.insert(
            "usage".into(),
            serde_json::json!({
                "input_tokens": resp.usage.input_tokens,
                "output_tokens": resp.usage.output_tokens,
                "total_tokens": resp.usage.total_tokens,
            }),
        );

        assert_eq!(outputs["output"], "Summary text.");
        assert_eq!(outputs["model"], "qwen3.5:9b");
        assert_eq!(outputs["finish_reason"], "end_turn");
        assert_eq!(outputs["usage"]["input_tokens"], 12);
        assert_eq!(outputs["usage"]["output_tokens"], 3);
        assert_eq!(outputs["usage"]["total_tokens"], 15);
        // No structured_output → key absent (skip_serializing_if parity).
        assert!(!outputs.contains_key("structured_output"));
    }

    /// `structured_output` is surfaced under the canonical `outputs` map when
    /// the port returns one (parity with `InferenceResponse`'s
    /// `skip_serializing_if = Option::is_none`).
    #[tokio::test]
    async fn execute_surfaces_structured_output_when_present() {
        struct StructuredPort;
        #[async_trait]
        impl CompletionPort for StructuredPort {
            async fn complete(
                &self,
                req: &CompletionRequest,
                _env: &HashMap<String, String>,
            ) -> Result<CompletionResponse, LlmError> {
                Ok(CompletionResponse {
                    content: "{}".to_string(),
                    model: req.model.clone(),
                    stop_reason: LlmStopReason::EndTurn,
                    usage: LlmUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        total_tokens: 2,
                    },
                    structured_output: Some(serde_json::json!({ "field": "value" })),
                    tool_calls: vec![],
                })
            }
            fn name(&self) -> &str {
                "structured"
            }
        }

        let req = exec_req(
            "StructuredOutput",
            Some("m"),
            serde_json::json!({}),
            serde_json::json!({ "field": "x" }),
        );
        let inference_req = build_inference_request(req).expect("builds");
        let resp = run_completion(&StructuredPort, inference_req, &HashMap::new())
            .await
            .expect("completion succeeds");

        let mut outputs: Map<String, Value> = Map::new();
        if let Some(so) = resp.structured_output {
            outputs.insert("structured_output".into(), so);
        }
        assert_eq!(outputs["structured_output"]["field"], "value");
    }

    /// The `execute` wrapper rejects a missing bearer before touching the
    /// port. We assert the guard at its source (`extract_bearer`) since
    /// constructing a live `InferenceState` requires spawning Ollama.
    #[test]
    fn execute_bearer_guard_rejects_missing_header() {
        let err = extract_bearer(&HeaderMap::new()).expect_err("missing bearer must 401");
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
    }
}
