use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use aithericon_executor_domain::{
    ExecutionJob, ExecutionResult, ExecutionSpec, ExecutionStatus, ExecutorError, RunContext,
};

/// Callback invoked by backends to report mid-execution status updates.
///
/// The backend calls this to report transitions like Running (with pid).
/// The callback handles publishing to NATS — backends never touch NATS directly.
pub type StatusCallback =
    Box<dyn Fn(ExecutionStatus, Value) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Trait for execution backends. Each backend knows how to execute
/// one or more `ExecutionSpec` types based on the `backend` field.
#[async_trait]
pub trait ExecutionBackend: Send + Sync + 'static {
    /// Backend-specific preparation. Called AFTER shared staging hooks.
    ///
    /// Default: no-op, returns ctx unchanged.
    async fn prepare(
        &self,
        _job: &ExecutionJob,
        run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        Ok(run_context)
    }

    /// Execute within the prepared context.
    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError>;

    /// Human-readable backend name (e.g., "process", "docker").
    fn name(&self) -> &'static str;

    /// Whether this backend can handle the given spec variant.
    fn supports(&self, spec: &ExecutionSpec) -> bool;
}
