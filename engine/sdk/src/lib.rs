//! # Aithericon SDK
//!
//! Type-safe Rust SDK for building Colored Petri Net workflows.
//!
//! This SDK provides a DSL for defining workflow topologies that compile to the
//! AIR (Actor Interface Runtime) JSON format. The generated scenarios can be
//! deployed to the Petri-Lab execution engine.
//!
//! ## Core Concepts
//!
//! A Petri net workflow consists of:
//!
//! - **[Places](place::PlaceHandle)** - Containers that hold tokens (state, resource, signal, terminal)
//! - **[Tokens](token::Token)** - Typed data that flows through the workflow
//! - **[Transitions](transition::TransitionBuilder)** - Actions that consume/produce tokens
//! - **[Guards](transition::TransitionBuilder::guard)** - Rhai expressions controlling when transitions fire
//! - **[Logic](transition::TransitionBuilder::logic)** - Rhai scripts defining transition behavior
//!
//! ## Features
//!
//! - **Compile-time type safety**: PhantomData ensures places and ports have matching token types
//! - **Macro-based DSL**: `#[token]` and `#[step]` macros for concise workflow definitions
//! - **Build-time validation**: Rhai scripts are validated during compilation
//! - **JSON Schema embedding**: Automatic schema extraction for runtime validation
//! - **Reusable components**: [`Component`] trait for encapsulating common patterns
//! - **Clean DX**: Fluent API with `auto_input`/`auto_output` and [`run()`] for zero boilerplate
//!
//! ## Quick Start
//!
//! ```ignore
//! use aithericon_sdk::prelude::*;
//!
//! // Define token types with #[token] - adds Serialize, JsonSchema derives
//! #[token]
//! struct Task { id: String, name: String }
//!
//! #[token]
//! struct Worker { id: String, skill: String }
//!
//! #[token]
//! struct Assignment { task_id: String, worker_id: String }
//!
//! fn definition(ctx: &mut Context) {
//!     // Create typed places - 4 types: state, resource, signal, terminal
//!     let tasks = ctx.state::<Task>("tasks", "Task Queue");
//!     let workers = ctx.state::<Worker>("workers", "Workers");
//!     let in_progress = ctx.state::<Assignment>("in-progress", "In Progress");
//!
//!     // Fluent API: define ports and wire arcs in one call
//!     ctx.transition("allocate", "Allocate Task")
//!         .auto_input("task", &tasks)
//!         .auto_input("worker", &workers)
//!         .auto_output("assignment", &in_progress)
//!         .logic(r#"#{ assignment: #{ task_id: task.id, worker_id: worker.id } }"#);
//! }
//!
//! fn main() {
//!     // run() handles CLI args, validation, and deployment
//!     aithericon_sdk::run("my-workflow", "A sample workflow", definition);
//! }
//! ```
//!
//! ## Functional Step Macros
//!
//! For more complex workflows, use the `#[step]` macro for a functional syntax:
//!
//! ```ignore
//! use aithericon_sdk::prelude::*;
//!
//! #[token]
//! struct Request { id: String, amount: f64 }
//!
//! #[token]
//! struct Approved { id: String }
//!
//! #[token]
//! struct Rejected { id: String, reason: String }
//!
//! // Guard expressions control when transitions fire
//! #[step("t_auto_approve", "Auto Approve")]
//! #[guard("request.amount <= 1000.0")]
//! fn auto_approve(request: Request) -> Approved {
//!     r#"#{ approved: #{ id: request.id } }"#
//! }
//!
//! #[step("t_review", "Manual Review")]
//! #[guard("request.amount > 1000.0")]
//! fn review(request: Request) -> Rejected {
//!     r#"#{ rejected: #{ id: request.id, reason: "Needs review" } }"#
//! }
//!
//! fn definition(ctx: &mut Context) {
//!     let requests = ctx.state::<Request>("requests", "Requests");
//!     let approved = ctx.state::<Approved>("approved", "Approved");
//!     let rejected = ctx.state::<Rejected>("rejected", "Rejected");
//!
//!     // Wire the step - creates transition and arcs
//!     auto_approve(ctx, &requests, &approved);
//!     review(ctx, &requests, &rejected);
//! }
//! ```
//!
//! ## Advanced Usage
//!
//! For complex cases (custom cardinality, weighted arcs, components), use the tuple-based API:
//!
//! ```ignore
//! let (t, task_in) = ctx.transition("process", "Process Task")
//!     .input::<Task>("task", Cardinality::Batch);
//! let (t, worker_in) = t.input::<Worker>("worker", Cardinality::Single);
//!
//! t.wire_input(&tasks, &task_in)
//!  .wire_input(&workers, &worker_in)
//!  .logic(r#"#{ result: ... }"#);
//! ```
//!
//! ## Place Types
//!
//! | Type | Method | Purpose |
//! |------|--------|---------|
//! | State | [`Context::state`] | Workflow state markers (e.g., "Processing", "Pending") |
//! | Signal | [`Context::signal`] | External event inputs (e.g., "User Approved") |
//! | Resource | [`Context::resource_def`] | Resource state machines (e.g., "Workers", "GPUs") |
//!
//! ## Mock Adapters
//!
//! Simulate external services with [`Context::mock_adapter`] and [`Context::timeout_adapter`]:
//!
//! ```ignore
//! // Simulate a payment gateway with 2s latency
//! ctx.mock_adapter(
//!     &pending_payment,
//!     "Payment Gateway",
//!     2000,
//!     r#"#{ target_place: "p_payment_result", data: #{ id: token.id, success: true } }"#,
//! );
//!
//! // SLA timeout - only fires if token still exists after delay
//! ctx.timeout_adapter(
//!     &waiting,
//!     "SLA Monitor",
//!     30000,
//!     r#"#{ target_place: "p_timeout", data: #{ id: token.id } }"#,
//! );
//! ```
//!
//! ## Documentation
//!
//! For comprehensive documentation, see the `docs/` folder:
//! - `docs/sdk/core-concepts.md` - Places, tokens, transitions, arcs
//! - `docs/sdk/macros.md` - `#[token]` and `#[step]` macro reference
//! - `docs/sdk/contracts-and-helpers.md` - Typed effect contracts, helpers, components
//! - `docs/engine/air-format.md` - AIR JSON specification
//! - `docs/engine/execution-rules.md` - Engine behavior, firing rules, priority

pub mod bridge;
pub mod component;
pub mod components;
pub mod context;
pub mod contracts;
pub mod effect_tokens;
pub mod place;
pub mod port;
pub mod python_job;
pub mod resource;
pub mod runner;
pub mod scenario;
pub mod signal_tokens;
pub mod step;
pub mod token;
pub mod transition;
pub mod validation;

// Re-exports for convenience
pub use component::Component;
pub use components::{
    CancelledSignal, ClaimFailedJob, ClaimHandleToken, ClaimInput, ClaimJobResult, ClaimOutput,
    ClaimPattern, ClaimProcessing, ClaimRelease, CompletedSignal, ExecErrorSignal,
    ExecutorBridges, ExecutorLifecycleHandles, InvalidationSignal,
    executor_lifecycle,
};
pub use context::{Context, SpawnChildIO, SpawnHandles, TimerHandles};
pub use place::{PlaceHandle, Target};
pub use port::{Cardinality, InputPort, OutputPort};
pub use resource::{Resource, ResourceBuilder, ResourceStateBuilder};
pub use runner::run;
pub use scenario::{
    AdapterLogic, BridgeTargetDto, MockAdapterConfig, ScenarioArc, ScenarioDefinition,
    ScenarioPlace, ScenarioPort, ScenarioToken, ScenarioTransition, TransitionGuard,
    TransitionLogic,
};
pub use step::StepDefinition;
pub use token::{DynamicToken, IntegerToken, Token, UnitToken};
pub use contracts::{
    ExecutorCancel, ExecutorSubmit, HumanTaskCancel, HumanTaskSubmit, ProcessComplete, ProcessStart,
    SchedulerCancel, SchedulerSubmit, TimerCancel, TimerSchedule,
};
pub use transition::TransitionBuilder;
pub use validation::{mock_from_schema, validate, validate_script, validate_with_mocks, ValidationResult};

// Re-export effect descriptors from petri-domain for typed effect API
pub use petri_domain::effects::{self as effects, EffectDescriptor, ServiceCategory};

// Re-export typed effect tokens for built-in handlers
pub use effect_tokens::{
    EffectError, ExecutorCancelInput, ExecutorCancelled, ExecutorEventSignal, ExecutorStatusSignal,
    ExecutorSubmitInput, ExecutorSubmitted, HumanCancelInput, HumanTaskAssigned,
    HumanTaskCancelled, HumanTaskResponse, ProcessMetadata, ProcessStartConfig, ProcessStarted,
    ProcessStepDef, ProcessUpdate, ProcessUpdateType, SchedulerCancelInput, SchedulerCancelled,
    SchedulerStatusSignal, SchedulerSubmitInput, SchedulerSubmitted, TimerCancelInput,
    TimerCancelled, TimerInput, TimerScheduled,
};

// Re-export typed signal tokens for external signal sources
pub use signal_tokens::{CatalogueArtifact, CatalogueSignalToken};

// Re-export domain human task types for typed form definitions
pub use petri_domain::human::{
    CalloutSeverity, DownloadItem, HumanTaskRequest, SelectOption, TableAlignment, TaskBlock,
    TaskField, TaskFieldKind, TaskStep,
};

// Re-export executor domain types for typed execution specs, status, and results
pub use aithericon_executor_domain::{
    // Job construction
    ExecutionSpec, InputDeclaration, InputSource, OutputDeclaration, JobPriority,
    // Status & results
    ExecutionStatus, ExecutionOutcome, ExecutionResult,
    EventCategory, StatusUpdate, StatusDetail, ExecutionEvent,
    // Observability
    Artifact, ArtifactCategory, ArtifactManifest,
    Progress, Phase, PhaseStatus,
    LogLevel, LogEntry, LogSummary,
    MetricType, MetricPoint, MetricSummary,
};

// Re-export attribute macros from the derive crate
pub use aithericon_sdk_derive::step;
pub use aithericon_sdk_derive::token;

// Re-export Place type alias for the step macro
pub type Place = PlaceHandle<()>;

/// Create a secret reference string: `{{secret:KEY}}`.
///
/// Use this in [`TransitionBuilder::effect_with_config`] JSON to reference
/// runtime secrets. Secrets are resolved just-in-time by the engine before
/// handler execution and never appear in the event log.
///
/// # Example
///
/// ```ignore
/// use aithericon_sdk::secret;
///
/// ctx.transition("call_api", "Call API")
///     .effect_with_config("http_handler", serde_json::json!({
///         "url": "https://api.example.com",
///         "auth": { "token": secret("API_TOKEN") }
///     }));
/// ```
pub fn secret(key: &str) -> String {
    format!("{{{{secret:{key}}}}}")
}

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::bridge::{BridgeAddress, BridgeSource, BridgeTarget};
    pub use crate::component::Component;
    pub use crate::components::{
        CancelledSignal, ClaimFailedJob, ClaimHandleToken, ClaimInput, ClaimJobResult, ClaimOutput,
        ClaimPattern, ClaimProcessing, ClaimRelease, CompletedSignal, ExecErrorSignal,
        ExecutorBridges, ExecutorLifecycleHandles, InvalidationSignal,
        executor_lifecycle,
    };
    pub use crate::context::{Context, SpawnChildIO, SpawnHandles};
    pub use crate::place::{PlaceHandle, Target};
    pub use crate::port::{Cardinality, InputPort, OutputPort};
    pub use crate::resource::{Resource, ResourceBuilder, ResourceStateBuilder};
    pub use crate::run;
    pub use crate::scenario::{
        BridgeTargetDto, ScenarioDefinition, TransitionGuard, TransitionLogic,
    };
    pub use crate::step::StepDefinition;
    pub use crate::token::{DynamicToken, IntegerToken, Token, UnitToken};
    pub use crate::contracts::{
        ExecutorCancel, ExecutorSubmit, HumanTaskCancel, HumanTaskSubmit, ProcessComplete,
        ProcessStart, SchedulerCancel, SchedulerSubmit, TimerCancel, TimerSchedule,
    };
    pub use crate::validation::{validate, validate_with_mocks, ValidationResult};
    pub use crate::secret;
    pub use crate::Place;

    // Re-export attribute macros
    pub use aithericon_sdk_derive::step;
    pub use aithericon_sdk_derive::token;

    // Re-export typed effect tokens
    pub use crate::effect_tokens::{
        EffectError, ExecutorCancelInput, ExecutorCancelled, ExecutorEventSignal,
        ExecutorStatusSignal, ExecutorSubmitInput, ExecutorSubmitted, HumanCancelInput,
        HumanTaskAssigned, HumanTaskCancelled, HumanTaskResponse, ProcessMetadata,
        ProcessStartConfig, ProcessStarted, ProcessStepDef, ProcessUpdate, ProcessUpdateType,
        SchedulerCancelInput, SchedulerCancelled, SchedulerStatusSignal, SchedulerSubmitInput,
        SchedulerSubmitted, TimerCancelInput, TimerCancelled, TimerInput, TimerScheduled,
    };

    // Re-export typed signal tokens for external signal sources
    pub use crate::signal_tokens::{CatalogueArtifact, CatalogueSignalToken};

    // Re-export domain human task types for typed form definitions
    pub use petri_domain::human::{
        CalloutSeverity, DownloadItem, HumanTaskRequest, SelectOption, TableAlignment, TaskBlock,
        TaskField, TaskFieldKind, TaskStep,
    };

    // Re-export executor domain types for typed execution specs
    pub use aithericon_executor_domain::{
        // Job construction
        ExecutionSpec, InputDeclaration, InputSource, OutputDeclaration, JobPriority,
        // Status & results
        ExecutionStatus, ExecutionOutcome, ExecutionResult,
        EventCategory, StatusUpdate, StatusDetail, ExecutionEvent,
        // Observability
        Artifact, ArtifactCategory, ArtifactManifest,
        Progress, Phase, PhaseStatus,
        LogLevel, LogEntry, LogSummary,
        MetricType, MetricPoint, MetricSummary,
    };

    // Re-export common derives (still available for manual use)
    pub use schemars::JsonSchema;
    pub use serde::Serialize;
}
