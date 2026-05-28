//! Typed effect contracts for built-in effect transitions.
//!
//! Each struct bundles the full I/O wiring for one effect handler:
//! input places, output places, error output, signal routing, and
//! optional process context.  Pass them to the corresponding
//! [`TransitionBuilder`] method (e.g. `executor_submit_to`) to wire
//! everything in one call.
//!
//! [`TransitionBuilder`]: crate::transition::TransitionBuilder

use crate::effect_tokens::{
    EffectError, ExecutorCancelInput, ExecutorCancelled, ExecutorEventSignal,
    ExecutorStatusSignal, ExecutorSubmitInput, ExecutorSubmitted, HumanCancelInput,
    HumanTaskAssigned, HumanTaskCancelled, HumanTaskResponse, ProcessStartConfig, ProcessStarted,
    SchedulerCancelInput, SchedulerCancelled, SchedulerStatusSignal, SchedulerSubmitInput,
    SchedulerSubmitted, SubWorkflowCancelInput, SubWorkflowCancelled, TimerCancelInput,
    TimerCancelled, TimerInput, TimerScheduled,
};
use crate::place::PlaceHandle;
use crate::token::DynamicToken;
use petri_domain::human::HumanTaskRequest;

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// Full contract for an `executor_submit` effect transition.
///
/// Bundles the transition's I/O port wiring, error handling, signal routing,
/// and process context into a single typed declaration. The method
/// [`TransitionBuilder::executor_submit_to`] wires everything internally —
/// no free-form port names needed.
pub struct ExecutorSubmit<'a> {
    // ── Synchronous ports (transition I/O arcs) ─────────────────────────
    /// Input: place holding execution specs to submit.
    pub job: &'a PlaceHandle<ExecutorSubmitInput>,
    /// Output: place receiving submitted confirmation tokens.
    pub submitted: &'a PlaceHandle<ExecutorSubmitted>,
    /// Error output: place receiving effect handler failures.
    pub errors: &'a PlaceHandle<EffectError>,

    // ── Status signals (async — delivered by ExecutorWatcher) ────────────
    /// Signal place for "accepted" status.
    pub accepted: &'a PlaceHandle<ExecutorStatusSignal>,
    /// Signal place for "running" status.
    pub running: &'a PlaceHandle<ExecutorStatusSignal>,
    /// Signal place for "completed" status.
    pub completed: &'a PlaceHandle<ExecutorStatusSignal>,
    /// Signal place for "failed" status.
    pub failed: &'a PlaceHandle<ExecutorStatusSignal>,
    /// Signal place for "timed_out" status.
    pub timed_out: &'a PlaceHandle<ExecutorStatusSignal>,
    /// Signal place for "cancelled" status.
    pub cancelled: &'a PlaceHandle<ExecutorStatusSignal>,

    // ── Event signals (optional) ────────────────────────────────────────
    /// Optional signal place for progress events.
    pub progress: Option<&'a PlaceHandle<ExecutorEventSignal>>,
    /// Optional signal place for artifact events.
    pub artifact: Option<&'a PlaceHandle<ExecutorEventSignal>>,
    /// Optional signal place for metric events.
    pub metric: Option<&'a PlaceHandle<DynamicToken>>,
    /// Optional signal place for phase events.
    pub phase: Option<&'a PlaceHandle<DynamicToken>>,
    /// Optional signal place for output events.
    pub output: Option<&'a PlaceHandle<DynamicToken>>,
    /// Optional signal place for log message events.
    pub log: Option<&'a PlaceHandle<DynamicToken>>,

    // ── Process context (optional) ──────────────────────────────────────
    /// Optional process ID for workflow event correlation.
    pub process_id: Option<&'a str>,
    /// Optional process step name, paired with `process_id`.
    pub process_step: Option<&'a str>,
}

/// Full contract for an `executor_cancel` effect transition.
///
/// Bundles input/output port wiring, correlation, error handling, and
/// causation into a single typed declaration.
pub struct ExecutorCancel<'a> {
    /// Input: place holding the running execution to cancel.
    pub job: &'a PlaceHandle<DynamicToken>,
    /// Input: signal place with cancel requests (correlated on `execution_id`).
    pub cancel_request: &'a PlaceHandle<ExecutorCancelInput>,
    /// Output: place receiving cancel-in-progress acknowledgment tokens.
    pub cancelling: &'a PlaceHandle<ExecutorCancelled>,
    /// Error output: place receiving effect handler failures.
    pub errors: &'a PlaceHandle<EffectError>,
    /// Signal: where the ExecutorWatcher delivers cancelled confirmation.
    pub cancelled_signal: &'a PlaceHandle<ExecutorStatusSignal>,
}

// ---------------------------------------------------------------------------
// Human Task
// ---------------------------------------------------------------------------

/// Full contract for a `human_task` effect transition.
///
/// Wires input/output/error ports and signal routing for human-in-the-loop tasks.
pub struct HumanTaskSubmit<'a> {
    /// Input: place holding the task definition ([`HumanTaskRequest`] schema).
    ///
    /// The handler validates `title` and `steps` from this token. Extra business
    /// fields (e.g., `invoice_number`) are preserved through to the output.
    /// If your place uses a custom `#[token]` struct, use `.retyped()`.
    pub task: &'a PlaceHandle<HumanTaskRequest>,
    /// Output: place receiving the assigned confirmation (includes `task_id`).
    ///
    /// The handler merges all input fields into the output plus adds `task_id`,
    /// `net_id`, `place`, and `response_subject`. The [`HumanTaskAssigned`]
    /// schema validates `task_id` while allowing the merged extras.
    pub assigned: &'a PlaceHandle<HumanTaskAssigned>,
    /// Error output: place receiving effect handler failures.
    pub errors: &'a PlaceHandle<EffectError>,
    /// Signal: where the human's response arrives from the UI.
    pub response_signal: &'a PlaceHandle<HumanTaskResponse>,
}

/// Full contract for a `human_cancel` effect transition.
pub struct HumanTaskCancel<'a> {
    /// Input: place holding the cancel request (`task_id` + `place`).
    pub task: &'a PlaceHandle<HumanCancelInput>,
    /// Output: place receiving cancellation acknowledgment.
    pub cancelled: &'a PlaceHandle<HumanTaskCancelled>,
    /// Error output: place receiving effect handler failures.
    pub errors: &'a PlaceHandle<EffectError>,
}

// ---------------------------------------------------------------------------
// Timer
// ---------------------------------------------------------------------------

/// Full contract for a `timer_schedule` effect transition.
pub struct TimerSchedule<'a> {
    /// Input: place holding the timer request (`delay_ms`, `target_place_id`, `payload`).
    pub timer: &'a PlaceHandle<TimerInput>,
    /// Output: place receiving scheduled confirmation (`timer_correlation_id`).
    pub scheduled: &'a PlaceHandle<TimerScheduled>,
    /// Error output: place receiving effect handler failures.
    pub errors: &'a PlaceHandle<EffectError>,
    /// Signal: place where the timer fires after the delay (causation arc).
    pub signal: &'a PlaceHandle<DynamicToken>,
}

/// Full contract for a `timer_cancel` effect transition.
pub struct TimerCancel<'a> {
    /// Input: place holding the cancel request (`timer_correlation_id`).
    pub timer: &'a PlaceHandle<TimerCancelInput>,
    /// Output: place receiving cancellation acknowledgment.
    pub cancelled: &'a PlaceHandle<TimerCancelled>,
    /// Error output: place receiving effect handler failures.
    pub errors: &'a PlaceHandle<EffectError>,
}

// ---------------------------------------------------------------------------
// Subworkflow
// ---------------------------------------------------------------------------

/// Full contract for a `subworkflow_cancel` effect transition.
///
/// Used by the Timeout node's body-cancellation post-pass: when the timer
/// wins, one `subworkflow_cancel` is fired per SubWorkflow body child to
/// terminate the running child net.
pub struct SubWorkflowCancel<'a> {
    /// Input: place holding the cancel request (`child_net_id`).
    pub cancel: &'a PlaceHandle<SubWorkflowCancelInput>,
    /// Output: place receiving cancellation acknowledgment.
    pub cancelled: &'a PlaceHandle<SubWorkflowCancelled>,
    /// Error output: place receiving effect handler failures.
    pub errors: &'a PlaceHandle<EffectError>,
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// Full contract for a `scheduler_submit` effect transition.
pub struct SchedulerSubmit<'a> {
    /// Input: place holding the job spec.
    pub job: &'a PlaceHandle<SchedulerSubmitInput>,
    /// Output: place receiving submitted confirmation (`scheduler_job_id`).
    pub submitted: &'a PlaceHandle<SchedulerSubmitted>,
    /// Error output: place receiving effect handler failures.
    pub errors: &'a PlaceHandle<EffectError>,
    /// Causation: signal place for "running" status (from Nomad/Slurm watcher).
    pub running: &'a PlaceHandle<SchedulerStatusSignal>,
    /// Causation: signal place for "completed" status.
    pub completed: &'a PlaceHandle<SchedulerStatusSignal>,
    /// Causation: signal place for "failed" status.
    pub failed: &'a PlaceHandle<SchedulerStatusSignal>,
    /// Causation: optional signal place for "timed_out" status (Slurm only).
    pub timed_out: Option<&'a PlaceHandle<SchedulerStatusSignal>>,
}

/// Full contract for a `scheduler_cancel` effect transition.
pub struct SchedulerCancel<'a> {
    /// Input: place holding the running job to cancel.
    pub job: &'a PlaceHandle<DynamicToken>,
    /// Input: signal place with cancel requests (correlated on `scheduler_job_id`).
    pub cancel_request: &'a PlaceHandle<SchedulerCancelInput>,
    /// Output: place receiving cancellation acknowledgment.
    pub cancelled: &'a PlaceHandle<SchedulerCancelled>,
    /// Error output: place receiving effect handler failures.
    pub errors: &'a PlaceHandle<EffectError>,
}

// ---------------------------------------------------------------------------
// Process Lifecycle
// ---------------------------------------------------------------------------

/// Full contract for a `process_start` effect transition.
pub struct ProcessStart<'a> {
    /// Input: place holding the workflow trigger token.
    pub trigger: &'a PlaceHandle<DynamicToken>,
    /// Output: place receiving the `ProcessStarted` token (with `process_id`).
    pub process: &'a PlaceHandle<ProcessStarted>,
    /// Process configuration: name, steps, ID generation strategy.
    pub config: ProcessStartConfig,
}

/// Full contract for a `process_complete` effect transition.
pub struct ProcessComplete<'a> {
    /// Read input: place holding the `ProcessStarted` token (non-consuming, for `process_id`).
    pub process: &'a PlaceHandle<ProcessStarted>,
    /// Input: place holding the completion trigger token.
    pub done: &'a PlaceHandle<DynamicToken>,
    /// Output: place receiving the completed token (typically terminal).
    pub completed: &'a PlaceHandle<DynamicToken>,
}
