use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    LogEntry, LogLevel, LogSummary, MetricSummary, Progress, RunContext,
};

use crate::config::{ExtractionMode, KreuzbergConfig, KreuzbergConfigExt, ResolvedKreuzbergConfig};

/// Backend that extracts text, metadata, and tables from documents via the
/// kreuzberg library.
pub struct KreuzbergBackend;

impl KreuzbergBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for KreuzbergBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionBackend for KreuzbergBackend {
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
        .map_err(|e| ExecutorError::Config(format!("kreuzberg input resolution: {e}")))?;

        // Deserialize resolved config.
        let config: KreuzbergConfig = serde_json::from_value(raw_config)
            .map_err(|e| ExecutorError::Config(format!("invalid kreuzberg config: {e}")))?;

        // Resolve target file(s) and validate they exist in staged_inputs.
        let resolved = match config.mode {
            ExtractionMode::Single => {
                let (name, path) = config.resolve_target_file(&run_context.staged_inputs)?;
                ResolvedKreuzbergConfig {
                    config,
                    target_file: Some(path),
                    target_name: Some(name),
                    target_files: vec![],
                }
            }
            ExtractionMode::Batch => {
                let targets = config.resolve_target_files(&run_context.staged_inputs)?;
                ResolvedKreuzbergConfig {
                    config,
                    target_file: None,
                    target_name: None,
                    target_files: targets,
                }
            }
        };

        run_context.backend_state = serde_json::to_value(&resolved).map_err(|e| {
            ExecutorError::Config(format!("failed to serialize resolved config: {e}"))
        })?;
        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        _event_stream: Option<std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let resolved: ResolvedKreuzbergConfig =
            serde_json::from_value(run_context.backend_state.clone()).map_err(|e| {
                ExecutorError::Config(format!("failed to deserialize resolved config: {e}"))
            })?;

        match resolved.config.mode {
            ExtractionMode::Single => {
                execute_single(run_context, &resolved, status_cb, cancel).await
            }
            ExtractionMode::Batch => execute_batch(run_context, &resolved, status_cb, cancel).await,
        }
    }

    fn name(&self) -> &'static str {
        "kreuzberg"
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "kreuzberg"
    }
}

// ---------------------------------------------------------------------------
// Single-file extraction
// ---------------------------------------------------------------------------

async fn execute_single(
    run_context: &RunContext,
    resolved: &ResolvedKreuzbergConfig,
    status_cb: StatusCallback,
    cancel: CancellationToken,
) -> Result<ExecutionResult, ExecutorError> {
    let file_path = resolved.target_file.as_ref().ok_or_else(|| {
        ExecutorError::Config(
            "kreuzberg single mode: target_file missing from resolved config".into(),
        )
    })?;
    let file_name = resolved.target_name.as_deref().unwrap_or("file");
    let extraction_config = resolved.config.build_extraction_config();
    let mime = resolved.config.mime_type.as_deref();

    let start = tokio::time::Instant::now();

    // Report Running status.
    status_cb(
        ExecutionStatus::Running,
        serde_json::json!({
            "mode": "single",
            "file": file_name,
        }),
    )
    .await;

    let path_str = file_path.to_string_lossy().to_string();

    // Three-way select: cancellation, timeout, or extraction.
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
        result = kreuzberg::extract_file(&path_str, mime, &extraction_config) => {
            let duration = start.elapsed();
            match result {
                Ok(extraction) => {
                    let (outputs, metrics, stdout_tail) = build_single_outputs(&extraction, duration);

                    // Write declared outputs to expected_outputs file paths.
                    // No remapping: the value must come from a kreuzberg-native
                    // output key. If the user declared a name that doesn't
                    // exist in `outputs`, skip the write and let the executor's
                    // required-output check fail with a clear error.
                    for (name, path) in &run_context.expected_outputs {
                        if path.exists() {
                            continue;
                        }
                        let Some(val) = outputs.get(name) else { continue };
                        let content = serde_json::to_string_pretty(val).unwrap_or_default();
                        if let Err(e) = std::fs::write(path, content) {
                            warn!(output = %name, "failed to write output file: {e}");
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
                        metrics: Some(metrics),
                        logs,
                    })
                },
                Err(e) => {
                    let error_entry = LogEntry {
                        level: LogLevel::Error,
                        message: format!("kreuzberg extraction failed: {e}"),
                        timestamp: Utc::now(),
                        fields: HashMap::from([
                            ("file".into(), file_name.to_string()),
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

// ---------------------------------------------------------------------------
// Batch extraction
// ---------------------------------------------------------------------------

async fn execute_batch(
    run_context: &RunContext,
    resolved: &ResolvedKreuzbergConfig,
    status_cb: StatusCallback,
    cancel: CancellationToken,
) -> Result<ExecutionResult, ExecutorError> {
    let extraction_config = resolved.config.build_extraction_config();
    let targets = &resolved.target_files;
    let total = targets.len();

    let start = tokio::time::Instant::now();

    // Report Running status.
    status_cb(
        ExecutionStatus::Running,
        serde_json::json!({
            "mode": "batch",
            "total_files": total,
        }),
    )
    .await;

    let mut results = Vec::with_capacity(total);
    let mut errors = Vec::new();
    let mut successful = 0usize;
    let mut failed = 0usize;
    let mut total_content_length = 0usize;
    let mut total_table_count = 0usize;

    for (idx, (name, path)) in targets.iter().enumerate() {
        // Check cancellation before each file.
        if cancel.is_cancelled() {
            return Ok(ExecutionResult::cancelled(
                start.elapsed(),
                Some(run_context.run_dir.clone()),
                Some("execution cancelled".into()),
                Some(Progress {
                    fraction: idx as f64 / total as f64,
                    message: Some(format!("cancelled after {idx}/{total} files")),
                    current_step: idx as u64,
                    total_steps: total as u64,
                    phases: vec![],
                    updated_at: Utc::now(),
                }),
            ));
        }

        // Check timeout.
        if start.elapsed() >= run_context.timeout {
            return Ok(ExecutionResult::timed_out(
                start.elapsed(),
                Some(run_context.run_dir.clone()),
                Some(format!("timed out after {:?}", run_context.timeout)),
                Some(Progress {
                    fraction: idx as f64 / total as f64,
                    message: Some(format!("timed out after {idx}/{total} files")),
                    current_step: idx as u64,
                    total_steps: total as u64,
                    phases: vec![],
                    updated_at: Utc::now(),
                }),
            ));
        }

        let path_str = path.to_string_lossy().to_string();
        let mime = resolved.config.mime_type.as_deref();

        match kreuzberg::extract_file(&path_str, mime, &extraction_config).await {
            Ok(extraction) => {
                total_content_length += extraction.content.len();
                total_table_count += extraction.tables.len();
                successful += 1;

                let mut entry = serde_json::to_value(&extraction).unwrap_or(serde_json::json!({}));
                if let serde_json::Value::Object(ref mut map) = entry {
                    map.insert("file".into(), serde_json::json!(name));
                }
                results.push(entry);
            }
            Err(e) => {
                failed += 1;
                errors.push(serde_json::json!({
                    "file": name,
                    "error": e.to_string(),
                }));
            }
        }

        // Report progress after each file.
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

    let outputs = HashMap::from([
        ("results".into(), serde_json::json!(results)),
        ("total_files".into(), serde_json::json!(total)),
        ("successful".into(), serde_json::json!(successful)),
        ("failed".into(), serde_json::json!(failed)),
        ("errors".into(), serde_json::json!(errors)),
    ]);

    let metrics = MetricSummary {
        total_points: 6,
        metric_names: vec![
            "kreuzberg/total_extraction_time_ms".into(),
            "kreuzberg/total_files".into(),
            "kreuzberg/successful_files".into(),
            "kreuzberg/failed_files".into(),
            "kreuzberg/total_content_length".into(),
            "kreuzberg/total_table_count".into(),
        ],
        latest_values: HashMap::from([
            (
                "kreuzberg/total_extraction_time_ms".into(),
                duration.as_millis() as f64,
            ),
            ("kreuzberg/total_files".into(), total as f64),
            ("kreuzberg/successful_files".into(), successful as f64),
            ("kreuzberg/failed_files".into(), failed as f64),
            (
                "kreuzberg/total_content_length".into(),
                total_content_length as f64,
            ),
            (
                "kreuzberg/total_table_count".into(),
                total_table_count as f64,
            ),
        ]),
    };

    let stdout_tail = format!(
        "Extracted {successful}/{total} files ({failed} failed, {total_table_count} tables)"
    );

    let outcome = if failed > 0 && successful == 0 {
        ExecutionOutcome::BackendError {
            message: format!("all {failed} files failed extraction"),
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
            message: Some(format!("{successful}/{total} files extracted")),
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
// Output helpers
// ---------------------------------------------------------------------------

/// Build the standard outputs for a single extraction result.
///
/// Emits kreuzberg's native `ExtractionResult` shape 1:1 — every top-level
/// field becomes an output of the same name. No renaming, no derived counts,
/// no remapping. Consumers declare outputs using kreuzberg's vocabulary:
/// `content`, `mime_type`, `metadata`, `tables`, `detected_languages`, and
/// the optional `chunks`, `images`, `pages`, `elements`, `djot_content` when
/// the corresponding extraction config is enabled.
fn build_single_outputs(
    result: &kreuzberg::ExtractionResult,
    duration: std::time::Duration,
) -> (HashMap<String, serde_json::Value>, MetricSummary, String) {
    let value = serde_json::to_value(result).unwrap_or(serde_json::json!({}));
    let mut outputs: HashMap<String, serde_json::Value> = HashMap::new();
    if let serde_json::Value::Object(map) = value {
        for (k, v) in map {
            outputs.insert(k, v);
        }
    }

    let metrics = MetricSummary {
        total_points: 2,
        metric_names: vec![
            "kreuzberg/extraction_time_ms".into(),
            "kreuzberg/content_length".into(),
        ],
        latest_values: HashMap::from([
            (
                "kreuzberg/extraction_time_ms".into(),
                duration.as_millis() as f64,
            ),
            (
                "kreuzberg/content_length".into(),
                result.content.len() as f64,
            ),
        ]),
    };

    // Truncate content for stdout_tail (keep first 1024 chars).
    let stdout_tail = if result.content.len() > 1024 {
        format!(
            "{}... ({} chars total)",
            &result.content[..1024],
            result.content.len()
        )
    } else {
        result.content.clone()
    };

    (outputs, metrics, stdout_tail)
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

    fn make_job(spec: &ExecutionSpec) -> ExecutionJob {
        ExecutionJob {
            execution_id: format!(
                "kreuzberg-test-{}",
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
        let id = format!("kreuzberg-test-{}-{}", std::process::id(), seq);
        RunContext::for_test(
            id.clone(),
            spec,
            RunDirectory::new(&std::env::temp_dir(), &id),
            timeout,
        )
    }

    fn make_spec(config: serde_json::Value) -> ExecutionSpec {
        ExecutionSpec {
            backend: "kreuzberg".into(),
            inputs: vec![],
            outputs: vec![],
            config,
            config_ref: None,
        }
    }

    // -----------------------------------------------------------------------
    // Trait method tests
    // -----------------------------------------------------------------------

    #[test]
    fn name_returns_kreuzberg() {
        let backend = KreuzbergBackend::new();
        assert_eq!(backend.name(), "kreuzberg");
    }

    #[test]
    fn supports_kreuzberg_backend() {
        let backend = KreuzbergBackend::new();
        let spec = make_spec(serde_json::json!({}));
        assert!(backend.supports(&spec));
    }

    #[test]
    fn does_not_support_other_backends() {
        let backend = KreuzbergBackend::new();
        let spec = ExecutionSpec {
            backend: "process".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({}),
            config_ref: None,
        };
        assert!(!backend.supports(&spec));
    }

    // -----------------------------------------------------------------------
    // prepare() tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn prepare_single_mode_with_one_staged_input() {
        let backend = KreuzbergBackend::new();
        let spec = make_spec(serde_json::json!({}));
        let job = make_job(&spec);
        let mut ctx = make_run_context(spec, Duration::from_secs(30));
        ctx.staged_inputs
            .insert("file".into(), PathBuf::from("/tmp/test.pdf"));

        let ctx = backend.prepare(&job, ctx).await.unwrap();

        let resolved: ResolvedKreuzbergConfig =
            serde_json::from_value(ctx.backend_state.clone()).unwrap();
        assert!(resolved.target_file.is_some());
        assert_eq!(resolved.target_name.as_deref(), Some("file"));
        assert!(resolved.target_files.is_empty());
    }

    #[tokio::test]
    async fn prepare_batch_mode() {
        let backend = KreuzbergBackend::new();
        let spec = make_spec(serde_json::json!({ "mode": "batch" }));
        let job = make_job(&spec);
        let mut ctx = make_run_context(spec, Duration::from_secs(30));
        ctx.staged_inputs
            .insert("a".into(), PathBuf::from("/tmp/a.txt"));
        ctx.staged_inputs
            .insert("b".into(), PathBuf::from("/tmp/b.txt"));

        let ctx = backend.prepare(&job, ctx).await.unwrap();

        let resolved: ResolvedKreuzbergConfig =
            serde_json::from_value(ctx.backend_state.clone()).unwrap();
        assert!(resolved.target_file.is_none());
        assert_eq!(resolved.target_files.len(), 2);
    }

    #[tokio::test]
    async fn prepare_fails_with_ambiguous_inputs() {
        let backend = KreuzbergBackend::new();
        let spec = make_spec(serde_json::json!({}));
        let job = make_job(&spec);
        let mut ctx = make_run_context(spec, Duration::from_secs(30));
        ctx.staged_inputs
            .insert("a".into(), PathBuf::from("/tmp/a.txt"));
        ctx.staged_inputs
            .insert("b".into(), PathBuf::from("/tmp/b.txt"));

        let err = backend.prepare(&job, ctx).await.unwrap_err();
        assert!(matches!(err, ExecutorError::Config(_)));
    }

    #[tokio::test]
    async fn prepare_invalid_config_fails() {
        let backend = KreuzbergBackend::new();
        let spec = make_spec(serde_json::json!({ "mode": 42 }));
        let job = make_job(&spec);
        let mut ctx = make_run_context(spec, Duration::from_secs(30));
        ctx.staged_inputs
            .insert("file".into(), PathBuf::from("/tmp/test.pdf"));

        let err = backend.prepare(&job, ctx).await.unwrap_err();
        assert!(matches!(err, ExecutorError::Config(_)));
    }

    #[tokio::test]
    async fn prepare_explicit_file_not_in_staged_inputs() {
        let backend = KreuzbergBackend::new();
        let spec = make_spec(serde_json::json!({ "file": "missing" }));
        let job = make_job(&spec);
        let mut ctx = make_run_context(spec, Duration::from_secs(30));
        ctx.staged_inputs
            .insert("other".into(), PathBuf::from("/tmp/other.txt"));

        let err = backend.prepare(&job, ctx).await.unwrap_err();
        assert!(matches!(err, ExecutorError::Config(_)));
    }
}
