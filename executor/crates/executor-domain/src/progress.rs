use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Status of an execution phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

/// A named phase within an execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
pub struct Phase {
    /// Phase name (unique within an execution).
    pub name: String,

    /// Current status of this phase.
    pub status: PhaseStatus,

    /// Optional message describing what this phase is doing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// When this phase started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,

    /// When this phase ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
}

/// Progress information for an execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema, utoipa::ToSchema))]
pub struct Progress {
    /// Overall fraction complete (0.0 to 1.0).
    pub fraction: f64,

    /// Human-readable progress message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Current step number.
    #[serde(default)]
    pub current_step: u64,

    /// Total number of steps (0 if unknown).
    #[serde(default)]
    pub total_steps: u64,

    /// Phases within this execution.
    #[serde(default)]
    pub phases: Vec<Phase>,

    /// When progress was last updated.
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_serde_roundtrip() {
        let progress = Progress {
            fraction: 0.5,
            message: Some("Training epoch 5/10".into()),
            current_step: 5,
            total_steps: 10,
            phases: vec![Phase {
                name: "training".into(),
                status: PhaseStatus::Running,
                message: Some("Epoch 5".into()),
                started_at: Some(Utc::now()),
                ended_at: None,
            }],
            updated_at: Utc::now(),
        };

        let json = serde_json::to_string(&progress).unwrap();
        let deserialized: Progress = serde_json::from_str(&json).unwrap();
        assert!((deserialized.fraction - 0.5).abs() < f64::EPSILON);
        assert_eq!(deserialized.current_step, 5);
        assert_eq!(deserialized.phases.len(), 1);
        assert_eq!(deserialized.phases[0].status, PhaseStatus::Running);
    }

    #[test]
    fn phase_status_serde() {
        for status in [
            PhaseStatus::Pending,
            PhaseStatus::Running,
            PhaseStatus::Completed,
            PhaseStatus::Failed,
            PhaseStatus::Skipped,
        ] {
            let json = serde_json::to_value(status).unwrap();
            let deserialized: PhaseStatus = serde_json::from_value(json).unwrap();
            assert_eq!(status, deserialized);
        }
    }
}
