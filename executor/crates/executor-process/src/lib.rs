pub mod child;

#[cfg(test)]
mod tests;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_backend::SandboxConfig;
use aithericon_executor_backend::DEFAULT_MAX_OUTPUT_BYTES;
use aithericon_executor_domain::{
    ExecutionJob, ExecutionResult, ExecutionSpec, ExecutorError, RunContext,
};

// Re-export config type from the shared configs crate.
pub use aithericon_executor_backend_configs::process::ProcessConfig;

/// Backend that executes process jobs via fork+exec.
pub struct ProcessBackend {
    max_output_bytes: usize,
    /// When set, child commands are wrapped in nsjail. Default `None`
    /// (unsandboxed) so existing behavior is unchanged unless wired in.
    sandbox: Option<SandboxConfig>,
}

impl ProcessBackend {
    pub fn new() -> Self {
        Self {
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            sandbox: None,
        }
    }

    pub fn with_max_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }

    /// Wrap executed commands in nsjail using the given sandbox config.
    pub fn with_sandbox(mut self, cfg: SandboxConfig) -> Self {
        self.sandbox = Some(cfg);
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
        _event_stream: Option<std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let process_config = ProcessConfig::from_spec(&run_context.spec)?;
        child::run_process(
            &process_config,
            run_context,
            self.max_output_bytes,
            &status_cb,
            cancel,
            self.sandbox.as_ref(),
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
