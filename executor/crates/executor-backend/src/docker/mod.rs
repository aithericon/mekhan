pub mod container;
#[cfg(test)]
mod tests;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::info;

use aithericon_executor_domain::{ExecutionJob, ExecutionResult, ExecutionSpec, ExecutorError, RunContext};

use crate::traits::{ExecutionBackend, StatusCallback};

/// Default max output capture: 64 KB per stream.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Container-internal mount point for the run directory.
pub const CONTAINER_RUN_DIR: &str = "/aithericon";

// Re-export config types from the shared configs crate.
pub use aithericon_executor_backend_configs::docker::{DockerConfig, PullPolicy, ResourceLimits};

/// Backend that executes jobs inside Docker containers.
pub struct DockerBackend {
    client: bollard::Docker,
    max_output_bytes: usize,
}

impl DockerBackend {
    /// Create a new DockerBackend connecting to the default Docker daemon.
    ///
    /// On Unix, connects via `/var/run/docker.sock`.
    /// On Windows, connects via named pipe.
    /// Respects `DOCKER_HOST` env var.
    pub fn new() -> Result<Self, ExecutorError> {
        let client = bollard::Docker::connect_with_local_defaults().map_err(|e| {
            ExecutorError::Config(format!("failed to connect to Docker daemon: {e}"))
        })?;
        Ok(Self {
            client,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        })
    }

    /// Create a DockerBackend with a specific bollard client (for testing).
    pub fn with_client(client: bollard::Docker) -> Self {
        Self {
            client,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_max_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }
}

#[async_trait]
impl ExecutionBackend for DockerBackend {
    async fn prepare(
        &self,
        _job: &ExecutionJob,
        mut ctx: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        let config = DockerConfig::from_spec(&ctx.spec)?;

        // Pull image based on pull_policy
        container::ensure_image(&self.client, &config.image, config.pull_policy).await?;

        // Mark that we've done docker-specific preparation
        ctx.backend_state = serde_json::json!({ "docker_prepared": true });

        info!(image = %config.image, "docker image ready");
        Ok(ctx)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let config = DockerConfig::from_spec(&run_context.spec)?;
        container::run_container(
            &self.client,
            &config,
            run_context,
            self.max_output_bytes,
            &status_cb,
            cancel,
        )
        .await
    }

    fn name(&self) -> &'static str {
        "docker"
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "docker"
    }
}
