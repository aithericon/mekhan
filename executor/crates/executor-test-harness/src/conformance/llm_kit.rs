use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_domain::{
    ExecutionJob, ExecutionSpec, JobPriority, RunContext, RunDirectory,
};

/// Factory trait that LLM backends implement to participate in conformance testing.
///
/// LLM backends differ from process-style backends: no stdout echo, no env vars,
/// no exit codes. Errors produce `BackendError` rather than `ExitFailure`.
/// This trait (and its tests) mirror the pattern of `FileOpsTestKit`.
#[async_trait]
pub trait LlmTestKit: Send + Sync {
    /// Human-readable name for test output (e.g., "llm").
    fn backend_name(&self) -> &'static str;

    /// Create the backend instance for testing.
    async fn create_backend(&self) -> Result<Arc<dyn ExecutionBackend>, String>;

    /// Returns `Some("reason")` if tests should be skipped (e.g., Docker/Ollama unavailable).
    async fn skip_reason(&self) -> Option<String> {
        None
    }

    // ─── Spec factories ──────────────────────────────────────────────

    /// Spec for a basic chat prompt that should succeed.
    fn chat_spec(&self) -> ExecutionSpec;

    /// Spec for extract mode with a valid output_schema.
    fn extract_spec(&self) -> ExecutionSpec;

    /// Spec for extract mode WITHOUT output_schema (should fail at prepare).
    fn extract_no_schema_spec(&self) -> ExecutionSpec;

    /// Spec with malformed config that should fail at prepare (deserialization).
    fn invalid_config_spec(&self) -> ExecutionSpec;

    /// Spec with valid config but nonexistent model (should produce BackendError at execute).
    fn api_error_spec(&self) -> ExecutionSpec;

    // ─── RunContext lifecycle ────────────────────────────────────────

    /// Build a RunContext for a backend-level test.
    async fn make_run_context(
        &self,
        spec: ExecutionSpec,
        timeout: Duration,
        env: HashMap<String, String>,
    ) -> RunContext {
        let execution_id = format!("llm-conform-{}", uuid::Uuid::new_v4());
        let mut ctx = RunContext::for_test(
            execution_id.clone(),
            spec,
            RunDirectory::new(&PathBuf::from("/tmp"), &execution_id),
            timeout,
        );
        ctx.env = env;
        ctx
    }

    /// Cleanup after a backend-level test.
    async fn cleanup_run_context(&self, _ctx: &RunContext) {}

    /// Convert a spec into a full `ExecutionJob`.
    fn spec_to_job(
        &self,
        eid: &str,
        spec: ExecutionSpec,
        timeout: Option<Duration>,
    ) -> ExecutionJob {
        ExecutionJob {
            execution_id: eid.to_string(),
            workspace_id: String::new(),
            spec,
            metadata: HashMap::new(),
            timeout,
            priority: JobPriority::Medium,
            stream_events: None,
            feed_chunks: false,
            channels: Vec::new(),
            wrapped_secrets: None,
        }
    }
}
