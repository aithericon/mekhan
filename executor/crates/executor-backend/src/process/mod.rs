pub mod child;
pub mod stream;

#[cfg(test)]
mod tests;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use aithericon_executor_domain::{
    ExecutionJob, ExecutionResult, ExecutionSpec, ExecutorError, RunContext,
};

use crate::traits::{ExecutionBackend, StatusCallback};

/// Default max output capture: 64 KB per stream.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;

// Re-export config type from the shared configs crate.
pub use aithericon_executor_backend_configs::process::ProcessConfig;

/// Backend that executes process jobs via fork+exec.
pub struct ProcessBackend {
    max_output_bytes: usize,
}

impl ProcessBackend {
    pub fn new() -> Self {
        Self {
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_max_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }
}

impl Default for ProcessBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionBackend for ProcessBackend {
    async fn prepare(
        &self,
        _job: &ExecutionJob,
        run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let process_config = ProcessConfig::from_spec(&run_context.spec)?;
        child::run_process(
            &process_config,
            run_context,
            self.max_output_bytes,
            &status_cb,
            cancel,
        )
        .await
    }

    fn name(&self) -> &'static str {
        "process"
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "process"
    }
}
