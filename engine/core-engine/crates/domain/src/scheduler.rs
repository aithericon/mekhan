//! Scheduler contract types for external job schedulers (Nomad, Slurm, etc.).
//!
//! Defines the abstract interface between the Petri engine and external compute
//! schedulers. The engine uses effect handlers (Side 1) for imperative commands
//! (submit, cancel, query) and signal injection via NATS (Side 2) for reactive
//! observations (job completed, failed, lost).

use serde::{Deserialize, Serialize};

/// Abstract job lifecycle status, normalized across scheduler implementations.
///
/// Maps to scheduler-native states:
///
/// | Abstract   | Nomad                          | Slurm                            |
/// |------------|--------------------------------|----------------------------------|
/// | Queued     | pending allocation             | PENDING, CONFIGURING             |
/// | Running    | running allocation             | RUNNING                          |
/// | Completed  | dead + exit_code=0             | COMPLETED                        |
/// | Failed     | dead + exit_code!=0            | FAILED, NODE_FAIL, OUT_OF_MEMORY |
/// | Cancelled  | dead + DesiredStatus=stop      | CANCELLED                        |
/// | TimedOut   | (timeout event)                | TIMEOUT                          |
/// | Lost       | (alloc GC'd)                   | (disappeared from sacct)         |
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Accepted by scheduler, waiting for resources.
    Queued,
    /// Actively executing on allocated resources.
    Running,
    /// Completed successfully (exit code 0).
    Completed,
    /// Failed (non-zero exit, OOM, crash).
    Failed,
    /// Cancelled by the engine or operator.
    Cancelled,
    /// Wall-clock limit exceeded.
    TimedOut,
    /// Scheduler lost track of the job.
    Lost,
}

impl JobStatus {
    /// Whether this status represents a terminal state (job is done).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobStatus::Completed
                | JobStatus::Failed
                | JobStatus::Cancelled
                | JobStatus::TimedOut
                | JobStatus::Lost
        )
    }

    /// Whether the failure is retryable (infrastructure issue, not the job's fault).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            JobStatus::Failed | JobStatus::TimedOut | JobStatus::Lost
        )
    }

    /// All valid snake_case status names, matching `as_str()` output.
    ///
    /// Used for validating `SCHEDULER_SIGNAL_ROUTES` configuration at startup.
    pub const ALL_NAMES: &[&str] = &[
        "queued",
        "running",
        "completed",
        "failed",
        "cancelled",
        "timed_out",
        "lost",
    ];

    /// String representation matching `#[serde(rename_all = "snake_case")]`.
    ///
    /// Used by NomadWatcher for per-status signal routing lookup.
    pub fn as_str(&self) -> &str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
            JobStatus::TimedOut => "timed_out",
            JobStatus::Lost => "lost",
        }
    }
}

/// Request to submit a job to an external scheduler.
///
/// The effect handler builds this from the consumed input tokens and passes
/// it to the `SchedulerClient`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitRequest {
    /// Reference to job template in the adapter's external storage (DB, config).
    /// The adapter resolves this ID to a full job specification.
    pub job_template_id: String,

    /// Signal key: `"{job_id}:{run}"` — unique per submission attempt.
    /// Used to match status signals from the external bridge.
    pub signal_key: String,

    /// Engine-assigned execution id. Threaded through to the executor so that
    /// per-job consumer modes (sbatch one-shot) can target the right job's
    /// NATS subject. Stamped by the scheduler-net submit handler before the
    /// scheduler dispatch and the downstream executor publish, both of which
    /// must agree on this id.
    pub execution_id: String,

    /// Token data from the triggering token — dynamic per-job context
    /// (model_name, hyperparams, etc.) that the adapter merges with the template.
    pub token_data: serde_json::Value,
}

/// Result of a successful job submission.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitResult {
    /// Scheduler-native job ID (Nomad job ID, Slurm integer job ID).
    /// Stored in the event log for replay and used for cancellation.
    pub scheduler_job_id: String,
}

/// Signal message published to NATS when an external system changes state.
///
/// Generic across scheduler backends (Nomad, Slurm, K8s, etc.).
/// Published to `petri.signal.{net_id}.{place_name}` by the watcher component.
/// The `payload` becomes the token color when injected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalSignal {
    /// Source system identifier (e.g., "nomad", "slurm", "k8s").
    pub source: String,
    /// Signal key matching what `submit()` stored (e.g., "train-alpha:0").
    pub signal_key: String,
    /// Signal payload — becomes the token color when injected.
    pub payload: serde_json::Value,
    /// When the signal was generated.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Deterministic dedup identifier for this specific signal event.
    /// One-shot publishers (slurm/nomad watchers) set this to a stable id
    /// like `"slurm:{job}:{status}"` so a redelivered or re-detected signal
    /// is suppressed at the engine. Streaming publishers (executor metrics,
    /// logs, phases, progress) leave this `None` — every emit is a new token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_id: Option<String>,
}

/// Errors from scheduler operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SchedulerError {
    /// Submission failed (scheduler rejected the job). Retryable.
    #[error("Submission failed: {0}")]
    SubmissionFailed(String),

    /// Fatal error (bad template, invalid config). Not retryable.
    #[error("Fatal scheduler error: {0}")]
    Fatal(String),

    /// Cancellation failed.
    #[error("Cancellation failed: {0}")]
    CancellationFailed(String),

    /// Query failed.
    #[error("Status query failed: {0}")]
    QueryFailed(String),

    /// Client not connected.
    #[error("Scheduler client not connected: {0}")]
    NotConnected(String),
}

/// Abstraction over different scheduler backends (Nomad, Slurm, Mock).
///
/// Implementations handle the specifics of each scheduler's API.
/// The engine interacts with this trait through effect handlers.
#[async_trait::async_trait]
pub trait SchedulerClient: Send + Sync {
    /// Submit a job to the scheduler.
    ///
    /// The client resolves `job_template_id` from its own storage, merges
    /// with `token_data`, and submits to the scheduler API.
    async fn submit(&self, request: SubmitRequest) -> Result<SubmitResult, SchedulerError>;

    /// Cancel a running job.
    async fn cancel(&self, scheduler_job_id: &str) -> Result<(), SchedulerError>;

    /// Query the current status of a job.
    async fn status(&self, scheduler_job_id: &str) -> Result<JobStatus, SchedulerError>;

    /// Human-readable name for this client (e.g., "nomad", "slurm", "mock").
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_status_terminal() {
        assert!(!JobStatus::Queued.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
        assert!(JobStatus::Completed.is_terminal());
        assert!(JobStatus::Failed.is_terminal());
        assert!(JobStatus::Cancelled.is_terminal());
        assert!(JobStatus::TimedOut.is_terminal());
        assert!(JobStatus::Lost.is_terminal());
    }

    #[test]
    fn test_job_status_retryable() {
        assert!(!JobStatus::Queued.is_retryable());
        assert!(!JobStatus::Running.is_retryable());
        assert!(!JobStatus::Completed.is_retryable());
        assert!(JobStatus::Failed.is_retryable());
        assert!(!JobStatus::Cancelled.is_retryable());
        assert!(JobStatus::TimedOut.is_retryable());
        assert!(JobStatus::Lost.is_retryable());
    }

    #[test]
    fn test_job_status_serialization() {
        let status = JobStatus::Completed;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"completed\"");

        let deserialized: JobStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, JobStatus::Completed);
    }

    #[test]
    fn test_as_str_matches_serde() {
        let statuses = [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
            JobStatus::TimedOut,
            JobStatus::Lost,
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
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
            JobStatus::TimedOut,
            JobStatus::Lost,
        ];
        let names: Vec<&str> = statuses.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            names,
            JobStatus::ALL_NAMES,
            "ALL_NAMES must match as_str() for every variant"
        );
    }

    #[test]
    fn test_submit_request_serialization() {
        let request = SubmitRequest {
            job_template_id: "gpu-training-v2".into(),
            signal_key: "train-alpha:0".into(),
            execution_id: "exec-abc123".into(),
            token_data: serde_json::json!({
                "model_name": "ResNet-50",
                "hyperparams": { "lr": 0.001 }
            }),
        };

        let json = serde_json::to_string(&request).unwrap();
        let deserialized: SubmitRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.job_template_id, "gpu-training-v2");
        assert_eq!(deserialized.signal_key, "train-alpha:0");
        assert_eq!(deserialized.execution_id, "exec-abc123");
    }

    #[test]
    fn test_submit_result_serialization() {
        let result = SubmitResult {
            scheduler_job_id: "nomad-alloc-abc123".into(),
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: SubmitResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.scheduler_job_id, "nomad-alloc-abc123");
    }

    #[test]
    fn test_external_signal_roundtrip() {
        let signal = ExternalSignal {
            source: "nomad".to_string(),
            signal_key: "train-alpha:0".to_string(),
            payload: serde_json::json!({
                "scheduler_job_id": "my-job/dispatch-123",
                "job_status": "completed",
                "exit_code": 0,
            }),
            timestamp: chrono::Utc::now(),
            dedup_id: None,
        };

        let json = serde_json::to_string(&signal).unwrap();
        let parsed: ExternalSignal = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.source, "nomad");
        assert_eq!(parsed.signal_key, "train-alpha:0");
        assert_eq!(parsed.payload["job_status"], "completed");
    }

    #[test]
    fn test_external_signal_null_payload() {
        let signal = ExternalSignal {
            source: "test".to_string(),
            signal_key: "key".to_string(),
            payload: serde_json::Value::Null,
            timestamp: chrono::Utc::now(),
            dedup_id: None,
        };

        let json = serde_json::to_string(&signal).unwrap();
        let parsed: ExternalSignal = serde_json::from_str(&json).unwrap();
        assert!(parsed.payload.is_null());
    }
}
