//! [`ExecutionBackend`] implementation for the file-ops backend.
//!
//! Registers as `"file_ops"` and dispatches to the appropriate operation
//! handler based on the `"operation"` key in the job config. See [`crate::config`]
//! for the full schema of each operation.

use std::collections::HashMap;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError,
    RunContext,
};

use crate::config::FileOpsConfig;
use crate::ops;

/// Backend that executes general-purpose file operations via OpenDAL.
///
/// Stateless dispatcher — each operation config carries its own
/// [`StorageConfig`](aithericon_executor_storage::StorageConfig) and operators
/// are built on-the-fly. This means the backend itself holds no state and a
/// single instance can serve all storage backends concurrently.
///
/// The lifecycle follows the standard executor contract:
/// 1. [`prepare()`](ExecutionBackend::prepare) — deserializes and validates the
///    config, storing the parsed [`FileOpsConfig`] in `backend_state`.
/// 2. [`execute()`](ExecutionBackend::execute) — runs a three-way
///    `tokio::select!` (cancellation, timeout, operation dispatch).
///    Operation errors become [`ExecutionOutcome::BackendError`], not `Err`.
pub struct FileOpsBackend;

impl FileOpsBackend {
    /// Create a new stateless file-ops backend instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileOpsBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionBackend for FileOpsBackend {
    async fn prepare(
        &self,
        _job: &ExecutionJob,
        mut run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // Resolve {{input:NAME}} patterns in the raw config JSON before
        // deserializing into typed config. Staged inputs are already
        // populated by the StageInputsHook at this point.
        let mut raw_config = run_context.spec.config.clone();
        crate::resolve::resolve_inputs(&mut raw_config, &run_context.staged_inputs)
            .map_err(|e| ExecutorError::Config(format!("file_ops input resolution: {e}")))?;

        // Deserialize resolved config
        let config: FileOpsConfig = serde_json::from_value(raw_config).map_err(|e| {
            ExecutorError::Config(format!("invalid file_ops backend config: {e}"))
        })?;

        ops::validate(&config)
            .map_err(|e| ExecutorError::Config(format!("file_ops validation: {e}")))?;

        // Store validated config in backend_state for execute()
        run_context.backend_state = serde_json::to_value(&config).map_err(|e| {
            ExecutorError::Config(format!("failed to serialize file_ops config: {e}"))
        })?;
        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let config: FileOpsConfig =
            serde_json::from_value(run_context.backend_state.clone()).map_err(|e| {
                ExecutorError::Config(format!("failed to deserialize file_ops config: {e}"))
            })?;

        let start = tokio::time::Instant::now();

        // Report Running status with operation info
        let op_name = match &config {
            FileOpsConfig::Probe(_) => "probe",
            FileOpsConfig::Copy(_) => "copy",
            FileOpsConfig::Move(_) => "move",
            FileOpsConfig::Delete(_) => "delete",
            FileOpsConfig::Annotate(_) => "annotate",
            FileOpsConfig::List(_) => "list",
            FileOpsConfig::Stat(_) => "stat",
        };

        status_cb(
            ExecutionStatus::Running,
            serde_json::json!({ "operation": op_name }),
        )
        .await;

        debug!(operation = op_name, "dispatching file_ops operation");

        // Three-way select: cancellation, timeout, or operation
        tokio::select! { biased;
            _ = cancel.cancelled() => {
                Ok(ExecutionResult {
                    outcome: ExecutionOutcome::Cancelled,
                    duration: start.elapsed(),
                    stdout_tail: None,
                    stderr_tail: None,
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
                    stderr_tail: None,
                    artifact_manifest: None,
                    outputs: HashMap::new(),
                    progress: None,
                    run_dir: Some(run_context.run_dir.clone()),
                    metrics: None,
                    logs: None,
                })
            },
            result = ops::dispatch(&config, &run_context.run_dir.artifacts_dir) => {
                let duration = start.elapsed();
                match result {
                    Ok(outputs) => {
                        Ok(ExecutionResult {
                            outcome: ExecutionOutcome::Success,
                            duration,
                            stdout_tail: None,
                            stderr_tail: None,
                            artifact_manifest: None,
                            outputs,
                            progress: None,
                            run_dir: Some(run_context.run_dir.clone()),
                            metrics: None,
                            logs: None,
                        })
                    },
                    Err(e) => {
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
                            logs: None,
                        })
                    },
                }
            },
        }
    }

    fn name(&self) -> &'static str {
        "file_ops"
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "file_ops"
    }
}
