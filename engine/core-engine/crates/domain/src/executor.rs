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

/// Lifecycle status of an execution, normalized for Petri net signal routing.
///
/// Maps directly to `executor-domain::ExecutionStatus` values:
///
/// | Status    | Meaning                                          |
/// |-----------|--------------------------------------------------|
/// | Accepted  | Job received and queued by executor               |
/// | Running   | Backend started execution                         |
/// | Completed | Process exited with code 0                        |
/// | Failed    | Non-zero exit, killed by signal, or backend error |
/// | Cancelled | Cancelled via CancellationToken                   |
/// | TimedOut  | Exceeded timeout threshold                        |
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Accepted,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl ExecutionStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ExecutionStatus::Completed
                | ExecutionStatus::Failed
                | ExecutionStatus::Cancelled
                | ExecutionStatus::TimedOut
        )
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self, ExecutionStatus::Failed | ExecutionStatus::TimedOut)
    }

    /// All valid snake_case status names, for validating `EXECUTOR_SIGNAL_ROUTES`.
    pub const ALL_NAMES: &[&str] = &[
        "accepted",
        "running",
        "completed",
        "failed",
        "cancelled",
        "timed_out",
    ];

    pub fn as_str(&self) -> &str {
        match self {
            ExecutionStatus::Accepted => "accepted",
            ExecutionStatus::Running => "running",
            ExecutionStatus::Completed => "completed",
            ExecutionStatus::Failed => "failed",
            ExecutionStatus::Cancelled => "cancelled",
            ExecutionStatus::TimedOut => "timed_out",
        }
    }
}

/// Mid-execution event categories that can optionally be routed to signal places.
///
/// Maps directly to `executor-domain::EventCategory`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEventCategory {
    Artifact,
    Progress,
    Phase,
    Log,
    Output,
    Metric,
}

impl ExecutionEventCategory {
    /// All valid category names, for validating `EXECUTOR_EVENT_ROUTES`.
    pub const ALL_NAMES: &[&str] = &["artifact", "progress", "phase", "log", "output", "metric"];

    pub fn as_str(&self) -> &str {
        match self {
            Self::Artifact => "artifact",
            Self::Progress => "progress",
            Self::Phase => "phase",
            Self::Log => "log",
            Self::Output => "output",
            Self::Metric => "metric",
        }
    }
}

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

    /// Human-readable name for this client (e.g., "executor-nats").
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_status_terminal() {
        assert!(!ExecutionStatus::Accepted.is_terminal());
        assert!(!ExecutionStatus::Running.is_terminal());
        assert!(ExecutionStatus::Completed.is_terminal());
        assert!(ExecutionStatus::Failed.is_terminal());
        assert!(ExecutionStatus::Cancelled.is_terminal());
        assert!(ExecutionStatus::TimedOut.is_terminal());
    }

    #[test]
    fn test_execution_status_retryable() {
        assert!(!ExecutionStatus::Accepted.is_retryable());
        assert!(!ExecutionStatus::Running.is_retryable());
        assert!(!ExecutionStatus::Completed.is_retryable());
        assert!(ExecutionStatus::Failed.is_retryable());
        assert!(!ExecutionStatus::Cancelled.is_retryable());
        assert!(ExecutionStatus::TimedOut.is_retryable());
    }

    #[test]
    fn test_execution_status_serialization() {
        let status = ExecutionStatus::Completed;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"completed\"");

        let deserialized: ExecutionStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, ExecutionStatus::Completed);
    }

    #[test]
    fn test_as_str_matches_serde() {
        let statuses = [
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
            ExecutionStatus::Failed,
            ExecutionStatus::Cancelled,
            ExecutionStatus::TimedOut,
        ];
        for status in &statuses {
            let serde_str = serde_json::to_value(status)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(
                status.as_str(),
                serde_str,
                "as_str() must match serde for {:?}",
                status
            );
        }
    }

    #[test]
    fn test_all_names_matches_variants() {
        let statuses = [
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
            ExecutionStatus::Failed,
            ExecutionStatus::Cancelled,
            ExecutionStatus::TimedOut,
        ];
        let names: Vec<&str> = statuses.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            ExecutionStatus::ALL_NAMES,
            "ALL_NAMES must match as_str() for every variant"
        );
    }

    #[test]
    fn test_event_category_all_names() {
        let categories = [
            ExecutionEventCategory::Artifact,
            ExecutionEventCategory::Progress,
            ExecutionEventCategory::Phase,
            ExecutionEventCategory::Log,
            ExecutionEventCategory::Output,
            ExecutionEventCategory::Metric,
        ];
        let names: Vec<&str> = categories.iter().map(|c| c.as_str()).collect();
        assert_eq!(names, ExecutionEventCategory::ALL_NAMES);
    }

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
