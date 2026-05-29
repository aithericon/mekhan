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
use aithericon_executor_storage::StorageConfig;

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
///
/// The only state held is an optional `default_storage`, used as a fallback
/// for the `probe` operation when it omits `storage` (e.g. compiler-injected
/// probes against the platform's own object store) — mirroring
/// `InputSource::StoragePath { storage: Option<_> }`.
#[derive(Default)]
pub struct FileOpsBackend {
    default_storage: Option<StorageConfig>,
}

impl FileOpsBackend {
    /// Create a new file-ops backend instance with no default storage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the default storage used as a fallback for `probe` operations
    /// that omit their own `storage` config — typically the executor's
    /// globally-configured object store.
    pub fn with_default_storage(mut self, storage: Option<StorageConfig>) -> Self {
        self.default_storage = storage;
        self
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
        let mut config: FileOpsConfig = serde_json::from_value(raw_config).map_err(|e| {
            ExecutorError::Config(format!("invalid file_ops backend config: {e}"))
        })?;

        // Overlay any workspace-resource bindings (storage.resource_alias)
        // before validation — `validate` checks for empty endpoint/bucket
        // on S3 and we want those to be filled from the resource first.
        crate::resource_overlay::overlay_file_ops_resources(&mut config, &run_context)?;

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
        _event_stream: Option<std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>>,
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
                Ok(ExecutionResult::cancelled(
                    start.elapsed(),
                    Some(run_context.run_dir.clone()),
                    None,
                    None,
                ))
            },
            _ = tokio::time::sleep(run_context.timeout) => {
                Ok(ExecutionResult::timed_out(
                    start.elapsed(),
                    Some(run_context.run_dir.clone()),
                    None,
                    None,
                ))
            },
            result = ops::dispatch(&config, &run_context.run_dir.artifacts_dir, self.default_storage.as_ref()) => {
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
