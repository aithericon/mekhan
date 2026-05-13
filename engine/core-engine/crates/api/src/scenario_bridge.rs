//! Bridge between API DTOs and the application's ScenarioParser.
//!
//! This module provides conversion from API-facing DTOs to the application-layer
//! parsing types, enabling a clean separation between API concerns and domain logic.

use crate::dto::{
    ScenarioArc, ScenarioPlace, ScenarioPort, ScenarioToken, ScenarioTransition,
    TransitionLogic,
};
use petri_application::scenario_loader::{
    ScenarioArcInput, ScenarioGuardInput, ScenarioLogicInput, ScenarioParser, ScenarioPlaceInput,
    ScenarioPortInput, ScenarioSimulationInput, ScenarioTokenInput, ScenarioTransitionInput,
};

/// Convert API DTOs to parser input types.
pub struct ScenarioBridge;

impl ScenarioBridge {
    /// Convert a ScenarioPlace DTO to parser input.
    pub fn convert_place(place: &ScenarioPlace) -> ScenarioPlaceInput {
        ScenarioPlaceInput {
            id: place.id.clone(),
            name: place.name.clone(),
            place_type: place.place_type.clone(),
            capacity: place.capacity,
            group_id: place.group_id.clone(),
            initial_tokens: place
                .initial_tokens
                .iter()
                .map(Self::convert_token)
                .collect(),
            bridge_out: place.bridge_out.as_ref().map(|b| {
                (
                    b.target_net_id.clone(),
                    b.target_place_name.clone(),
                    b.reply_to.clone(),
                    b.label.clone(),
                )
            }),
            bridge_out_reply_channels: place
                .bridge_out
                .as_ref()
                .and_then(|b| b.reply_channels.clone()),
            bridge_reply: place.bridge_reply,
            bridge_reply_channel: place.bridge_reply_channel.clone(),
            token_schema: place.token_schema.clone(),
            bridge_in_source: place.bridge_in.as_ref().map(|b| {
                (b.source_net_id.clone(), b.source_place_name.clone())
            }),
        }
    }

    /// Convert a ScenarioToken DTO to parser input.
    pub fn convert_token(token: &ScenarioToken) -> ScenarioTokenInput {
        match token {
            ScenarioToken::Unit => ScenarioTokenInput::Unit,
            ScenarioToken::Integer(n) => ScenarioTokenInput::Integer(*n),
            ScenarioToken::Data(v) => ScenarioTokenInput::Data(v.clone()),
        }
    }

    /// Convert a ScenarioPort DTO to parser input.
    pub fn convert_port(port: &ScenarioPort) -> ScenarioPortInput {
        ScenarioPortInput {
            name: port.name.clone(),
            cardinality: port.cardinality.clone(),
            schema_ref: port.schema_ref.clone(),
        }
    }

    /// Convert a ScenarioArc DTO to parser input.
    pub fn convert_arc(arc: &ScenarioArc) -> ScenarioArcInput {
        ScenarioArcInput {
            place: arc.place.clone(),
            port: arc.port.clone(),
            weight: arc.weight,
            read: arc.read,
        }
    }

    /// Convert a ScenarioTransition DTO to parser input.
    pub fn convert_transition(transition: &ScenarioTransition) -> ScenarioTransitionInput {
        let logic = match &transition.logic {
            TransitionLogic::Rhai { source } => ScenarioLogicInput::Rhai {
                source: source.clone(),
            },
            TransitionLogic::Wasm { module, .. } => ScenarioLogicInput::Wasm {
                module: module.clone(),
            },
            TransitionLogic::Effect { handler_id, config } => ScenarioLogicInput::Effect {
                handler_id: handler_id.clone(),
                config: config.clone(),
            },
        };

        let guard = transition.guard.as_ref().map(|g| ScenarioGuardInput {
            rhai_source: g.as_rhai_source().map(|s| s.to_string()),
        });

        let simulation = transition
            .simulation
            .as_ref()
            .map(|s| ScenarioSimulationInput {
                duration_ms: s.duration_ms,
                variance_ms: s.variance_ms,
            });

        ScenarioTransitionInput {
            id: transition.id.clone(),
            name: transition.name.clone(),
            input_ports: transition
                .input_ports
                .iter()
                .map(Self::convert_port)
                .collect(),
            output_ports: transition
                .output_ports
                .iter()
                .map(Self::convert_port)
                .collect(),
            logic,
            effect_config: None, // From top-level field if added
            guard,
            simulation,
            group_id: transition.group_id.clone(),
            inputs: transition.inputs.iter().map(Self::convert_arc).collect(),
            outputs: transition.outputs.iter().map(Self::convert_arc).collect(),
            caused_signals: transition.caused_signals.clone(),
            process_step_started: transition.process_step_started.clone(),
            process_step_completed: transition.process_step_completed.clone(),
        }
    }

    /// Parse a scenario from DTOs.
    pub fn parse(
        places: &[ScenarioPlace],
        transitions: &[ScenarioTransition],
        definitions: std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<
        petri_application::scenario_loader::ParsedScenario,
        petri_application::scenario_loader::ScenarioLoadError,
    > {
        let place_inputs: Vec<_> = places.iter().map(Self::convert_place).collect();
        let transition_inputs: Vec<_> = transitions.iter().map(Self::convert_transition).collect();
        ScenarioParser::parse(place_inputs, transition_inputs, definitions)
    }
}
