use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::artifact::ArtifactManifest;
use crate::logs::LogSummary;
use crate::metrics::MetricSummary;
use crate::progress::Progress;
use crate::run_dir::RunDirectory;

/// The outcome of a backend execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExecutionResult {
    /// What happened.
    pub outcome: ExecutionOutcome,

    /// Wall-clock duration of the execution.
    #[serde(with = "crate::serde_duration")]
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub duration: Duration,

    /// Last N bytes of stdout (ring-buffer captured).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,

    /// Last N bytes of stderr (ring-buffer captured).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,

    /// Artifact manifest collected from IPC sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_manifest: Option<ArtifactManifest>,

    /// Output values collected from IPC sidecar.
    #[serde(default)]
    pub outputs: HashMap<String, serde_json::Value>,

    /// Final progress state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,

    /// Run directory used for this execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_dir: Option<RunDirectory>,

    /// Metrics summary collected from IPC sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<MetricSummary>,

    /// Log summary collected from IPC sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<LogSummary>,
}

/// Discriminated outcome of an execution attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionOutcome {
    /// Process exited with code 0.
    Success,

    /// Process exited with non-zero code.
    ExitFailure { exit_code: i32 },

    /// Process was killed by a signal.
    Signal { signal: i32 },

    /// Execution exceeded the timeout.
    TimedOut,

    /// Backend-level error (e.g., spawn failure).
    BackendError { message: String },

    /// Execution was cancelled via CancellationToken.
    Cancelled,
}

impl ExecutionResult {
    /// Construct a result for an execution cancelled via its
    /// `CancellationToken`. Backends share this so the early-return arm of
    /// their `select!` is identical: only the wall-clock `duration`, the run
    /// directory, an optional `stderr_tail` note, and an optional `progress`
    /// snapshot vary.
    pub fn cancelled(
        duration: Duration,
        run_dir: Option<RunDirectory>,
        stderr_tail: Option<String>,
        progress: Option<Progress>,
    ) -> Self {
        Self {
            outcome: ExecutionOutcome::Cancelled,
            duration,
            stdout_tail: None,
            stderr_tail,
            artifact_manifest: None,
            outputs: HashMap::new(),
            progress,
            run_dir,
            metrics: None,
            logs: None,
        }
    }

    /// Construct a result for an execution that exceeded its timeout. See
    /// [`ExecutionResult::cancelled`] for the rationale.
    pub fn timed_out(
        duration: Duration,
        run_dir: Option<RunDirectory>,
        stderr_tail: Option<String>,
        progress: Option<Progress>,
    ) -> Self {
        Self {
            outcome: ExecutionOutcome::TimedOut,
            duration,
            stdout_tail: None,
            stderr_tail,
            artifact_manifest: None,
            outputs: HashMap::new(),
            progress,
            run_dir,
            metrics: None,
            logs: None,
        }
    }
}

impl ExecutionOutcome {
    /// Whether this outcome should be reported as Completed (vs Failed/TimedOut/Cancelled).
    pub fn to_status(&self) -> crate::ExecutionStatus {
        match self {
            Self::Success => crate::ExecutionStatus::Completed,
            Self::ExitFailure { .. } | Self::Signal { .. } | Self::BackendError { .. } => {
                crate::ExecutionStatus::Failed
            }
            Self::TimedOut => crate::ExecutionStatus::TimedOut,
            Self::Cancelled => crate::ExecutionStatus::Cancelled,
        }
    }
}
