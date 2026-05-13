//! Conversion from SDK ScenarioDefinition to TestScenario.
//!
//! This enables using the SDK's fluent API to build test scenarios,
//! ensuring the SDK actually works end-to-end.

use std::collections::HashMap;

use aithericon_sdk::{ScenarioDefinition, ScenarioToken};
use petri_domain::{
    Arc as PetriArc, PetriNet, Place, PlaceId, Port, PortCardinality, Token, TokenColor,
    Transition, TransitionId,
};

use super::TestScenario;

impl TestScenario {
    /// Create a TestScenario from an SDK ScenarioDefinition.
    ///
    /// This converts the SDK's output format into the domain types used by the
    /// test harness, enabling end-to-end testing of SDK-defined workflows.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aithericon_sdk::prelude::*;
    /// use petri_test_harness::fixtures::TestScenario;
    ///
    /// let mut ctx = Context::new("test");
    /// let a = ctx.state::<UnitToken>("a", "A");
    /// let b = ctx.state::<UnitToken>("b", "B");
    /// ctx.transition("pass", "Pass")
    ///     .auto_input("inp", &a)
    ///     .auto_output("out", &b)
    ///     .logic("#{ out: inp }");
    /// ctx.seed(&a, vec![UnitToken]);
    ///
    /// let scenario = TestScenario::from_sdk(ctx.build());
    /// ```
    pub fn from_sdk(definition: ScenarioDefinition) -> Self {
        let mut net = PetriNet::new();
        let mut place_ids: HashMap<String, PlaceId> = HashMap::new();
        let mut transition_ids: HashMap<String, TransitionId> = HashMap::new();
        let mut initial_tokens: Vec<(PlaceId, Token)> = Vec::new();

        // Convert places
        for sp in &definition.places {
            let mut place = if sp.bridge_reply {
                if let Some(ref ch) = sp.bridge_reply_channel {
                    Place::bridge_reply_channel(&sp.name, ch)
                } else {
                    Place::bridge_reply(&sp.name)
                }
            } else if let Some(ref bridge) = sp.bridge_out {
                if let Some(ref channels) = bridge.reply_channels {
                    Place::bridge_out_reply_channels(
                        &sp.name,
                        &bridge.target_net_id,
                        &bridge.target_place_name,
                        channels.clone(),
                    )
                } else if let Some(ref reply_to) = bridge.reply_to {
                    Place::bridge_out_reply(
                        &sp.name,
                        &bridge.target_net_id,
                        &bridge.target_place_name,
                        reply_to,
                    )
                } else {
                    Place::bridge_out(&sp.name, &bridge.target_net_id, &bridge.target_place_name)
                }
            } else {
                match sp.place_type.as_str() {
                    "signal" => Place::signal(&sp.name),
                    "bridge_in" => {
                        if let Some(ref source) = sp.bridge_in {
                            Place::bridge_in_from(&sp.name, &source.source_net_id, &source.source_place_name)
                        } else {
                            Place::bridge_in(&sp.name)
                        }
                    }
                    _ => Place::internal(&sp.name),
                }
            };
            if let Some(cap) = sp.capacity {
                place = place.with_capacity(cap);
            }
            if let Some(ref gid) = sp.group_id {
                place = place.with_group_id(gid);
            }

            // Use the SDK ID as the PlaceId (NATS-safe, no spaces) while keeping
            // the display name. This ensures signal routes and resolve_place_id()
            // work with SDK IDs like "sig_accepted" instead of "Accepted Signals".
            place = place.with_id(PlaceId::named(&sp.id));

            place_ids.insert(sp.id.clone(), place.id.clone());

            // Collect initial tokens
            for token in &sp.initial_tokens {
                let color = match token {
                    ScenarioToken::Unit => TokenColor::Unit,
                    ScenarioToken::Integer(n) => TokenColor::Integer(*n),
                    ScenarioToken::Data(v) => TokenColor::Data(v.clone()),
                };
                initial_tokens.push((place.id.clone(), Token::new(color)));
            }

            net.add_place(place);
        }

        // Convert transitions
        for st in &definition.transitions {
            // Build input ports
            let input_ports: Vec<Port> = st
                .input_ports
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
                .collect();

            // Build output ports
            let output_ports: Vec<Port> = st
                .output_ports
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
                .collect();

            // Extract script and optional effect handler from polymorphic logic
            let (script, effect_handler_id) = match &st.logic {
                aithericon_sdk::TransitionLogic::Rhai { source } => (source.as_str(), None),
                aithericon_sdk::TransitionLogic::Wasm { .. } => {
                    panic!("Wasm logic not supported in tests");
                }
                aithericon_sdk::TransitionLogic::Effect { handler_id, .. } => {
                    ("", Some(handler_id.clone()))
                }
            };

            // Create transition
            let mut transition = Transition::new(&st.name, script)
                .with_input_ports(input_ports)
                .with_output_ports(output_ports);

            if let Some(handler_id) = effect_handler_id {
                transition = transition.with_effect_handler(handler_id);
            }

            // Add guard if present
            if let Some(guard) = &st.guard {
                match guard {
                    aithericon_sdk::TransitionGuard::Rhai { source } => {
                        transition = transition.with_guard(source);
                    }
                    aithericon_sdk::TransitionGuard::Wasm { .. } => {
                        panic!("Wasm guard not supported in tests");
                    }
                }
            }

            // Add priority if present
            if let Some(priority) = &st.priority {
                match priority {
                    aithericon_sdk::scenario::TransitionPriority::Rhai { source } => {
                        transition = transition.with_priority(source);
                    }
                    aithericon_sdk::scenario::TransitionPriority::Wasm { .. } => {
                        panic!("Wasm priority not supported in tests");
                    }
                }
            }

            if let Some(ref gid) = st.group_id {
                transition = transition.with_group_id(gid);
            }

            if !st.caused_signals.is_empty() {
                // Resolve scenario string IDs → PlaceId UUID strings
                let resolved: Vec<String> = st
                    .caused_signals
                    .iter()
                    .filter_map(|sid| place_ids.get(sid).map(|pid| pid.to_string()))
                    .collect();
                transition = transition.with_caused_signals(resolved);
            }

            transition_ids.insert(st.id.clone(), transition.id.clone());
            net.add_transition(transition);
        }

        // Create arcs
        for st in &definition.transitions {
            let tid = transition_ids
                .get(&st.id)
                .expect("transition must exist")
                .clone();

            // Input arcs
            for input_arc in &st.inputs {
                if let Some(pid) = place_ids.get(&input_arc.place) {
                    let arc = PetriArc::input(pid.clone(), tid.clone(), &input_arc.port)
                        .with_weight(input_arc.weight);
                    net.add_arc(arc);
                }
            }

            // Output arcs
            for output_arc in &st.outputs {
                if let Some(pid) = place_ids.get(&output_arc.place) {
                    let arc = PetriArc::output(tid.clone(), &output_arc.port, pid.clone())
                        .with_weight(output_arc.weight);
                    net.add_arc(arc);
                }
            }
        }

        // Build name → ID maps for test access
        // Use the scenario ID (e.g., "a") as the key, and also the display name
        let mut places_map = HashMap::new();
        for sp in &definition.places {
            if let Some(pid) = place_ids.get(&sp.id) {
                places_map.insert(sp.id.clone(), pid.clone());
                // Also map by display name for convenience
                places_map.insert(sp.name.clone(), pid.clone());
            }
        }

        let mut transitions_map = HashMap::new();
        for st in &definition.transitions {
            if let Some(tid) = transition_ids.get(&st.id) {
                transitions_map.insert(st.id.clone(), tid.clone());
                transitions_map.insert(st.name.clone(), tid.clone());
            }
        }

        TestScenario {
            net,
            initial_tokens,
            places: places_map,
            transitions: transitions_map,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_sdk::{Context, UnitToken};

    #[test]
    fn test_simple_conversion() {
        let mut ctx = Context::new("test");
        let a = ctx.state::<UnitToken>("a", "A");
        let b = ctx.state::<UnitToken>("b", "B");

        ctx.transition("pass", "Pass")
            .auto_input("inp", &a)
            .auto_output("out", &b)
            .logic("#{ out: inp }");

        ctx.seed(&a, vec![UnitToken]);

        let scenario = TestScenario::from_sdk(ctx.build());

        assert_eq!(scenario.places.len(), 4); // 2 IDs + 2 names
        assert!(scenario.places.contains_key("a"));
        assert!(scenario.places.contains_key("A"));
        assert_eq!(scenario.transitions.len(), 2); // 1 ID + 1 name
        assert_eq!(scenario.initial_tokens.len(), 1);
    }

    #[test]
    fn test_resource_allocation_conversion() {
        use aithericon_sdk::DynamicToken;

        let mut ctx = Context::new("resource_alloc");

        let workers = ctx.state::<DynamicToken>("workers", "Workers");
        let tasks = ctx.state::<DynamicToken>("tasks", "Tasks");
        let in_progress = ctx.state::<DynamicToken>("in_progress", "InProgress");
        let completed = ctx.state::<DynamicToken>("completed", "Completed");

        ctx.transition("assign", "Assign")
            .auto_input("worker", &workers)
            .auto_input("task", &tasks)
            .auto_output("work", &in_progress)
            .logic("#{ work: #{ worker: worker, task: task } }");

        ctx.transition("complete", "Complete")
            .auto_input("work", &in_progress)
            .auto_output("worker_out", &workers)
            .auto_output("done", &completed)
            .logic("#{ worker_out: work.worker, done: work.task }");

        let scenario = TestScenario::from_sdk(ctx.build());

        assert!(scenario.places.contains_key("workers"));
        assert!(scenario.places.contains_key("Workers"));
        assert!(scenario.transitions.contains_key("assign"));
        assert!(scenario.transitions.contains_key("Assign"));
    }

    #[test]
    fn test_guarded_transition_conversion() {
        use aithericon_sdk::DynamicToken;

        let mut ctx = Context::new("guarded");

        let input = ctx.state::<DynamicToken>("input", "Input");
        let approved = ctx.state::<DynamicToken>("approved", "Approved");

        ctx.transition("approve", "Approve")
            .auto_input("request", &input)
            .auto_output("out", &approved)
            .guard("request.amount >= 100")
            .logic("#{ out: request }");

        let scenario = TestScenario::from_sdk(ctx.build());

        // Verify the transition has a guard
        let transition = scenario
            .net
            .transitions
            .values()
            .find(|t| t.name == "Approve")
            .expect("transition not found");
        assert!(transition.guard.is_some());
        assert_eq!(transition.guard.as_deref(), Some("request.amount >= 100"));
    }
}
