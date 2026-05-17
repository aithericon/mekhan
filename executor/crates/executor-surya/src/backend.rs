//! `SuryaBackend` — `aithericon_executor_backend::traits::ExecutionBackend`
//! implementation for Surya OCR.
//!
//! Mirrors `aithericon_executor_kreuzberg::KreuzbergBackend` shape EXACTLY:
//! two-mode dispatch (Single / Batch), three-way `tokio::select!` of
//! cancel / timeout / actual extraction, status callbacks, error mapping.
//! Substitutes Surya-specific details (call [`crate::adapters::surya::SuryaAdapter`]
//! against the managed subprocess; map Surya's wire response to the
//! `outputs` map) without re-architecting the trait surface.
//!
//! ## Base URL resolution
//!
//! Reads `RunContext.env["SURYA_BASE_URL"]` (defaulting to
//! `http://127.0.0.1:7160`). Item 5's `register_as_pool` flow injects
//! the real URL from the spawned [`crate::surya_subprocess::SuryaSubprocess::base_url`]
//! into the env map at boot — parallel to executor-llm's
//! `OLLAMA_API_BASE_URL` convention.

use std::collections::HashMap;

use async_trait::async_trait;
use base64::Engine;
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    LogEntry, LogLevel, LogSummary, MetricSummary, Progress, RunContext,
};

use crate::adapters::surya::SuryaAdapter;
use crate::config::{guess_mime_from_path, ExtractionMode, ResolvedSuryaConfig, SuryaConfig};
use crate::port::{OcrRequest, OcrResponse};

/// Default Surya HTTP base URL — used when `RunContext.env["SURYA_BASE_URL"]`
/// is absent. Matches [`crate::surya_subprocess::SuryaSubprocess::base_url`]
/// at the documented default port (7160).
const DEFAULT_SURYA_BASE_URL: &str = "http://127.0.0.1:7160";

/// Env-var key carrying the Surya HTTP base URL. Mirrors executor-llm's
/// `OLLAMA_API_BASE_URL` convention — backends read the URL out of the
/// per-execution env map so callers (Item 5's pool boot) can inject it
/// from the spawned subprocess without backends holding instance state.
pub const SURYA_BASE_URL_ENV: &str = "SURYA_BASE_URL";

/// Backend that performs OCR via Surya through the managed Python
/// subprocess.
pub struct SuryaBackend;

impl SuryaBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SuryaBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionBackend for SuryaBackend {
    async fn prepare(
        &self,
        _job: &ExecutionJob,
        mut run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // Resolve {{input:NAME}} / {{input_path:NAME}} patterns in the
        // raw config JSON (parity with KreuzbergBackend).
        let mut raw_config = run_context.spec.config.clone();
        aithericon_executor_backend::resolve::resolve_inputs(
            &mut raw_config,
            &run_context.staged_inputs,
        )
        .map_err(|e| ExecutorError::Config(format!("surya input resolution: {e}")))?;

        let config: SuryaConfig = serde_json::from_value(raw_config)
            .map_err(|e| ExecutorError::Config(format!("invalid surya config: {e}")))?;

        let resolved = match config.mode {
            ExtractionMode::Single => {
                let (name, path) = config.resolve_target_file(&run_context.staged_inputs)?;
                ResolvedSuryaConfig {
                    config,
                    target_file: Some(path),
                    target_name: Some(name),
                    target_files: vec![],
                }
            }
            ExtractionMode::Batch => {
                let targets = config.resolve_target_files(&run_context.staged_inputs)?;
                ResolvedSuryaConfig {
                    config,
                    target_file: None,
                    target_name: None,
                    target_files: targets,
                }
            }
        };

        run_context.backend_state = serde_json::to_value(&resolved).map_err(|e| {
            ExecutorError::Config(format!("failed to serialize resolved surya config: {e}"))
        })?;
        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let resolved: ResolvedSuryaConfig = serde_json::from_value(run_context.backend_state.clone())
            .map_err(|e| {
                ExecutorError::Config(format!("failed to deserialize resolved surya config: {e}"))
            })?;

        let base_url = run_context
            .env
            .get(SURYA_BASE_URL_ENV)
            .cloned()
            .unwrap_or_else(|| DEFAULT_SURYA_BASE_URL.to_string());
        let adapter = SuryaAdapter::new(base_url);

        match resolved.config.mode {
            ExtractionMode::Single => {
                execute_single(run_context, &resolved, &adapter, status_cb, cancel).await
            }
            ExtractionMode::Batch => {
                execute_batch(run_context, &resolved, &adapter, status_cb, cancel).await
            }
        }
    }

    fn name(&self) -> &'static str {
        "surya"
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "surya"
    }
}

// ---------------------------------------------------------------------------
// Single-file OCR
// ---------------------------------------------------------------------------

async fn execute_single(
    run_context: &RunContext,
    resolved: &ResolvedSuryaConfig,
    adapter: &SuryaAdapter,
    status_cb: StatusCallback,
    cancel: CancellationToken,
) -> Result<ExecutionResult, ExecutorError> {
    let file_path = resolved.target_file.as_ref().unwrap();
    let file_name = resolved.target_name.as_deref().unwrap_or("file");
    let mime_type = match resolved.config.mime_type.as_deref() {
        Some(m) => m.to_string(),
        None => match guess_mime_from_path(file_path) {
            Some(m) => m.to_string(),
            None => {
                return Err(ExecutorError::Config(format!(
                    "surya: could not infer mime_type for '{}' (extension not recognized); \
                     set `mime_type` explicitly in the backend config",
                    file_path.display()
                )));
            }
        },
    };

    let start = tokio::time::Instant::now();

    status_cb(
        ExecutionStatus::Running,
        serde_json::json!({
            "mode": "single",
            "file": file_name,
            "mime_type": mime_type,
        }),
    )
    .await;

    // Stage the request body. tokio::fs::read for async I/O; base64
    // encoding for the wire envelope per the legacy ocr/ sidecar
    // contract.
    let bytes = match tokio::fs::read(file_path).await {
        Ok(b) => b,
        Err(e) => {
            return Ok(backend_error_result(
                run_context,
                start.elapsed(),
                format!("read input '{file_name}' from {}: {e}", file_path.display()),
                file_name,
            ));
        }
    };
    let input_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let request = OcrRequest {
        input_b64,
        mime_type: mime_type.clone(),
        filename: Some(file_name.to_string()),
    };

    tokio::select! { biased;
        _ = cancel.cancelled() => {
            Ok(cancelled_result(run_context, start.elapsed()))
        },
        _ = tokio::time::sleep(run_context.timeout) => {
            Ok(timed_out_result(run_context, start.elapsed(), run_context.timeout))
        },
        result = adapter.ocr(&request) => {
            let duration = start.elapsed();
            match result {
                Ok(response) => Ok(success_result_single(run_context, duration, response, file_name)),
                Err(e) => Ok(backend_error_result(
                    run_context,
                    duration,
                    format!("Surya OCR failed for '{file_name}': {e}"),
                    file_name,
                )),
            }
        },
    }
}

fn success_result_single(
    run_context: &RunContext,
    duration: std::time::Duration,
    response: OcrResponse,
    file_name: &str,
) -> ExecutionResult {
    let mut outputs: HashMap<String, serde_json::Value> = HashMap::from([
        ("ocr_text".into(), serde_json::json!(response.ocr_text)),
        ("page_count".into(), serde_json::json!(response.page_count)),
        ("engine".into(), serde_json::json!(response.engine)),
        ("mime_type".into(), serde_json::json!(response.mime_type)),
    ]);

    // Mirror KreuzbergBackend: undeclared spec outputs default to the
    // content value (analog to "response" in LlmBackend).
    let content_value = serde_json::json!(response.ocr_text);
    for decl in &run_context.spec.outputs {
        if !outputs.contains_key(&decl.name) {
            outputs.insert(decl.name.clone(), content_value.clone());
        }
    }

    // Write to expected_outputs file paths.
    for (name, path) in &run_context.expected_outputs {
        if !path.exists() {
            let content = if let Some(val) = outputs.get(name) {
                serde_json::to_string_pretty(val).unwrap_or_default()
            } else {
                serde_json::to_string_pretty(&content_value).unwrap_or_default()
            };
            if let Err(e) = std::fs::write(path, content) {
                warn!(output = %name, "failed to write output file: {e}");
            }
        }
    }

    let chars = response.ocr_text.chars().count() as f64;
    let metrics = Some(MetricSummary {
        total_points: 3,
        metric_names: vec![
            "surya/extraction_time_ms".into(),
            "surya/content_length".into(),
            "surya/page_count".into(),
        ],
        latest_values: HashMap::from([
            ("surya/extraction_time_ms".into(), duration.as_millis() as f64),
            ("surya/content_length".into(), chars),
            ("surya/page_count".into(), response.page_count as f64),
        ]),
    });

    let stdout_tail = if response.ocr_text.len() > 1024 {
        format!(
            "surya[{file_name}] {}... ({} chars total)",
            &response.ocr_text[..1024],
            response.ocr_text.len()
        )
    } else {
        format!("surya[{file_name}] {}", response.ocr_text)
    };

    let logs = Some(LogSummary {
        total_entries: 1,
        count_by_level: HashMap::from([("info".into(), 1)]),
        recent_errors: vec![],
        dropped_count: 0,
    });

    ExecutionResult {
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
    }
}

// ---------------------------------------------------------------------------
// Batch OCR
// ---------------------------------------------------------------------------

async fn execute_batch(
    run_context: &RunContext,
    resolved: &ResolvedSuryaConfig,
    adapter: &SuryaAdapter,
    status_cb: StatusCallback,
    cancel: CancellationToken,
) -> Result<ExecutionResult, ExecutorError> {
    let targets = &resolved.target_files;
    let total = targets.len();
    let start = tokio::time::Instant::now();

    status_cb(
        ExecutionStatus::Running,
        serde_json::json!({
            "mode": "batch",
            "total_files": total,
        }),
    )
    .await;

    let mut results: Vec<serde_json::Value> = Vec::with_capacity(total);
    let mut errors: Vec<serde_json::Value> = Vec::new();
    let mut successful = 0usize;
    let mut failed = 0usize;
    let mut total_content_length = 0usize;

    for (idx, (name, path)) in targets.iter().enumerate() {
        if cancel.is_cancelled() {
            return Ok(cancelled_result_batch(run_context, start.elapsed(), idx, total));
        }
        if start.elapsed() >= run_context.timeout {
            return Ok(timed_out_result_batch(
                run_context,
                start.elapsed(),
                run_context.timeout,
                idx,
                total,
            ));
        }

        let mime_type = match resolved.config.mime_type.as_deref() {
            Some(m) => m.to_string(),
            None => match guess_mime_from_path(path) {
                Some(m) => m.to_string(),
                None => {
                    failed += 1;
                    errors.push(serde_json::json!({
                        "file": name,
                        "error": format!(
                            "could not infer mime_type for '{}' (extension not recognized)",
                            path.display()
                        ),
                    }));
                    continue;
                }
            },
        };

        let bytes = match tokio::fs::read(path).await {
            Ok(b) => b,
            Err(e) => {
                failed += 1;
                errors.push(serde_json::json!({
                    "file": name,
                    "error": format!("read failed: {e}"),
                }));
                continue;
            }
        };
        let input_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let request = OcrRequest {
            input_b64,
            mime_type: mime_type.clone(),
            filename: Some(name.clone()),
        };

        // Per-iteration cancel/timeout/work select. Cancel between
        // iterations is also checked at the top of the loop; the inner
        // select handles the in-flight HTTP call.
        let inner = tokio::select! { biased;
            _ = cancel.cancelled() => {
                return Ok(cancelled_result_batch(run_context, start.elapsed(), idx, total));
            },
            _ = tokio::time::sleep(run_context.timeout.saturating_sub(start.elapsed())) => {
                return Ok(timed_out_result_batch(
                    run_context,
                    start.elapsed(),
                    run_context.timeout,
                    idx,
                    total,
                ));
            },
            r = adapter.ocr(&request) => r,
        };

        match inner {
            Ok(response) => {
                total_content_length += response.ocr_text.len();
                successful += 1;
                results.push(serde_json::json!({
                    "file": name,
                    "ocr_text": response.ocr_text,
                    "page_count": response.page_count,
                    "mime_type": response.mime_type,
                    "char_count": response.ocr_text.len(),
                }));
            }
            Err(e) => {
                failed += 1;
                errors.push(serde_json::json!({
                    "file": name,
                    "error": e.to_string(),
                }));
            }
        }

        let completed = idx + 1;
        status_cb(
            ExecutionStatus::Running,
            serde_json::json!({
                "mode": "batch",
                "progress": {
                    "completed": completed,
                    "total": total,
                    "fraction": completed as f64 / total as f64,
                },
            }),
        )
        .await;
    }

    let duration = start.elapsed();
    let outputs: HashMap<String, serde_json::Value> = HashMap::from([
        ("results".into(), serde_json::json!(results)),
        ("total_files".into(), serde_json::json!(total)),
        ("successful".into(), serde_json::json!(successful)),
        ("failed".into(), serde_json::json!(failed)),
        ("errors".into(), serde_json::json!(errors)),
    ]);

    let metrics = MetricSummary {
        total_points: 5,
        metric_names: vec![
            "surya/total_extraction_time_ms".into(),
            "surya/total_files".into(),
            "surya/successful_files".into(),
            "surya/failed_files".into(),
            "surya/total_content_length".into(),
        ],
        latest_values: HashMap::from([
            ("surya/total_extraction_time_ms".into(), duration.as_millis() as f64),
            ("surya/total_files".into(), total as f64),
            ("surya/successful_files".into(), successful as f64),
            ("surya/failed_files".into(), failed as f64),
            ("surya/total_content_length".into(), total_content_length as f64),
        ]),
    };

    let stdout_tail = format!(
        "Surya OCR: {successful}/{total} files ({failed} failed, {total_content_length} chars total)"
    );

    let outcome = if failed > 0 && successful == 0 {
        ExecutionOutcome::BackendError {
            message: format!("all {failed} files failed Surya OCR"),
        }
    } else {
        ExecutionOutcome::Success
    };

    let logs = Some(LogSummary {
        total_entries: total as u64,
        count_by_level: HashMap::from([
            ("info".into(), successful as u64),
            ("error".into(), failed as u64),
        ]),
        recent_errors: errors
            .iter()
            .map(|e| LogEntry {
                level: LogLevel::Error,
                message: e.to_string(),
                timestamp: Utc::now(),
                fields: HashMap::new(),
                repeat_count: 1,
            })
            .collect(),
        dropped_count: 0,
    });

    Ok(ExecutionResult {
        outcome,
        duration,
        stdout_tail: Some(stdout_tail),
        stderr_tail: None,
        artifact_manifest: None,
        outputs,
        progress: Some(Progress {
            fraction: 1.0,
            message: Some(format!("{successful}/{total} files OCR'd")),
            current_step: total as u64,
            total_steps: total as u64,
            phases: vec![],
            updated_at: Utc::now(),
        }),
        run_dir: Some(run_context.run_dir.clone()),
        metrics: Some(metrics),
        logs,
    })
}

// ---------------------------------------------------------------------------
// Outcome helpers — keep parity with KreuzbergBackend / LlmBackend shapes.
// ---------------------------------------------------------------------------

fn cancelled_result(run_context: &RunContext, duration: std::time::Duration) -> ExecutionResult {
    ExecutionResult {
        outcome: ExecutionOutcome::Cancelled,
        duration,
        stdout_tail: None,
        stderr_tail: Some("execution cancelled".into()),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

fn cancelled_result_batch(
    run_context: &RunContext,
    duration: std::time::Duration,
    completed_idx: usize,
    total: usize,
) -> ExecutionResult {
    ExecutionResult {
        outcome: ExecutionOutcome::Cancelled,
        duration,
        stdout_tail: None,
        stderr_tail: Some("execution cancelled".into()),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: Some(Progress {
            fraction: completed_idx as f64 / total.max(1) as f64,
            message: Some(format!("cancelled after {completed_idx}/{total} files")),
            current_step: completed_idx as u64,
            total_steps: total as u64,
            phases: vec![],
            updated_at: Utc::now(),
        }),
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

fn timed_out_result(
    run_context: &RunContext,
    duration: std::time::Duration,
    timeout: std::time::Duration,
) -> ExecutionResult {
    ExecutionResult {
        outcome: ExecutionOutcome::TimedOut,
        duration,
        stdout_tail: None,
        stderr_tail: Some(format!("timed out after {timeout:?}")),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

fn timed_out_result_batch(
    run_context: &RunContext,
    duration: std::time::Duration,
    timeout: std::time::Duration,
    completed_idx: usize,
    total: usize,
) -> ExecutionResult {
    ExecutionResult {
        outcome: ExecutionOutcome::TimedOut,
        duration,
        stdout_tail: None,
        stderr_tail: Some(format!("timed out after {timeout:?}")),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: Some(Progress {
            fraction: completed_idx as f64 / total.max(1) as f64,
            message: Some(format!("timed out after {completed_idx}/{total} files")),
            current_step: completed_idx as u64,
            total_steps: total as u64,
            phases: vec![],
            updated_at: Utc::now(),
        }),
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: None,
    }
}

fn backend_error_result(
    run_context: &RunContext,
    duration: std::time::Duration,
    message: String,
    file_name: &str,
) -> ExecutionResult {
    let error_entry = LogEntry {
        level: LogLevel::Error,
        message: message.clone(),
        timestamp: Utc::now(),
        fields: HashMap::from([("file".into(), file_name.to_string())]),
        repeat_count: 1,
    };
    ExecutionResult {
        outcome: ExecutionOutcome::BackendError { message: message.clone() },
        duration,
        stdout_tail: None,
        stderr_tail: Some(message),
        artifact_manifest: None,
        outputs: HashMap::new(),
        progress: None,
        run_dir: Some(run_context.run_dir.clone()),
        metrics: None,
        logs: Some(LogSummary {
            total_entries: 1,
            count_by_level: HashMap::from([("error".into(), 1)]),
            recent_errors: vec![error_entry],
            dropped_count: 0,
        }),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use aithericon_executor_domain::{JobPriority, RunDirectory};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_spec(config: serde_json::Value) -> ExecutionSpec {
        ExecutionSpec {
            backend: "surya".into(),
            inputs: vec![],
            outputs: vec![],
            config,
        }
    }

    fn make_job(spec: &ExecutionSpec) -> ExecutionJob {
        ExecutionJob {
            execution_id: format!(
                "surya-test-{}",
                TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
            ),
            spec: spec.clone(),
            metadata: HashMap::new(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        }
    }

    fn make_run_context(spec: ExecutionSpec, timeout: Duration) -> RunContext {
        let seq = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("surya-test-{}-{}", std::process::id(), seq);
        RunContext {
            execution_id: id.clone(),
            spec,
            run_dir: RunDirectory::new(&std::env::temp_dir(), &id),
            timeout,
            env: HashMap::new(),
            metadata: HashMap::new(),
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: Vec::new(),
            backend_state: serde_json::Value::Null,
        }
    }

    // -- Trait method tests --

    #[test]
    fn name_returns_surya() {
        let backend = SuryaBackend::new();
        assert_eq!(backend.name(), "surya");
    }

    #[test]
    fn supports_surya_backend() {
        let backend = SuryaBackend::new();
        let spec = make_spec(serde_json::json!({}));
        assert!(backend.supports(&spec));
    }

    #[test]
    fn does_not_support_other_backends() {
        let backend = SuryaBackend::new();
        let spec = ExecutionSpec {
            backend: "test-backend-a".into(), // placeholder, NOT a real model/backend name
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({}),
        };
        assert!(!backend.supports(&spec));
    }

    #[test]
    fn does_not_support_kreuzberg_backend() {
        // Honest-absence: SuryaBackend must NOT accept a kreuzberg spec
        // (the two are sibling OCR backends that route via cap-routing's
        // pick — never via overlapping supports() shadowing).
        let backend = SuryaBackend::new();
        let spec = ExecutionSpec {
            backend: "kreuzberg".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({}),
        };
        assert!(!backend.supports(&spec));
    }

    // -- prepare() tests --

    #[tokio::test]
    async fn prepare_single_mode_with_one_staged_input() {
        let backend = SuryaBackend::new();
        let spec = make_spec(serde_json::json!({}));
        let job = make_job(&spec);
        let mut ctx = make_run_context(spec, Duration::from_secs(30));
        ctx.staged_inputs
            .insert("file".into(), PathBuf::from("/tmp/test.pdf"));

        let ctx = backend.prepare(&job, ctx).await.unwrap();

        let resolved: ResolvedSuryaConfig =
            serde_json::from_value(ctx.backend_state.clone()).unwrap();
        assert!(resolved.target_file.is_some());
        assert_eq!(resolved.target_name.as_deref(), Some("file"));
        assert!(resolved.target_files.is_empty());
    }

    #[tokio::test]
    async fn prepare_batch_mode_resolves_all_inputs() {
        let backend = SuryaBackend::new();
        let spec = make_spec(serde_json::json!({ "mode": "batch" }));
        let job = make_job(&spec);
        let mut ctx = make_run_context(spec, Duration::from_secs(30));
        ctx.staged_inputs
            .insert("a".into(), PathBuf::from("/tmp/a.png"));
        ctx.staged_inputs
            .insert("b".into(), PathBuf::from("/tmp/b.png"));

        let ctx = backend.prepare(&job, ctx).await.unwrap();

        let resolved: ResolvedSuryaConfig =
            serde_json::from_value(ctx.backend_state.clone()).unwrap();
        assert!(resolved.target_file.is_none());
        assert_eq!(resolved.target_files.len(), 2);
    }

    #[tokio::test]
    async fn prepare_fails_with_ambiguous_inputs() {
        let backend = SuryaBackend::new();
        let spec = make_spec(serde_json::json!({}));
        let job = make_job(&spec);
        let mut ctx = make_run_context(spec, Duration::from_secs(30));
        ctx.staged_inputs
            .insert("a".into(), PathBuf::from("/tmp/a.pdf"));
        ctx.staged_inputs
            .insert("b".into(), PathBuf::from("/tmp/b.pdf"));

        let err = backend.prepare(&job, ctx).await.unwrap_err();
        assert!(matches!(err, ExecutorError::Config(_)));
    }

    #[tokio::test]
    async fn prepare_fails_with_invalid_config_shape() {
        let backend = SuryaBackend::new();
        let spec = make_spec(serde_json::json!({ "mode": 42 }));
        let job = make_job(&spec);
        let mut ctx = make_run_context(spec, Duration::from_secs(30));
        ctx.staged_inputs
            .insert("file".into(), PathBuf::from("/tmp/test.pdf"));

        let err = backend.prepare(&job, ctx).await.unwrap_err();
        assert!(matches!(err, ExecutorError::Config(_)));
    }

    #[tokio::test]
    async fn prepare_fails_with_explicit_file_not_staged() {
        let backend = SuryaBackend::new();
        let spec = make_spec(serde_json::json!({ "file": "missing" }));
        let job = make_job(&spec);
        let mut ctx = make_run_context(spec, Duration::from_secs(30));
        ctx.staged_inputs
            .insert("other".into(), PathBuf::from("/tmp/other.pdf"));

        let err = backend.prepare(&job, ctx).await.unwrap_err();
        assert!(matches!(err, ExecutorError::Config(_)));
    }
}
