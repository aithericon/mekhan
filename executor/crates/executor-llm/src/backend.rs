use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

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
        // Resolve {{input:NAME}} patterns in the raw config JSON.
        let mut raw_config = run_context.spec.config.clone();
        aithericon_executor_backend::resolve::resolve_inputs(
            &mut raw_config,
            &run_context.staged_inputs,
        )
        .map_err(|e| ExecutorError::Config(format!("llm input resolution: {e}")))?;

        // Deserialize resolved config
        let config: LlmConfig = serde_json::from_value(raw_config)
            .map_err(|e| ExecutorError::Config(format!("invalid llm backend config: {e}")))?;

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

        // Merge config-level api_key and base_url into env so adapters can
        // find them alongside process-level environment variables.
        let mut env = run_context.env.clone();
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

                        // Map spec-declared output names to the response value.
                        for decl in &run_context.spec.outputs {
                            if !outputs.contains_key(&decl.name) {
                                outputs.insert(decl.name.clone(), response_value.clone());
                            }
                        }

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

                        let logs = Some(LogSummary {
                            total_entries: 1,
                            count_by_level: HashMap::from([("info".into(), 1)]),
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

                        let logs = Some(LogSummary {
                            total_entries: 1,
                            count_by_level: HashMap::from([("error".into(), 1)]),
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
