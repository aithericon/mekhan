use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use aithericon_executor_backend::outputs::{
    fill_missing_declared, unpack_by_name, MissingOutputFallback,
};
use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    LogEntry, LogLevel, LogSummary, MetricSummary, RunContext,
};

use aithericon_executor_backend_configs::llm::ResolvedOpenAiResource;

use crate::adapters;
use crate::config::{ImageInput, LlmConfig};
use crate::port::{
    CompletionPort, CompletionRequest, CompletionResponse, ImageData, LlmError, ResponseFormat,
};

/// Bounded retry for a TRANSIENT router error (`LlmError::Retryable`: 503 no-live-
/// replica while the pool scales from zero, or 429 all-replicas-saturated). The
/// model-pool autoscaler reacts to the router's demand signal and brings a replica
/// up within its reconcile (≤15s) + cold-load window, so riding it out here keeps a
/// SINGLE execution alive instead of fast-failing and forcing an engine resubmit.
/// Exponential backoff (1s, 2s, 4s, …) capped per-sleep; the step timeout is the
/// hard outer bound (these retries race it in the execute `select!`).
const TRANSIENT_RETRY_MAX_ATTEMPTS: u32 = 8;
const TRANSIENT_RETRY_BASE_MS: u64 = 1_000;
const TRANSIENT_RETRY_CAP_MS: u64 = 15_000;

/// Call the adapter, retrying `LlmError::Retryable` with bounded exponential
/// backoff. A non-retryable error (or success) returns immediately; exhausting the
/// retry budget downgrades the last transient error to `LlmError::Api` so it
/// surfaces as a normal backend error rather than looping forever.
async fn complete_with_retry(
    adapter: &std::sync::Arc<dyn CompletionPort>,
    request: &CompletionRequest,
    env: &HashMap<String, String>,
) -> Result<CompletionResponse, LlmError> {
    let mut attempt: u32 = 0;
    loop {
        match adapter.complete(request, env).await {
            Err(LlmError::Retryable(msg)) if attempt < TRANSIENT_RETRY_MAX_ATTEMPTS => {
                let backoff_ms =
                    (TRANSIENT_RETRY_BASE_MS << attempt.min(6)).min(TRANSIENT_RETRY_CAP_MS);
                attempt += 1;
                warn!(
                    attempt,
                    backoff_ms,
                    model = %request.model,
                    "LLM transient error (pool scaling from zero / saturated); retrying: {msg}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            }
            Err(LlmError::Retryable(msg)) => return Err(LlmError::Api(msg)),
            other => return other,
        }
    }
}

/// Backend that executes LLM completions via direct HTTP calls — no rig dependency.
pub struct LlmBackend;

impl LlmBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LlmBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionBackend for LlmBackend {
    async fn prepare(
        &self,
        _job: &ExecutionJob,
        mut run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // Start from the secret-resolved overlay when PlanSecretsHook has
        // populated it (e.g. `api_key: {{secret:openai-api-key#login}}` from
        // an `openai` resource), otherwise fall back to `spec.config`. We
        // MUST NOT read `spec.config` directly — it still carries the
        // unresolved `{{secret:...}}` templates and would feed them straight
        // into the provider adapter as a Bearer token.
        let mut raw_config = run_context
            .resolved_config
            .clone()
            .unwrap_or_else(|| run_context.spec.config.clone());
        aithericon_executor_backend::resolve::resolve_inputs(
            &mut raw_config,
            &run_context.staged_inputs,
        )
        .map_err(|e| ExecutorError::Config(format!("llm input resolution: {e}")))?;

        // Deserialize resolved config
        let mut config: LlmConfig = serde_json::from_value(raw_config)
            .map_err(|e| ExecutorError::Config(format!("invalid llm backend config: {e}")))?;

        // Resource binding: when the step is bound to a workspace resource
        // (e.g. `openai_prod`), the compiler stages `<alias>.json` into the
        // run dir's inputs via a ResourceEnvelope borrow. PlanSecretsHook
        // resolves any `{{secret:...}}` refs in that file at staging time, so
        // it carries plaintext credentials by the time we read it. Per-step
        // overrides (set in the LLM panel) WIN over resource values — that
        // matches how SMTP treats `from` against `from_address`.
        if let Some(alias) = config.resource_alias.clone() {
            if !alias.is_empty() {
                overlay_resource(&mut config, &alias, &run_context)?;
            }
        }

        // Validate: model must be non-empty
        if config.model.is_empty() {
            return Err(ExecutorError::Config("model must not be empty".into()));
        }

        // Validate: json_schema response_format requires a schema
        if let Some(ResponseFormat::JsonSchema { ref schema }) = config.response_format {
            if schema.is_null() {
                return Err(ExecutorError::Config(
                    "json_schema response_format requires a non-null output_schema".into(),
                ));
            }
        }

        run_context.backend_state = serde_json::to_value(&config)
            .map_err(|e| ExecutorError::Config(format!("failed to serialize llm config: {e}")))?;
        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        event_stream: Option<std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let config: LlmConfig = serde_json::from_value(run_context.backend_state.clone())
            .map_err(|e| ExecutorError::Config(format!("failed to deserialize llm config: {e}")))?;

        let adapter = adapters::adapter_for(&config.provider);
        let start = tokio::time::Instant::now();

        // Report Running status with provider/model info
        status_cb(
            ExecutionStatus::Running,
            serde_json::json!({
                "provider": adapter.name(),
                "model": config.model,
            }),
        )
        .await;

        // Build the provider-agnostic request
        let mut request = CompletionRequest::from_config(&config);

        // Load and attach images to the last (user) message
        if !config.images.is_empty() {
            let images = load_images(&config.images)?;
            if let Some(last_msg) = request.messages.last_mut() {
                last_msg.images = images;
            }
        }

        // Dispatch log — flows through the same NATS subject the IPC
        // sidecar uses for child-process SDK logs, so it lands in
        // mekhan's hpi_logs as a per-message entry rather than only
        // appearing in the end-of-execution LogSummary count.
        if let Some(ref es) = event_stream {
            es.log(
                LogLevel::Info,
                format!(
                    "dispatching LLM request to {}/{}",
                    adapter.name(),
                    config.model
                ),
                HashMap::from([
                    ("provider".into(), adapter.name().to_string()),
                    ("model".into(), config.model.clone()),
                    ("image_count".into(), config.images.len().to_string()),
                ]),
            )
            .await;
        }

        // Merge config-level api_key and base_url into env so adapters can
        // find them alongside process-level environment variables.
        //
        // Overlay `resolved_env` on top of `env`: any env entry that carried
        // a `{{secret:KEY}}` template has its plaintext in `resolved_env`
        // courtesy of `PlanSecretsHook`. Without this overlay, an env-routed
        // `OPENAI_API_KEY={{secret:...}}` would reach the adapter as the
        // literal template string (and produce a 401 from the provider).
        let mut env = run_context.env.clone();
        for (k, v) in &run_context.resolved_env {
            env.insert(k.clone(), v.clone());
        }
        if let Some(ref api_key) = config.api_key {
            // Set the provider-specific env var key
            let env_key = match config.provider {
                crate::config::Provider::OpenAi => "OPENAI_API_KEY",
                crate::config::Provider::Anthropic => "ANTHROPIC_API_KEY",
                crate::config::Provider::Ollama => "OLLAMA_API_KEY",
            };
            env.entry(env_key.to_string())
                .or_insert_with(|| api_key.clone());
        }
        if let Some(ref base_url) = config.base_url {
            let env_key = match config.provider {
                crate::config::Provider::OpenAi => "OPENAI_BASE_URL",
                crate::config::Provider::Anthropic => "ANTHROPIC_BASE_URL",
                crate::config::Provider::Ollama => "OLLAMA_API_BASE_URL",
            };
            env.entry(env_key.to_string())
                .or_insert_with(|| base_url.clone());
        }

        // Stamp inference-attribution identity into env under well-known
        // keys so the OpenAI-compatible adapter can forward them as
        // `X-Instance-Id` / `X-Step-Id` request headers WITHOUT a
        // signature change. The router (`internal_llm` pool) reads those
        // headers into its MeterContext so each inference call in the audit
        // ledger is attributable to the workflow instance + step that
        // issued it. The engine stamps the running net id + place id into
        // `run_context.metadata` under `petri_net_id` / `petri_place`
        // (see scheduler-bridge `meta.rs`). A request id is optional — the
        // router synthesizes one when the header is absent.
        if let Some(v) = run_context.metadata.get("petri_net_id") {
            env.entry("__inference_instance_id".into())
                .or_insert_with(|| v.clone());
        }
        if let Some(v) = run_context.metadata.get("petri_place") {
            env.entry("__inference_step_id".into())
                .or_insert_with(|| v.clone());
        }

        // Three-way select: cancellation, timeout, or LLM execution
        tokio::select! { biased;
            _ = cancel.cancelled() => {
                Ok(ExecutionResult::cancelled(
                    start.elapsed(),
                    Some(run_context.run_dir.clone()),
                    Some("execution cancelled".into()),
                    None,
                ))
            },
            _ = tokio::time::sleep(run_context.timeout) => {
                Ok(ExecutionResult::timed_out(
                    start.elapsed(),
                    Some(run_context.run_dir.clone()),
                    Some(format!("timed out after {:?}", run_context.timeout)),
                    None,
                ))
            },
            result = complete_with_retry(&adapter, &request, &env) => {
                let duration = start.elapsed();
                match result {
                    Ok(resp) => {
                        // Always populate metrics (unlike rig which only had them in extract mode)
                        let metrics = Some(MetricSummary {
                            total_points: 3,
                            metric_names: vec![
                                "llm/input_tokens".into(),
                                "llm/output_tokens".into(),
                                "llm/total_tokens".into(),
                            ],
                            latest_values: HashMap::from([
                                ("llm/input_tokens".into(), resp.usage.input_tokens as f64),
                                ("llm/output_tokens".into(), resp.usage.output_tokens as f64),
                                ("llm/total_tokens".into(), resp.usage.total_tokens as f64),
                            ]),
                        });

                        // Build response value and stdout_tail
                        let (response_value, stdout_tail) = if let Some(ref extracted) = resp.structured_output {
                            let pretty = serde_json::to_string_pretty(extracted)
                                .unwrap_or_else(|_| extracted.to_string());
                            (extracted.clone(), pretty)
                        } else {
                            (serde_json::json!(resp.content), resp.content.clone())
                        };

                        let mut outputs = HashMap::from([
                            ("response".into(), response_value.clone()),
                            ("usage".into(), serde_json::json!({
                                "input_tokens": resp.usage.input_tokens,
                                "output_tokens": resp.usage.output_tokens,
                                "total_tokens": resp.usage.total_tokens,
                            })),
                            ("finish_reason".into(), serde_json::json!(resp.stop_reason.to_string())),
                            ("model".into(), serde_json::json!(resp.model)),
                        ]);

                        // Agent path: when the LLM may have called a tool,
                        // surface a normalized turn_result envelope and
                        // (in tool_use stop branches) the raw tool_calls
                        // array. The agent compiler's `t_route` transition
                        // reads `response.tool_calls[0].name` to decide
                        // which dispatch place to deposit to; in the final
                        // branch it reads `response.content`. Single-shot
                        // LLM AutomatedSteps never hit this — the compiler
                        // never declares tools for them, so `request.tools`
                        // is empty and the adapter returns empty tool_calls.
                        if !request.tools.is_empty() {
                            let turn_result = aithericon_executor_domain::LlmTurnResult {
                                content: if resp.content.is_empty() {
                                    None
                                } else {
                                    Some(resp.content.clone())
                                },
                                tool_calls: resp.tool_calls.clone(),
                                stop_reason: resp.stop_reason.clone(),
                                usage: resp.usage.clone(),
                            };
                            outputs.insert(
                                "turn_result".into(),
                                serde_json::to_value(&turn_result).unwrap_or_default(),
                            );

                            // Emit a per-turn event so the UI can render
                            // each tool call as it happens. Gated by the
                            // job's `stream_events` set: non-agent LLM
                            // jobs don't include `AgentTurn` in stream_events
                            // and the event drops silently. v1 reports
                            // `turn: 0` — runtime turn-threading lands when
                            // the engine wires p_state into config_ref
                            // (docs/12 § 3.4 follow-up).
                            if let Some(ref es) = event_stream {
                                es.agent_turn(
                                    0,
                                    resp.stop_reason.clone(),
                                    turn_result.content.clone(),
                                    resp.tool_calls.clone(),
                                    resp.usage.clone(),
                                )
                                .await;
                            }
                        }

                        // Per-key unpack: when the LLM returned a structured-JSON
                        // object and the spec declares multiple output fields,
                        // map each top-level key to the matching port by name —
                        // mirrors the Python backend's name-based output sweep.
                        // Declared port names WIN over built-ins (response/usage/
                        // finish_reason/model) so workflow authors don't have to
                        // dodge vendor-side reserved names.
                        if let Some(ref extracted) = resp.structured_output {
                            unpack_by_name(&mut outputs, &run_context.spec.outputs, extracted);
                        }

                        // Fallback for declared outputs the per-key unpack
                        // didn't fill: REQUIRED ports get the whole
                        // `response_value` (keeps single-output / free-text
                        // shapes working — a `prompt: "summarize"` step with
                        // one declared `response` field would otherwise emit
                        // `outputs = {}` and the required check would fail);
                        // OPTIONAL ports get `null` (the LLM legitimately
                        // omits them when response_format marks them
                        // omittable — Python-idiomatic, schemas allow null on
                        // declared scalars).
                        fill_missing_declared(
                            &mut outputs,
                            &run_context.spec.outputs,
                            MissingOutputFallback::RequiredOrNull(&response_value),
                        );

                        // Write to expected_outputs file paths
                        for (name, path) in &run_context.expected_outputs {
                            if !path.exists() {
                                let content = if let Some(val) = outputs.get(name) {
                                    serde_json::to_string_pretty(val).unwrap_or_default()
                                } else {
                                    serde_json::to_string_pretty(&response_value).unwrap_or_default()
                                };
                                if let Err(e) = std::fs::write(path, content) {
                                    warn!(output = %name, "failed to write output file: {e}");
                                }
                            }
                        }

                        // Completion log — counterpart to the dispatch log
                        // above. Surfaces timing + token usage as a real
                        // hpi_logs entry, not just the LogSummary count.
                        if let Some(ref es) = event_stream {
                            es.log(
                                LogLevel::Info,
                                format!(
                                    "LLM response received from {} in {}ms ({} input + {} output tokens)",
                                    resp.model,
                                    duration.as_millis(),
                                    resp.usage.input_tokens,
                                    resp.usage.output_tokens,
                                ),
                                HashMap::from([
                                    ("provider".into(), adapter.name().to_string()),
                                    ("model".into(), resp.model.clone()),
                                    ("duration_ms".into(), duration.as_millis().to_string()),
                                    ("input_tokens".into(), resp.usage.input_tokens.to_string()),
                                    ("output_tokens".into(), resp.usage.output_tokens.to_string()),
                                    ("total_tokens".into(), resp.usage.total_tokens.to_string()),
                                    ("finish_reason".into(), resp.stop_reason.to_string()),
                                ]),
                            )
                            .await;
                        }

                        let logs = Some(LogSummary {
                            total_entries: 2,
                            count_by_level: HashMap::from([("info".into(), 2)]),
                            recent_errors: vec![],
                            dropped_count: 0,
                        });

                        Ok(ExecutionResult {
                            outcome: ExecutionOutcome::Success,
                            duration,
                            stdout_tail: Some(stdout_tail),
                            stderr_tail: None,
                            artifact_manifest: None,
                            outputs,
                            progress: None,
                            run_dir: Some(run_context.run_dir.clone()),
                            metrics,
                            logs,
                        })
                    },
                    Err(e) => {
                        let error_entry = LogEntry {
                            level: LogLevel::Error,
                            message: format!("LLM execution failed: {e}"),
                            timestamp: Utc::now(),
                            fields: HashMap::from([
                                ("provider".into(), adapter.name().to_string()),
                                ("model".into(), config.model.clone()),
                            ]),
                            repeat_count: 1,
                        };

                        // Per-message error log so the failure shows up in
                        // hpi_logs alongside the dispatch entry, not only
                        // in the failed-execution summary.
                        if let Some(ref es) = event_stream {
                            es.log(
                                LogLevel::Error,
                                format!("LLM execution failed: {e}"),
                                HashMap::from([
                                    ("provider".into(), adapter.name().to_string()),
                                    ("model".into(), config.model.clone()),
                                    ("duration_ms".into(), duration.as_millis().to_string()),
                                ]),
                            )
                            .await;
                        }

                        let logs = Some(LogSummary {
                            total_entries: 2,
                            count_by_level: HashMap::from([
                                ("info".into(), 1),
                                ("error".into(), 1),
                            ]),
                            recent_errors: vec![error_entry],
                            dropped_count: 0,
                        });

                        Ok(ExecutionResult {
                            outcome: ExecutionOutcome::BackendError { message: e.to_string() },
                            duration,
                            stdout_tail: None,
                            stderr_tail: Some(e.to_string()),
                            artifact_manifest: None,
                            outputs: HashMap::new(),
                            progress: None,
                            run_dir: Some(run_context.run_dir.clone()),
                            metrics: None,
                            logs,
                        })
                    },
                }
            },
        }
    }

    fn name(&self) -> &'static str {
        "llm"
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "llm"
    }
}

// ---------------------------------------------------------------------------
// Resource envelope overlay
// ---------------------------------------------------------------------------

/// Read `<alias>.json` from the staged-inputs side-channel and overlay
/// the fields the LLM backend cares about (`api_key`, `base_url`) onto
/// the deserialized config. Per-step values, when set, take precedence —
/// callers can still pin a one-off api_key on a single step without
/// touching the resource.
fn overlay_resource(
    config: &mut LlmConfig,
    alias: &str,
    run_context: &RunContext,
) -> Result<(), ExecutorError> {
    let resource =
        aithericon_executor_backend::load_resource::<ResolvedOpenAiResource>(run_context, alias)?;

    if config.api_key.is_none() {
        config.api_key = resource.api_key;
    }
    if config.base_url.is_none() {
        config.base_url = resource.base_url;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Image helpers
// ---------------------------------------------------------------------------

fn load_images(inputs: &[ImageInput]) -> Result<Vec<ImageData>, ExecutorError> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;

    inputs
        .iter()
        .map(|img| {
            let bytes = std::fs::read(&img.path).map_err(|e| {
                ExecutorError::Config(format!("failed to read image '{}': {e}", img.path))
            })?;
            let media_type = img
                .media_type
                .clone()
                .unwrap_or_else(|| guess_media_type(&img.path));
            Ok(ImageData {
                base64: engine.encode(&bytes),
                media_type,
            })
        })
        .collect()
}

fn guess_media_type(path: &str) -> String {
    match path.rsplit('.').next().map(|s| s.to_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("tiff" | "tif") => "image/tiff",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use aithericon_executor_domain::{LlmStopReason, LlmUsage};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// An adapter that returns `Retryable` for its first `fail_n` calls, then `Ok`.
    /// Counts calls so the test can assert how many attempts were made.
    struct FlakyAdapter {
        fail_n: u32,
        calls: AtomicU32,
    }

    #[async_trait]
    impl CompletionPort for FlakyAdapter {
        async fn complete(
            &self,
            _request: &CompletionRequest,
            _env: &HashMap<String, String>,
        ) -> Result<CompletionResponse, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_n {
                Err(LlmError::Retryable(format!(
                    "no live replica serves model (attempt {n})"
                )))
            } else {
                Ok(CompletionResponse {
                    content: "ok".into(),
                    usage: LlmUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        total_tokens: 2,
                    },
                    model: "test-model".into(),
                    stop_reason: LlmStopReason::EndTurn,
                    structured_output: None,
                    tool_calls: vec![],
                })
            }
        }
        fn name(&self) -> &str {
            "flaky"
        }
    }

    fn req() -> CompletionRequest {
        CompletionRequest {
            model: "llama3.2:1b".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            reasoning: None,
            response_format: ResponseFormat::Text,
            tools: vec![],
        }
    }

    /// A transient router error (503 no-live-replica while the pool scales from
    /// zero) is retried with backoff and SUCCEEDS once a replica comes up — the
    /// whole execution rides out the cold start instead of fast-failing. `start_paused`
    /// auto-advances the backoff sleeps so the test is instant.
    #[tokio::test(start_paused = true)]
    async fn retries_transient_then_succeeds() {
        let flaky = Arc::new(FlakyAdapter {
            fail_n: 3, // 503 three times, then a replica is live
            calls: AtomicU32::new(0),
        });
        let adapter: Arc<dyn CompletionPort> = flaky.clone();
        let env = HashMap::new();
        let resp = complete_with_retry(&adapter, &req(), &env)
            .await
            .expect("a transient 503 must be retried until a replica is live");
        assert_eq!(resp.content, "ok");
        // 3 transient failures + 1 success = 4 attempts.
        assert_eq!(flaky.calls.load(Ordering::SeqCst), 4);
    }

    /// A persistently transient error eventually exhausts the bounded budget and is
    /// downgraded to a normal `Api` error (a clean backend failure), NOT retried
    /// forever.
    #[tokio::test(start_paused = true)]
    async fn exhausts_then_downgrades_to_api_error() {
        let adapter: Arc<dyn CompletionPort> = Arc::new(FlakyAdapter {
            fail_n: u32::MAX, // never recovers
            calls: AtomicU32::new(0),
        });
        let env = HashMap::new();
        let err = complete_with_retry(&adapter, &req(), &env)
            .await
            .expect_err("a never-recovering transient error must eventually fail");
        // Downgraded to Api (not Retryable) so the caller treats it as terminal.
        assert!(
            matches!(err, LlmError::Api(_)),
            "exhausted retries downgrade to Api, got {err:?}"
        );
    }

    /// A NON-transient error (e.g. a 400/parse) is returned immediately — no retry.
    #[tokio::test(start_paused = true)]
    async fn non_transient_is_not_retried() {
        struct HardFail(AtomicU32);
        #[async_trait]
        impl CompletionPort for HardFail {
            async fn complete(
                &self,
                _r: &CompletionRequest,
                _e: &HashMap<String, String>,
            ) -> Result<CompletionResponse, LlmError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(LlmError::Api("bad request".into()))
            }
            fn name(&self) -> &str {
                "hard"
            }
        }
        let adapter: Arc<dyn CompletionPort> = Arc::new(HardFail(AtomicU32::new(0)));
        let env = HashMap::new();
        let err = complete_with_retry(&adapter, &req(), &env)
            .await
            .expect_err("non-transient error surfaces");
        assert!(matches!(err, LlmError::Api(_)));
    }
}
