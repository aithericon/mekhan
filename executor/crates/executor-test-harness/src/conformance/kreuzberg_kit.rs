//! Factory trait for the Kreuzberg document-extraction backend conformance
//! tests.
//!
//! Kreuzberg backends differ from process-style backends: no stdout/stderr,
//! no exit codes, no env vars. Failures surface as `BackendError` from
//! `execute()` (or `Config` from `prepare()`), not `ExitFailure`. This trait
//! provides extraction-specific spec factories + file-staging helpers so the
//! shared test functions can drive any kreuzberg-backed implementation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_domain::{ExecutionJob, ExecutionSpec, JobPriority, RunContext};

#[async_trait]
pub trait KreuzbergTestKit: Send + Sync {
    /// Human-readable name for test output (e.g., "kreuzberg").
    fn backend_name(&self) -> &'static str;

    /// Create the backend instance for testing.
    async fn create_backend(&self) -> Result<Arc<dyn ExecutionBackend>, String>;

    /// Returns `Some("reason")` if tests should be skipped.
    async fn skip_reason(&self) -> Option<String> {
        None
    }

    // ─── Spec factories ──────────────────────────────────────────────

    /// Spec for single-file extraction of the default staged input.
    /// The corresponding `RunContext.staged_inputs` will carry one entry
    /// keyed `"file"` (see [`stage_single_text_file`]).
    fn single_extract_spec(&self) -> ExecutionSpec;

    /// Spec for batch extraction across all staged inputs.
    fn batch_extract_spec(&self) -> ExecutionSpec;

    /// Spec referencing an input name that won't exist in staged_inputs —
    /// must fail at `prepare()` or `execute()` with a clean error, not panic.
    fn missing_input_spec(&self) -> ExecutionSpec;

    // ─── RunContext lifecycle ────────────────────────────────────────

    /// Build a RunContext with a single text file staged as input `"file"`.
    /// Returns the run context + the temp file handle (caller must hold it
    /// until the test completes; dropping it deletes the underlying file).
    async fn stage_single_text_file(
        &self,
        spec: ExecutionSpec,
        timeout: Duration,
        content: &str,
    ) -> (RunContext, tempfile::NamedTempFile);

    /// Build a RunContext with N text files staged as `"file_0".."file_{N-1}"`.
    /// Returns the run context + the file handles (caller must hold them).
    async fn stage_batch_text_files(
        &self,
        spec: ExecutionSpec,
        timeout: Duration,
        contents: &[&str],
    ) -> (RunContext, Vec<tempfile::NamedTempFile>);

    /// Build a RunContext with no staged inputs (for missing-input tests).
    async fn make_empty_run_context(&self, spec: ExecutionSpec, timeout: Duration) -> RunContext;

    /// Cleanup after a backend-level test (best-effort).
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
