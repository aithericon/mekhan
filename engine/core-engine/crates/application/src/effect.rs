//! Effect handler trait and types for side-effect transitions.
//!
//! Effect transitions execute side effects (HTTP calls, Nevergrad, SLURM, etc.)
//! in live mode but replay deterministically from stored event results in replay mode.

use std::collections::HashMap;

use petri_domain::TransitionId;
use serde::{Deserialize, Serialize};

/// Execution mode for the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    /// Normal execution — effect handlers are called.
    #[default]
    Live,
    /// Replay from event log — stored results are used instead.
    Replay,
}

/// Input provided to an effect handler.
#[derive(Clone, Debug)]
pub struct EffectInput {
    /// The transition being fired.
    pub transition_id: TransitionId,
    /// Bound input tokens as port_name → JSON data.
    pub inputs: HashMap<String, serde_json::Value>,
    /// Optional static configuration from the transition definition.
    pub config: Option<serde_json::Value>,
    /// Read-arc tokens as port_name → JSON data (borrowed, not consumed).
    pub read_inputs: HashMap<String, serde_json::Value>,
    /// Process step key from the transition's annotation (for handlers like HumanTaskHandler).
    pub process_step: Option<String>,
}

/// Output from an effect handler.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectOutput {
    /// Produced tokens as port_name → JSON data.
    pub tokens: HashMap<String, serde_json::Value>,
    /// Opaque result payload stored in the event log for replay.
    pub result: serde_json::Value,
}

/// Declared port schema contracts for an effect handler.
///
/// When provided via `port_schemas()`, the service validates at registration
/// time that the handler's declared ports and schema_refs match the transition
/// definitions in the topology.
#[derive(Clone, Debug, Default)]
pub struct EffectPortSchemas {
    /// Input port name → expected schema_ref
    pub inputs: HashMap<String, String>,
    /// Output port name → expected schema_ref
    pub outputs: HashMap<String, String>,
}

/// Trait for effect handlers that execute side effects.
///
/// Implementations are registered on the service via `register_effect_handler`
/// and looked up by `effect_handler_id` on the transition.
#[async_trait::async_trait]
pub trait EffectHandler: Send + Sync {
    /// Execute the side effect (called in live mode only).
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError>;

    /// Rebuild owned state from a stored effect result during replay.
    ///
    /// Called in replay mode instead of `execute()`. Default is no-op.
    /// Override this when the handler maintains internal state that must
    /// be reconstructed from the event log (e.g., Nevergrad optimizer
    /// tracking explored parameter space).
    fn replay(&self, _input: &EffectInput, _stored_result: &serde_json::Value) {}

    /// Human-readable name for this handler.
    fn name(&self) -> &str;

    /// Declare the port schema contract for this handler.
    ///
    /// If `Some`, the service validates at registration time that the handler's
    /// declared ports match the transition definitions. Default is `None`
    /// (no contract validation).
    fn port_schemas(&self) -> Option<EffectPortSchemas> {
        None
    }
}

/// Errors from effect handler execution.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EffectError {
    #[error("Effect execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Effect fatal error: {0}")]
    Fatal(String),

    #[error("Effect handler not found: {0}")]
    HandlerNotFound(String),

    #[error("Replay event missing for effect transition {0}")]
    ReplayMissing(String),
}

impl EffectError {
    /// Whether the error is retryable.
    ///
    /// `ExecutionFailed` is treated as transient (retryable).
    /// `Fatal` is permanent (not retryable).
    /// Infrastructure errors (`HandlerNotFound`, `ReplayMissing`) are not retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(self, EffectError::ExecutionFailed(_))
    }
}
