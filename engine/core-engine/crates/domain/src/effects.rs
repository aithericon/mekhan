//! Built-in effect handler descriptors.
//!
//! Single source of truth for handler IDs, default port names, and service
//! categories. Shared between the SDK (compile-time safety) and the engine
//! (handler registration).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Service category for infrastructure requirements.
///
/// Each category corresponds to a subsystem that must be configured in the
/// engine for the associated effect handlers to work.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceCategory {
    /// External job scheduler (Nomad, Slurm, or Mock).
    Scheduler,
    /// Execution backend (aithericon-executor via NATS).
    Executor,
    /// Durable timer service (Clockmaster).
    Timer,
    /// Human-in-the-loop task service.
    Human,
    /// Orchestration services (dynamic net spawn).
    Orchestration,
    /// Data catalogue service (artifact registry).
    Catalogue,
}

impl ServiceCategory {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Scheduler => "scheduler",
            Self::Executor => "executor",
            Self::Timer => "timer",
            Self::Human => "human",
            Self::Orchestration => "orchestration",
            Self::Catalogue => "catalogue",
        }
    }
}

impl std::fmt::Display for ServiceCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Describes a built-in effect handler's contract.
///
/// Provides handler ID, default port names, service category, and expected
/// token schema references. Both SDK and engine reference these descriptors
/// instead of raw strings.
#[derive(Clone, Debug)]
pub struct EffectDescriptor {
    /// Handler ID as registered in the engine (e.g., "scheduler_submit").
    pub handler_id: &'static str,
    /// Default input port name (e.g., "job").
    pub default_input_port: &'static str,
    /// Default output port name (e.g., "submitted").
    pub default_output_port: &'static str,
    /// Service category this handler belongs to.
    pub category: ServiceCategory,
    /// Expected JSON Schema `$ref` for the default input port token type
    /// (e.g., `"#/definitions/SchedulerSubmitInput"`). `None` when the handler
    /// accepts arbitrary input shapes.
    pub default_input_schema: Option<&'static str>,
    /// Expected JSON Schema `$ref` for the default output port token type
    /// (e.g., `"#/definitions/SchedulerSubmitted"`). `None` when the handler
    /// produces dynamic output shapes.
    pub default_output_schema: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// Built-in descriptors (const, zero-cost)
// ---------------------------------------------------------------------------

/// Submit a job to the scheduler service (Nomad, Slurm, or Mock).
pub const SCHEDULER_SUBMIT: EffectDescriptor = EffectDescriptor {
    handler_id: "scheduler_submit",
    default_input_port: "job",
    default_output_port: "submitted",
    category: ServiceCategory::Scheduler,
    default_input_schema: Some("#/definitions/SchedulerSubmitInput"),
    default_output_schema: Some("#/definitions/SchedulerSubmitted"),
};

/// Cancel a running scheduler job.
pub const SCHEDULER_CANCEL: EffectDescriptor = EffectDescriptor {
    handler_id: "scheduler_cancel",
    default_input_port: "job",
    default_output_port: "cancelled",
    category: ServiceCategory::Scheduler,
    default_input_schema: Some("#/definitions/SchedulerCancelInput"),
    default_output_schema: Some("#/definitions/SchedulerCancelled"),
};

/// Submit an execution to the executor service.
///
/// ## Sub-phase 2.5d-tools: agent_loop config keys (carried in `config`)
///
/// When the target executor runs an LLM Agent stage with tool use, the
/// transition's `logic.config` carries two additional keys alongside the
/// standard `task_kind`, `required_capabilities`, etc.:
///
/// - `tool_names: Vec<String>` — names of tools the LLM may invoke, resolved
///   by name against the submission's `tool_catalogue`. The executor's
///   `run_agent_loop` passes only the named tools to the LLM.
///
/// - `max_tool_iterations: usize` (optional; default 16) — hard cap on the
///   number of LLM ↔ tool turns before the loop terminates with an error.
///   Prevents runaway tool loops from consuming unbounded tokens.
///
/// Both keys are absent from `ExecutorSubmitInput` schema validation today
/// (the executor reads them from raw `config` JSON). Schema reference will
/// be updated in 2.5e when the clinic-side pipeline engine deletes its own
/// tool-loop and defers fully to mekhan.
pub const EXECUTOR_SUBMIT: EffectDescriptor = EffectDescriptor {
    handler_id: "executor_submit",
    default_input_port: "job",
    default_output_port: "submitted",
    category: ServiceCategory::Executor,
    default_input_schema: Some("#/definitions/ExecutorSubmitInput"),
    default_output_schema: Some("#/definitions/ExecutorSubmitted"),
};

/// Cancel a running execution.
pub const EXECUTOR_CANCEL: EffectDescriptor = EffectDescriptor {
    handler_id: "executor_cancel",
    default_input_port: "job",
    default_output_port: "cancelled",
    category: ServiceCategory::Executor,
    default_input_schema: Some("#/definitions/ExecutorCancelInput"),
    default_output_schema: Some("#/definitions/ExecutorCancelled"),
};

/// Feed a data chunk into a running reducer job.
pub const EXECUTOR_STREAM_FEED: EffectDescriptor = EffectDescriptor {
    handler_id: "executor_stream_feed",
    default_input_port: "feed",
    default_output_port: "fed",
    category: ServiceCategory::Executor,
    default_input_schema: None,
    default_output_schema: None,
};

/// Deposit a dynamically-emitted control token into a statically-declared
/// channel place (docs/25 streaming-channels).
///
/// A running executor job emits `signal` / `scatter` control tokens
/// mid-execution; the worker publishes each as a `control_emit` executor event,
/// the watcher routes it (via the job's `event_routes`) to the node's control
/// inbox, and a transition draining that inbox carries THIS effect. The handler
/// (`ControlEmitHandler`) resolves the emit's `channel` against the
/// `channel_routes` map baked on the transition's `effect_config` and forwards
/// the payload into the channel's own place `p_{node}_{channel}`. Fire-and-
/// forget: the engine NEVER gates or declines (no `max_fanout` enforcement —
/// that is the compiler/validation layer's concern; back-pressure is
/// JetStream's). `default_input_port` / `default_output_port` are unused — the
/// handler reads the single bound input and deposits onto the place id resolved
/// per-fire from `channel_routes`.
pub const CONTROL_EMIT: EffectDescriptor = EffectDescriptor {
    handler_id: "control_emit",
    default_input_port: "emit",
    default_output_port: "emitted",
    category: ServiceCategory::Executor,
    default_input_schema: None,
    default_output_schema: None,
};

/// Schedule a durable timer via Clockmaster.
pub const TIMER_SCHEDULE: EffectDescriptor = EffectDescriptor {
    handler_id: "timer_schedule",
    default_input_port: "timer",
    default_output_port: "scheduled",
    category: ServiceCategory::Timer,
    default_input_schema: Some("#/definitions/TimerInput"),
    default_output_schema: Some("#/definitions/TimerScheduled"),
};

/// Cancel a scheduled timer.
pub const TIMER_CANCEL: EffectDescriptor = EffectDescriptor {
    handler_id: "timer_cancel",
    default_input_port: "timer",
    default_output_port: "cancelled",
    category: ServiceCategory::Timer,
    default_input_schema: Some("#/definitions/TimerCancelInput"),
    default_output_schema: Some("#/definitions/TimerCancelled"),
};

/// Submit a human-in-the-loop task.
pub const HUMAN_TASK: EffectDescriptor = EffectDescriptor {
    handler_id: "human_task",
    default_input_port: "task",
    default_output_port: "assigned",
    category: ServiceCategory::Human,
    default_input_schema: None,
    default_output_schema: None,
};

/// Cancel a human task.
pub const HUMAN_CANCEL: EffectDescriptor = EffectDescriptor {
    handler_id: "human_cancel",
    default_input_port: "task",
    default_output_port: "cancelled",
    category: ServiceCategory::Human,
    default_input_schema: Some("#/definitions/HumanCancelInput"),
    default_output_schema: Some("#/definitions/HumanTaskCancelled"),
};

/// Spawn a child net dynamically (create net + bridge initial token).
pub const SPAWN_NET: EffectDescriptor = EffectDescriptor {
    handler_id: "spawn_net",
    default_input_port: "spawn_request",
    default_output_port: "spawned",
    category: ServiceCategory::Orchestration,
    default_input_schema: None,
    default_output_schema: None,
};

/// Cancel a running child net (terminate the spawned subworkflow).
pub const SUBWORKFLOW_CANCEL: EffectDescriptor = EffectDescriptor {
    handler_id: "subworkflow_cancel",
    default_input_port: "cancel",
    default_output_port: "cancelled",
    category: ServiceCategory::Orchestration,
    default_input_schema: Some("#/definitions/SubWorkflowCancelInput"),
    default_output_schema: Some("#/definitions/SubWorkflowCancelled"),
};

/// Start a process lifecycle (publishes "process started", outputs process token).
pub const PROCESS_START: EffectDescriptor = EffectDescriptor {
    handler_id: "process_start",
    default_input_port: "trigger",
    default_output_port: "process",
    category: ServiceCategory::Orchestration,
    default_input_schema: None,
    default_output_schema: Some("#/definitions/ProcessStarted"),
};

/// Complete a process lifecycle (publishes "process completed", passes through input).
pub const PROCESS_COMPLETE: EffectDescriptor = EffectDescriptor {
    handler_id: "process_complete",
    default_input_port: "done",
    default_output_port: "completed",
    category: ServiceCategory::Orchestration,
    default_input_schema: None,
    default_output_schema: None,
};

/// Fail a process lifecycle (publishes "process failed", passes through input).
///
/// Symmetric to [`PROCESS_COMPLETE`] but **tolerant**: the handler does not
/// require a `process_id` in the token (authored graph nodes pass through the
/// plain workflow token with no read-arc). The owning process is resolved by
/// the causality tag graph in the projection layer, not the handler.
pub const PROCESS_FAIL: EffectDescriptor = EffectDescriptor {
    handler_id: "process_fail",
    default_input_port: "failure",
    default_output_port: "failed",
    category: ServiceCategory::Orchestration,
    default_input_schema: None,
    default_output_schema: None,
};

/// Register artifacts in the data catalogue.
pub const CATALOGUE_REGISTER: EffectDescriptor = EffectDescriptor {
    handler_id: "catalogue_register",
    default_input_port: "artifacts",
    default_output_port: "catalogued",
    category: ServiceCategory::Catalogue,
    default_input_schema: None,
    default_output_schema: None,
};

/// Query the data catalogue for matching entries.
pub const CATALOGUE_LOOKUP: EffectDescriptor = EffectDescriptor {
    handler_id: "catalogue_lookup",
    default_input_port: "query",
    default_output_port: "results",
    category: ServiceCategory::Catalogue,
    default_input_schema: None,
    default_output_schema: None,
};

/// Subscribe to reactive catalogue change notifications.
pub const CATALOGUE_SUBSCRIBE: EffectDescriptor = EffectDescriptor {
    handler_id: "catalogue_subscribe",
    default_input_port: "subscription",
    default_output_port: "subscribed",
    category: ServiceCategory::Catalogue,
    default_input_schema: None,
    default_output_schema: None,
};

/// Unsubscribe from catalogue change notifications.
pub const CATALOGUE_UNSUBSCRIBE: EffectDescriptor = EffectDescriptor {
    handler_id: "catalogue_unsubscribe",
    default_input_port: "handle",
    default_output_port: "unsubscribed",
    category: ServiceCategory::Catalogue,
    default_input_schema: None,
    default_output_schema: None,
};

/// Record a typed process phase transition.
///
/// Carries a serialized `aithericon_executor_domain::StatusDetail::PhaseChanged`
/// (or the bare `PhaseChanged` payload) on its input port. Unlike
/// [`PROCESS_LOG_MESSAGE`], the payload is *not* downgraded to a stringly log
/// breadcrumb — the handler echoes it verbatim into `effect_result` so the
/// causality consumer can deserialize the whole typed variant.
pub const PROCESS_PHASE: EffectDescriptor = EffectDescriptor {
    handler_id: "process_phase",
    default_input_port: "phase",
    default_output_port: "recorded",
    category: ServiceCategory::Orchestration,
    default_input_schema: None,
    default_output_schema: None,
};

/// Record a typed process progress update.
///
/// Carries a serialized `aithericon_executor_domain::StatusDetail::ProgressUpdated`
/// (or the bare `ProgressUpdated` payload) on its input port. Unlike
/// [`PROCESS_LOG_METRIC`], `current_step`/`total_steps`/`message` are not
/// dropped — the handler echoes the payload verbatim into `effect_result` for
/// typed projection by the causality consumer.
pub const PROCESS_PROGRESS: EffectDescriptor = EffectDescriptor {
    handler_id: "process_progress",
    default_input_port: "progress",
    default_output_port: "recorded",
    category: ServiceCategory::Orchestration,
    default_input_schema: None,
    default_output_schema: None,
};

/// Log a numeric metric to the process trace (e.g., loss, accuracy, acquisition value).
pub const PROCESS_LOG_METRIC: EffectDescriptor = EffectDescriptor {
    handler_id: "process_log_metric",
    default_input_port: "metric",
    default_output_port: "logged",
    category: ServiceCategory::Human,
    default_input_schema: None,
    default_output_schema: None,
};

/// Log a structured message to the process trace (e.g., status updates, warnings).
pub const PROCESS_LOG_MESSAGE: EffectDescriptor = EffectDescriptor {
    handler_id: "process_log_message",
    default_input_port: "message",
    default_output_port: "logged",
    category: ServiceCategory::Human,
    default_input_schema: None,
    default_output_schema: None,
};

/// Acquire a lease/allocation on an external cluster (datacenter resource).
///
/// R4: the `scheduler` deployment backend's `lease` operation. The handler
/// (`ResourceLeaseAcquireHandler`) POSTs the claim request to the allocator URL
/// (resolved per-fire from the datacenter resource secret in `effect_config`)
/// and emits the typed lease (`{ grant_id, alloc_id, node?, expiry?, scheduler }`)
/// — the `DatacenterLease` shape from `aithericon_resources::pool`. The
/// allocator is the source of truth; the net holds only the lease handle.
///
/// Idempotency/replay: `grant_id` (replay-safe `instance_id:node_id`) is the
/// allocator idempotency key, and replay re-emits the journaled lease without
/// calling the allocator. Categorised under [`ServiceCategory::Scheduler`] — a
/// lease is "external cluster" admission, the same subsystem as submit.
pub const RESOURCE_LEASE_ACQUIRE: EffectDescriptor = EffectDescriptor {
    handler_id: "resource_lease_acquire",
    default_input_port: "request",
    default_output_port: "lease",
    category: ServiceCategory::Scheduler,
    default_input_schema: None,
    default_output_schema: None,
};

/// Release a previously acquired cluster lease/allocation.
///
/// Symmetric to [`RESOURCE_LEASE_ACQUIRE`]: the handler
/// (`ResourceLeaseReleaseHandler`) DELETEs the allocation at the allocator
/// (`{allocator_url}/{alloc_id}`) and emits `{ grant_id }`. Replay re-emits the
/// journaled result without calling the allocator.
pub const RESOURCE_LEASE_RELEASE: EffectDescriptor = EffectDescriptor {
    handler_id: "resource_lease_release",
    default_input_port: "release",
    default_output_port: "released",
    category: ServiceCategory::Scheduler,
    default_input_schema: None,
    default_output_schema: None,
};

/// Stage a job template onto an external cluster (datacenter resource).
///
/// Phase 4 of the control plane: an INLINE engine effect that *registers* a
/// job template onto the cluster the datacenter `effect_config` resolves to,
/// using the engine's existing cluster connection (the same
/// `DatacenterConnection.effect_config()` JSON the lease adapter consumes).
///
///   - **Nomad:** render the typed `spec`/`escape_hatch` → a Nomad PARAMETERIZED
///     job JSON and `PUT /v1/job/{slug}` (the REGISTER endpoint, NOT dispatch).
///     `remote_ref` = the slug. The registered job's `ParameterizedJob.MetaOptional`
///     carries the routing meta keys the later `submit` dispatch path sends, so
///     it is dispatchable.
///   - **Slurm:** render → an sbatch script, delivered over SSH to
///     `{template_dir}/{slug}.sh`. `remote_ref` = the remote path.
///
/// Categorised under [`ServiceCategory::Scheduler`] — staging is "external
/// cluster" admission, the same subsystem as submit/lease. The handler returns
/// `status:"staged"|"failed"` DATA on BOTH success and cluster failure (a
/// staging failure is recorded data, NOT a `NetFailed`); only truly-fatal
/// config/parse errors return `Err`. `replay()` is a no-op (stateless).
pub const STAGE_TEMPLATE: EffectDescriptor = EffectDescriptor {
    handler_id: "stage_template",
    default_input_port: "request",
    default_output_port: "staged",
    category: ServiceCategory::Scheduler,
    default_input_schema: None,
    default_output_schema: None,
};

/// `materialize_image` — pull an OCI image to an Apptainer `.sif` on the cluster
/// (docs/22 container staging). Structurally symmetric with [`STAGE_TEMPLATE`]:
/// inline `Scheduler`-category effect, returns `status:"ready"|"failed"` DATA on
/// BOTH success and cluster failure (a pull failure is recorded data, NOT a
/// `NetFailed`); only truly-fatal config/parse errors return `Err`. `replay()`
/// is a no-op (stateless — the cluster is not re-pulled on replay).
pub const MATERIALIZE_IMAGE: EffectDescriptor = EffectDescriptor {
    handler_id: "materialize_image",
    default_input_port: "request",
    default_output_port: "materialized",
    category: ServiceCategory::Scheduler,
    default_input_schema: None,
    default_output_schema: None,
};

/// All built-in effect descriptors.
pub const ALL_BUILTIN: &[&EffectDescriptor] = &[
    &SCHEDULER_SUBMIT,
    &SCHEDULER_CANCEL,
    &EXECUTOR_SUBMIT,
    &EXECUTOR_CANCEL,
    &EXECUTOR_STREAM_FEED,
    &CONTROL_EMIT,
    &TIMER_SCHEDULE,
    &TIMER_CANCEL,
    &HUMAN_TASK,
    &HUMAN_CANCEL,
    &SPAWN_NET,
    &SUBWORKFLOW_CANCEL,
    &PROCESS_START,
    &PROCESS_COMPLETE,
    &PROCESS_FAIL,
    &CATALOGUE_REGISTER,
    &CATALOGUE_LOOKUP,
    &CATALOGUE_SUBSCRIBE,
    &CATALOGUE_UNSUBSCRIBE,
    &PROCESS_PHASE,
    &PROCESS_PROGRESS,
    &PROCESS_LOG_METRIC,
    &PROCESS_LOG_MESSAGE,
    &RESOURCE_LEASE_ACQUIRE,
    &RESOURCE_LEASE_RELEASE,
    &STAGE_TEMPLATE,
    &MATERIALIZE_IMAGE,
];

/// Look up a built-in descriptor by handler_id.
pub fn builtin_by_id(handler_id: &str) -> Option<&'static EffectDescriptor> {
    ALL_BUILTIN
        .iter()
        .find(|d| d.handler_id == handler_id)
        .copied()
}

/// Infrastructure requirement declared by a scenario.
///
/// Serialized into the AIR JSON `requirements` section so the engine
/// can validate that all needed services are configured before running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServiceRequirement {
    /// Service category (e.g., "scheduler", "executor").
    pub category: ServiceCategory,
    /// Specific handler IDs used (e.g., ["scheduler_submit", "scheduler_cancel"]).
    pub handler_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_by_id_finds_known_handlers() {
        assert_eq!(
            builtin_by_id("scheduler_submit").unwrap().handler_id,
            "scheduler_submit"
        );
        assert_eq!(
            builtin_by_id("executor_cancel").unwrap().handler_id,
            "executor_cancel"
        );
        assert_eq!(
            builtin_by_id("timer_schedule").unwrap().handler_id,
            "timer_schedule"
        );
        assert_eq!(
            builtin_by_id("human_task").unwrap().handler_id,
            "human_task"
        );
    }

    #[test]
    fn builtin_by_id_returns_none_for_unknown() {
        assert!(builtin_by_id("nonexistent").is_none());
        assert!(builtin_by_id("").is_none());
    }

    #[test]
    fn all_builtin_covers_all_handlers() {
        assert_eq!(ALL_BUILTIN.len(), 26);
        let ids: Vec<&str> = ALL_BUILTIN.iter().map(|d| d.handler_id).collect();
        assert!(ids.contains(&"scheduler_submit"));
        assert!(ids.contains(&"scheduler_cancel"));
        assert!(ids.contains(&"executor_submit"));
        assert!(ids.contains(&"executor_cancel"));
        assert!(ids.contains(&"control_emit"));
        assert!(ids.contains(&"timer_schedule"));
        assert!(ids.contains(&"timer_cancel"));
        assert!(ids.contains(&"human_task"));
        assert!(ids.contains(&"human_cancel"));
        assert!(ids.contains(&"subworkflow_cancel"));
        assert!(ids.contains(&"process_start"));
        assert!(ids.contains(&"process_complete"));
        assert!(ids.contains(&"process_fail"));
        assert!(ids.contains(&"catalogue_register"));
        assert!(ids.contains(&"catalogue_lookup"));
        assert!(ids.contains(&"catalogue_subscribe"));
        assert!(ids.contains(&"catalogue_unsubscribe"));
        assert!(ids.contains(&"process_phase"));
        assert!(ids.contains(&"process_progress"));
        assert!(ids.contains(&"process_log_metric"));
        assert!(ids.contains(&"process_log_message"));
        assert!(ids.contains(&"resource_lease_acquire"));
        assert!(ids.contains(&"resource_lease_release"));
        assert!(ids.contains(&"stage_template"));
    }

    #[test]
    fn service_category_serialization_roundtrip() {
        let categories = [
            ServiceCategory::Scheduler,
            ServiceCategory::Executor,
            ServiceCategory::Timer,
            ServiceCategory::Human,
            ServiceCategory::Orchestration,
            ServiceCategory::Catalogue,
        ];
        for cat in &categories {
            let json = serde_json::to_string(cat).unwrap();
            let deserialized: ServiceCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(&deserialized, cat);
        }
    }

    #[test]
    fn service_category_as_str_matches_serde() {
        let categories = [
            ServiceCategory::Scheduler,
            ServiceCategory::Executor,
            ServiceCategory::Timer,
            ServiceCategory::Human,
            ServiceCategory::Orchestration,
            ServiceCategory::Catalogue,
        ];
        for cat in &categories {
            let serde_str = serde_json::to_value(cat)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(cat.as_str(), serde_str);
        }
    }

    #[test]
    fn service_requirement_serialization_roundtrip() {
        let req = ServiceRequirement {
            category: ServiceCategory::Executor,
            handler_ids: vec!["executor_submit".into(), "executor_cancel".into()],
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: ServiceRequirement = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, req);
    }
}
