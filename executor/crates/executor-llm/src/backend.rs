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

use crate::adapters;
use crate::config::{ImageInput, LlmConfig};
use crate::port::{CompletionRequest, ImageData, ResponseFormat};

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
            .map_err(|e| {
                ExecutorError::Config(format!("failed to deserialize llm config: {e}"))
            })?;

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
                format!("dispatching LLM request to {}/{}", adapter.name(), config.model),
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
            env.entry(env_key.to_string()).or_insert_with(|| api_key.clone());
        }
        if let Some(ref base_url) = config.base_url {
            let env_key = match config.provider {
                crate::config::Provider::OpenAi => "OPENAI_BASE_URL",
                crate::config::Provider::Anthropic => "ANTHROPIC_BASE_URL",
                crate::config::Provider::Ollama => "OLLAMA_API_BASE_URL",
            };
            env.entry(env_key.to_string()).or_insert_with(|| base_url.clone());
        }

        // Three-way select: cancellation, timeout, or LLM execution
        tokio::select! { biased;
            _ = cancel.cancelled() => {
                Ok(ExecutionResult {
                    outcome: ExecutionOutcome::Cancelled,
                    duration: start.elapsed(),
                    stdout_tail: None,
                    stderr_tail: Some("execution cancelled".into()),
                    artifact_manifest: None,
                    outputs: HashMap::new(),
                    progress: None,
                    run_dir: Some(run_context.run_dir.clone()),
                    metrics: None,
                    logs: None,
                })
            },
            _ = tokio::time::sleep(run_context.timeout) => {
                Ok(ExecutionResult {
                    outcome: ExecutionOutcome::TimedOut,
                    duration: start.elapsed(),
                    stdout_tail: None,
                    stderr_tail: Some(format!("timed out after {:?}", run_context.timeout)),
                    artifact_manifest: None,
                    outputs: HashMap::new(),
                    progress: None,
                    run_dir: Some(run_context.run_dir.clone()),
                    metrics: None,
                    logs: None,
                })
            },
            result = adapter.complete(&request, &env) => {
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
                            ("finish_reason".into(), serde_json::json!(resp.finish_reason.to_string())),
                            ("model".into(), serde_json::json!(resp.model)),
                        ]);

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
                                    ("finish_reason".into(), resp.finish_reason.to_string()),
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

/// Read `<alias>.json` from the staged-inputs side-channel and overlay the
/// fields the LLM backend cares about (`api_key`, `base_url`, `organization`)
/// onto the deserialized config. Per-step values, when set, take precedence —
/// callers can still pin a one-off api_key on a single step without touching
/// the resource.
fn overlay_resource(
    config: &mut LlmConfig,
    alias: &str,
    run_context: &RunContext,
) -> Result<(), ExecutorError> {
    let value = aithericon_executor_backend::load_resource_envelope(run_context, alias)?;
    let obj = value.as_object().ok_or_else(|| {
        ExecutorError::Config(format!(
            "llm backend: resource '{alias}' envelope must be a JSON object"
        ))
    })?;

    if config.api_key.is_none() {
        if let Some(v) = obj.get("api_key").and_then(|v| v.as_str()) {
            config.api_key = Some(v.to_string());
        }
    }
    if config.base_url.is_none() {
        if let Some(v) = obj.get("base_url").and_then(|v| v.as_str()) {
            config.base_url = Some(v.to_string());
        }
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
    match path
        .rsplit('.')
        .next()
        .map(|s| s.to_lowercase())
        .as_deref()
    {
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
