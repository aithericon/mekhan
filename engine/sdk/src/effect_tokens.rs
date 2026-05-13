//! Typed token types for built-in effect handlers.
//!
//! Each built-in effect handler (`scheduler_submit`, `scheduler_cancel`,
//! `executor_submit`, `executor_cancel`, `timer_schedule`, `timer_cancel`,
//! `human_task`, `human_cancel`) has an implicit contract for its input and
//! output token shapes. This module provides typed structs that make those
//! contracts explicit, with the standard lifecycle fields used across the
//! three-layer composition (job → scheduler → executor).
//!
//! # Scheduler effect tokens
//!
//! ```ignore
//! let job_inbox = ctx.bridge_in_from::<SchedulerSubmitInput>(...);
//! let submitted = ctx.state::<SchedulerSubmitted>("submitted", "Submitted");
//! ```
//!
//! # Executor effect tokens
//!
//! ```ignore
//! let exec_queue = ctx.state::<ExecutorSubmitInput>("exec_queue", "Queue");
//! let submitted = ctx.state::<ExecutorSubmitted>("submitted", "Submitted");
//! ```
//!
//! # Timer effect tokens
//!
//! ```ignore
//! let timer_data = ctx.state::<TimerInput>("timer_data", "Timer Data");
//! let timer_scheduled = ctx.state::<TimerScheduled>("timer_scheduled", "Scheduled");
//! let timer_to_cancel = ctx.state::<TimerCancelInput>("cancel_data", "Cancel Data");
//! let timer_cancelled = ctx.state::<TimerCancelled>("cancelled", "Cancelled");
//! ```
//!
//! # Human task effect tokens
//!
//! For the human task *input*, use [`HumanTaskRequest`] directly — it IS the
//! handler's input contract (`title`, `instructions_mdsvex`, `steps`, etc.).
//! If your workflow needs extra fields merged into the output (e.g., an
//! `invoice_number` for downstream correlation), define a custom `#[token]`
//! struct that includes the base fields plus your extras.
//!
//! ```ignore
//! // Simple case — use HumanTaskRequest directly:
//! let form = ctx.state::<HumanTaskRequest>("form", "Task Form");
//!
//! // Workflow-specific case — extend with extra fields:
//! #[token]
//! struct ReviewForm {
//!     title: String,
//!     instructions_mdsvex: String,
//!     steps: Vec<TaskStep>,
//!     invoice_number: String,  // merged into output by handler
//! }
//! ```
//!
//! Human task *output* and *cancel* tokens have fixed contracts:
//!
//! ```ignore
//! let task = ctx.state::<HumanTaskAssigned>("task", "Assigned Task");
//! let cancel_input = ctx.state::<HumanCancelInput>("cancel", "Cancel Input");
//! let cancelled = ctx.state::<HumanTaskCancelled>("cancelled", "Cancelled");
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ─── Scheduler effect tokens ────────────────────────────────────────────────

/// Input to the `scheduler_submit` effect handler.
///
/// Submits a job to the scheduler backend (Nomad, Slurm, or mock). The handler
/// reads `job_id`, `model_name`, and `spec` to construct the scheduler request.
/// Lifecycle fields (`run`, `retries`, `max_retries`) are passed through to
/// the output for downstream retry logic.
///
/// This type is also used by the job net as its internal job representation,
/// since the fields are identical to what the scheduler expects.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SchedulerSubmitInput {
    /// Unique job identifier for correlation across all layers.
    pub job_id: String,
    /// Human-readable model/job name (used by scheduler for job naming).
    pub model_name: String,
    /// Execution attempt number (0-indexed, incremented on retry).
    pub run: i64,
    /// Number of retries so far.
    pub retries: i64,
    /// Maximum retries before dead-lettering.
    pub max_retries: i64,
    /// Optional per-job scheduler template override. When `Some`, the handler
    /// dispatches to this template ID instead of its configured default. Enables
    /// routing individual jobs to different Nomad parameterized job templates
    /// (e.g., a GPU-enabled template for a subset of jobs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_template_id: Option<String>,
    /// Execution specification forwarded to the executor (opaque to scheduler).
    pub spec: serde_json::Value,
}

/// Output from the `scheduler_submit` effect handler.
///
/// Contains all input fields plus the `scheduler_job_id` assigned by the
/// scheduler backend. This ID is used to correlate scheduler status signals
/// (running, completed, failed) back to the submitted job.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SchedulerSubmitted {
    /// Job identifier (echoed from input).
    pub job_id: String,
    /// Model name (echoed from input).
    pub model_name: String,
    /// Execution attempt (echoed from input).
    pub run: i64,
    /// Retries so far (echoed from input).
    pub retries: i64,
    /// Max retries (echoed from input).
    pub max_retries: i64,
    /// Scheduler-assigned job ID for status correlation.
    pub scheduler_job_id: String,
    /// Engine-stamped execution id. Authoritatively set by the scheduler
    /// submit handler and propagated through the bridge into the executor
    /// net so the dispatch (e.g. sbatch's `EXECUTOR_TARGET_EXEC_ID`) and the
    /// executor's NATS publish target the same PerJob consumer.
    pub execution_id: String,
    /// Execution spec (echoed from input).
    pub spec: serde_json::Value,
}

/// Input to the `scheduler_cancel` effect handler.
///
/// Cancels a previously submitted scheduler job.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SchedulerCancelInput {
    /// Scheduler job ID to cancel (from `SchedulerSubmitted.scheduler_job_id`).
    pub scheduler_job_id: String,
}

/// Output from the `scheduler_cancel` effect handler.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SchedulerCancelled {
    /// Scheduler job ID that was cancelled.
    pub scheduler_job_id: String,
}

/// Scheduler status signal delivered by NomadWatcher or SlurmWatcher.
///
/// Tagged on the `source` field to discriminate between backends.
/// Both variants share `scheduler_job_id` and `job_status` — use the
/// accessor methods for backend-agnostic access.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source")]
pub enum SchedulerStatusSignal {
    /// Signal from the NomadWatcher (task-level or allocation-level).
    #[serde(rename = "nomad")]
    Nomad {
        /// Scheduler-assigned job ID for correlation.
        scheduler_job_id: String,
        /// Job status: running, completed, failed, timed_out, cancelled.
        job_status: String,
        /// Nomad allocation ID.
        allocation_id: String,
        /// Task exit code (null when unavailable).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
        /// Task event display message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// Nomad node ID.
        node_id: String,
        /// Nomad node name.
        node_name: String,
        /// Nomad client status (allocation-level fallback only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_status: Option<String>,
    },
    /// Signal from the SlurmWatcher.
    #[serde(rename = "slurm")]
    Slurm {
        /// Scheduler-assigned job ID for correlation.
        scheduler_job_id: String,
        /// Job status: running, completed, failed, timed_out.
        job_status: String,
        /// Slurm-formatted exit code (e.g., "0:0").
        exit_code: String,
        /// Comma-separated list of allocated nodes.
        node_list: String,
    },
}

impl SchedulerStatusSignal {
    /// Get the scheduler job ID (shared across backends).
    pub fn scheduler_job_id(&self) -> &str {
        match self {
            Self::Nomad { scheduler_job_id, .. } => scheduler_job_id,
            Self::Slurm { scheduler_job_id, .. } => scheduler_job_id,
        }
    }

    /// Get the job status string (shared across backends).
    pub fn job_status(&self) -> &str {
        match self {
            Self::Nomad { job_status, .. } => job_status,
            Self::Slurm { job_status, .. } => job_status,
        }
    }
}

// ─── Executor effect tokens ─────────────────────────────────────────────────

/// Input to the `executor_submit` effect handler.
///
/// Submit an execution job with a typed [`ExecutionSpec`] describing the work
/// to perform (backend type, config, inputs, outputs). Lifecycle fields
/// (`run`, `retries`, `max_retries`) are passed through for retry tracking.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ExecutorSubmitInput {
    /// Unique job identifier for correlation.
    pub job_id: String,
    /// Execution attempt number (0-indexed, incremented on retry).
    pub run: i64,
    /// Number of retries so far.
    pub retries: i64,
    /// Maximum retries before dead-lettering.
    pub max_retries: i64,
    /// Engine-stamped execution id flowed in from the scheduler net (where
    /// the same id was passed to the dispatcher, e.g. as
    /// `EXECUTOR_TARGET_EXEC_ID` for sbatch). The executor submit handler
    /// reuses this id as the NATS subject suffix so a one-shot consumer in
    /// the dispatched process exact-matches its own message.
    pub execution_id: String,
    /// Typed execution specification (backend, config, inputs, outputs).
    pub spec: aithericon_executor_domain::ExecutionSpec,
}

/// Output from the `executor_submit` effect handler.
///
/// Contains all input fields plus the engine-assigned `execution_id` for
/// tracking the execution through its lifecycle (accepted → running →
/// completed/failed/timed_out/cancelled).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ExecutorSubmitted {
    /// Job identifier (echoed from input).
    pub job_id: String,
    /// Execution attempt (echoed from input).
    pub run: i64,
    /// Retries so far (echoed from input).
    pub retries: i64,
    /// Max retries (echoed from input).
    pub max_retries: i64,
    /// Engine-assigned execution identifier for lifecycle tracking.
    pub execution_id: String,
    /// Execution spec (echoed from input, needed for retry resubmission).
    pub spec: aithericon_executor_domain::ExecutionSpec,
}

/// Input to the `executor_cancel` effect handler.
///
/// Cancels a running execution identified by its execution ID.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ExecutorCancelInput {
    /// Execution ID to cancel (from `ExecutorSubmitted.execution_id`).
    pub execution_id: String,
}

/// Output from the `executor_cancel` effect handler.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ExecutorCancelled {
    /// Execution ID that was cancelled.
    pub execution_id: String,
}

// ─── Timer effect tokens ────────────────────────────────────────────────────

/// Input to the `timer_schedule` effect handler.
///
/// Schedule a durable timer that fires a signal after `delay_ms` milliseconds.
/// The `payload` becomes the signal token injected into `target_place_id`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TimerInput {
    /// Delay in milliseconds before the timer fires.
    pub delay_ms: u64,
    /// Signal place where the timer payload will be injected.
    pub target_place_id: String,
    /// Data to inject as the signal token when the timer fires.
    pub payload: serde_json::Value,
}

/// Output from the `timer_schedule` effect handler.
///
/// Contains the original input fields plus the engine-assigned correlation ID.
/// Use `timer_correlation_id` to cancel the timer later via `timer_cancel`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TimerScheduled {
    /// Delay in milliseconds (echoed from input).
    pub delay_ms: u64,
    /// Signal place (echoed from input).
    pub target_place_id: String,
    /// Payload (echoed from input).
    pub payload: serde_json::Value,
    /// Engine-assigned correlation ID for cancellation.
    pub timer_correlation_id: String,
}

/// Input to the `timer_cancel` effect handler.
///
/// Cancels a previously scheduled timer identified by its correlation ID.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TimerCancelInput {
    /// Correlation ID from the `TimerScheduled` output.
    pub timer_correlation_id: String,
    /// Target place of the timer to cancel.
    pub target_place_id: String,
}

/// Output from the `timer_cancel` effect handler.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TimerCancelled {
    /// Correlation ID of the cancelled timer.
    pub timer_correlation_id: String,
    /// Target place of the cancelled timer.
    pub target_place_id: String,
}

// ─── Human task effect tokens ───────────────────────────────────────────────

/// Minimal output from the `human_task` effect handler.
///
/// The handler merges all input fields into the output, so you can define a
/// custom output type with extra fields (e.g., `invoice_number`). Use this
/// type when you only need the `task_id` for downstream correlation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HumanTaskAssigned {
    /// Engine-assigned task ID for correlation with the response signal.
    pub task_id: String,
}

/// Input to the `human_cancel` effect handler.
///
/// Cancels a human task in the UI. The `place` must match the signal place
/// where the task's response would have been delivered.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HumanCancelInput {
    /// Task ID to cancel (from `HumanTaskAssigned.task_id`).
    pub task_id: String,
    /// Signal place name where the task response would arrive.
    pub place: String,
    /// Optional reason shown to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Output from the `human_cancel` effect handler.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HumanTaskCancelled {
    /// Task ID that was cancelled.
    pub task_id: String,
    /// Signal place of the cancelled task.
    pub place: String,
}

/// Response signal delivered by the Human UI when a task is resolved.
///
/// The human result listener transforms UI completion/failure/cancellation
/// events into this envelope and injects it into the response signal place.
/// The `data` field contains the form submission for completed tasks.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HumanTaskResponse {
    /// Outcome status: `"completed"`, `"failed"`, or `"cancelled"`.
    pub status: String,
    /// Task ID for correlation (matches `HumanTaskAssigned.task_id`).
    pub task_id: String,
    /// Form submission data (present when `status == "completed"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Failure or cancellation reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Correlation ID (echoed from the original task request).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr_id: Option<String>,
}

// ─── Process lifecycle effect tokens ────────────────────────────────────────

/// A step definition within a process workflow.
///
/// Steps define the logical phases of a process and are used both in the
/// process configuration (via [`ProcessStartConfig`]) and in the progress
/// events published to the Human UI timeline. Reference step keys from
/// transitions via `.process_step("key")`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ProcessStepDef {
    /// Unique step key (referenced by `.process_step("key")` on transitions).
    pub key: String,
    /// Human-readable label shown in the UI timeline.
    pub label: String,
    /// Whether this step involves human interaction (shown differently in UI).
    #[serde(default)]
    pub human: bool,
}

impl ProcessStepDef {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self { key: key.into(), label: label.into(), human: false }
    }
    pub fn human(mut self) -> Self { self.human = true; self }
}

/// Typed configuration for the `process_start` effect.
///
/// Passed as `ProcessStart.config` to define the process metadata, step
/// definitions, and ID generation strategy. The handler serializes this to
/// JSON for the effect config.
///
/// # Example
///
/// ```ignore
/// ctx.transition("start", "Start Process")
///     .process_start_to(ProcessStart {
///         trigger: &inbox,
///         process: &processes,
///         config: ProcessStartConfig {
///             name: "Invoice Processing".into(),
///             steps: vec![
///                 ProcessStepDef { key: "entry".into(), label: "Data Entry".into(), human: true },
///                 ProcessStepDef { key: "review".into(), label: "Review".into(), human: true },
///             ],
///             process_id_prefix: Some("inv-".into()),
///             ..Default::default()
///         },
///     });
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ProcessStartConfig {
    /// Human-readable process name.
    pub name: String,
    /// Optional description of the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Field name in the trigger token to extract as process ID suffix.
    /// Defaults to `"id"`. If the field is missing, a UUID is generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id_field: Option<String>,
    /// Prefix prepended to the extracted/generated process ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id_prefix: Option<String>,
    /// Ordered list of workflow steps (shown in the UI timeline).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<ProcessStepDef>,
    /// Additional output port names that receive a copy of the trigger token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_ports: Option<Vec<String>>,
}

impl ProcessStartConfig {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Default::default() }
    }
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into()); self
    }
    pub fn process_id_field(mut self, field: impl Into<String>) -> Self {
        self.process_id_field = Some(field.into()); self
    }
    pub fn process_id_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.process_id_prefix = Some(prefix.into()); self
    }
    pub fn step(mut self, key: impl Into<String>, label: impl Into<String>) -> Self {
        self.steps.push(ProcessStepDef::new(key, label)); self
    }
    pub fn human_step(mut self, key: impl Into<String>, label: impl Into<String>) -> Self {
        self.steps.push(ProcessStepDef::new(key, label).human()); self
    }
    pub fn forward_ports(mut self, ports: Vec<String>) -> Self {
        self.forward_ports = Some(ports); self
    }
}

/// Output from the `process_start` effect handler.
///
/// Contains the engine-assigned process ID and the process name from config.
/// Use this as the place type for the process token that read arcs reference
/// throughout the workflow.
///
/// ```ignore
/// let processes = ctx.state::<ProcessStarted>("processes", "Active Processes");
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProcessStarted {
    /// Engine-assigned process identifier.
    pub process_id: String,
    /// Human-readable process name (from effect config).
    pub name: String,
}

// ─── Process workflow progress events ───────────────────────────────────────

/// Metadata for a newly started process.
///
/// Published as part of the `Started` variant in [`ProcessUpdateType`].
/// Contains the full process definition including step list.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProcessMetadata {
    /// Engine-assigned process identifier.
    pub process_id: String,
    /// Net/namespace this process belongs to.
    pub namespace: String,
    /// Human-readable process name.
    pub name: String,
    /// Optional description of the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Ordered list of workflow steps.
    pub steps: Vec<ProcessStepDef>,
    /// ISO 8601 timestamp when the process started.
    pub started_at: String,
}

/// All workflow progress event variants published to the Human UI timeline.
///
/// The engine publishes these to the `HUMAN_PROCESS` NATS stream as transitions
/// fire and executors report progress. The Human UI consumes them to render
/// real-time process timelines.
///
/// Step-level events (`StepStarted`, `StepCompleted`, `StepFailed`, `Progress`)
/// are published automatically when transitions annotated with
/// `.process_step("key")` fire. Executor-originated events are published by
/// the ExecutorWatcher when executions report progress.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessUpdateType {
    /// Process has started with full metadata.
    Started {
        metadata: ProcessMetadata,
    },
    /// A workflow step has begun.
    StepStarted {
        step: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A workflow step has completed successfully.
    StepCompleted {
        step: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
    },
    /// A workflow step has failed.
    StepFailed {
        step: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Incremental progress within a step.
    Progress {
        step: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        percent: Option<f64>,
    },
    /// Process completed successfully.
    Completed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// Process failed.
    Failed {
        error: String,
    },
    // ── Executor-originated events ──────────────────────────────────────
    /// An execution has started for a step.
    ExecutionStarted {
        step: String,
        execution_id: String,
    },
    /// Execution progress update.
    ExecutionProgress {
        step: String,
        execution_id: String,
        fraction: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Execution completed.
    ExecutionCompleted {
        step: String,
        execution_id: String,
        duration_ms: u64,
    },
    /// Execution failed.
    ExecutionFailed {
        step: String,
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// An artifact was logged during execution.
    ArtifactLogged {
        step: String,
        execution_id: String,
        artifact_id: String,
        name: String,
    },
}

/// A workflow progress event published to the `HUMAN_PROCESS` NATS stream.
///
/// Wraps a [`ProcessUpdateType`] variant with routing metadata (process_id,
/// namespace) and a timestamp. Consumed by the Human UI to render process
/// timelines and by any downstream system that needs workflow observability.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProcessUpdate {
    /// Process identifier for routing.
    pub process_id: String,
    /// Net/namespace this update belongs to.
    pub namespace: String,
    /// The specific update event.
    pub update_type: ProcessUpdateType,
    /// ISO 8601 timestamp of the event.
    pub timestamp: String,
}

// ─── Effect error token ────────────────────────────────────────────────────

/// Error token produced by the engine when an effect handler fails.
///
/// Constructed in `firing.rs` for every effect transition `_error` port. The
/// `inputs` field contains the original port inputs (dynamic), enabling retry
/// logic to re-queue the original job via `err.inputs.job`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EffectError {
    /// Error message from the handler.
    pub error: String,
    /// Handler that failed (e.g., `"executor_submit"`).
    pub handler_id: String,
    /// Transition ID where the failure occurred.
    pub transition_id: String,
    /// Original port inputs (dynamic — shape depends on the transition).
    pub inputs: serde_json::Value,
    /// Whether the error is retryable.
    pub retryable: bool,
}

// ─── Executor signal tokens ────────────────────────────────────────────────

/// Status signal delivered by the ExecutorWatcher.
///
/// All executor lifecycle statuses (accepted, running, completed, failed,
/// timed_out, cancelled) share this envelope. The `detail` field carries
/// status-specific data (e.g., outputs for completed, error info for failed).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ExecutorStatusSignal {
    /// Execution ID for correlation.
    pub execution_id: String,
    /// Status name (accepted, running, completed, failed, timed_out, cancelled).
    pub status: String,
    /// Status-specific detail payload.
    pub detail: serde_json::Value,
    /// Source identifier (e.g., executor instance).
    pub source: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
}

/// Event signal delivered by the ExecutorWatcher for mid-execution events.
///
/// Covers progress, artifact, metric, phase, output, and log events.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ExecutorEventSignal {
    /// Execution ID for correlation.
    pub execution_id: String,
    /// Event category (progress, artifact, metric, phase, output, log).
    pub category: String,
    /// Event-specific detail payload.
    pub detail: serde_json::Value,
    /// Monotonic sequence number within this execution.
    pub sequence: i64,
    /// Source identifier.
    pub source: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
}
