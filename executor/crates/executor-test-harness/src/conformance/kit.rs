use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_domain::{ExecutionJob, ExecutionSpec, JobPriority, RunContext};

/// Factory trait that backends implement to participate in conformance testing.
///
/// Each method returns backend-appropriate specs for a specific test scenario.
/// The conformance tests call these factories, execute, and assert the contract.
#[async_trait]
pub trait BackendTestKit: Send + Sync {
    /// Human-readable name for test output (e.g., "process", "docker").
    fn backend_name(&self) -> &'static str;

    /// Create the backend instance for testing.
    async fn create_backend(&self) -> Result<Arc<dyn ExecutionBackend>, String>;

    /// Returns `Some("reason")` if tests should be skipped (e.g., Docker unavailable).
    async fn skip_reason(&self) -> Option<String> {
        None
    }

    // ─── Spec factories ──────────────────────────────────────────────

    /// Spec that prints "hello" to stdout and exits 0.
    fn echo_spec(&self) -> ExecutionSpec;

    /// Spec that exits with code 1.
    fn failing_spec(&self) -> ExecutionSpec;

    /// Spec that sleeps for `secs` seconds.
    fn sleep_spec(&self, secs: u64) -> ExecutionSpec;

    /// Spec that prints "stdout_marker" to stdout and "stderr_marker" to stderr.
    fn dual_output_spec(&self) -> ExecutionSpec;

    /// Spec that prints the value of `$CONFORMANCE_TEST_VAR` to stdout.
    fn env_echo_spec(&self) -> ExecutionSpec;

    /// Spec that produces `bytes` bytes of output to stdout.
    fn large_output_spec(&self, bytes: usize) -> ExecutionSpec;

    // ─── RunContext lifecycle ────────────────────────────────────────

    /// Build a RunContext for a backend-level test.
    async fn make_run_context(
        &self,
        spec: ExecutionSpec,
        timeout: Duration,
        env: HashMap<String, String>,
    ) -> RunContext;

    /// Cleanup after a backend-level test.
    async fn cleanup_run_context(&self, ctx: &RunContext);

    // ─── Pipeline-level helpers ─────────────────────────────────────

    /// Any pre-test setup (e.g., pulling Docker images).
    async fn pipeline_setup(&self) -> Result<(), String> {
        Ok(())
    }

    /// Convert a spec into a full `ExecutionJob`.
    fn spec_to_job(
        &self,
        eid: &str,
        spec: ExecutionSpec,
        timeout: Option<Duration>,
    ) -> ExecutionJob {
        ExecutionJob {
            execution_id: eid.to_string(),
            spec,
            metadata: HashMap::new(),
            timeout,
            priority: JobPriority::Medium,
            stream_events: None,
            feed_chunks: false,
            wrapped_secrets: None,
        }
    }
}
