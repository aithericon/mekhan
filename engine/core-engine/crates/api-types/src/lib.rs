pub mod subjects;

use std::collections::HashMap;

use petri_domain::{Marking, PersistedEvent, PetriNet, PlaceId, TokenColor, TransitionId};
// Re-export adapter types from domain (polymorphic AdapterLogic with Rhai support)
pub use petri_domain::{AdapterLogic, MockAdapterConfig};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Response for GET /api/topology
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct TopologyResponse {
    pub topology: Option<PetriNet>,
}

/// Response for GET /api/events
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct EventsResponse {
    pub events: Vec<PersistedEvent>,
    pub chain_valid: bool,
}

/// Status of a transition (enabled or reason for being disabled)
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TransitionStatus {
    /// Transition can fire
    Enabled,
    /// Transition has a guard but no tokens to evaluate
    DisabledNoTokens { missing_place: String },
    /// Guard evaluated to false
    DisabledGuardFailed { guard: String },
    /// Guard script error
    DisabledGuardError { error: String },
}

/// Response for GET /api/state
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct StateResponse {
    pub marking: Marking,
    pub enabled_transitions: Vec<TransitionId>,
    /// Status for all transitions (enabled or why disabled)
    pub transition_statuses: HashMap<String, TransitionStatus>,
    /// Current run mode of the engine
    pub run_mode: RunMode,
}

/// Request for POST /api/command/create-token
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateTokenRequest {
    pub place_id: PlaceId,
    pub color: TokenColor,
}

/// Request for PATCH /api/topology/transition/:id
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct UpdateTransitionRequest {
    /// The new script (Rhai code)
    pub script: String,
    /// Optional guard script (Rhai code that returns bool)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
}

/// Generic command response
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct CommandResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<PersistedEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CommandResponse {
    pub fn success(event: PersistedEvent) -> Self {
        Self {
            success: true,
            event: Some(event),
            error: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            event: None,
            error: Some(message.into()),
        }
    }
}

/// Error response
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

impl ErrorResponse {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
        }
    }
}

// =============================================================================
// Scenario Loading DTOs (AIR - Aithericon Intermediate Representation)
// =============================================================================

/// A place definition in the scenario JSON format.
/// Uses simple string IDs for user-friendliness.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ScenarioPlace {
    /// Unique string identifier (e.g., "workers", "tasks")
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Type: "state" or "signal"
    #[serde(rename = "type", default = "default_place_type")]
    pub place_type: String,
    /// Group this place belongs to (for visualization)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Maximum capacity (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<usize>,
    /// Initial tokens at this place
    #[serde(default)]
    pub initial_tokens: Vec<ScenarioToken>,
    /// JSON Schema reference for tokens in this place (e.g., "#/definitions/Task")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_schema: Option<String>,
    /// If set, tokens produced here are forwarded to a remote net via bridge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_out: Option<BridgeTargetDto>,
    /// If true, tokens produced here are routed back to the reply address
    /// from consumed tokens' reply_routing (response half of request-reply)
    #[serde(default)]
    pub bridge_reply: bool,
    /// Named reply channel for bridge_reply places (reads from reply_channels map instead of reply_to)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_reply_channel: Option<String>,
    /// Bridge-in source annotation (where tokens come from)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_in: Option<BridgeSourceDto>,
}

/// Bridge target for cross-net token transfer.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct BridgeTargetDto {
    pub target_net_id: String,
    pub target_place_name: String,
    /// Local place name to receive replies (enables request-reply pattern)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Named reply channels: channel_name → local_place_name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_channels: Option<std::collections::HashMap<String, String>>,
    /// Display name for UI grouping (used instead of target_net_id when present).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Bridge source annotation for bridge_in places (visualization metadata).
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct BridgeSourceDto {
    pub source_net_id: String,
    pub source_place_name: String,
}

fn default_place_type() -> String {
    "state".to_string()
}

/// A token definition in the scenario.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum ScenarioToken {
    /// Simple unit token (just a counter)
    Unit,
    /// Integer value token
    Integer(i64),
    /// Rich data token with JSON payload
    Data(serde_json::Value),
}

/// A port definition for transitions.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ScenarioPort {
    /// Port name (used in scripts and arc connections)
    pub name: String,
    /// Optional schema reference for type validation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_ref: Option<String>,
    /// Cardinality: "single" (default) or "batch"
    #[serde(default = "default_cardinality")]
    pub cardinality: String,
}

fn default_cardinality() -> String {
    "single".to_string()
}

/// An arc definition connecting a place to a transition port.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ScenarioArc {
    /// Place ID (source for input arcs, target for output arcs)
    pub place: String,
    /// Port name on the transition
    pub port: String,
    /// Arc weight (default: 1)
    #[serde(default = "default_weight")]
    pub weight: usize,
    /// If true, this is a read arc: token consumed for evaluation, auto-produced back.
    /// Only meaningful on input arcs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub read: bool,
    /// Gather barrier: a producer-namespaced reference (e.g. `"expected.k"`) to a
    /// field on a bound coordinator token supplying the count `K` of result tokens
    /// this Batch input arc must accumulate before the transition fires. `None`
    /// (the default) means today's behavior — no count-gate. Only meaningful on
    /// input arcs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_from: Option<String>,
    /// Gather barrier: an optional field name read from the coordinator token and
    /// matched against the same-named field on result tokens, so only tokens from
    /// one gather group (e.g. one loop iteration's `iteration_id`) are consumed.
    /// `None` makes every token in the place eligible. Only meaningful alongside
    /// `count_from`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlate_on: Option<String>,
    /// Output arc only: emit the produced token WITHOUT inheriting the firing's
    /// consumed reply-routing (it starts routing-less). `false` (default) keeps
    /// today's inherit-and-merge behavior. Set for a recycled resource token
    /// that must stay re-grantable (see engine `Arc::reset_reply_routing`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub reset_reply_routing: bool,
}

fn default_weight() -> usize {
    1
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Simulation configuration for async mock behavior (DEPRECATED).
/// Use mock_adapters at the scenario level instead.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct SimulationConfig {
    /// Base delay in milliseconds before the transition fires
    pub duration_ms: u64,
    /// Random variance (+/-) in milliseconds for realism
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variance_ms: Option<u64>,
}

/// Polymorphic transition logic - supports Rhai now, Wasm later, Effect for side effects
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransitionLogic {
    /// Rhai script (editable in Lab)
    Rhai { source: String },
    /// Wasm module (future - compiled Rust)
    Wasm {
        /// Base64-encoded wasm or path to .wasm file
        module: String,
        /// Export function name to call
        function: String,
    },
    /// Effect transition — side effect executed by a registered handler
    Effect {
        /// ID of the registered EffectHandler
        handler_id: String,
        /// Optional static configuration for the effect handler
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<serde_json::Value>,
    },
}

impl TransitionLogic {
    /// Extract the Rhai source if this is a Rhai logic block
    pub fn as_rhai_source(&self) -> Option<&str> {
        match self {
            TransitionLogic::Rhai { source } => Some(source),
            _ => None,
        }
    }

    /// Extract the effect handler ID if this is an Effect logic block
    pub fn as_effect_handler_id(&self) -> Option<&str> {
        match self {
            TransitionLogic::Effect { handler_id, .. } => Some(handler_id),
            _ => None,
        }
    }
}

/// Polymorphic guard condition
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransitionGuard {
    /// Rhai script that returns bool
    Rhai { source: String },
    /// Wasm module (future)
    Wasm { module: String, function: String },
}

impl TransitionGuard {
    /// Extract the Rhai source if this is a Rhai guard
    pub fn as_rhai_source(&self) -> Option<&str> {
        match self {
            TransitionGuard::Rhai { source } => Some(source),
            _ => None,
        }
    }
}

/// Polymorphic priority expression — evaluated against `port_inputs` at
/// selection time to produce a numeric score. Higher score = higher priority.
/// Mirrors `aithericon_sdk::TransitionPriority` and `TransitionGuard`'s shape.
///
/// Was previously dropped silently on the API boundary (no field on
/// `ScenarioTransition`), so cascade-aware lowerings like Decision/Loop fell
/// back to alphabetical ID tiebreak — `t_dec_deadend` < `t_dec_default`,
/// so the deadend won the default's tiebreaker when its guard was satisfied.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransitionPriority {
    /// Rhai script that returns a numeric value
    Rhai { source: String },
    /// Wasm module (future)
    Wasm { module: String, function: String },
}

impl TransitionPriority {
    /// Extract the Rhai source if this is a Rhai priority expression
    pub fn as_rhai_source(&self) -> Option<&str> {
        match self {
            TransitionPriority::Rhai { source } => Some(source),
            _ => None,
        }
    }
}

/// A transition definition in the scenario.
/// Transitions are "chips" with named input/output ports.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ScenarioTransition {
    /// Unique string identifier (e.g., "assign", "complete")
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Group this transition belongs to (for visualization)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Input ports - "pins" on the left side of the chip
    #[serde(default)]
    pub input_ports: Vec<ScenarioPort>,
    /// Output ports - "pins" on the right side of the chip
    #[serde(default)]
    pub output_ports: Vec<ScenarioPort>,
    /// Input arcs - connect places to input ports
    #[serde(default)]
    pub inputs: Vec<ScenarioArc>,
    /// Output arcs - connect output ports to places
    #[serde(default)]
    pub outputs: Vec<ScenarioArc>,
    /// Guard condition - optional, must evaluate to bool
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<TransitionGuard>,
    /// Priority expression — selection-time tiebreaker after enabling time
    /// and input-count specificity. Higher score wins; `None` falls through
    /// to alphabetical transition-id ordering. Must be plumbed end-to-end
    /// (api-types → scenario_loader → domain.Transition.priority) or
    /// cascade-style lowerings (Decision deadend, Loop continue/exit) get
    /// silently mis-ordered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<TransitionPriority>,
    /// Finalizer flag — see `petri_domain::Transition::finalizer`. A finalizer
    /// fires ONLY during the engine's post-failure drain (never in normal
    /// evaluation); it releases resources a net still holds when it fails
    /// permanently (e.g. a LeaseScope's held lease). Must be plumbed end-to-end
    /// (SDK builder → AIR → api-types → scenario_loader → domain.Transition) or
    /// failure-path lease release is silently dropped.
    #[serde(default, skip_serializing_if = "is_false")]
    pub finalizer: bool,
    /// Polymorphic logic definition
    pub logic: TransitionLogic,
    /// Combined input schema (for Wasm validation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// Output schema (for Wasm validation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// Simulation configuration for async mock behavior (frontend-only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulation: Option<SimulationConfig>,
    /// Signal place IDs that this transition is expected to cause (causation arcs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caused_signals: Vec<String>,
    /// Process step key: publish "step_started" after this transition fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_step_started: Option<String>,
    /// Process step key: publish "step_completed" after this transition fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_step_completed: Option<String>,
}

/// Group definition for visual components (alias for the domain-level `Group`).
pub type ScenarioGroup = petri_domain::Group;

/// The highest AIR format version this build can interpret. Single source for
/// BOTH sides of the contract: the service compiler stamps it into emitted AIR
/// and the engine's deploy gate rejects `air_version > SUPPORTED_AIR_VERSION`.
///
/// Bump it for any change a v(N) engine cannot safely interpret — a field
/// whose absence/old reading silently changes execution semantics, a reshaped
/// structure, a new load-bearing logic variant. Purely additive, ignorable
/// metadata does NOT bump it (the struct stays forward-tolerant: no
/// `deny_unknown_fields`).
pub const SUPPORTED_AIR_VERSION: u32 = 1;

/// `air_version` serde default: a payload without the field predates
/// versioning and is by definition v1. Deliberately a literal `1`, NOT
/// `SUPPORTED_AIR_VERSION` — when the supported version bumps, old unversioned
/// payloads still mean v1.
fn default_air_version() -> u32 {
    1
}

/// Complete scenario definition (the "AIR" format).
/// This is what users will write in JSON to define Petri Nets.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ScenarioDefinition {
    /// AIR format version this definition was emitted for. Bumped only for
    /// changes a v(N) engine cannot safely interpret (see
    /// [`SUPPORTED_AIR_VERSION`]); validated at deploy time. Missing on
    /// pre-versioning payloads ⇒ v1. Always serialized.
    #[serde(default = "default_air_version")]
    pub air_version: u32,
    /// Scenario name
    pub name: String,
    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Places in the net
    pub places: Vec<ScenarioPlace>,
    /// Transitions in the net
    pub transitions: Vec<ScenarioTransition>,
    /// Groups for visualization (hierarchical components)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<ScenarioGroup>,
    /// Mock adapters for simulating external systems
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mock_adapters: Vec<MockAdapterConfig>,
    /// JSON Schema definitions for all token types used
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub definitions: HashMap<String, serde_json::Value>,
    /// Infrastructure requirements declared by this scenario.
    /// The engine validates all required services are configured at load time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirements: Vec<petri_domain::effects::ServiceRequirement>,
}

/// Per-run dispatch options re-exported from `petri-domain`. See
/// [`petri_domain::DispatchOptions`] for full semantics. Re-exported here so
/// `LoadScenarioRequest` (an api-types wire DTO) and the cloud-layer-side
/// adapter's mirror can share the same canonical type without the api-types
/// crate having to define a parallel shape.
pub use petri_domain::DispatchOptions;

/// Request body for `POST /api/scenario` + `POST /api/nets/{net_id}/scenario`
/// (sub-phase 2.5e-γ.mekhan envelope shape — replaces the prior bare-
/// `ScenarioDefinition` type alias per
/// `feedback_no_backward_compat_hedging_in_migration_waves`; cloud-layer-
/// workflow's `core_engine_client` sends this shape unconditionally since
/// commit `5b5f308`).
///
/// `skip_mask` + `stage_overrides` serialize-skip-if-empty, so a load
/// without dispatch options renders as `{ "scenario": <scenario> }` on the
/// wire — still the envelope shape, just with no additive keys.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct LoadScenarioRequest {
    /// Petri-net definition (canonical structural shape; unchanged from the
    /// pre-γ.mekhan `ScenarioDefinition` semantics).
    pub scenario: ScenarioDefinition,
    /// First-class tenant (workspace) identifier for this net instance.
    ///
    /// Per ADR-09 every NATS subject/stream/KV/durable the engine creates for
    /// this net carries a `{workspace_id}` segment, giving hard subject-level
    /// isolation between tenants hosted in one engine process. It is stored
    /// PER-`NetInstance` at load time and threaded into THAT net's
    /// publisher/consumer/listener/KV — it is NEVER a process-global.
    ///
    /// Deliberately first-class (NOT carried in the opaque `net_parameters`
    /// bag): the engine ascribes routing semantics to it. Absent ⇒ the net
    /// routes on the reserved `"default"` workspace sentinel
    /// ([`subjects::Subjects::DEFAULT_WORKSPACE`]) for legacy/SDK/demo/dev
    /// loads. Serialize-skips when absent so such loads render byte-identically
    /// to the prior shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Transition IDs to skip at evaluate-time. See [`DispatchOptions::skip_mask`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_mask: Vec<String>,
    /// Per-transition JSON merge-patch overrides keyed by transition_id. See
    /// [`DispatchOptions::stage_overrides`].
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub stage_overrides: HashMap<String, serde_json::Value>,
    /// Net-level parameter bag for the spawned instance. Stored on the engine's
    /// `PetriNetService` via `set_net_parameters` at load time and consulted by
    /// the firing path for `$params.` resolution and pre-dispatch metadata
    /// (e.g. `tenant_id`). Opaque, submitter-owned JSON — the engine ascribes
    /// no domain semantics to its contents. Serialize-skips when absent so a
    /// load without parameters renders byte-identically to the prior shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_parameters: Option<serde_json::Value>,
}

impl LoadScenarioRequest {
    /// Extract the dispatch options into a flat `DispatchOptions` for storage
    /// on the engine's `NetInstance`.
    pub fn dispatch_options(&self) -> DispatchOptions {
        DispatchOptions {
            skip_mask: self.skip_mask.clone(),
            stage_overrides: self.stage_overrides.clone(),
        }
    }

    /// Consume the envelope, returning the inner `ScenarioDefinition`.
    pub fn into_scenario(self) -> ScenarioDefinition {
        self.scenario
    }

    /// Construct an envelope from a bare scenario, with no dispatch options
    /// and no explicit workspace (routes on the `"default"` sentinel).
    /// Convenience for tests and ergonomic call sites that don't need
    /// ablation.
    pub fn from_scenario(scenario: ScenarioDefinition) -> Self {
        Self {
            scenario,
            skip_mask: Vec::new(),
            stage_overrides: HashMap::new(),
            net_parameters: None,
            workspace_id: None,
        }
    }

    /// Resolve the effective workspace for this load: the explicit
    /// `workspace_id` if present, else the reserved
    /// [`subjects::Subjects::DEFAULT_WORKSPACE`] sentinel.
    pub fn workspace(&self) -> &str {
        self.workspace_id
            .as_deref()
            .unwrap_or(crate::subjects::Subjects::DEFAULT_WORKSPACE)
    }
}

/// Response for POST /api/scenario
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct LoadScenarioResponse {
    pub success: bool,
    /// Number of places loaded
    pub places_count: usize,
    /// Number of transitions loaded
    pub transitions_count: usize,
    /// Number of initial tokens created
    pub tokens_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// =============================================================================
// Run Mode and Evaluation DTOs
// =============================================================================

/// Engine run mode - controls automatic evaluation behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    /// Engine is stopped - transitions only fire on explicit commands
    #[default]
    Stopped,
    /// Engine is running - automatically evaluates on token events
    Running,
}

/// Request for POST /api/command/evaluate (one-shot evaluation)
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
pub struct EvaluateRequest {
    /// Maximum number of transitions to fire (default: 1000)
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
}

fn default_max_steps() -> usize {
    1000
}

/// A transition that was fired during evaluation
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct FiredTransition {
    /// The transition ID (UUID)
    pub transition_id: String,
    /// The event sequence number
    pub sequence: u64,
}

/// Final state after evaluation
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvaluateFinalState {
    /// No more transitions can fire
    Quiescent,
    /// Reached the max_steps limit
    LimitReached,
}

/// Response for POST /api/command/evaluate
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct EvaluateResponse {
    pub success: bool,
    /// Number of transitions fired
    pub steps_executed: usize,
    /// Final state after evaluation
    pub final_state: EvaluateFinalState,
    /// List of transitions that were fired
    pub transitions_fired: Vec<FiredTransition>,
    /// Error message if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl EvaluateResponse {
    pub fn success(
        steps_executed: usize,
        final_state: EvaluateFinalState,
        transitions_fired: Vec<FiredTransition>,
    ) -> Self {
        Self {
            success: true,
            steps_executed,
            final_state,
            transitions_fired,
            error: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            steps_executed: 0,
            final_state: EvaluateFinalState::Quiescent,
            transitions_fired: vec![],
            error: Some(message.into()),
        }
    }
}

/// Request for PUT /api/run-mode
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct SetRunModeRequest {
    pub mode: RunMode,
}

/// Response for GET/PUT /api/run-mode
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct RunModeResponse {
    pub success: bool,
    /// Previous mode (only present in PUT response)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_mode: Option<RunMode>,
    /// Current mode
    pub current_mode: RunMode,
}

/// Response for GET /api/services
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ServicesResponse {
    /// All registered effect handler IDs
    pub handlers: Vec<String>,
    /// Handlers grouped by service category
    pub categories: HashMap<String, Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_response_roundtrip() {
        let json = serde_json::json!({
            "marking": {
                "tokens": {
                    "start": [{
                        "id": "00000000-0000-0000-0000-000000000001",
                        "color": {"type": "Unit"},
                        "created_at": "2026-01-01T00:00:00Z"
                    }]
                }
            },
            "enabled_transitions": ["t1"],
            "transition_statuses": {
                "t1": {"status": "enabled"},
                "t2": {"status": "disabled_no_tokens", "missing_place": "input"}
            },
            "run_mode": "running"
        });

        let state: StateResponse = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(state.run_mode, RunMode::Running);
        assert_eq!(state.enabled_transitions.len(), 1);
        assert_eq!(state.transition_statuses.len(), 2);
        assert!(matches!(
            state.transition_statuses.get("t2"),
            Some(TransitionStatus::DisabledNoTokens { .. })
        ));

        // Roundtrip
        let reserialized = serde_json::to_value(&state).unwrap();
        let _: StateResponse = serde_json::from_value(reserialized).unwrap();
    }

    #[test]
    fn run_mode_response_roundtrip() {
        let json = serde_json::json!({
            "success": true,
            "previous_mode": "stopped",
            "current_mode": "running"
        });

        let resp: RunModeResponse = serde_json::from_value(json).unwrap();
        assert!(resp.success);
        assert_eq!(resp.previous_mode, Some(RunMode::Stopped));
        assert_eq!(resp.current_mode, RunMode::Running);
    }

    #[test]
    fn topology_response_roundtrip() {
        let json = serde_json::json!({
            "topology": null
        });
        let resp: TopologyResponse = serde_json::from_value(json).unwrap();
        assert!(resp.topology.is_none());
    }

    #[test]
    fn evaluate_response_roundtrip() {
        let json = serde_json::json!({
            "success": true,
            "steps_executed": 3,
            "final_state": "quiescent",
            "transitions_fired": [
                {"transition_id": "t1", "sequence": 0},
                {"transition_id": "t2", "sequence": 1}
            ]
        });

        let resp: EvaluateResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.steps_executed, 3);
        assert_eq!(resp.final_state, EvaluateFinalState::Quiescent);
        assert_eq!(resp.transitions_fired.len(), 2);
    }

    #[test]
    fn scenario_definition_roundtrip() {
        let json = serde_json::json!({
            "name": "test-scenario",
            "places": [{
                "id": "p1",
                "name": "Place 1",
                "type": "state",
                "initial_tokens": [42, {"key": "value"}]
            }],
            "transitions": [{
                "id": "t1",
                "name": "Transition 1",
                "inputs": [{"place": "p1", "port": "in"}],
                "outputs": [],
                "logic": {"type": "rhai", "source": "#{out: in}"}
            }]
        });

        let scenario: ScenarioDefinition = serde_json::from_value(json).unwrap();
        assert_eq!(scenario.name, "test-scenario");
        assert_eq!(scenario.places.len(), 1);
        assert_eq!(scenario.places[0].initial_tokens.len(), 2);
        assert_eq!(scenario.transitions.len(), 1);
        assert!(matches!(
            &scenario.transitions[0].logic,
            TransitionLogic::Rhai { source } if source == "#{out: in}"
        ));
    }

    #[test]
    fn command_response_constructors() {
        let err = CommandResponse::error("something went wrong");
        assert!(!err.success);
        assert_eq!(err.error.as_deref(), Some("something went wrong"));
        assert!(err.event.is_none());
    }

    /// Pre-versioning AIR payloads carry no `air_version` — they MUST
    /// deserialize as v1, and re-serialize with the field made explicit
    /// (no skip_serializing_if).
    #[test]
    fn scenario_missing_air_version_defaults_to_v1() {
        let json = serde_json::json!({
            "name": "legacy",
            "places": [],
            "transitions": []
        });
        let scenario: ScenarioDefinition = serde_json::from_value(json).unwrap();
        assert_eq!(scenario.air_version, 1);

        let reserialized = serde_json::to_value(&scenario).unwrap();
        assert_eq!(reserialized["air_version"], 1);
    }

    /// A future version number survives the deserialize→serialize round trip
    /// untouched — the deploy-time gate (not serde) is what rejects it.
    #[test]
    fn scenario_air_version_roundtrips() {
        let json = serde_json::json!({
            "name": "from-the-future",
            "air_version": 999,
            "places": [],
            "transitions": []
        });
        let scenario: ScenarioDefinition = serde_json::from_value(json).unwrap();
        assert_eq!(scenario.air_version, 999);

        let reserialized = serde_json::to_value(&scenario).unwrap();
        let back: ScenarioDefinition = serde_json::from_value(reserialized).unwrap();
        assert_eq!(back.air_version, 999);
    }
}
