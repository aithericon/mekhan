//! Scenario loading and parsing utilities.
//!
//! This module extracts the scenario parsing logic from the API handler,
//! providing a clean separation between parsing, initialization, and adapter registration.

use std::collections::HashMap;

use serde_json::Value as JsonValue;

use petri_domain::{
    arc::Arc as PetriArc,
    net::PetriNet,
    place::Place,
    port::{Port, PortCardinality},
    token::TokenColor,
    transition::{SimulationConfig, Transition},
    PlaceId, TransitionId,
};
use thiserror::Error;

/// Errors that can occur during scenario loading.
#[derive(Error, Debug)]
pub enum ScenarioLoadError {
    #[error("Unknown place type: {0}")]
    InvalidPlaceType(String),

    #[error("Unsupported logic type: {0}")]
    InvalidLogicType(String),

    #[error("Unknown place reference: {0}")]
    UnknownPlace(String),

    #[error("Unknown transition reference: {0}")]
    UnknownTransition(String),
}

/// Result of parsing a scenario - contains all data needed to initialize a service.
#[derive(Clone)]
pub struct ParsedScenario {
    /// The constructed Petri net
    pub net: PetriNet,
    /// Mapping from scenario place IDs to internal PlaceIds
    pub place_ids: HashMap<String, PlaceId>,
    /// Mapping from scenario transition IDs to internal TransitionIds
    pub transition_ids: HashMap<String, TransitionId>,
    /// Initial tokens to create
    pub initial_tokens: Vec<(PlaceId, TokenColor)>,
    /// JSON Schema definitions for token type validation
    pub definitions: HashMap<String, JsonValue>,
}

/// Input for a scenario place.
#[derive(Default)]
pub struct ScenarioPlaceInput {
    pub id: String,
    pub name: String,
    pub place_type: String,
    pub capacity: Option<usize>,
    pub group_id: Option<String>,
    pub initial_tokens: Vec<ScenarioTokenInput>,
    /// If set, tokens produced here are forwarded to a remote net via bridge
    /// (target_net_id, target_place_name, optional reply_to place name, optional label)
    pub bridge_out: Option<(String, String, Option<String>, Option<String>)>,
    /// Named reply channels for bridge_out places: channel_name → local_place_name.
    pub bridge_out_reply_channels: Option<std::collections::HashMap<String, String>>,
    /// If true, tokens produced here are routed back via consumed reply_routing
    pub bridge_reply: bool,
    /// If set, this bridge_reply place reads from a named channel instead of default reply_to
    pub bridge_reply_channel: Option<String>,
    /// JSON Schema reference for tokens at this place (e.g., "#/definitions/Task")
    pub token_schema: Option<String>,
    /// Bridge-in source annotation: (source_net_id, source_place_name)
    pub bridge_in_source: Option<(String, String)>,
}

/// Input for a scenario token.
pub enum ScenarioTokenInput {
    Unit,
    Integer(i64),
    Data(serde_json::Value),
}

/// Input for a scenario port.
pub struct ScenarioPortInput {
    pub name: String,
    pub cardinality: String,
    pub schema_ref: Option<String>,
}

/// Input for transition logic.
pub enum ScenarioLogicInput {
    Rhai {
        source: String,
    },
    Wasm {
        module: String,
    },
    /// Effect transition — side effect executed by a registered handler.
    Effect {
        handler_id: String,
        config: Option<serde_json::Value>,
    },
}

/// Input for a guard.
pub struct ScenarioGuardInput {
    pub rhai_source: Option<String>,
}

/// Input for simulation config.
pub struct ScenarioSimulationInput {
    pub duration_ms: u64,
    pub variance_ms: Option<u64>,
}

/// Input for an arc connection.
pub struct ScenarioArcInput {
    pub place: String,
    pub port: String,
    pub weight: usize,
    /// If true, this is a read arc (token consumed for evaluation, auto-produced back).
    pub read: bool,
    /// Gather barrier: reference to a coordinator field supplying the count `K`
    /// (e.g. `"expected.k"`). `None` = no count-gate (today's behavior).
    pub count_from: Option<String>,
    /// Gather barrier: optional correlate field matched against result tokens.
    pub correlate_on: Option<String>,
    /// Output arc only: emit the produced token routing-less (don't inherit the
    /// firing's consumed reply-routing). See domain `Arc::reset_reply_routing`.
    pub reset_reply_routing: bool,
}

/// Input for a scenario transition.
pub struct ScenarioTransitionInput {
    pub id: String,
    pub name: String,
    pub input_ports: Vec<ScenarioPortInput>,
    pub output_ports: Vec<ScenarioPortInput>,
    pub logic: ScenarioLogicInput,
    pub effect_config: Option<serde_json::Value>,
    pub guard: Option<ScenarioGuardInput>,
    /// Optional Rhai priority expression — `Transition::with_priority` source.
    /// `None` defers to alphabetical-id tiebreak (see `select_next_transition`).
    pub priority: Option<String>,
    /// Finalizer flag — fires only during the post-failure finalizer drain.
    /// See `petri_domain::Transition::finalizer`.
    pub finalizer: bool,
    pub simulation: Option<ScenarioSimulationInput>,
    pub group_id: Option<String>,
    pub inputs: Vec<ScenarioArcInput>,
    pub outputs: Vec<ScenarioArcInput>,
    pub caused_signals: Vec<String>,
    /// Process step key: publish "step_started" after this transition fires.
    pub process_step_started: Option<String>,
    /// Process step key: publish "step_completed" after this transition fires.
    pub process_step_completed: Option<String>,
}

/// Parses a scenario definition into domain objects.
pub struct ScenarioParser;

impl ScenarioParser {
    /// Parse places and build the place ID mapping.
    pub fn parse_places(
        places: &[ScenarioPlaceInput],
    ) -> Result<(Vec<Place>, HashMap<String, PlaceId>), ScenarioLoadError> {
        let mut domain_places = Vec::with_capacity(places.len());
        let mut place_ids = HashMap::new();

        for sp in places {
            let mut place = if sp.bridge_reply {
                if let Some(ref ch) = sp.bridge_reply_channel {
                    Place::bridge_reply_channel(&sp.name, ch)
                } else {
                    Place::bridge_reply(&sp.name)
                }
            } else if let Some(ref bridge) = sp.bridge_out {
                if bridge.3.is_some() {
                    // Has a label — use labeled constructor
                    Place::bridge_out_labeled(
                        &sp.name,
                        &bridge.0,
                        &bridge.1,
                        bridge.2.clone(),
                        bridge.3.as_deref().unwrap(),
                    )
                } else if let Some(ref channels) = sp.bridge_out_reply_channels {
                    Place::bridge_out_reply_channels(
                        &sp.name,
                        &bridge.0,
                        &bridge.1,
                        channels.clone(),
                    )
                } else if let Some(ref reply_to) = bridge.2 {
                    Place::bridge_out_reply(&sp.name, &bridge.0, &bridge.1, reply_to)
                } else {
                    Place::bridge_out(&sp.name, &bridge.0, &bridge.1)
                }
            } else {
                match sp.place_type.as_str() {
                    "signal" => Place::signal(&sp.name),
                    "bridge_in" => {
                        if let Some((ref net_id, ref place_name)) = sp.bridge_in_source {
                            Place::bridge_in_from(&sp.name, net_id, place_name)
                        } else {
                            Place::bridge_in(&sp.name)
                        }
                    }
                    "terminal" => Place::terminal(&sp.name),
                    "sink" => Place::sink(&sp.name),
                    "state" | "resource" | "internal" | "" => Place::internal(&sp.name),
                    other => return Err(ScenarioLoadError::InvalidPlaceType(other.to_string())),
                }
            };

            if let Some(cap) = sp.capacity {
                place = place.with_capacity(cap);
            }
            if let Some(ref gid) = sp.group_id {
                place = place.with_group_id(gid);
            }
            if let Some(ref schema) = sp.token_schema {
                place = place.with_token_schema(schema);
            }

            // Override ID with the scenario's string ID
            place = place.with_id(PlaceId(sp.id.clone()));
            place_ids.insert(sp.id.clone(), place.id.clone());
            domain_places.push(place);
        }

        Ok((domain_places, place_ids))
    }

    /// Parse transitions and build the transition ID mapping.
    pub fn parse_transitions(
        transitions: &[ScenarioTransitionInput],
    ) -> Result<(Vec<Transition>, HashMap<String, TransitionId>), ScenarioLoadError> {
        let mut domain_transitions = Vec::with_capacity(transitions.len());
        let mut transition_ids = HashMap::new();

        for st in transitions {
            let input_ports = Self::parse_ports(&st.input_ports);
            let output_ports = Self::parse_ports(&st.output_ports);

            let (script, effect_handler_id, effect_config) = match &st.logic {
                ScenarioLogicInput::Rhai { source } => (source.as_str(), None, None),
                ScenarioLogicInput::Wasm { .. } => {
                    return Err(ScenarioLoadError::InvalidLogicType("wasm".to_string()));
                }
                ScenarioLogicInput::Effect { handler_id, config } => {
                    // Effect transitions don't need a Rhai script
                    ("", Some(handler_id.clone()), config.clone())
                }
            };

            let mut transition = Transition::new(&st.name, script)
                .with_input_ports(input_ports)
                .with_output_ports(output_ports);

            if let Some(handler_id) = effect_handler_id {
                transition = transition.with_effect_handler(handler_id);
            }

            if let Some(config) = effect_config {
                transition = transition.with_effect_config(config);
            }

            if let Some(config) = &st.effect_config {
                transition = transition.with_effect_config(config.clone());
            }

            if let Some(guard) = &st.guard {
                if let Some(ref source) = guard.rhai_source {
                    transition = transition.with_guard(source);
                }
            }

            if let Some(ref priority_src) = st.priority {
                transition = transition.with_priority(priority_src.clone());
            }

            if st.finalizer {
                transition = transition.with_finalizer(true);
            }

            if let Some(sim) = &st.simulation {
                let mut config = SimulationConfig::new(sim.duration_ms);
                if let Some(variance) = sim.variance_ms {
                    config = config.with_variance(variance);
                }
                transition = transition.with_simulation(config);
            }

            if let Some(ref gid) = st.group_id {
                transition = transition.with_group_id(gid);
            }

            if !st.caused_signals.is_empty() {
                transition = transition.with_caused_signals(st.caused_signals.clone());
            }

            if let Some(ref step) = st.process_step_started {
                transition = transition.with_process_step_started(step);
            }
            if let Some(ref step) = st.process_step_completed {
                transition = transition.with_process_step_completed(step);
            }

            // Override ID with the scenario's string ID
            transition = transition.with_id(TransitionId(st.id.clone()));
            transition_ids.insert(st.id.clone(), transition.id.clone());
            domain_transitions.push(transition);
        }

        Ok((domain_transitions, transition_ids))
    }

    /// Parse port definitions.
    fn parse_ports(ports: &[ScenarioPortInput]) -> Vec<Port> {
        ports
            .iter()
            .map(|sp| {
                let cardinality = if sp.cardinality == "batch" {
                    PortCardinality::Batch
                } else {
                    PortCardinality::Single
                };
                let mut port = Port::new(&sp.name).with_cardinality(cardinality);
                if let Some(schema) = &sp.schema_ref {
                    port = port.with_schema(schema);
                }
                port
            })
            .collect()
    }

    /// Parse arcs from transition definitions.
    pub fn parse_arcs(
        transitions: &[ScenarioTransitionInput],
        place_ids: &HashMap<String, PlaceId>,
        transition_ids: &HashMap<String, TransitionId>,
    ) -> Result<Vec<PetriArc>, ScenarioLoadError> {
        let mut arcs = Vec::new();

        for st in transitions {
            let tid = transition_ids
                .get(&st.id)
                .ok_or_else(|| ScenarioLoadError::UnknownTransition(st.id.clone()))?
                .clone();

            for input_arc in &st.inputs {
                let pid = place_ids
                    .get(&input_arc.place)
                    .ok_or_else(|| ScenarioLoadError::UnknownPlace(input_arc.place.clone()))?;
                let mut arc = PetriArc::input(pid.clone(), tid.clone(), &input_arc.port)
                    .with_weight(input_arc.weight)
                    .with_read(input_arc.read);
                if let Some(count_from) = &input_arc.count_from {
                    arc = arc.with_count_from(count_from.clone());
                }
                if let Some(correlate_on) = &input_arc.correlate_on {
                    arc = arc.with_correlate_on(correlate_on.clone());
                }
                arcs.push(arc);
            }

            for output_arc in &st.outputs {
                let pid = place_ids
                    .get(&output_arc.place)
                    .ok_or_else(|| ScenarioLoadError::UnknownPlace(output_arc.place.clone()))?;
                let arc = PetriArc::output(tid.clone(), &output_arc.port, pid.clone())
                    .with_weight(output_arc.weight)
                    .with_reset_reply_routing(output_arc.reset_reply_routing);
                arcs.push(arc);
            }
        }

        Ok(arcs)
    }

    /// Parse initial tokens from places.
    pub fn parse_initial_tokens(
        places: &[ScenarioPlaceInput],
        place_ids: &HashMap<String, PlaceId>,
    ) -> Vec<(PlaceId, TokenColor)> {
        let mut tokens: Vec<(PlaceId, TokenColor)> = Vec::new();

        for sp in places {
            if let Some(pid) = place_ids.get(&sp.id) {
                for token in &sp.initial_tokens {
                    let color: TokenColor = match token {
                        ScenarioTokenInput::Unit => TokenColor::Unit,
                        ScenarioTokenInput::Integer(n) => TokenColor::Integer(*n),
                        ScenarioTokenInput::Data(v) => TokenColor::Data(v.clone()),
                    };
                    tokens.push((pid.clone(), color));
                }
            }
        }

        tokens
    }

    /// Parse a complete scenario into domain objects.
    pub fn parse(
        places: Vec<ScenarioPlaceInput>,
        transitions: Vec<ScenarioTransitionInput>,
        definitions: HashMap<String, JsonValue>,
    ) -> Result<ParsedScenario, ScenarioLoadError> {
        // Parse places
        let (domain_places, place_ids) = Self::parse_places(&places)?;

        // Parse transitions
        let (mut domain_transitions, transition_ids) = Self::parse_transitions(&transitions)?;

        // Validate caused_signals: filter to only known place IDs
        for transition in &mut domain_transitions {
            let resolved: Vec<String> = transition
                .caused_signals
                .iter()
                .filter(|scenario_id| place_ids.contains_key(scenario_id.as_str()))
                .cloned()
                .collect();
            transition.caused_signals = resolved;
        }

        // Parse arcs
        let arcs = Self::parse_arcs(&transitions, &place_ids, &transition_ids)?;

        // Parse initial tokens
        let initial_tokens = Self::parse_initial_tokens(&places, &place_ids);

        // Build the net
        let mut net = PetriNet::new();
        for place in domain_places {
            net.add_place(place);
        }
        for transition in domain_transitions {
            net.add_transition(transition);
        }
        for arc in arcs {
            net.add_arc(arc);
        }

        Ok(ParsedScenario {
            net,
            place_ids,
            transition_ids,
            initial_tokens,
            definitions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_scenario() {
        let result = ScenarioParser::parse(vec![], vec![], HashMap::new());
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert!(parsed.place_ids.is_empty());
        assert!(parsed.transition_ids.is_empty());
        assert!(parsed.initial_tokens.is_empty());
    }

    #[test]
    fn test_parse_place_types() {
        let places = vec![
            ScenarioPlaceInput {
                id: "p1".to_string(),
                name: "state_place".to_string(),
                place_type: "state".to_string(),
                capacity: Some(10),
                ..Default::default()
            },
            ScenarioPlaceInput {
                id: "p2".to_string(),
                name: "signal_place".to_string(),
                place_type: "signal".to_string(),
                ..Default::default()
            },
            ScenarioPlaceInput {
                id: "p3".to_string(),
                name: "resource_compat".to_string(),
                place_type: "resource".to_string(), // backward compat → maps to State
                ..Default::default()
            },
        ];

        let result = ScenarioParser::parse(places, vec![], HashMap::new());
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.place_ids.len(), 3);
    }

    #[test]
    fn test_invalid_place_type() {
        let places = vec![ScenarioPlaceInput {
            id: "p1".to_string(),
            name: "bad_place".to_string(),
            place_type: "unknown_type".to_string(),
            ..Default::default()
        }];

        let result = ScenarioParser::parse(places, vec![], HashMap::new());
        assert!(matches!(
            result,
            Err(ScenarioLoadError::InvalidPlaceType(_))
        ));
    }

    #[test]
    fn test_parse_initial_tokens() {
        let places = vec![ScenarioPlaceInput {
            id: "p1".to_string(),
            name: "test_place".to_string(),
            place_type: "state".to_string(),
            initial_tokens: vec![
                ScenarioTokenInput::Unit,
                ScenarioTokenInput::Integer(42),
                ScenarioTokenInput::Data(serde_json::json!({"key": "value"})),
            ],
            ..Default::default()
        }];

        let result = ScenarioParser::parse(places, vec![], HashMap::new());
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.initial_tokens.len(), 3);
    }

    #[test]
    fn test_parse_place_bridge_primitives() {
        let places = vec![ScenarioPlaceInput {
            id: "outbox".to_string(),
            name: "Outbox".to_string(),
            place_type: "state".to_string(),
            bridge_out: Some(("remote-net".to_string(), "inbox".to_string(), None, None)),
            ..Default::default()
        }];

        let result = ScenarioParser::parse(places, vec![], HashMap::new());
        assert!(result.is_ok());
        let parsed = result.unwrap();

        let net = parsed.net;
        let outbox_id = parsed.place_ids.get("outbox").unwrap();
        let outbox_place = net.get_place(outbox_id).unwrap();
        assert!(outbox_place.is_bridge_out());
    }
}
