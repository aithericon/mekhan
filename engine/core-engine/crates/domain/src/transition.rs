use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{Port, TransitionId};

/// serde `skip_serializing_if` helper — omit `finalizer: false` so existing AIR
/// round-trips byte-identically (only `finalizer: true` is emitted).
fn is_false(b: &bool) -> bool {
    !*b
}

/// Simulation configuration for async mock behavior.
/// When present, the frontend will simulate a delay before firing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SimulationConfig {
    /// Base delay in milliseconds before the transition fires
    pub duration_ms: u64,
    /// Random variance (+/-) in milliseconds for realism
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variance_ms: Option<u64>,
}

impl SimulationConfig {
    /// Create a new simulation config with the given duration.
    pub fn new(duration_ms: u64) -> Self {
        Self {
            duration_ms,
            variance_ms: None,
        }
    }

    /// Add variance to the simulation.
    pub fn with_variance(mut self, variance_ms: u64) -> Self {
        self.variance_ms = Some(variance_ms);
        self
    }
}

/// A transition (action) in the Petri Net that consumes tokens from input
/// places and produces tokens in output places.
///
/// Transitions are modeled as "chips" with named input and output ports.
/// Arcs connect places to specific ports on the transition.
///
/// The execution flow is:
/// 1. Guard script (optional) evaluates to bool - determines if transition can fire
/// 2. Script (required) returns a map of port_name → token_data for routing
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Transition {
    /// Unique identifier
    pub id: TransitionId,

    /// Human-readable name
    pub name: String,

    /// Input ports - the "pins" on the left side of the chip that receive tokens
    #[serde(default)]
    pub input_ports: Vec<Port>,

    /// Output ports - the "pins" on the right side of the chip that emit tokens
    #[serde(default)]
    pub output_ports: Vec<Port>,

    /// Guard script (Rhai) - evaluated to determine if transition is enabled.
    /// Receives input tokens as scope variables and must return a boolean.
    /// If None, transition is enabled when all input places have sufficient tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,

    /// Main script (Rhai) - executed when transition fires.
    /// Receives input tokens as scope variables.
    /// Must return a Map<String, Dynamic> where keys are output port names
    /// and values are the token data to produce at each port.
    /// Omitted keys mean no token is produced for that port.
    pub script: String,

    /// Priority expression (Rhai) - evaluated to determine firing priority.
    /// Receives input tokens as scope variables and must return a numeric value.
    /// Higher values = higher priority. Evaluated after enabling time and input count.
    /// If None, no token-based priority is applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,

    /// Finalizer flag. When `true`, this transition is **never** selected during
    /// normal evaluation, and fires ONLY during the engine's post-failure
    /// finalizer drain (see `evaluate_until_quiescent`). It exists to release
    /// resources a net still holds when it fails permanently — e.g. a
    /// `LeaseScope`'s held presence/datacenter lease, whose normal release
    /// (`t_<id>_exit`) is gated on body SUCCESS and so can never fire on the
    /// failure path. The finalizer consumes the still-parked held token and
    /// emits the release to the pool net, so the lease is reclaimed
    /// exactly-once on failure too — fully event-sourced (it fires as an
    /// ordinary `TransitionFired` BEFORE the driver appends `NetFailed`), so a
    /// restart replays the release and the unit is never stranded.
    #[serde(default, skip_serializing_if = "is_false")]
    pub finalizer: bool,

    /// Simulation configuration for async mock behavior.
    /// When present, the frontend will simulate a delay before firing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulation: Option<SimulationConfig>,

    /// Group ID for visualization (hierarchical components)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,

    /// If set, this is an effect transition executed by the named handler.
    /// Effect transitions run side effects in live mode and replay from
    /// stored results in replay mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_handler_id: Option<String>,

    /// Optional static configuration for the effect handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_config: Option<serde_json::Value>,

    /// Signal place IDs that this transition is expected to cause.
    /// Metadata for visualization (causation arcs) — not used by the engine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caused_signals: Vec<String>,

    /// Process step key: publish "step_started" after this transition fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_step_started: Option<String>,

    /// Process step key: publish "step_completed" after this transition fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_step_completed: Option<String>,
}

impl Transition {
    /// Create a new transition with the given name and script.
    pub fn new(name: impl Into<String>, script: impl Into<String>) -> Self {
        let name_str: String = name.into();
        Self {
            id: TransitionId(name_str.clone()),
            name: name_str,
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            guard: None,
            script: script.into(),
            priority: None,
            finalizer: false,
            simulation: None,
            group_id: None,
            effect_handler_id: None,
            effect_config: None,
            caused_signals: Vec::new(),
            process_step_started: None,
            process_step_completed: None,
        }
    }

    /// Add an input port to this transition.
    pub fn with_input_port(mut self, port: Port) -> Self {
        self.input_ports.push(port);
        self
    }

    /// Add multiple input ports to this transition.
    pub fn with_input_ports(mut self, ports: impl IntoIterator<Item = Port>) -> Self {
        self.input_ports.extend(ports);
        self
    }

    /// Add an output port to this transition.
    pub fn with_output_port(mut self, port: Port) -> Self {
        self.output_ports.push(port);
        self
    }

    /// Add multiple output ports to this transition.
    pub fn with_output_ports(mut self, ports: impl IntoIterator<Item = Port>) -> Self {
        self.output_ports.extend(ports);
        self
    }

    /// Set a guard script for this transition.
    pub fn with_guard(mut self, guard: impl Into<String>) -> Self {
        self.guard = Some(guard.into());
        self
    }

    /// Set a priority expression for this transition.
    /// The expression receives input tokens as scope variables and must return a numeric value.
    /// Higher values = higher priority.
    pub fn with_priority(mut self, priority: impl Into<String>) -> Self {
        self.priority = Some(priority.into());
        self
    }

    /// Mark this transition as a finalizer (fires only during the post-failure
    /// finalizer drain). See [`Transition::finalizer`].
    pub fn with_finalizer(mut self, finalizer: bool) -> Self {
        self.finalizer = finalizer;
        self
    }

    /// Set simulation configuration for async mock behavior.
    pub fn with_simulation(mut self, simulation: SimulationConfig) -> Self {
        self.simulation = Some(simulation);
        self
    }

    /// Set the ID for this transition.
    pub fn with_id(mut self, id: TransitionId) -> Self {
        self.id = id;
        self
    }

    /// Set group ID for visualization.
    pub fn with_group_id(mut self, group_id: impl Into<String>) -> Self {
        self.group_id = Some(group_id.into());
        self
    }

    /// Set this transition as an effect transition with the given handler ID.
    pub fn with_effect_handler(mut self, handler_id: impl Into<String>) -> Self {
        self.effect_handler_id = Some(handler_id.into());
        self
    }

    /// Set static configuration for the effect handler.
    pub fn with_effect_config(mut self, config: serde_json::Value) -> Self {
        self.effect_config = Some(config);
        self
    }

    /// Set caused signal place IDs (visualization metadata).
    pub fn with_caused_signals(mut self, signals: Vec<String>) -> Self {
        self.caused_signals = signals;
        self
    }

    /// Set the process step key for "step_started" auto-publish.
    pub fn with_process_step_started(mut self, step: impl Into<String>) -> Self {
        self.process_step_started = Some(step.into());
        self
    }

    /// Set the process step key for "step_completed" auto-publish.
    pub fn with_process_step_completed(mut self, step: impl Into<String>) -> Self {
        self.process_step_completed = Some(step.into());
        self
    }

    /// Check if this transition is an effect transition.
    pub fn is_effect(&self) -> bool {
        self.effect_handler_id.is_some()
    }

    /// Get an input port by name.
    pub fn input_port(&self, name: &str) -> Option<&Port> {
        self.input_ports.iter().find(|p| p.name == name)
    }

    /// Get an output port by name.
    pub fn output_port(&self, name: &str) -> Option<&Port> {
        self.output_ports.iter().find(|p| p.name == name)
    }

    /// Check if this transition has a guard script.
    pub fn has_guard(&self) -> bool {
        self.guard.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Port, PortCardinality};

    #[test]
    fn test_transition_new() {
        let transition = Transition::new("process", "#{output: input}");
        assert_eq!(transition.name, "process");
        assert_eq!(transition.script, "#{output: input}");
        assert!(transition.guard.is_none());
        assert!(transition.input_ports.is_empty());
        assert!(transition.output_ports.is_empty());
    }

    #[test]
    fn test_transition_with_ports() {
        let transition = Transition::new(
            "route",
            "if ok { #{success: data} } else { #{error: data} }",
        )
        .with_input_port(Port::new("request"))
        .with_output_port(Port::new("success"))
        .with_output_port(Port::new("error"));

        assert_eq!(transition.input_ports.len(), 1);
        assert_eq!(transition.output_ports.len(), 2);
        assert_eq!(transition.input_ports[0].name, "request");
        assert_eq!(transition.output_ports[0].name, "success");
        assert_eq!(transition.output_ports[1].name, "error");
    }

    #[test]
    fn test_transition_with_guard() {
        let transition =
            Transition::new("guarded", "#{out: input}").with_guard("input.status == \"ready\"");

        assert!(transition.has_guard());
        assert_eq!(
            transition.guard,
            Some("input.status == \"ready\"".to_string())
        );
    }

    #[test]
    fn test_transition_port_lookup() {
        let transition = Transition::new("lookup_test", "#{}")
            .with_input_port(Port::new("req").with_schema("Request"))
            .with_input_port(Port::batch("items").with_schema("Item"))
            .with_output_port(Port::new("result"));

        assert!(transition.input_port("req").is_some());
        assert!(transition.input_port("items").is_some());
        assert!(transition.input_port("nonexistent").is_none());
        assert!(transition.output_port("result").is_some());

        let items_port = transition.input_port("items").unwrap();
        assert_eq!(items_port.cardinality, PortCardinality::Batch);
    }

    #[test]
    fn test_transition_serialization() {
        let transition = Transition::new("serialize_test", "#{out: inp}")
            .with_input_port(Port::new("inp"))
            .with_output_port(Port::new("out"))
            .with_guard("inp.valid");

        let json = serde_json::to_string(&transition).unwrap();
        let deserialized: Transition = serde_json::from_str(&json).unwrap();

        assert_eq!(transition.name, deserialized.name);
        assert_eq!(transition.script, deserialized.script);
        assert_eq!(transition.guard, deserialized.guard);
        assert_eq!(transition.input_ports.len(), deserialized.input_ports.len());
        assert_eq!(
            transition.output_ports.len(),
            deserialized.output_ports.len()
        );
    }

    #[test]
    fn test_transition_json_format() {
        let transition = Transition::new(
            "booking",
            r#"
            if signal.status == "OK" {
                #{ success: #{ id: ctx.id, resource: signal.resource_id } }
            } else if ctx.retry_count < 3 {
                #{ retry: #{ id: ctx.id, retry_count: ctx.retry_count + 1 } }
            } else {
                #{ fatal: #{ error: "Max retries", id: ctx.id } }
            }
        "#,
        )
        .with_input_port(Port::new("ctx").with_schema("BookingContext"))
        .with_input_port(Port::new("signal").with_schema("AllocationSignal"))
        .with_output_port(Port::new("success"))
        .with_output_port(Port::new("retry"))
        .with_output_port(Port::new("fatal"))
        .with_guard("signal.id == ctx.id");

        let json = serde_json::to_string_pretty(&transition).unwrap();

        // Verify structure
        assert!(json.contains("\"input_ports\""));
        assert!(json.contains("\"output_ports\""));
        assert!(json.contains("\"guard\""));
        assert!(json.contains("\"script\""));
        assert!(json.contains("\"ctx\""));
        assert!(json.contains("\"signal\""));
    }

    #[test]
    fn test_is_effect_false_by_default() {
        let transition = Transition::new("process", "#{output: input}");
        assert!(!transition.is_effect());
        assert!(transition.effect_handler_id.is_none());
    }

    #[test]
    fn test_is_effect_with_handler() {
        let transition = Transition::new("call_api", "").with_effect_handler("http_handler");
        assert!(transition.is_effect());
        assert_eq!(
            transition.effect_handler_id.as_deref(),
            Some("http_handler")
        );
    }

    #[test]
    fn test_effect_handler_serialization_roundtrip() {
        let transition = Transition::new("call_api", "")
            .with_input_port(Port::new("req"))
            .with_output_port(Port::new("resp"))
            .with_effect_handler("nevergrad_optimizer");

        let json = serde_json::to_string(&transition).unwrap();
        assert!(json.contains("\"effect_handler_id\":\"nevergrad_optimizer\""));

        let deserialized: Transition = serde_json::from_str(&json).unwrap();
        assert!(deserialized.is_effect());
        assert_eq!(
            deserialized.effect_handler_id.as_deref(),
            Some("nevergrad_optimizer")
        );
    }

    #[test]
    fn test_effect_handler_not_serialized_when_none() {
        let transition = Transition::new("normal", "#{out: inp}");
        let json = serde_json::to_string(&transition).unwrap();
        assert!(!json.contains("effect_handler_id"));
    }
}
