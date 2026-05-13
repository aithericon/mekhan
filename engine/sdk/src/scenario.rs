//! Scenario output DTOs - the JSON AIR format consumed by the Engine.
//!
//! Supports polymorphic logic (Rhai now, Wasm later) with embedded JSON Schemas.
//! Groups are metadata for visualization (ignored by execution engine).

use petri_domain::effects::ServiceRequirement;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Complete scenario definition with embedded type schemas
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub places: Vec<ScenarioPlace>,
    pub transitions: Vec<ScenarioTransition>,
    /// Groups for visualization (hierarchical components)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<ScenarioGroup>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mock_adapters: Vec<MockAdapterConfig>,
    /// JSON Schema definitions for all token types used in this scenario
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub definitions: HashMap<String, serde_json::Value>,
    /// Infrastructure requirements declared by this scenario.
    /// The engine validates all required services are configured at load time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirements: Vec<ServiceRequirement>,
}

/// Group definition for visual components
///
/// Groups are "debug symbols" - metadata for visualization that the execution
/// engine ignores. They enable hierarchical views in the Lab UI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioGroup {
    pub id: String,
    pub name: String,
    /// Parent group ID for nested groups
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Optional metadata (e.g., {"image": "ffmpeg:latest"})
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Place definition with type information
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioPlace {
    pub id: String,
    pub name: String,
    #[serde(rename = "type", default = "default_place_type")]
    pub place_type: String,
    /// Group this place belongs to (for visualization)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initial_tokens: Vec<ScenarioToken>,
    /// JSON Schema reference for tokens in this place (e.g., "#/definitions/Task")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_schema: Option<String>,
    /// Bridge-out target (for bridge_out and bridge_out_reply places)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_out: Option<BridgeTargetDto>,
    /// If true, this place receives reply tokens from a bridge-out interaction
    #[serde(default, skip_serializing_if = "is_false")]
    pub bridge_reply: bool,
    /// Named reply channel for bridge_reply places
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_reply_channel: Option<String>,
    /// Bridge-in source annotation (which remote net sends tokens here)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_in: Option<BridgeSourceDto>,
}

/// Bridge target configuration for cross-net communication.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeTargetDto {
    pub target_net_id: String,
    pub target_place_name: String,
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
///
/// Declares which remote net's bridge_out sends tokens to this bridge_in.
/// This is metadata for visualization and does not affect execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeSourceDto {
    pub source_net_id: String,
    pub source_place_name: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Token value - supports unit, integer, and complex data
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScenarioToken {
    /// Null represents a unit token (classic Petri net marker)
    Unit,
    /// Integer token (fungible resource)
    Integer(i64),
    /// Complex data token (JSON object)
    Data(serde_json::Value),
}

/// Transition definition with polymorphic logic and type contracts
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioTransition {
    pub id: String,
    pub name: String,
    /// Group this transition belongs to (for visualization)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_ports: Vec<ScenarioPort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_ports: Vec<ScenarioPort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ScenarioArc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<ScenarioArc>,
    /// Guard condition (polymorphic - Rhai or Wasm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard: Option<TransitionGuard>,
    /// Priority expression (polymorphic - Rhai or Wasm)
    /// Higher values = higher priority when selecting among enabled transitions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<TransitionPriority>,
    /// Main logic (polymorphic - Rhai or Wasm)
    pub logic: TransitionLogic,
    /// Optional static configuration for the effect handler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect_config: Option<serde_json::Value>,
    /// List of signal names that this transition is expected to cause/wait for.
    /// This is metadata for visualization and validation (causation arcs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caused_signals: Vec<String>,
    /// Combined input schema (for Wasm validation in future)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// Output schema (for Wasm validation in future)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// Process step key: publish "step_started" after this transition fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_step_started: Option<String>,
    /// Process step key: publish "step_completed" after this transition fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_step_completed: Option<String>,
}

/// Polymorphic logic - supports Rhai (now), Wasm (future), and Effect (side effects)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransitionLogic {
    /// Rhai script (editable in Lab, returns #{port: value, ...})
    Rhai { source: String },
    /// Wasm module (future - compiled Rust, near-native performance)
    Wasm {
        /// Base64-encoded wasm bytecode or path to .wasm file
        module: String,
        /// Export function name to call
        function: String,
    },
    /// Effect transition — side effect executed by a registered handler.
    /// The handler produces output tokens and a result payload stored
    /// in the event log for deterministic replay.
    Effect {
        /// ID of the registered EffectHandler.
        handler_id: String,
        /// Optional static configuration for the effect handler.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<serde_json::Value>,
    },
}

/// Guard condition - also polymorphic
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransitionGuard {
    /// Rhai script returning bool
    Rhai { source: String },
    /// Wasm function returning bool (future)
    Wasm { module: String, function: String },
}

/// Priority expression - evaluated to determine firing order among enabled transitions.
/// Higher values = higher priority.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransitionPriority {
    /// Rhai expression returning a numeric value
    Rhai { source: String },
    /// Wasm function returning numeric (future)
    Wasm { module: String, function: String },
}

impl TransitionPriority {
    /// Create Rhai priority expression
    pub fn rhai(source: impl Into<String>) -> Self {
        Self::Rhai {
            source: source.into(),
        }
    }

    /// Create Wasm priority (for future use)
    pub fn wasm(module: impl Into<String>, function: impl Into<String>) -> Self {
        Self::Wasm {
            module: module.into(),
            function: function.into(),
        }
    }
}

/// Port definition with type reference
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioPort {
    pub name: String,
    /// JSON Schema reference for this port's token type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_ref: Option<String>,
    #[serde(default = "default_cardinality")]
    pub cardinality: String,
}

/// Arc connecting place to port
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScenarioArc {
    pub place: String,
    pub port: String,
    #[serde(default = "default_weight")]
    pub weight: usize,
    /// If true, this is a read arc: token consumed for evaluation, auto-produced back.
    #[serde(default, skip_serializing_if = "is_false_arc")]
    pub read: bool,
}

fn is_false_arc(b: &bool) -> bool {
    !*b
}

/// Mock adapter configuration for simulating external services
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MockAdapterConfig {
    pub name: String,
    pub trigger_place_id: String,
    pub latency_ms: u64,
    pub logic: AdapterLogic,
    /// If true, verify the triggering token still exists in the place before executing.
    /// This enables timeout patterns where the adapter only fires if the token hasn't
    /// been consumed by another transition (e.g., patient still waiting for doctor).
    #[serde(default)]
    pub check_token_exists: bool,
}

/// Polymorphic adapter logic - supports Rhai (now), Wasm and JavaScript (future)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdapterLogic {
    /// Rhai script - evaluated by the engine
    /// Must return: #{ target_place: "place_id", data: { ... } }
    Rhai { source: String },
    /// JavaScript - for frontend-only simulation (deprecated)
    #[serde(rename = "js")]
    JavaScript { source: String },
    /// Wasm module (future)
    Wasm { module: String, function: String },
}

impl AdapterLogic {
    /// Create Rhai adapter logic
    pub fn rhai(source: impl Into<String>) -> Self {
        Self::Rhai {
            source: source.into(),
        }
    }

    /// Create JavaScript adapter logic (for legacy frontend compatibility)
    pub fn js(source: impl Into<String>) -> Self {
        Self::JavaScript {
            source: source.into(),
        }
    }
}

// Default value functions
fn default_place_type() -> String {
    "state".into()
}

fn default_cardinality() -> String {
    "single".into()
}

fn default_weight() -> usize {
    1
}

impl TransitionLogic {
    /// Create Rhai logic
    pub fn rhai(source: impl Into<String>) -> Self {
        Self::Rhai {
            source: source.into(),
        }
    }

    /// Create Wasm logic (for future use)
    pub fn wasm(module: impl Into<String>, function: impl Into<String>) -> Self {
        Self::Wasm {
            module: module.into(),
            function: function.into(),
        }
    }

    /// Create Effect logic — side effect executed by a registered handler.
    pub fn effect(handler_id: impl Into<String>) -> Self {
        Self::Effect {
            handler_id: handler_id.into(),
            config: None,
        }
    }

    /// Create Effect logic with static configuration.
    pub fn effect_with_config(handler_id: impl Into<String>, config: serde_json::Value) -> Self {
        Self::Effect {
            handler_id: handler_id.into(),
            config: Some(config),
        }
    }
}

impl TransitionGuard {
    /// Create Rhai guard
    pub fn rhai(source: impl Into<String>) -> Self {
        Self::Rhai {
            source: source.into(),
        }
    }

    /// Create Wasm guard (for future use)
    pub fn wasm(module: impl Into<String>, function: impl Into<String>) -> Self {
        Self::Wasm {
            module: module.into(),
            function: function.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scenario_definition_new() {
        let scenario = ScenarioDefinition::new("Test Scenario");
        assert_eq!(scenario.name, "Test Scenario");
        assert!(scenario.description.is_none());
        assert!(scenario.places.is_empty());
        assert!(scenario.transitions.is_empty());
    }

    #[test]
    fn test_scenario_token_unit_serialization() {
        let token = ScenarioToken::Unit;
        let json = serde_json::to_string(&token).unwrap();
        assert_eq!(json, "null");
    }

    #[test]
    fn test_scenario_token_integer_serialization() {
        let token = ScenarioToken::Integer(42);
        let json = serde_json::to_string(&token).unwrap();
        assert_eq!(json, "42");
    }

    #[test]
    fn test_scenario_token_data_serialization() {
        let token = ScenarioToken::Data(serde_json::json!({"id": "task_1"}));
        let json = serde_json::to_string(&token).unwrap();
        assert!(json.contains("task_1"));
    }

    #[test]
    fn test_transition_logic_rhai() {
        let logic = TransitionLogic::rhai("#{ result: input }");
        if let TransitionLogic::Rhai { source } = &logic {
            assert_eq!(source, "#{ result: input }");
        } else {
            panic!("Expected Rhai logic");
        }
    }

    #[test]
    fn test_transition_logic_wasm() {
        let logic = TransitionLogic::wasm("module.wasm", "transform");
        if let TransitionLogic::Wasm { module, function } = &logic {
            assert_eq!(module, "module.wasm");
            assert_eq!(function, "transform");
        } else {
            panic!("Expected Wasm logic");
        }
    }

    #[test]
    fn test_transition_guard_rhai() {
        let guard = TransitionGuard::rhai("input > 0");
        if let TransitionGuard::Rhai { source } = &guard {
            assert_eq!(source, "input > 0");
        } else {
            panic!("Expected Rhai guard");
        }
    }

    #[test]
    fn test_transition_priority_rhai() {
        let priority = TransitionPriority::rhai("task.urgency");
        if let TransitionPriority::Rhai { source } = &priority {
            assert_eq!(source, "task.urgency");
        } else {
            panic!("Expected Rhai priority");
        }
    }

    #[test]
    fn test_adapter_logic_rhai() {
        let logic = AdapterLogic::rhai("#{ target_place: \"result\" }");
        if let AdapterLogic::Rhai { source } = &logic {
            assert!(source.contains("target_place"));
        } else {
            panic!("Expected Rhai adapter logic");
        }
    }

    #[test]
    fn test_scenario_to_json() {
        let scenario = ScenarioDefinition::new("JSON Test");
        let json = scenario.to_json().unwrap();
        assert!(json.contains("JSON Test"));
        assert!(json.contains("\"name\""));
    }

    #[test]
    fn test_scenario_to_json_compact() {
        let scenario = ScenarioDefinition::new("Compact Test");
        let json = scenario.to_json_compact().unwrap();
        assert!(json.contains("Compact Test"));
        // Compact JSON shouldn't have leading newlines
        assert!(!json.starts_with('\n'));
    }

    #[test]
    fn test_scenario_roundtrip() {
        let mut scenario = ScenarioDefinition::new("Roundtrip Test");
        scenario.description = Some("Test description".into());
        scenario.places.push(ScenarioPlace {
            id: "p1".into(),
            name: "Place 1".into(),
            place_type: "state".into(),
            group_id: None,
            capacity: Some(10),
            initial_tokens: vec![ScenarioToken::Unit],
            token_schema: Some("#/definitions/Task".into()),
            bridge_out: None,
            bridge_reply: false,
            bridge_reply_channel: None,
            bridge_in: None,
        });

        let json = scenario.to_json().unwrap();
        let deserialized: ScenarioDefinition = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.name, scenario.name);
        assert_eq!(deserialized.description, scenario.description);
        assert_eq!(deserialized.places.len(), 1);
        assert_eq!(deserialized.places[0].id, "p1");
        assert_eq!(deserialized.places[0].capacity, Some(10));
    }

    #[test]
    fn test_logic_serialization_tagged() {
        let logic = TransitionLogic::rhai("test");
        let json = serde_json::to_string(&logic).unwrap();
        // Should use snake_case tag
        assert!(json.contains("\"type\":\"rhai\""));
        assert!(json.contains("\"source\":\"test\""));
    }

    #[test]
    fn test_scenario_arc_defaults() {
        let json = r#"{"place": "p1", "port": "input"}"#;
        let arc: ScenarioArc = serde_json::from_str(json).unwrap();
        assert_eq!(arc.place, "p1");
        assert_eq!(arc.port, "input");
        assert_eq!(arc.weight, 1); // Default weight
    }

    #[test]
    fn test_scenario_port_defaults() {
        let json = r#"{"name": "task"}"#;
        let port: ScenarioPort = serde_json::from_str(json).unwrap();
        assert_eq!(port.name, "task");
        assert_eq!(port.cardinality, "single"); // Default cardinality
    }
}

impl ScenarioDefinition {
    /// Create a new empty scenario
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            places: vec![],
            transitions: vec![],
            groups: vec![],
            mock_adapters: vec![],
            definitions: HashMap::new(),
            requirements: vec![],
        }
    }

    /// Serialize to pretty JSON
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Serialize to compact JSON
    pub fn to_json_compact(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}
