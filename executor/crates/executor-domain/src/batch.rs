use serde::{Deserialize, Serialize};

use crate::job::ExecutionJob;
use crate::status::ExecutionStatus;

/// A batch manifest defines a set of jobs to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BatchManifest {
    /// Jobs to execute, in order.
    pub jobs: Vec<ExecutionJob>,
}

/// Summary of a batch execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BatchResult {
    /// Total number of jobs in the manifest.
    pub total: usize,
    /// Number of jobs that completed successfully.
    pub succeeded: usize,
    /// Number of jobs that failed (including timed out, cancelled).
    pub failed: usize,
    /// Per-job results, in execution order.
    pub results: Vec<JobResult>,
}

/// Result of a single job within a batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct JobResult {
    pub execution_id: String,
    pub status: ExecutionStatus,
    pub duration_ms: u128,
    pub detail: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionSpec, JobPriority};
    use std::collections::HashMap;

    fn sample_job(eid: &str) -> ExecutionJob {
        ExecutionJob {
            execution_id: eid.to_string(),
            workspace_id: String::new(),
            spec: ExecutionSpec {
                backend: "process".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({"command": "echo", "args": ["hello"]}),
                config_ref: None,
            },
            metadata: HashMap::new(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: None,
            feed_chunks: false,
            channels: Vec::new(),
            wrapped_secrets: None,
        }
    }

    #[test]
    fn batch_manifest_serde_roundtrip() {
        let manifest = BatchManifest {
            jobs: vec![sample_job("job-1"), sample_job("job-2")],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: BatchManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.jobs.len(), 2);
        assert_eq!(deserialized.jobs[0].execution_id, "job-1");
        assert_eq!(deserialized.jobs[1].execution_id, "job-2");
    }

    #[test]
    fn batch_result_serde_roundtrip() {
        let result = BatchResult {
            total: 3,
            succeeded: 2,
            failed: 1,
            results: vec![
                JobResult {
                    execution_id: "j1".into(),
                    status: ExecutionStatus::Completed,
                    duration_ms: 100,
                    detail: serde_json::json!({}),
                },
                JobResult {
                    execution_id: "j2".into(),
                    status: ExecutionStatus::Completed,
                    duration_ms: 200,
                    detail: serde_json::json!({}),
                },
                JobResult {
                    execution_id: "j3".into(),
                    status: ExecutionStatus::Failed,
                    duration_ms: 50,
                    detail: serde_json::json!({"error": "exit code 1"}),
                },
            ],
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: BatchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total, 3);
        assert_eq!(deserialized.succeeded, 2);
        assert_eq!(deserialized.failed, 1);
        assert_eq!(deserialized.results.len(), 3);
    }

    #[test]
    fn job_result_serde_roundtrip() {
        let result = JobResult {
            execution_id: "test-1".into(),
            status: ExecutionStatus::TimedOut,
            duration_ms: 60_000,
            detail: serde_json::json!({"outcome": "timed_out"}),
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: JobResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.execution_id, "test-1");
        assert_eq!(deserialized.status, ExecutionStatus::TimedOut);
        assert_eq!(deserialized.duration_ms, 60_000);
    }
}
