//! HTTP-sync `EffectHandler` implementation for cap-routed dispatch over the
//! generic `/v1/execute` pool surface.
//!
//! Sub-phase 2.3b — implements `HttpInferenceHandler`, the cloud-layer path
//! that replaces NATS-async dispatch when cap-routing's `HttpPreDispatchHook`
//! has enriched `EffectInput.config` with `base_url` + `lease_token` +
//! `backend`.
//!
//! ## Generic `/v1/execute` dispatch (Stage 3)
//!
//! The handler is task-agnostic: it forwards the enriched `effect_config` and
//! the (system-field-stripped) input token to the routed pool's
//! `POST /v1/execute` endpoint and projects the pool's canonical `outputs`
//! map back onto the output token. ALL body-shaping — Vision image turns,
//! Chat/Agent prompt serialization, OCR request mapping — now lives POOL-SIDE
//! (`executor-llm::execute_handler`, `executor-surya::execute_handler`,
//! Stage 2). The engine no longer dispatches on `task_kind`; it passes
//! `task_kind` through on the wire so the pool can shape the call.
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
//! - `backend: String` — pool backend selector (e.g. `"llm"`, `"surya"`),
//!   forwarded as [`ExecuteRequest::backend`]
//! - `task_kind: String` — task discriminator the pool dispatches on
//! - `pool_id: String` — cap-routing pool identifier
//! - `hardware_kind: Option<String>` — present when cap-routing knows it
//!
//! The original scenario fields (`required_model`, `system_prompt`, etc.) are
//! preserved via LWW merge and are also readable from `EffectInput.config`.
//!
//! ## No-default-model stays enforced AT THE POOL
//!
//! `required_model` is read as an `Option<String>` and forwarded as
//! [`ExecuteRequest::model`]. The LLM pool 400s on an absent/empty model
//! (`feedback_no_default_model`), so the engine can carry it as an `Option`
//! without weakening the rule — surya and other model-free backends are not
//! forced to invent a model.
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
use crate::execute_contract::{ExecuteRequest, ExecuteResponse};

/// HTTP-sync generic `/v1/execute` dispatch handler.
///
/// Reads cap-routing pre-dispatch enrichment (`base_url`, `lease_token`,
/// `backend`, `task_kind`) from `EffectInput.config`, builds a generic
/// [`ExecuteRequest`] from the enriched config + the (stripped) input token,
/// and POSTs to `{base_url}/v1/execute` — the shared generic surface every
/// executor pool serves (`executor-llm` / `executor-surya`). Body-shaping
/// lives pool-side; this handler is task-agnostic.
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

        // `backend` selects the pool family (e.g. "llm", "surya"); cap-routing
        // injects it as `pool_backend` → `backend`. Fail closed if absent: a
        // missing backend means cap-routing enrichment is malformed.
        let backend = config
            .get("backend")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal(
                    "HttpInferenceHandler requires backend in enriched effect_config".into(),
                )
            })?
            .to_string();

        // `task_kind` is forwarded on the wire; the POOL dispatches on it to
        // shape the call. The engine no longer matches on it.
        let task_kind = config
            .get("task_kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal(
                    "HttpInferenceHandler requires task_kind in effect_config".into(),
                )
            })?
            .to_string();

        // `model` is OPTIONAL on the engine side — the no-default-model rule is
        // enforced AT THE POOL (the LLM pool 400s on absent/empty model). Read
        // it as Option<String> and pass it through; model-free backends (surya)
        // simply omit it.
        let model = config
            .get("required_model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // 3. Build the generic ExecuteRequest. Body-shaping lives pool-side:
        //    we forward the enriched effect_config as `config` and the
        //    system-field-stripped input token as `input`. The pool reshapes
        //    these into a provider call keyed on `task_kind`.
        let input_payload = strip_system_fields(job_data);

        let request = ExecuteRequest {
            backend,
            task_kind,
            model,
            config: config.clone(),
            input: input_payload,
        };

        // 4. HTTP-dispatch synchronously to the generic /v1/execute surface.
        let execute_url = format!("{}/v1/execute", base_url);
        let dispatch_result = self
            .http
            .post(&execute_url)
            .bearer_auth(lease_token)
            .json(&request)
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

        // 5. Parse the generic ExecuteResponse and project it into EffectOutput.
        //    The pool's canonical `outputs` map is nested under the output
        //    token's `detail.outputs` — clinic Rhai's `outputs_of(tok)` reads
        //    `tok.detail.outputs` FIRST. The input-token root fields are kept
        //    for back-compat (downstream transitions that still read root keys).
        let execute_response: ExecuteResponse = serde_json::from_value(response_body.clone())
            .map_err(|e| {
                EffectError::ExecutionFailed(format!(
                    "ExecuteResponse parse: {e} (body: {response_body})"
                ))
            })?;

        let mut output_data = job_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            let detail = obj
                .entry("detail".to_string())
                .or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
            // `detail` may have been carried on the input token as a non-object;
            // coerce to an object so we can nest `outputs` deterministically.
            if !detail.is_object() {
                *detail = JsonValue::Object(serde_json::Map::new());
            }
            if let Some(detail_obj) = detail.as_object_mut() {
                detail_obj.insert(
                    "outputs".to_string(),
                    JsonValue::Object(execute_response.outputs.clone()),
                );
            }
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            // The raw ExecuteResponse json is the opaque replay payload.
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

/// No-op `executor_cancel` handler for HTTP-sync dispatch.
///
/// In HTTP-sync mode every `executor_submit` is a synchronous HTTP round-trip
/// that has already completed by the time the effect returns, so there is no
/// async executor job to cancel. But the graph→AIR compiler emits an
/// `executor_cancel` transition for every executor step, and the engine's
/// deploy validation requires every referenced effect handler to be
/// registered. This handler satisfies that contract: on the (rare) cancel
/// path it acks immediately and makes no remote call. Registered alongside
/// [`HttpInferenceHandler`] whenever HTTP-dispatch is configured.
pub struct HttpExecutorCancelNoop {
    output_port: String,
}

impl HttpExecutorCancelNoop {
    pub fn new(output_port: impl Into<String>) -> Self {
        Self {
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for HttpExecutorCancelNoop {
    async fn execute(&self, _input: EffectInput) -> Result<EffectOutput, EffectError> {
        // Nothing to cancel under synchronous HTTP dispatch — ack and proceed.
        let mut tokens = HashMap::new();
        tokens.insert(
            self.output_port.clone(),
            serde_json::json!({ "cancelled": true, "mode": "http_sync_noop" }),
        );
        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({ "cancelled": "http_sync_noop" }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {}

    fn name(&self) -> &str {
        "http_executor_cancel_noop"
    }
}

/// Strip the system fields `parameterize_air` / `parameterize_for_place`
/// inject into every seeded token. Keep this list in sync with those
/// functions in mekhan-service's `petri::instance` module.
///
/// The stripped token becomes [`ExecuteRequest::input`]; pools that need the
/// raw domain payload (Chat/Agent prompt serialization, Vision image fields)
/// read it from there. Body-shaping lives pool-side (Stage 2).
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
    async fn test_handler_rejects_missing_backend() {
        // `backend` (cap-routing's pool_backend) is required enrichment;
        // its absence means the enriched effect_config is malformed.
        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": "http://127.0.0.1:9999",
            "lease_token": "tok",
            "task_kind": "Chat",
            "required_model": "test-model-a",
        });
        let input = make_input_with_config("job", json!({"x": 1}), Some(config));
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::Fatal(ref msg) if msg.contains("backend")),
            "expected Fatal with backend mention, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_handler_model_is_optional_no_default_enforced_at_pool() {
        // The engine no longer fails closed on a missing model — the pool does
        // (feedback_no_default_model). With no `required_model` in config, the
        // handler must still dispatch, omitting `model` from the wire so the
        // pool can apply its own fail-closed policy.
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_body = Arc::new(Mutex::new(None::<String>));
        let canned = r#"{"outputs":{"full_text":"ocr text"}}"#;
        let addr = spawn_stub_server(200, canned, captured_headers, captured_body.clone());

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "tok-surya",
            "backend": "surya",
            "task_kind": "Ocr",
            // no required_model — surya is model-free
        });
        let input = make_input_with_config("job", json!({"file_b64": "abc"}), Some(config));
        let _ = handler.execute(input).await.expect("model-free dispatch must succeed");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let body_str = captured_body.lock().unwrap().clone().expect("captured body");
        let parsed: JsonValue = serde_json::from_str(&body_str).expect("valid JSON");
        assert!(
            parsed.get("model").is_none(),
            "None model must be omitted from the wire, got: {:?}",
            parsed.get("model")
        );
        assert_eq!(parsed["backend"], "surya");
        assert_eq!(parsed["task_kind"], "Ocr");
    }

    #[tokio::test]
    async fn test_handler_dispatches_generic_execute_request_with_bearer_auth() {
        // Asserts the generic /v1/execute wire shape: ExecuteRequest fields
        // (backend, task_kind, model, config, input) on the POST body, bearer
        // lease forwarded, and the URL ending in /v1/execute (the stub captures
        // the request line implicitly via a single accepted connection).
        let captured_headers = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured_body = Arc::new(Mutex::new(None::<String>));
        let canned = r#"{"outputs":{"output":"test result","model":"test-model-a","usage":{"prompt_tokens":10,"completion_tokens":20}}}"#;
        let addr = spawn_stub_server(200, canned, captured_headers.clone(), captured_body.clone());

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "Bearer-test-token-xyz",
            "backend": "llm",
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

        // Generic ExecuteRequest wire shape.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let body_str = captured_body.lock().unwrap().clone().expect("captured body");
        let parsed: JsonValue = serde_json::from_str(&body_str).expect("valid JSON");
        assert_eq!(parsed["backend"], "llm");
        assert_eq!(parsed["task_kind"], "Vision");
        assert_eq!(parsed["model"], "test-model-a");
        // `config` carries the enriched effect_config; `input` the stripped token.
        assert_eq!(parsed["config"]["task_kind"], "Vision");
        assert_eq!(parsed["input"]["file_b64"], "aGVsbG8=");
        assert_eq!(parsed["input"]["document_id"], "doc-001");
        // Parseable as the typed ExecuteRequest (cross-workspace contract).
        let typed: ExecuteRequest =
            serde_json::from_str(&body_str).expect("body must parse as ExecuteRequest");
        assert_eq!(typed.backend, "llm");
        assert_eq!(typed.model.as_deref(), Some("test-model-a"));

        // ExecuteResponse.outputs lands under detail.outputs on the output token.
        let submitted = output.tokens.get("submitted").expect("submitted port");
        assert_eq!(submitted["detail"]["outputs"]["output"], "test result");
        assert_eq!(submitted["detail"]["outputs"]["model"], "test-model-a");
        // Input-token root fields preserved for back-compat.
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
            "backend": "llm",
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
        // The stub only accepted one connection (the execute call); no second
        // connection means the release endpoint was never contacted.
    }

    #[tokio::test]
    async fn test_handler_forwards_stripped_input_token_to_pool() {
        // The engine forwards the system-field-stripped input token as
        // ExecuteRequest.input; pool-side body-shaping reads file_b64/mime_type
        // from there. The engine itself no longer builds an `images` array.
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_body = Arc::new(Mutex::new(None::<String>));
        let canned = r#"{"outputs":{"output":"ok","model":"test-model-a"}}"#;
        let addr = spawn_stub_server(200, canned, captured_headers, captured_body.clone());

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "tok-img",
            "backend": "llm",
            "required_model": "test-model-a",
            "task_kind": "Vision",
        });
        let token = json!({
            "file_b64": "aW1hZ2VkYXRh",
            "mime_type": "image/jpeg",
            "document_id": "doc-img-001",
            "_instance_id": "instance-strip-me",
        });
        let input = make_input_with_config("job", token, Some(config));
        let _ = handler.execute(input).await.expect("must succeed");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let body_opt = captured_body.lock().unwrap().clone();
        let body_str = body_opt.expect("request body must have been captured");
        let parsed: JsonValue =
            serde_json::from_str(&body_str).expect("request body must be valid JSON");

        // The raw token fields land under `input` (system fields stripped); the
        // engine emits no `images` array — that is now pool-side body-shaping.
        assert_eq!(parsed["input"]["file_b64"], "aW1hZ2VkYXRh");
        assert_eq!(parsed["input"]["mime_type"], "image/jpeg");
        assert!(
            parsed["input"].get("_instance_id").is_none(),
            "system fields must be stripped from the input payload"
        );
        assert!(
            parsed.get("images").is_none(),
            "engine no longer builds an images array; body-shaping is pool-side"
        );
    }

    // ── generic /v1/execute dispatch tests (Stage 3) ─────────────────────────

    #[tokio::test]
    async fn test_handler_rejects_missing_task_kind() {
        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": "http://127.0.0.1:9999",
            "lease_token": "tok",
            "backend": "llm",
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
    async fn test_handler_forwards_config_and_task_kind_verbatim() {
        // The engine no longer dispatches on task_kind, nor interprets
        // system_prompt / tool_catalogue — it forwards the whole enriched
        // config (and an arbitrary task_kind) to the pool, which shapes the
        // call. An exotic task_kind is NOT rejected engine-side.
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_body = Arc::new(Mutex::new(None::<String>));
        let canned = r#"{"outputs":{"output":"validated"}}"#;
        let addr = spawn_stub_server(200, canned, captured_headers, captured_body.clone());

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "tok-agent",
            "backend": "llm",
            "required_model": "test-model-a",
            "task_kind": "Agent",
            "system_prompt": "You are a clinical assistant.",
            "tool_catalogue": [
                { "name": "lookup_drug", "description": "Look up a drug." }
            ],
        });
        let token = json!({
            "claims": [{ "claim": "patient has diabetes" }],
            "_template_id": "strip-me",
        });
        let input = make_input_with_config("job", token, Some(config));
        let _ = handler.execute(input).await.expect("dispatch must succeed");

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let body_str = captured_body.lock().unwrap().clone().expect("captured body");
        let parsed: JsonValue = serde_json::from_str(&body_str).expect("valid JSON");

        // task_kind passes through on the wire untouched.
        assert_eq!(parsed["task_kind"], "Agent");
        // The enriched config is forwarded verbatim under `config` — the pool
        // reads system_prompt / tool_catalogue from there.
        assert_eq!(parsed["config"]["system_prompt"], "You are a clinical assistant.");
        assert_eq!(parsed["config"]["tool_catalogue"][0]["name"], "lookup_drug");
        // Domain payload survives under `input`; system fields stripped.
        assert_eq!(parsed["input"]["claims"][0]["claim"], "patient has diabetes");
        assert!(parsed["input"].get("_template_id").is_none());
    }

    #[tokio::test]
    async fn test_handler_nests_outputs_under_detail_outputs() {
        // The core projection contract: ExecuteResponse.outputs lands under the
        // output token's detail.outputs (clinic Rhai's outputs_of reads
        // tok.detail.outputs first), while input-token root fields survive.
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_body = Arc::new(Mutex::new(None::<String>));
        let canned = r#"{"outputs":{"full_text":"OCR text","words":[{"text":"hello"}],"page_count":1,"engine":"surya"}}"#;
        let addr = spawn_stub_server(200, canned, captured_headers, captured_body);

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "tok-ocr",
            "backend": "surya",
            "task_kind": "Ocr",
        });
        let token = json!({ "document_id": "doc-ocr-1", "file_b64": "abc" });
        let input = make_input_with_config("job", token, Some(config));
        let output = handler.execute(input).await.expect("must succeed");

        let submitted = output.tokens.get("submitted").expect("submitted port");
        // outputs nested under detail.outputs by canonical key.
        assert_eq!(submitted["detail"]["outputs"]["full_text"], "OCR text");
        assert_eq!(submitted["detail"]["outputs"]["words"][0]["text"], "hello");
        assert_eq!(submitted["detail"]["outputs"]["page_count"], 1);
        assert_eq!(submitted["detail"]["outputs"]["engine"], "surya");
        // Back-compat: input-token root fields preserved.
        assert_eq!(submitted["document_id"], "doc-ocr-1");
        assert_eq!(submitted["file_b64"], "abc");

        // EffectOutput.result is the raw ExecuteResponse json.
        assert_eq!(output.result["outputs"]["full_text"], "OCR text");
    }

    #[tokio::test]
    async fn test_handler_rejects_non_execute_response_shape() {
        // A pool reply that isn't an ExecuteResponse (no `outputs` map) must
        // surface as ExecutionFailed — guards against a pool returning the old
        // flat /v1/inference shape on the generic surface.
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_body = Arc::new(Mutex::new(None));
        let canned = r#"{"output":"flat shape","model":"m"}"#;
        let addr = spawn_stub_server(200, canned, captured_headers, captured_body);

        let handler = HttpInferenceHandler::new("job", "submitted");
        let config = json!({
            "base_url": format!("http://{}", addr),
            "lease_token": "tok",
            "backend": "llm",
            "required_model": "test-model-a",
            "task_kind": "Chat",
        });
        let input = make_input_with_config("job", json!({"x": 1}), Some(config));
        let err = handler.execute(input).await.unwrap_err();
        assert!(
            matches!(err, EffectError::ExecutionFailed(ref msg) if msg.contains("ExecuteResponse")),
            "expected ExecutionFailed flagging ExecuteResponse parse, got: {:?}",
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
