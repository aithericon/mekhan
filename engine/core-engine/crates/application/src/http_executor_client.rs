//! HTTP-sync `EffectHandler` implementation for cap-routed inference dispatch.
//!
//! Sub-phase 2.3b — implements `HttpInferenceHandler`, the cloud-layer path
//! that replaces NATS-async dispatch when cap-routing's `HttpPreDispatchHook`
//! has enriched `EffectInput.config` with `base_url` + `lease_token`.
//!
//! ## Why `EffectHandler`, not `ExecutorClient`
//!
//! `ExecutorClient` is the submit-then-poll-for-signals contract (async
//! NATS-style). HTTP-sync is request/response: we POST, await the response,
//! and return. The cleanest fit is an `EffectHandler` that sits parallel to
//! `ExecutorSubmitHandler` — same trait, different dispatch path.
//!
//! ## Enrichment field shape
//!
//! Cloud-layer-workflow's `CapabilityRoutingHook::merge_enrichment` overlays
//! these fields into the effect_config before the handler fires:
//! - `base_url: String` — executor pool HTTP base URL (e.g. `http://host:3301`)
//! - `lease_token: String` — bearer token for the pool request
//! - `pool_id: String` — cap-routing pool identifier
//! - `hardware_kind: Option<String>` — present when cap-routing knows it
//!
//! The original scenario fields (`required_model`, `system_prompt`, etc.) are
//! preserved via LWW merge and are also readable from `EffectInput.config`.
//!
//! ## Lease-release semantics (deferred)
//!
//! Handler-side lease release is NOT implemented in this slice. Cloud-layer-
//! workflow's `CapabilityRoutingHook::merge_enrichment` does not enrich a
//! cap-routing release URL into `EffectInput.config`; the dispatcher in
//! cloud-layer-workflow holds cap-routing's base URL out-of-band and releases
//! via that path. Cap-routing's TTL eviction (15s, per A3 § 7 workaround 1)
//! is the backstop for this slice. A follow-up slice will either (a) enrich
//! `cap_routing_release_url` so the handler can release explicitly, or (b)
//! wire cloud-layer-workflow's existing dispatcher to release based on engine
//! event-stream observations. Tracked under workstream #61.
//!
//! ## Registration
//!
//! Item 3 (downstream serial) wires `HttpInferenceHandler` into
//! `net_registry.rs` under the `executor_submit` handler_id when
//! `CLOUD_LAYER_BASE_URL` is set. This file owns only the handler body.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput, EffectPortSchemas};

/// HTTP-sync inference dispatch handler.
///
/// Reads cap-routing pre-dispatch enrichment (`base_url`, `lease_token`)
/// from `EffectInput.config`, builds an inference request from the input
/// token, and POSTs to `{base_url}/v1/inference` — the endpoint implemented
/// in Item 1 (`executor-llm/src/inference_handler.rs`).
pub struct HttpInferenceHandler {
    http: reqwest::Client,
    input_port: String,
    output_port: String,
}

impl HttpInferenceHandler {
    pub fn new(input_port: impl Into<String>, output_port: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            // Vision-model inference on qwen3.6:35b-a3b on M5 Metal can take
            // 5-30 s per image; 300 s is generous to prevent spurious timeouts
            // on cold starts or queued requests.
            .timeout(Duration::from_secs(300))
            .build()
            .expect("reqwest::Client::builder must not fail with default config");
        Self {
            http,
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for HttpInferenceHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        // 1. Read the input token from the configured port.
        let job_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "HttpInferenceHandler: missing input port '{}'",
                self.input_port
            ))
        })?;

        // 2. Read enrichment from effect_config. The pre-dispatch hook
        //    overlays base_url + lease_token via LWW while preserving the
        //    original scenario fields (required_model, system_prompt, …).
        let config = input.config.as_ref().ok_or_else(|| {
            EffectError::Fatal(
                "HttpInferenceHandler requires pre-dispatch enrichment in effect_config \
                 (missing entirely)"
                    .into(),
            )
        })?;

        let base_url = config
            .get("base_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal(
                    "HttpInferenceHandler requires base_url in enriched effect_config".into(),
                )
            })?
            .trim_end_matches('/');

        let lease_token = config
            .get("lease_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal(
                    "HttpInferenceHandler requires lease_token in enriched effect_config".into(),
                )
            })?;

        // Per feedback_no_default_model: fail closed if required_model absent.
        let required_model = config
            .get("required_model")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal(
                    "HttpInferenceHandler requires required_model in effect_config".into(),
                )
            })?;

        let system_prompt = config.get("system_prompt").and_then(|v| v.as_str());

        // 3. Build the inference request — task_kind-dispatched body-builder
        //    (#126.3). Replaces the DI-shape-hardcoded prompt at this site
        //    with a per-task_kind body shape; the prior session's Sections
        //    D/E/F (letter / patient_qa / context-cite / clinical_validation)
        //    failed under the hardcoded path because Chat-style scenarios
        //    were silently fed a DI-extraction prompt + empty images.
        //
        //    task_kind values clinic uses (per server/data/petri-nets/*.json
        //    inventory 2026-05-22): "Vision", "Chat", "Agent",
        //    "StructuredOutput", "Asr", "Embeddings". The first four route
        //    through `/v1/inference`; "Asr" and "Embeddings" run on different
        //    executor handlers (not this dispatch path) and surface as a hard
        //    Fatal here so misconfigured routing fails loudly.
        let task_kind = config
            .get("task_kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal(
                    "HttpInferenceHandler requires task_kind in effect_config".into(),
                )
            })?;

        let mut inference_body = match task_kind {
            "Vision" => build_vision_body(required_model, job_data),
            "Chat" | "Agent" | "StructuredOutput" => {
                build_chat_body(required_model, job_data, config)?
            }
            "Embeddings" | "Asr" => {
                return Err(EffectError::Fatal(format!(
                    "HttpInferenceHandler: task_kind '{task_kind}' is not served by /v1/inference; \
                     route to the appropriate executor handler"
                )));
            }
            other => {
                return Err(EffectError::Fatal(format!(
                    "HttpInferenceHandler: unrecognized task_kind '{other}'"
                )));
            }
        };

        if let Some(sp) = system_prompt {
            if let Some(obj) = inference_body.as_object_mut() {
                obj.insert("system_prompt".to_string(), JsonValue::String(sp.to_string()));
            }
        }

        // 4. HTTP-dispatch synchronously.
        let inference_url = format!("{}/v1/inference", base_url);
        let dispatch_result = self
            .http
            .post(&inference_url)
            .bearer_auth(lease_token)
            .json(&inference_body)
            .send()
            .await;

        let response_body = match dispatch_result {
            Ok(resp) if resp.status().is_success() => resp
                .json::<JsonValue>()
                .await
                .map_err(|e| EffectError::ExecutionFailed(format!("response JSON parse: {e}")))?,
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(EffectError::ExecutionFailed(format!(
                    "HTTP {status}: {body}"
                )));
            }
            Err(e) => {
                return Err(EffectError::ExecutionFailed(format!("HTTP transport: {e}")));
            }
        };

        // 5. Project response into EffectOutput.
        //    Merge inference response fields into the output token so
        //    downstream Rhai/transitions can read `output`, `model`, `usage`,
        //    `structured_output`, etc.
        let mut output_data = job_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            if let Some(resp_obj) = response_body.as_object() {
                for (k, v) in resp_obj {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            result: response_body,
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {}

    fn name(&self) -> &str {
        "http_inference"
    }

    fn port_schemas(&self) -> Option<EffectPortSchemas> {
        Some(EffectPortSchemas {
            inputs: HashMap::from([(
                self.input_port.clone(),
                "#/definitions/HttpInferenceInput".into(),
            )]),
            outputs: HashMap::from([(
                self.output_port.clone(),
                "#/definitions/HttpInferenceSubmitted".into(),
            )]),
        })
    }
}

/// Build a Vision-style inference body: extracts `file_b64` + `mime_type` +
/// `document_id` from the input token and authors the DI-extraction prompt.
/// This is the historical path; #126.3 preserves it verbatim and routes via
/// `task_kind == "Vision"`.
fn build_vision_body(required_model: &str, job_data: &JsonValue) -> JsonValue {
    let file_b64 = job_data.get("file_b64").and_then(|v| v.as_str());
    let mime_type = job_data
        .get("mime_type")
        .and_then(|v| v.as_str())
        .unwrap_or("image/png");
    let images: JsonValue = if let Some(b64) = file_b64 {
        serde_json::json!([{ "base64": b64, "mime_type": mime_type }])
    } else {
        serde_json::json!([])
    };

    let document_id = job_data
        .get("document_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let prompt = if document_id.is_empty() {
        "Extract structured fields from the attached image.".to_string()
    } else {
        format!(
            "Extract structured fields from the attached image. document_id={}",
            document_id
        )
    };

    serde_json::json!({
        "model": required_model,
        "prompt": prompt,
        "images": images,
    })
}

/// Build a Chat-style inference body for `Chat` / `Agent` / `StructuredOutput`
/// task_kinds. The user prompt is the JSON-serialized input token with
/// system fields (`_instance_id` / `_template_id` / `_template_version` /
/// `_created_at` / `_created_by` — injected by `parameterize_*`) stripped so
/// the LLM sees only the clinical-domain payload. For `Agent`, an optional
/// `tool_catalogue` in effect_config is forwarded into the request body as
/// `tools` — executor-llm passes it through to the underlying provider.
///
/// No images: Chat/Agent/StructuredOutput scenarios don't bear image inputs;
/// if a scenario does, it belongs on the Vision branch.
fn build_chat_body(
    required_model: &str,
    job_data: &JsonValue,
    config: &JsonValue,
) -> Result<JsonValue, EffectError> {
    let stripped = strip_system_fields(job_data);
    let user_prompt = serde_json::to_string(&stripped).map_err(|e| {
        EffectError::Fatal(format!(
            "HttpInferenceHandler: failed to serialize input token as prompt JSON: {e}"
        ))
    })?;
    if user_prompt.is_empty() || user_prompt == "{}" || user_prompt == "null" {
        return Err(EffectError::Fatal(
            "HttpInferenceHandler: chat-style task_kind requires a non-empty input token \
             (system fields stripped — token had no domain payload)"
                .into(),
        ));
    }

    let mut body = serde_json::json!({
        "model": required_model,
        "prompt": user_prompt,
    });
    if let Some(tools) = config.get("tool_catalogue") {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("tools".to_string(), tools.clone());
        }
    }
    Ok(body)
}

/// Strip the system fields `parameterize_air` / `parameterize_for_place`
/// inject into every seeded token. Keep this list in sync with those
/// functions in mekhan-service's `petri::instance` module.
fn strip_system_fields(token: &JsonValue) -> JsonValue {
    const SYSTEM_FIELDS: &[&str] = &[
        "_instance_id",
        "_template_id",
        "_template_version",
        "_created_at",
        "_created_by",
    ];
    match token {
        JsonValue::Object(map) => {
            let mut clean = serde_json::Map::new();
            for (k, v) in map {
                if !SYSTEM_FIELDS.contains(&k.as_str()) {
                    clean.insert(k.clone(), v.clone());
                }
            }
            JsonValue::Object(clean)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::TransitionId;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn make_input_with_config(
        port: &str,
        data: JsonValue,
        config: Option<JsonValue>,
    ) -> EffectInput {
        let mut inputs = HashMap::new();
        inputs.insert(port.to_string(), data);
        EffectInput {
            transition_id: TransitionId::new(),
            inputs,
            config,
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    /// Spawn a one-shot HTTP stub server.
    /// Returns the bound address.
    /// `captured_headers` receives lines from the request that start with "authorization:".
    /// `captured_body` receives the request body (after the header/body separator).
    fn spawn_stub_server(
        response_status: u16,
        response_body: &'static str,
        captured_headers: Arc<Mutex<Vec<String>>>,
        captured_body: Arc<Mutex<Option<String>>>,
    ) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = vec![0u8; 16384];
                let n = sock.read(&mut buf).unwrap_or(0);
                let raw = String::from_utf8_lossy(&buf[..n]).to_string();

                for line in raw.lines() {
                    if line.to_lowercase().starts_with("authorization:") {
                        captured_headers.lock().unwrap().push(line.to_string());
                    }
                }
                if let Some(pos) = raw.find("\r\n\r\n") {
                    *captured_body.lock().unwrap() = Some(raw[pos + 4..].to_string());
                }

                let reason = if response_status == 200 { "OK" } else { "Error" };
                let resp = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_status, reason, response_body.len(), response_body
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            }
        });
        addr
    }

    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_handler_rejects_missing_effect_config() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        let input = make_input_with_config("job", json!({"file_b64": "abc"}), None);
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(_)),
            "expected Fatal, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_handler_rejects_missing_base_url() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "lease_token": "tok",
            "required_model": "test-model-a",
        });
        let input = make_input_with_config("job", json!({}), Some(config));
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(ref msg) if msg.contains("base_url")),
            "expected Fatal with base_url mention, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_handler_rejects_missing_lease_token() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": "http://127.0.0.1:9999",
            "required_model": "test-model-a",
        });
        let input = make_input_with_config("job", json!({}), Some(config));
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(ref msg) if msg.contains("lease_token")),
            "expected Fatal with lease_token mention, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_handler_rejects_missing_required_model() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": "http://127.0.0.1:9999",
            "lease_token": "tok",
        });
        let input = make_input_with_config("job", json!({}), Some(config));
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(ref msg) if msg.contains("required_model")),
            "expected Fatal with required_model mention, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_handler_dispatches_with_bearer_auth() {
        let captured_headers = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured_body = Arc::new(Mutex::new(None::<String>));
        let canned = r#"{"output":"test result","model":"test-model-a","usage":{"input_tokens":10,"output_tokens":20}}"#;
        let addr = spawn_stub_server(200, canned, captured_headers.clone(), captured_body.clone());

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "Bearer-test-token-xyz",
            "required_model": "test-model-a",
            "task_kind": "Vision",
        });
        let token_data = json!({
            "file_b64": "aGVsbG8=",
            "mime_type": "image/png",
            "document_id": "doc-001",
        });
        let input = make_input_with_config("job", token_data, Some(config));
        let output = handler.execute(input).await.expect("handler must succeed");

        let headers = captured_headers.lock().unwrap();
        assert!(
            headers
                .iter()
                .any(|h| h.to_lowercase().contains("bearer-test-token-xyz")),
            "expected Bearer token in Authorization header, got: {:?}",
            headers
        );

        let submitted = output.tokens.get("submitted").expect("submitted port");
        assert_eq!(submitted["output"], "test result");
        assert_eq!(submitted["file_b64"], "aGVsbG8=");
        assert_eq!(submitted["document_id"], "doc-001");
    }

    #[tokio::test]
    async fn test_handler_propagates_pool_error_without_release_attempt() {
        // Pool returns 500 → ExecutionFailed. Lease release is intentionally
        // deferred to cap-routing TTL eviction (workstream #61); no release
        // POST should be attempted by the handler.
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_body = Arc::new(Mutex::new(None));
        let addr = spawn_stub_server(
            500,
            r#"{"error":"internal server error"}"#,
            captured_headers,
            captured_body,
        );

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "tok-error",
            "required_model": "test-model-b",
            "task_kind": "Vision",
        });
        let input = make_input_with_config("job", json!({"document_id": "x"}), Some(config));
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::ExecutionFailed(_)),
            "expected ExecutionFailed on pool error, got: {:?}",
            err
        );
        // Honest-absence: no release POST — handler defers release to TTL.
        // The stub only accepted one connection (the inference call); no second
        // connection means the release endpoint was never contacted.
    }

    #[tokio::test]
    async fn test_handler_uses_input_token_for_images_field() {
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_body = Arc::new(Mutex::new(None::<String>));
        let canned = r#"{"output":"ok","model":"test-model-a"}"#;
        let addr = spawn_stub_server(200, canned, captured_headers, captured_body.clone());

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "tok-img",
            "required_model": "test-model-a",
            "task_kind": "Vision",
        });
        let token = json!({
            "file_b64": "aW1hZ2VkYXRh",
            "mime_type": "image/jpeg",
            "document_id": "doc-img-001",
        });
        let input = make_input_with_config("job", token, Some(config));
        let _ = handler.execute(input).await.expect("must succeed");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let body_opt = captured_body.lock().unwrap().clone();
        let body_str = body_opt.expect("request body must have been captured");
        let parsed: JsonValue =
            serde_json::from_str(&body_str).expect("request body must be valid JSON");

        let images = parsed.get("images").expect("images field required");
        let first = images.get(0).expect("at least one image entry");
        assert_eq!(first["base64"], "aW1hZ2VkYXRh");
        assert_eq!(first["mime_type"], "image/jpeg");
    }

    // ── #126.3 task_kind dispatch tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_handler_rejects_missing_task_kind() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": "http://127.0.0.1:9999",
            "lease_token": "tok",
            "required_model": "test-model-a",
        });
        let input = make_input_with_config("job", json!({}), Some(config));
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(ref msg) if msg.contains("task_kind")),
            "expected Fatal with task_kind mention, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_handler_rejects_unrecognized_task_kind() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": "http://127.0.0.1:9999",
            "lease_token": "tok",
            "required_model": "test-model-a",
            "task_kind": "Telepathy",
        });
        let input = make_input_with_config("job", json!({"x": 1}), Some(config));
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(ref msg) if msg.contains("Telepathy")),
            "expected Fatal naming the bad task_kind, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_handler_rejects_embeddings_task_kind() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": "http://127.0.0.1:9999",
            "lease_token": "tok",
            "required_model": "test-model-a",
            "task_kind": "Embeddings",
        });
        let input = make_input_with_config("job", json!({"text": "embed me"}), Some(config));
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(ref msg)
                if msg.contains("Embeddings") && msg.contains("/v1/inference")),
            "expected Fatal flagging the routing mismatch, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_handler_chat_serializes_token_as_prompt_without_images() {
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_body = Arc::new(Mutex::new(None::<String>));
        let canned = r#"{"output":"letter generated","model":"test-model-a"}"#;
        let addr = spawn_stub_server(200, canned, captured_headers, captured_body.clone());

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "tok-chat",
            "required_model": "test-model-a",
            "task_kind": "Chat",
            "system_prompt": "You are a clinical assistant.",
        });
        // System fields (_instance_id, etc.) are normally injected by
        // `parameterize_*`; include them here to verify the handler strips
        // them before serializing into the user prompt.
        let token = json!({
            "letter_type": "discharge",
            "patient_context": { "id": "p-001", "name": "Test" },
            "_instance_id": "instance-abc",
            "_template_id": "template-xyz",
            "_template_version": 1,
            "_created_at": "2026-05-22T10:00:00Z",
            "_created_by": "user-001",
        });
        let input = make_input_with_config("job", token, Some(config));
        let _ = handler.execute(input).await.expect("Chat path must succeed");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let body_str = captured_body.lock().unwrap().clone().expect("captured body");
        let parsed: JsonValue = serde_json::from_str(&body_str).expect("valid JSON");

        // No images: Chat path doesn't bear image inputs.
        let images = parsed.get("images").and_then(|v| v.as_array());
        assert!(
            images.is_none() || images.unwrap().is_empty(),
            "Chat task_kind must not carry images, got: {:?}",
            images
        );

        // The user prompt is the JSON-serialized token with system fields
        // stripped — letter_type + patient_context survive; _instance_id etc.
        // are stripped.
        let prompt = parsed["prompt"].as_str().expect("prompt is a string");
        assert!(
            prompt.contains("letter_type") && prompt.contains("discharge"),
            "prompt must serialize the domain payload; got: {prompt}"
        );
        assert!(
            !prompt.contains("_instance_id") && !prompt.contains("_template_id"),
            "system fields must be stripped from the prompt; got: {prompt}"
        );
        assert_eq!(parsed["system_prompt"], "You are a clinical assistant.");
    }

    #[tokio::test]
    async fn test_handler_agent_forwards_tool_catalogue() {
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_body = Arc::new(Mutex::new(None::<String>));
        let canned = r#"{"output":"validated","model":"test-model-a"}"#;
        let addr = spawn_stub_server(200, canned, captured_headers, captured_body.clone());

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "tok-agent",
            "required_model": "test-model-a",
            "task_kind": "Agent",
            "tool_catalogue": [
                {
                    "name": "lookup_drug",
                    "description": "Look up drug information by name.",
                    "input_schema": { "type": "object", "properties": { "name": { "type": "string" } } }
                }
            ],
        });
        let token = json!({ "claims": [{ "claim": "patient has diabetes" }] });
        let input = make_input_with_config("job", token, Some(config));
        let _ = handler.execute(input).await.expect("Agent path must succeed");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let body_str = captured_body.lock().unwrap().clone().expect("captured body");
        let parsed: JsonValue = serde_json::from_str(&body_str).expect("valid JSON");

        let tools = parsed.get("tools").expect("tools field forwarded for Agent");
        let arr = tools.as_array().expect("tools is an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "lookup_drug");
        // Domain payload survives in the prompt.
        let prompt = parsed["prompt"].as_str().expect("prompt is a string");
        assert!(prompt.contains("diabetes"), "got: {prompt}");
    }

    #[tokio::test]
    async fn test_handler_chat_rejects_empty_domain_token() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": "http://127.0.0.1:9999",
            "lease_token": "tok",
            "required_model": "test-model-a",
            "task_kind": "Chat",
        });
        // Only system fields present — domain payload is empty.
        let token = json!({
            "_instance_id": "x",
            "_template_id": "y",
            "_template_version": 1,
            "_created_at": "z",
            "_created_by": "u",
        });
        let input = make_input_with_config("job", token, Some(config));
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(ref msg) if msg.contains("non-empty")),
            "expected Fatal flagging empty domain token, got: {:?}",
            err
        );
    }

    #[test]
    fn test_name_returns_http_inference() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        assert_eq!(handler.name(), "http_inference");
    }

    #[test]
    fn test_port_schemas_declared() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        let schemas = handler.port_schemas().expect("port_schemas must be Some");
        assert!(
            schemas.inputs.contains_key("job"),
            "expected input port 'job' in schemas"
        );
        assert!(
            schemas.outputs.contains_key("submitted"),
            "expected output port 'submitted' in schemas"
        );
    }
}
