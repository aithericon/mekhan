//! Executor contract types for the aithericon-executor integration.
//!
//! Defines the abstract interface between the Petri engine and the execution
//! backend. The engine uses effect handlers (Side 1) for imperative commands
//! (submit, cancel) and signal injection via NATS (Side 2) for reactive
//! observations (execution accepted, running, completed, failed, etc.).
//!
//! Unlike `SchedulerClient` which manages allocation lifecycle, `ExecutorClient`
//! drives actual task execution on an allocated resource. In a layered topology:
//! Workflow Net -> Scheduler Net (allocation) -> Executor Net (execution).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Request to submit an execution job to the executor.
///
/// The effect handler builds this from consumed input tokens and passes
/// it to the `ExecutorClient`. The `token_data` carries the execution
/// specification as opaque JSON -- the client implementation interprets
/// it to build the concrete `ExecutionJob`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionSubmitRequest {
    /// Signal key: `"{job_id}:{run}"` -- unique per submission attempt.
    /// Used to match status signals from the executor watcher.
    pub signal_key: String,

    /// Token data from the triggering token -- contains the execution spec
    /// (command, backend, inputs, outputs, config) and any runtime parameters.
    pub token_data: serde_json::Value,

    /// Per-job signal route overrides (status name → scoped place ID).
    ///
    /// When present, these override the global `EXECUTOR_SIGNAL_ROUTES` config
    /// in the routing metadata stamped into the job. This allows scoped place
    /// names (e.g., `"exec/sig_completed"`) when the executor lifecycle is
    /// instantiated inside a `scoped_prefix`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_routes: Option<HashMap<String, String>>,

    /// Per-job event route overrides (category → scoped place ID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_routes: Option<HashMap<String, String>>,

    /// Caller-assigned execution id. When `Some`, the client uses this id
    /// (instead of generating one) so upstream coordinators (e.g. scheduler-net's
    /// SlurmClient that already stamped the same id into sbatch's
    /// `EXECUTOR_TARGET_EXEC_ID`) and downstream consumers agree on the value.
    /// Leave `None` for the legacy auto-generate behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,

    /// Per-job executor namespace override. When Some, the client publishes to
    /// {namespace}.{prio}.{exec_id} instead of its construction-time fixed
    /// namespace, so a leased body can target a lease-scoped queue
    /// (lease-<grant_id>) drained by a persistent executor. None → fixed default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    /// Opt-in for the INbound live chunk feed (the "live IPC reducer").
    /// Chunks are published to `executor.chunks.{execution_id}`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub feed_chunks: bool,
    }

    fn is_false(b: &bool) -> bool {
    !*b
    }

    /// Result of a successful execution submission.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionSubmitResult {
    /// Executor-assigned execution ID. Stored in the event log for replay
    /// and used for cancellation and signal correlation.
    pub execution_id: String,
}

/// Errors from executor operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ExecutorError {
    /// Submission failed (executor rejected the job). Retryable.
    #[error("Submission failed: {0}")]
    SubmissionFailed(String),

    /// Fatal error (bad spec, invalid config). Not retryable.
    #[error("Fatal executor error: {0}")]
    Fatal(String),

    /// Cancellation failed.
    #[error("Cancellation failed: {0}")]
    CancellationFailed(String),

    /// Client not connected.
    #[error("Executor client not connected: {0}")]
    NotConnected(String),
}

/// Abstraction over the executor backend.
///
/// Implementations handle the specifics of submitting execution jobs
/// and cancelling running executions. The engine interacts with this
/// trait through effect handlers.
#[async_trait::async_trait]
pub trait ExecutorClient: Send + Sync {
    /// Submit an execution job to the executor.
    ///
    /// The client interprets `token_data` from the request to build the
    /// concrete execution job, stamps routing metadata, and publishes
    /// to the executor's job stream.
    async fn submit(
        &self,
        request: ExecutionSubmitRequest,
    ) -> Result<ExecutionSubmitResult, ExecutorError>;

    /// Cancel a running execution.
    async fn cancel(&self, execution_id: &str) -> Result<(), ExecutorError>;

    /// Feed a data chunk into a running reducer job.
    ///
    /// The client publishes to `executor.chunks.{execution_id}` with the
    /// provided value and sequence.
    async fn feed_chunk(
        &self,
        execution_id: &str,
        value: serde_json::Value,
        sequence: u64,
        is_eof: bool,
    ) -> Result<(), ExecutorError>;

    /// Human-readable name for this client (e.g., "executor-nats").
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_submit_request_serialization() {
        let request = ExecutionSubmitRequest {
            signal_key: "train-alpha:0".into(),
            token_data: serde_json::json!({
                "backend": "process",
                "config": {
                    "command": "python3",
                    "args": ["train.py"]
                }
            }),
            signal_routes: None,
            event_routes: None,
            execution_id: None,
            namespace: None,
        };

        let json = serde_json::to_string(&request).unwrap();
        let deserialized: ExecutionSubmitRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.signal_key, "train-alpha:0");
    }

    #[test]
    fn test_submit_result_serialization() {
        let result = ExecutionSubmitResult {
            execution_id: "exec-abc123".into(),
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: ExecutionSubmitResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.execution_id, "exec-abc123");
    }
}
