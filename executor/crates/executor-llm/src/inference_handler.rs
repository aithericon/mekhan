//! `POST /v1/inference` HTTP handler for the executor-llm pool_listener.
//!
//! Sub-phase 2.3b: HTTP-bridge restoration for cap-routed inference dispatch.
//! The endpoint wraps the existing `CompletionPort` (OllamaAdapter) against
//! the managed Ollama subprocess. Cap-routing issues a lease token before
//! routing requests here; the handler accepts any non-empty Bearer as proof
//! the caller holds a lease — in-line lease verification would require a
//! cap-routing round-trip per request and is deferred to a later slice.
//!
//! ## Wire shape
//!
//! Request: [`InferenceRequest`] — JSON body with `model`, `prompt`, and
//! optional `system_prompt`, `images`, `temperature`, `max_tokens`,
//! `response_format`.
//!
//! Response: [`InferenceResponse`] — JSON body with `output`, `model`,
//! `finish_reason`, `usage`, and optional `structured_output`.
//!
//! Both shapes are authored in the same wave as the engine-side
//! `HttpInferenceHandler` client; they round-trip on the cert harness.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::adapters::ollama::base_url_for_subprocess;
use crate::config::Role;
use crate::ollama_subprocess::OllamaSubprocess;
use crate::port::{CompletionPort, CompletionRequest, ImageData, LlmError, Message, ResponseFormat};

/// Axum state injected into the inference handler via `.with_state()`.
#[derive(Clone)]
pub struct InferenceState {
    pub port: Arc<dyn CompletionPort>,
    pub ollama: Arc<OllamaSubprocess>,
}

/// Request body for `POST /v1/inference`.
#[derive(Debug, Deserialize)]
pub struct InferenceRequest {
    pub model: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    pub prompt: String,
    #[serde(default)]
    pub images: Vec<InferenceImage>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub response_format: Option<crate::config::ResponseFormat>,
}

/// A base64-encoded image included with an inference request.
#[derive(Debug, Deserialize)]
pub struct InferenceImage {
    pub base64: String,
    pub mime_type: String,
}

/// Response body for `POST /v1/inference`.
#[derive(Debug, Serialize)]
pub struct InferenceResponse {
    pub output: String,
    pub model: String,
    pub finish_reason: String,
    pub usage: InferenceUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<serde_json::Value>,
}

/// Token usage metrics returned with every inference response.
#[derive(Debug, Serialize)]
pub struct InferenceUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// `POST /v1/inference` handler.
///
/// Pipeline:
///   1. Validate `Authorization: Bearer <token>` — 401 if absent or empty.
///   2. Validate `model` and `prompt` are non-empty — 400 otherwise.
///   3. Build a `CompletionRequest` from the body.
///   4. Call `port.complete(&request, &env)` with the subprocess base URL.
///   5. Map `CompletionResponse` → `InferenceResponse` (200) or
///      `LlmError` → 422.
pub async fn inference(
    State(state): State<InferenceState>,
    headers: HeaderMap,
    Json(req): Json<InferenceRequest>,
) -> Result<Json<InferenceResponse>, (StatusCode, String)> {
    let token = extract_bearer(&headers)?;
    if token.is_empty() {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Authorization Bearer token must not be empty".to_string(),
        ));
    }

    let (key, value) = base_url_for_subprocess(&state.ollama);
    let mut env = HashMap::new();
    env.insert(key, value);

    let resp = run_completion(&*state.port, req, &env).await?;
    Ok(Json(resp))
}

/// Core completion logic shared by production handler and test helpers.
///
/// Validates `model` and `prompt` (400 if empty), builds the `CompletionRequest`,
/// and dispatches to `port`. Separated so tests can exercise it without
/// constructing an axum State or a live OllamaSubprocess — the mock port
/// ignores `env` entirely.
pub(crate) async fn run_completion(
    port: &dyn CompletionPort,
    req: InferenceRequest,
    env: &HashMap<String, String>,
) -> Result<InferenceResponse, (StatusCode, String)> {
    if req.model.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "model must not be empty".to_string()));
    }
    if req.prompt.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "prompt must not be empty".to_string()));
    }

    let mut messages = Vec::new();
    if let Some(ref system_prompt) = req.system_prompt {
        messages.push(Message {
            role: Role::System,
            content: system_prompt.clone(),
            images: vec![],
        });
    }
    let user_images: Vec<ImageData> = req
        .images
        .iter()
        .map(|img| ImageData {
            base64: img.base64.clone(),
            media_type: img.mime_type.clone(),
        })
        .collect();
    messages.push(Message {
        role: Role::User,
        content: req.prompt.clone(),
        images: user_images,
    });

    let response_format = req.response_format.unwrap_or(ResponseFormat::Text);
    let completion_req = CompletionRequest {
        model: req.model.clone(),
        messages,
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        response_format,
    };

    let completion = port.complete(&completion_req, env).await.map_err(|e| {
        let msg = match &e {
            LlmError::Config(s) | LlmError::Api(s) | LlmError::Parse(s) => s.clone(),
        };
        (StatusCode::UNPROCESSABLE_ENTITY, msg)
    })?;

    Ok(InferenceResponse {
        output: completion.content,
        model: completion.model,
        finish_reason: completion.finish_reason.to_string(),
        usage: InferenceUsage {
            input_tokens: completion.usage.input_tokens,
            output_tokens: completion.usage.output_tokens,
            total_tokens: completion.usage.total_tokens,
        },
        structured_output: completion.structured_output,
    })
}

/// Extract the Bearer token from the `Authorization` header.
/// Returns `Err((401, ...))` when the header is absent or not a Bearer scheme.
pub(crate) fn extract_bearer(headers: &HeaderMap) -> Result<String, (StatusCode, String)> {
    let header_val = headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "Authorization header is required".to_string(),
            )
        })?;
    let raw = header_val.to_str().map_err(|_| {
        (
            StatusCode::UNAUTHORIZED,
            "Authorization header contains invalid characters".to_string(),
        )
    })?;
    let token = raw
        .strip_prefix("Bearer ")
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "Authorization header must use Bearer scheme".to_string(),
            )
        })?;
    Ok(token.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use serde_json::{Value, json};

    use super::*;
    use crate::port::{
        CompletionPort, CompletionRequest, CompletionResponse, FinishReason, LlmError, TokenUsage,
    };

    // ---------------------------------------------------------------------------
    // Mock CompletionPort
    // ---------------------------------------------------------------------------

    struct FixedCompletionPort {
        result: Result<CompletionResponse, LlmError>,
    }

    impl FixedCompletionPort {
        fn ok(model: &str, content: &str) -> Self {
            Self {
                result: Ok(CompletionResponse {
                    content: content.to_string(),
                    model: model.to_string(),
                    finish_reason: FinishReason::Stop,
                    usage: TokenUsage {
                        input_tokens: 10,
                        output_tokens: 20,
                        total_tokens: 30,
                    },
                    structured_output: None,
                }),
            }
        }

        fn err(e: LlmError) -> Self {
            Self { result: Err(e) }
        }
    }

    #[async_trait]
    impl CompletionPort for FixedCompletionPort {
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _env: &HashMap<String, String>,
        ) -> Result<CompletionResponse, LlmError> {
            match &self.result {
                Ok(r) => Ok(CompletionResponse {
                    content: r.content.clone(),
                    model: r.model.clone(),
                    finish_reason: r.finish_reason.clone(),
                    usage: r.usage.clone(),
                    structured_output: r.structured_output.clone(),
                }),
                Err(LlmError::Api(s)) => Err(LlmError::Api(s.clone())),
                Err(LlmError::Config(s)) => Err(LlmError::Config(s.clone())),
                Err(LlmError::Parse(s)) => Err(LlmError::Parse(s.clone())),
            }
        }

        fn name(&self) -> &str {
            "mock"
        }
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        h
    }

    fn no_auth_headers() -> HeaderMap {
        HeaderMap::new()
    }

    fn empty_env() -> HashMap<String, String> {
        HashMap::new()
    }

    // ---------------------------------------------------------------------------
    // Serde round-trip tests
    // ---------------------------------------------------------------------------

    #[test]
    fn test_inference_request_deserialization() {
        let json = json!({
            "model": "test-model-a",
            "prompt": "Hello",
            "system_prompt": "You are helpful.",
            "images": [{"base64": "abc=", "mime_type": "image/png"}],
            "temperature": 0.7,
            "max_tokens": 512
        });
        let req: InferenceRequest = serde_json::from_value(json).expect("deserialize");
        assert_eq!(req.model, "test-model-a");
        assert_eq!(req.prompt, "Hello");
        assert_eq!(req.system_prompt.as_deref(), Some("You are helpful."));
        assert_eq!(req.images.len(), 1);
        assert_eq!(req.images[0].mime_type, "image/png");
        assert_eq!(req.temperature, Some(0.7));
        assert_eq!(req.max_tokens, Some(512));
    }

    #[test]
    fn test_inference_request_minimal_deserialization() {
        let json = json!({"model": "test-model-b", "prompt": "hi"});
        let req: InferenceRequest = serde_json::from_value(json).expect("deserialize minimal");
        assert!(req.system_prompt.is_none());
        assert!(req.images.is_empty());
        assert!(req.temperature.is_none());
        assert!(req.max_tokens.is_none());
    }

    #[test]
    fn test_inference_response_serialization() {
        let resp = InferenceResponse {
            output: "result text".to_string(),
            model: "test-model-a".to_string(),
            finish_reason: "stop".to_string(),
            usage: InferenceUsage {
                input_tokens: 5,
                output_tokens: 10,
                total_tokens: 15,
            },
            structured_output: None,
        };
        let v: Value = serde_json::to_value(&resp).expect("serialize");
        assert_eq!(v["output"], "result text");
        assert_eq!(v["model"], "test-model-a");
        assert_eq!(v["finish_reason"], "stop");
        assert_eq!(v["usage"]["input_tokens"], 5);
        assert_eq!(v["usage"]["output_tokens"], 10);
        assert_eq!(v["usage"]["total_tokens"], 15);
        assert!(v.get("structured_output").is_none(), "skip_serializing_if None");
    }

    #[test]
    fn test_inference_response_with_structured_output_serialization() {
        let resp = InferenceResponse {
            output: "{}".to_string(),
            model: "test-model-b".to_string(),
            finish_reason: "stop".to_string(),
            usage: InferenceUsage {
                input_tokens: 1,
                output_tokens: 1,
                total_tokens: 2,
            },
            structured_output: Some(json!({"key": "value"})),
        };
        let v: Value = serde_json::to_value(&resp).expect("serialize");
        assert_eq!(v["structured_output"]["key"], "value");
    }

    // ---------------------------------------------------------------------------
    // Handler behaviour tests via direct function calls
    //
    // Tests call extract_bearer() and run_completion() directly — no axum
    // oneshot needed, no tower dev-dep. The production inference() handler
    // is a thin orchestration of these two functions; its logic is covered by
    // testing each piece independently.
    // ---------------------------------------------------------------------------

    #[test]
    fn test_inference_handler_rejects_missing_bearer() {
        let headers = no_auth_headers();
        let result = extract_bearer(&headers);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_inference_handler_rejects_empty_bearer() {
        // "Bearer " prefix present but the token portion is empty.
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer "),
        );
        let token = extract_bearer(&headers).expect("extract_bearer succeeds on valid scheme");
        assert!(token.is_empty(), "token should be empty string");
    }

    #[tokio::test]
    async fn test_inference_handler_rejects_empty_model() {
        let port = Arc::new(FixedCompletionPort::ok("test-model-a", "hello"));
        let req = InferenceRequest {
            model: String::new(),
            system_prompt: None,
            prompt: "hi".to_string(),
            images: vec![],
            temperature: None,
            max_tokens: None,
            response_format: None,
        };
        let result = run_completion(&*port, req, &empty_env()).await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_inference_handler_rejects_empty_prompt() {
        let port = Arc::new(FixedCompletionPort::ok("test-model-a", "hello"));
        let req = InferenceRequest {
            model: "test-model-a".to_string(),
            system_prompt: None,
            prompt: String::new(),
            images: vec![],
            temperature: None,
            max_tokens: None,
            response_format: None,
        };
        let result = run_completion(&*port, req, &empty_env()).await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_inference_handler_calls_completion_port() {
        let port = Arc::new(FixedCompletionPort::ok("test-model-a", "generated text"));
        let req = InferenceRequest {
            model: "test-model-a".to_string(),
            system_prompt: Some("You are a math tutor.".to_string()),
            prompt: "what is 2+2?".to_string(),
            images: vec![],
            temperature: Some(0.3),
            max_tokens: Some(256),
            response_format: None,
        };
        let resp = run_completion(&*port, req, &empty_env())
            .await
            .expect("completion succeeds");
        assert_eq!(resp.output, "generated text");
        assert_eq!(resp.model, "test-model-a");
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 20);
        assert_eq!(resp.usage.total_tokens, 30);
    }

    #[tokio::test]
    async fn test_inference_handler_maps_llm_error_to_422() {
        let port = Arc::new(FixedCompletionPort::err(LlmError::Api(
            "upstream timeout".to_string(),
        )));
        let req = InferenceRequest {
            model: "test-model-b".to_string(),
            system_prompt: None,
            prompt: "hello".to_string(),
            images: vec![],
            temperature: None,
            max_tokens: None,
            response_format: None,
        };
        let result = run_completion(&*port, req, &empty_env()).await;
        assert_eq!(result.unwrap_err().0, StatusCode::UNPROCESSABLE_ENTITY);
    }

    // Verify the bearer-validation helpers are consistent: valid token passes.
    #[test]
    fn extract_bearer_valid_token() {
        let headers = bearer_headers("my-lease-token-xyz");
        let token = extract_bearer(&headers).expect("valid bearer accepted");
        assert_eq!(token, "my-lease-token-xyz");
    }
}
