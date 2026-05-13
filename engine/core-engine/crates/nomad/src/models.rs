//! Minimal Nomad API serde models.
//!
//! Covers the subset needed for parameterized job dispatch, cancellation,
//! status queries, and event stream observation. All use `#[serde(rename_all = "PascalCase")]`
//! to match Nomad's JSON format.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ==================== Dispatch (Parameterized Jobs) ====================

/// Request body for `POST /v1/job/{id}/dispatch`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct DispatchJobRequest {
    /// Base64-encoded payload (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// Metadata key-value pairs merged into the dispatched job.
    pub meta: HashMap<String, String>,
}

/// Response from `POST /v1/job/{id}/dispatch`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DispatchJobResponse {
    /// The ID of the dispatched (child) job.
    #[serde(rename = "DispatchedJobID")]
    pub dispatched_job_id: String,
    /// Evaluation ID created by the dispatch.
    #[serde(default, rename = "EvalID")]
    pub eval_id: String,
    /// Raft index.
    #[serde(default)]
    pub index: u64,
}

// ==================== Job (Status / Cancellation) ====================

/// Minimal job representation from `GET /v1/job/{id}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Job {
    /// Job ID.
    #[serde(default, rename = "ID")]
    pub id: String,
    /// Job name.
    #[serde(default)]
    pub name: String,
    /// Job status: "pending", "running", "dead".
    #[serde(default)]
    pub status: String,
    /// Whether the job was stopped.
    #[serde(default)]
    pub stop: bool,
    /// Job metadata.
    #[serde(default)]
    pub meta: HashMap<String, String>,
    /// Task groups (for extracting exit codes from dead jobs).
    #[serde(default)]
    pub task_groups: Vec<TaskGroup>,
}

/// Minimal task group for extracting task state from job queries.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TaskGroup {
    /// Task group name.
    #[serde(default)]
    pub name: String,
    /// Tasks within this group.
    #[serde(default)]
    pub tasks: Vec<Task>,
}

/// Minimal task for extracting name (used to match config.task_name).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Task {
    /// Task name.
    #[serde(default)]
    pub name: String,
}

/// Response from `DELETE /v1/job/{id}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct JobStopResponse {
    /// Evaluation ID created by the stop.
    #[serde(default, rename = "EvalID")]
    pub eval_id: String,
}

// ==================== Event Stream ====================

/// Top-level ndjson line from `GET /v1/event/stream`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EventStreamData {
    /// Raft index for this batch of events.
    #[serde(default)]
    pub index: u64,
    /// Events in this batch.
    #[serde(default)]
    pub events: Vec<EventStreamEntry>,
}

/// A single event entry from the event stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EventStreamEntry {
    /// Topic (e.g., "Allocation").
    #[serde(default)]
    pub topic: String,
    /// Event type (e.g., "AllocationUpdated").
    #[serde(default, rename = "Type")]
    pub type_field: String,
    /// Key (usually the allocation ID).
    #[serde(default)]
    pub key: String,
    /// Event payload.
    #[serde(default)]
    pub payload: EventPayload,
}

/// Event payload containing the allocation.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EventPayload {
    /// The allocation that changed.
    #[serde(default)]
    pub allocation: Option<Allocation>,
}

/// Nomad allocation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Allocation {
    /// Allocation ID.
    #[serde(default, rename = "ID")]
    pub id: String,
    /// Parent job ID.
    #[serde(default, rename = "JobID")]
    pub job_id: String,
    /// Client status: "pending", "running", "complete", "failed", "lost".
    #[serde(default)]
    pub client_status: String,
    /// Desired status: "run", "stop", "evict".
    #[serde(default)]
    pub desired_status: String,
    /// Task states keyed by task name.
    #[serde(default)]
    pub task_states: HashMap<String, TaskState>,
    /// Node ID where the allocation is placed.
    #[serde(default, rename = "NodeID")]
    pub node_id: String,
    /// Node name.
    #[serde(default)]
    pub node_name: String,
    /// The job spec (for accessing meta tags).
    #[serde(default)]
    pub job: Option<AllocationJob>,
}

/// Minimal job embedded in allocation events (for meta tag access).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AllocationJob {
    /// Job ID.
    #[serde(default, rename = "ID")]
    pub id: String,
    /// Job metadata containing petri routing tags.
    #[serde(default)]
    pub meta: HashMap<String, String>,
}

/// State of a task within an allocation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TaskState {
    /// Current state: "pending", "running", "dead".
    #[serde(default)]
    pub state: String,
    /// Whether the task failed.
    #[serde(default)]
    pub failed: bool,
    /// Task events (state transitions).
    #[serde(default)]
    pub events: Vec<TaskEvent>,
}

/// A task lifecycle event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TaskEvent {
    /// Event type: "Received", "Driver", "Started", "Terminated", "Killed", etc.
    #[serde(default, rename = "Type")]
    pub type_field: String,
    /// Exit code (relevant for "Terminated" events).
    #[serde(default)]
    pub exit_code: i32,
    /// Human-readable message.
    #[serde(default)]
    pub display_message: String,
    /// Event timestamp (Unix nanos).
    #[serde(default)]
    pub time: i64,
}

// ==================== Petri Meta Tag Constants ====================
// Re-exported from petri-scheduler-bridge for backward compatibility.

pub use petri_scheduler_bridge::meta::{
    parse_signal_meta_key, signal_meta_key, META_NET_ID, META_PLACE, META_SIGNAL_KEY, META_SIGNAL_PREFIX,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatch_request_serialization() {
        let mut meta = HashMap::new();
        meta.insert("petri_net_id".to_string(), "gpu-resource".to_string());
        meta.insert("petri_place".to_string(), "status_inbox".to_string());

        let req = DispatchJobRequest {
            payload: None,
            meta,
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"Meta\""));
        assert!(json.contains("petri_net_id"));
    }

    #[test]
    fn test_dispatch_response_deserialization() {
        let json = r#"{
            "DispatchedJobID": "my-job/dispatch-1234",
            "EvalID": "eval-abc",
            "Index": 42
        }"#;

        let resp: DispatchJobResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.dispatched_job_id, "my-job/dispatch-1234");
        assert_eq!(resp.eval_id, "eval-abc");
        assert_eq!(resp.index, 42);
    }

    #[test]
    fn test_job_deserialization_minimal() {
        let json = r#"{
            "ID": "my-job",
            "Name": "my-job",
            "Status": "running",
            "Stop": false,
            "Meta": {"petri_net_id": "test-net"}
        }"#;

        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.id, "my-job");
        assert_eq!(job.status, "running");
        assert!(!job.stop);
        assert_eq!(job.meta.get("petri_net_id").unwrap(), "test-net");
    }

    #[test]
    fn test_allocation_deserialization() {
        let json = r#"{
            "ID": "alloc-123",
            "JobID": "my-job",
            "ClientStatus": "running",
            "DesiredStatus": "run",
            "TaskStates": {
                "petri-worker": {
                    "State": "running",
                    "Failed": false,
                    "Events": [
                        {
                            "Type": "Started",
                            "ExitCode": 0,
                            "DisplayMessage": "Task started",
                            "Time": 1700000000000000000
                        }
                    ]
                }
            },
            "NodeID": "node-1",
            "NodeName": "worker-1",
            "Job": {
                "ID": "my-job",
                "Meta": {
                    "petri_net_id": "gpu-resource",
                    "petri_place": "status_inbox",
                    "petri_signal_key": "train-alpha:0"
                }
            }
        }"#;

        let alloc: Allocation = serde_json::from_str(json).unwrap();
        assert_eq!(alloc.id, "alloc-123");
        assert_eq!(alloc.client_status, "running");

        let job = alloc.job.unwrap();
        assert_eq!(job.meta.get("petri_net_id").unwrap(), "gpu-resource");
        assert_eq!(job.meta.get("petri_place").unwrap(), "status_inbox");

        let task_state = alloc.task_states.get("petri-worker").unwrap();
        assert_eq!(task_state.state, "running");
        assert_eq!(task_state.events[0].type_field, "Started");
    }

    #[test]
    fn test_event_stream_data_deserialization() {
        let json = r#"{
            "Index": 100,
            "Events": [
                {
                    "Topic": "Allocation",
                    "Type": "AllocationUpdated",
                    "Key": "alloc-123",
                    "Payload": {
                        "Allocation": {
                            "ID": "alloc-123",
                            "JobID": "my-job",
                            "ClientStatus": "complete",
                            "DesiredStatus": "run",
                            "TaskStates": {},
                            "NodeID": "node-1",
                            "NodeName": "worker-1"
                        }
                    }
                }
            ]
        }"#;

        let data: EventStreamData = serde_json::from_str(json).unwrap();
        assert_eq!(data.index, 100);
        assert_eq!(data.events.len(), 1);
        assert_eq!(data.events[0].topic, "Allocation");

        let alloc = data.events[0].payload.allocation.as_ref().unwrap();
        assert_eq!(alloc.client_status, "complete");
    }

    #[test]
    fn test_event_stream_missing_fields_use_defaults() {
        let json = r#"{"Index": 1, "Events": [{"Topic": "Allocation", "Payload": {}}]}"#;

        let data: EventStreamData = serde_json::from_str(json).unwrap();
        assert_eq!(data.events[0].type_field, "");
        assert!(data.events[0].payload.allocation.is_none());
    }

    #[test]
    fn test_task_event_terminated() {
        let json = r#"{
            "Type": "Terminated",
            "ExitCode": 1,
            "DisplayMessage": "Exit Code: 1",
            "Time": 1700000000000000000
        }"#;

        let event: TaskEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.type_field, "Terminated");
        assert_eq!(event.exit_code, 1);
    }
}
