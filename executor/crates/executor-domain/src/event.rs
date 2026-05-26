use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::artifact::ArtifactCategory;
use crate::llm::{LlmStopReason, LlmToolCall, LlmUsage};
use crate::metrics::MetricType;
use crate::progress::PhaseStatus;
use crate::result::ExecutionOutcome;

/// Category of an execution event (maps to NATS subject suffix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    Artifact,
    Progress,
    Phase,
    Log,
    Output,
    Metric,
    AgentTurn,
}

impl EventCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Artifact => "artifact",
            Self::Progress => "progress",
            Self::Phase => "phase",
            Self::Log => "log",
            Self::Output => "output",
            Self::Metric => "metric",
            Self::AgentTurn => "agent_turn",
        }
    }
}

impl std::fmt::Display for EventCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Event collected during staging, emitted after StreamContext is available.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StagedEvent {
    pub category: EventCategory,
    pub detail: StatusDetail,
}

/// Typed detail for status updates and execution events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum StatusDetail {
    /// Job accepted and queued.
    Accepted {},

    /// Execution started.
    Running {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },

    /// An artifact was logged by the child process.
    ArtifactLogged {
        artifact_id: String,
        name: String,
        category: ArtifactCategory,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        storage_path: Option<String>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        metadata: HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_metadata: Option<serde_json::Value>,
    },

    /// An input artifact was consumed (staged from storage).
    ArtifactConsumed {
        input_name: String,
        storage_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_bytes: Option<u64>,
    },

    /// Progress was updated.
    ProgressUpdated {
        fraction: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default)]
        current_step: u64,
        #[serde(default)]
        total_steps: u64,
    },

    /// A phase changed status.
    PhaseChanged {
        phase_name: String,
        status: PhaseStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// A structured log message from the child.
    LogMessage {
        level: String,
        message: String,
        #[serde(default)]
        fields: HashMap<String, String>,
    },

    /// An output value was set by the child.
    OutputSet {
        name: String,
        value: serde_json::Value,
    },

    /// Metrics were logged by the child process (end-of-execution summary).
    MetricsLogged {
        /// Number of metric points in this batch.
        count: u64,
        /// Distinct metric names in this batch.
        metric_names: Vec<String>,
    },

    /// A single metric data point streamed in real-time.
    MetricPointLogged {
        name: String,
        value: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u64>,
        #[serde(default)]
        metric_type: MetricType,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        labels: HashMap<String, String>,
    },

    /// Logs were forwarded by the child process.
    LogsForwarded {
        /// Total number of log entries forwarded.
        count: u64,
        /// Count of warn/error entries.
        warn_error_count: u64,
    },

    /// Execution completed successfully.
    Completed {
        outcome: ExecutionOutcome,
        duration_ms: u64,
    },

    /// Execution failed.
    Failed {
        outcome: ExecutionOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        duration_ms: u64,
    },

    /// One turn of an agent loop completed. The executor emits this on
    /// `executor.events.{exec_id}.agent_turn` for every LLM call that
    /// is part of an agent context (signalled by `metadata.agent_node_id`).
    /// Single-shot LLM AutomatedSteps do NOT emit this.
    AgentTurn {
        turn: u32,
        stop_reason: LlmStopReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<LlmToolCall>,
        usage: LlmUsage,
    },
}

/// Envelope for mid-execution events published to NATS.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExecutionEvent {
    /// The execution this event belongs to.
    pub execution_id: String,

    /// Event category (maps to NATS subject suffix).
    pub category: EventCategory,

    /// Typed event detail.
    pub detail: StatusDetail,

    /// Metadata echoed from the execution job.
    #[serde(default)]
    pub metadata: HashMap<String, String>,

    /// Which executor instance produced this event.
    pub source: String,

    /// When this event was produced.
    pub timestamp: DateTime<Utc>,

    /// Monotonically increasing sequence number per execution.
    pub sequence: u64,
}

impl ExecutionEvent {
    /// Build the NATS subject for this event.
    /// Pattern: `executor.events.{execution_id}.{category}`
    pub fn subject(&self) -> String {
        format!(
            "executor.events.{}.{}",
            crate::status::sanitize_subject_token(&self.execution_id),
            self.category.as_str()
        )
    }

    /// Deterministic message ID for JetStream dedup.
    ///
    /// Artifact, output, phase, and metric-point events derive their key from
    /// the event's stable content (artifact_id, output name, phase name, metric
    /// name) so an apalis redelivery that re-runs the job emits the same
    /// Nats-Msg-Id even though the per-execution `sequence` counter restarts at
    /// 0 and can drift with IPC streaming timing. Summary/streaming events
    /// (progress, log summaries, metric batches, raw log messages) legitimately
    /// multi-fire and fall back to the sequence-based ID.
    pub fn msg_id(&self) -> String {
        match &self.detail {
            StatusDetail::ArtifactLogged { artifact_id, .. } => {
                format!("{}-artifact-{}", self.execution_id, artifact_id)
            }
            StatusDetail::ArtifactConsumed { input_name, .. } => {
                format!("{}-artifact_in-{}", self.execution_id, input_name)
            }
            StatusDetail::OutputSet { name, .. } => {
                format!("{}-output-{}", self.execution_id, name)
            }
            StatusDetail::PhaseChanged { phase_name, status, .. } => {
                let status_str = match status {
                    crate::progress::PhaseStatus::Pending => "pending",
                    crate::progress::PhaseStatus::Running => "running",
                    crate::progress::PhaseStatus::Completed => "completed",
                    crate::progress::PhaseStatus::Failed => "failed",
                    crate::progress::PhaseStatus::Skipped => "skipped",
                };
                format!("{}-phase-{}-{}", self.execution_id, phase_name, status_str)
            }
            StatusDetail::MetricPointLogged { name, step, .. } => match step {
                Some(s) => format!("{}-metric_pt-{}-{}", self.execution_id, name, s),
                None => format!(
                    "{}-{}-{}",
                    self.execution_id,
                    self.category.as_str(),
                    self.sequence
                ),
            },
            _ => format!(
                "{}-{}-{}",
                self.execution_id,
                self.category.as_str(),
                self.sequence
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_detail_serde_roundtrip() {
        let details = vec![
            StatusDetail::Accepted {},
            StatusDetail::Running { pid: Some(1234) },
            StatusDetail::ArtifactLogged {
                artifact_id: "art-1".into(),
                name: "model.pt".into(),
                category: ArtifactCategory::Model,
                size_bytes: Some(1024),
                mime_type: None,
                storage_path: None,
                metadata: HashMap::new(),
                file_metadata: None,
            },
            StatusDetail::ArtifactConsumed {
                input_name: "dataset.csv".into(),
                storage_path: "artifacts/run-1/dataset.csv".into(),
                size_bytes: Some(2048),
            },
            StatusDetail::ProgressUpdated {
                fraction: 0.75,
                message: Some("Training".into()),
                current_step: 75,
                total_steps: 100,
            },
            StatusDetail::PhaseChanged {
                phase_name: "training".into(),
                status: PhaseStatus::Completed,
                message: None,
            },
            StatusDetail::LogMessage {
                level: "info".into(),
                message: "started training".into(),
                fields: HashMap::from([("epoch".into(), "1".into())]),
            },
            StatusDetail::OutputSet {
                name: "accuracy".into(),
                value: serde_json::json!(0.95),
            },
            StatusDetail::MetricsLogged {
                count: 42,
                metric_names: vec!["train/loss".into(), "accuracy".into()],
            },
            StatusDetail::MetricPointLogged {
                name: "train/loss".into(),
                value: 0.42,
                step: Some(100),
                metric_type: MetricType::Scalar,
                labels: HashMap::from([("run".into(), "alpha".into())]),
            },
            StatusDetail::LogsForwarded {
                count: 100,
                warn_error_count: 5,
            },
            StatusDetail::Completed {
                outcome: ExecutionOutcome::Success,
                duration_ms: 5000,
            },
            StatusDetail::Failed {
                outcome: ExecutionOutcome::ExitFailure { exit_code: 1 },
                error: Some("segfault".into()),
                duration_ms: 1000,
            },
        ];

        for detail in &details {
            let json = serde_json::to_string(detail).unwrap();
            let deserialized: StatusDetail = serde_json::from_str(&json).unwrap();
            // Verify round-trip produces valid JSON
            let _ = serde_json::to_string(&deserialized).unwrap();
        }
    }

    #[test]
    fn execution_event_subject_and_msg_id() {
        let event = ExecutionEvent {
            execution_id: "train-alpha-0".into(),
            category: EventCategory::Artifact,
            detail: StatusDetail::ArtifactLogged {
                artifact_id: "art-1".into(),
                name: "model.pt".into(),
                category: ArtifactCategory::Model,
                size_bytes: None,
                mime_type: None,
                storage_path: None,
                metadata: HashMap::new(),
                file_metadata: None,
            },
            metadata: Default::default(),
            source: "exec-1".into(),
            timestamp: Utc::now(),
            sequence: 42,
        };

        assert_eq!(event.subject(), "executor.events.train-alpha-0.artifact");
        // msg_id is content-addressable for artifacts so the id is stable
        // across apalis redeliveries where the per-execution sequence resets.
        assert_eq!(event.msg_id(), "train-alpha-0-artifact-art-1");
    }

    #[test]
    fn msg_id_is_stable_for_artifact_across_sequences() {
        let make = |sequence| ExecutionEvent {
            execution_id: "exec-42".into(),
            category: EventCategory::Artifact,
            detail: StatusDetail::ArtifactLogged {
                artifact_id: "observation.json".into(),
                name: "observation.json".into(),
                category: ArtifactCategory::Dataset,
                size_bytes: Some(100),
                mime_type: None,
                storage_path: Some("s3://bucket/obs".into()),
                metadata: HashMap::new(),
                file_metadata: None,
            },
            metadata: Default::default(),
            source: "exec-1".into(),
            timestamp: Utc::now(),
            sequence,
        };
        assert_eq!(make(0).msg_id(), make(7).msg_id());
    }

    #[test]
    fn msg_id_is_stable_for_output_across_sequences() {
        let make = |sequence| ExecutionEvent {
            execution_id: "exec-42".into(),
            category: EventCategory::Output,
            detail: StatusDetail::OutputSet {
                name: "model_meta".into(),
                value: serde_json::json!({"score": 0.1}),
            },
            metadata: Default::default(),
            source: "exec-1".into(),
            timestamp: Utc::now(),
            sequence,
        };
        assert_eq!(make(1).msg_id(), make(9).msg_id());
    }

    #[test]
    fn msg_id_is_unique_per_emit_for_progress() {
        let make = |sequence| ExecutionEvent {
            execution_id: "exec-42".into(),
            category: EventCategory::Progress,
            detail: StatusDetail::ProgressUpdated {
                fraction: 0.5,
                message: None,
                current_step: 5,
                total_steps: 10,
            },
            metadata: Default::default(),
            source: "exec-1".into(),
            timestamp: Utc::now(),
            sequence,
        };
        assert_ne!(make(1).msg_id(), make(2).msg_id());
    }

    #[test]
    fn event_category_display() {
        assert_eq!(EventCategory::Artifact.to_string(), "artifact");
        assert_eq!(EventCategory::Progress.to_string(), "progress");
        assert_eq!(EventCategory::Phase.to_string(), "phase");
        assert_eq!(EventCategory::Log.to_string(), "log");
        assert_eq!(EventCategory::Output.to_string(), "output");
        assert_eq!(EventCategory::Metric.to_string(), "metric");
    }
}
