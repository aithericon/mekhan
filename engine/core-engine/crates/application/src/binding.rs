use std::collections::HashMap;

use chrono::{DateTime, Utc};
use petri_domain::{
    Arc as PetriArc, Marking, PlaceId, PortCardinality, ReplyRouting, Token, TokenId, Transition,
};
use serde_json::Value as JsonValue;

use crate::rhai_runtime::token_color_to_json;
use crate::schema_registry::SchemaRegistry;
use crate::TransitionExecutor;

/// A valid binding of tokens to input ports for a transition.
#[derive(Clone, Debug)]
pub(crate) struct TokenBinding {
    /// The port inputs (port_name -> JSON data) for this binding
    pub port_inputs: HashMap<String, JsonValue>,
    /// The tokens to consume: (place_id, token_id)
    pub consumed_tokens: Vec<(PlaceId, TokenId)>,
    /// Tokens read via read arcs: (place_id, token). These are NOT removed from marking.
    pub read_tokens: Vec<(PlaceId, petri_domain::Token)>,
    /// The maximum creation time among bound tokens (for enabling time)
    pub max_created_at: Option<DateTime<Utc>>,
    /// Reply routing from consumed tokens (for propagation and bridge_reply resolution)
    pub consumed_reply_routing: Option<ReplyRouting>,
    /// Port names that came from read arcs (subset of port_inputs keys).
    pub read_port_names: Vec<String>,
}

/// Iterator over all combinations of token indices.
/// Given sizes [2, 3], generates: [0,0], [0,1], [0,2], [1,0], [1,1], [1,2]
struct CombinationIterator {
    sizes: Vec<usize>,
    current: Vec<usize>,
    done: bool,
}

impl CombinationIterator {
    fn new(sizes: Vec<usize>) -> Self {
        let done = sizes.contains(&0);
        let current = vec![0; sizes.len()];
        Self {
            sizes,
            current,
            done,
        }
    }
}

impl Iterator for CombinationIterator {
    type Item = Vec<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        let result = self.current.clone();

        // Increment with carry
        let mut carry = true;
        for i in (0..self.sizes.len()).rev() {
            if carry {
                self.current[i] += 1;
                if self.current[i] >= self.sizes[i] {
                    self.current[i] = 0;
                } else {
                    carry = false;
                }
            }
        }

        if carry {
            self.done = true;
        }

        Some(result)
    }
}

/// Find a valid token binding for a transition.
///
/// Searches through all combinations of tokens from input places to find
/// a binding that satisfies the guard. Returns None if no valid binding exists.
///
/// For transitions without guards, returns the first available binding (FIFO).
pub(crate) fn find_valid_binding(
    executor: &TransitionExecutor,
    transition: &Transition,
    input_arcs: &[&PetriArc],
    marking: &Marking,
    schema_registry: Option<&SchemaRegistry>,
) -> Option<TokenBinding> {
    // Collect tokens from each input place
    let mut arc_tokens: Vec<Vec<&Token>> = Vec::new();
    let mut arc_sizes: Vec<usize> = Vec::new();

    for arc in input_arcs {
        let tokens = marking.tokens_at(&arc.place_id);
        if tokens.len() < arc.weight {
            return None; // Not enough tokens
        }
        arc_sizes.push(tokens.len());
        arc_tokens.push(tokens.iter().collect());
    }

    // If no input arcs, return empty binding
    if input_arcs.is_empty() {
        return Some(TokenBinding {
            port_inputs: HashMap::new(),
            consumed_tokens: vec![],
            read_tokens: vec![],
            read_port_names: vec![],
            max_created_at: None,
            consumed_reply_routing: None,
        });
    }

    // If no guard, use FIFO (first token from each place)
    if transition.guard.is_none() && schema_registry.is_none() {
        return build_binding_for_indices(
            transition,
            input_arcs,
            &arc_tokens,
            &vec![0; input_arcs.len()],
            schema_registry,
        );
    }

    // If no guard but schema validation is active, still try FIFO first
    if transition.guard.is_none() {
        if let Some(binding) = build_binding_for_indices(
            transition,
            input_arcs,
            &arc_tokens,
            &vec![0; input_arcs.len()],
            schema_registry,
        ) {
            return Some(binding);
        }
        // FIFO failed schema validation — fall through to search all combinations
    }

    // Search all combinations for one that satisfies the guard (and schema)
    let combo_iter = CombinationIterator::new(arc_sizes);

    for indices in combo_iter {
        if let Some(binding) = build_binding_for_indices(
            transition,
            input_arcs,
            &arc_tokens,
            &indices,
            schema_registry,
        ) {
            // Check if guard passes
            if let Some(guard_script) = &transition.guard {
                match executor.evaluate_guard(guard_script, &binding.port_inputs) {
                    Ok(true) => return Some(binding),
                    Ok(false) => continue,
                    Err(_) => continue,
                }
            } else {
                return Some(binding);
            }
        }
    }

    None
}

/// Build a TokenBinding for a specific set of token indices.
fn build_binding_for_indices(
    transition: &Transition,
    input_arcs: &[&PetriArc],
    arc_tokens: &[Vec<&Token>],
    indices: &[usize],
    schema_registry: Option<&SchemaRegistry>,
) -> Option<TokenBinding> {
    let mut port_inputs: HashMap<String, JsonValue> = HashMap::new();
    let mut consumed_tokens: Vec<(PlaceId, TokenId)> = Vec::new();
    let mut read_tokens: Vec<(PlaceId, Token)> = Vec::new();
    let mut read_port_names: Vec<String> = Vec::new();
    let mut max_created_at: Option<DateTime<Utc>> = None;
    let mut consumed_reply_routing: Option<ReplyRouting> = None;

    for (arc_idx, arc) in input_arcs.iter().enumerate() {
        let token_idx = indices[arc_idx];
        let tokens = &arc_tokens[arc_idx];

        if token_idx >= tokens.len() {
            return None;
        }

        let token = tokens[token_idx];

        // Merge reply_routing from consumed tokens (skip read arcs)
        if !arc.read {
            if let Some(incoming) = &token.reply_routing {
                consumed_reply_routing = match consumed_reply_routing {
                    None => Some(incoming.clone()),
                    Some(existing) => match merge_reply_routing(existing, incoming) {
                        Some(merged) => Some(merged),
                        None => {
                            tracing::debug!(
                                arc_port = %arc.port_name,
                                "reply_routing merge conflict — skipping binding"
                            );
                            return None;
                        }
                    },
                };
            }
        }

        // Get cardinality
        let port = transition.input_port(&arc.port_name);
        let cardinality = port
            .map(|p| &p.cardinality)
            .unwrap_or(&PortCardinality::Single);

        // For Single cardinality, we just use the one token at this index
        // For Batch, we'd need different logic (not changing that behavior)
        let token_data: JsonValue = match cardinality {
            PortCardinality::Single => token_color_to_json(&token.color),
            PortCardinality::Batch => {
                // For batch, collect ALL tokens from this place
                let batch_tokens: Vec<JsonValue> = tokens
                    .iter()
                    .map(|t| token_color_to_json(&t.color))
                    .collect();
                JsonValue::Array(batch_tokens)
            }
        };

        // Track consumed or read tokens
        if arc.read {
            // Read arc: token is available to script but NOT removed from marking
            read_port_names.push(arc.port_name.clone());
            match cardinality {
                PortCardinality::Single => {
                    read_tokens.push((arc.place_id.clone(), token.clone()));
                    max_created_at =
                        Some(max_created_at.map_or(token.created_at, |t| t.max(token.created_at)));
                }
                PortCardinality::Batch => {
                    for t in tokens.iter() {
                        read_tokens.push((arc.place_id.clone(), (*t).clone()));
                        max_created_at =
                            Some(max_created_at.map_or(t.created_at, |m| m.max(t.created_at)));
                    }
                }
            }
        } else {
            // Normal arc: token is consumed
            match cardinality {
                PortCardinality::Single => {
                    consumed_tokens.push((arc.place_id.clone(), token.id.clone()));
                    max_created_at =
                        Some(max_created_at.map_or(token.created_at, |t| t.max(token.created_at)));
                }
                PortCardinality::Batch => {
                    for t in tokens.iter().skip(token_idx).take(arc.weight) {
                        consumed_tokens.push((arc.place_id.clone(), t.id.clone()));
                        max_created_at =
                            Some(max_created_at.map_or(t.created_at, |m| m.max(t.created_at)));
                    }
                }
            }
        }

        // Validate token data against port schema if registry is present
        if let Some(registry) = schema_registry {
            if let Some(port) = transition.input_port(&arc.port_name) {
                if let Some(ref schema_ref) = port.schema_ref {
                    if registry.validate(schema_ref, &token_data).is_err() {
                        return None; // Wrong-shaped token — skip this binding
                    }
                }
            }
        }

        port_inputs.insert(arc.port_name.clone(), token_data);
    }

    Some(TokenBinding {
        port_inputs,
        consumed_tokens,
        read_tokens,
        read_port_names,
        max_created_at,
        consumed_reply_routing,
    })
}

/// Merge two `ReplyRouting` values. Returns `None` on conflict.
///
/// - `reply_to`: must be identical if both are `Some`
/// - `reply_channels`: maps are merged; conflicting keys (same name, different address) → `None`
fn merge_reply_routing(existing: ReplyRouting, incoming: &ReplyRouting) -> Option<ReplyRouting> {
    // Merge reply_to: if both present, they must match
    let reply_to = match (&existing.reply_to, &incoming.reply_to) {
        (Some(a), Some(b)) if a != b => return None,
        (Some(_), _) => existing.reply_to,
        (None, other) => other.clone(),
    };

    // Merge reply_channels maps
    let reply_channels = match (existing.reply_channels, &incoming.reply_channels) {
        (Some(mut a), Some(b)) => {
            for (key, addr) in b {
                if let Some(existing_addr) = a.get(key) {
                    if existing_addr != addr {
                        return None; // Conflicting channel key
                    }
                } else {
                    a.insert(key.clone(), addr.clone());
                }
            }
            Some(a)
        }
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    };

    Some(ReplyRouting {
        reply_to,
        reply_channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::{
        Arc as PetriArc, BridgeReplyAddress, Marking, PlaceId, Port, Token, TokenColor, Transition,
        TransitionId,
    };
    use serde_json::json;

    use crate::TransitionExecutor;

    /// Helper: create a Token with JSON data.
    fn data_token(value: serde_json::Value) -> Token {
        Token::new(TokenColor::Data(value))
    }

    /// Helper: build a minimal Transition with given input ports.
    fn transition_with_ports(input_ports: Vec<Port>) -> Transition {
        let mut t = Transition::new("test_transition", r#"#{}"#);
        t.input_ports = input_ports;
        t
    }

    // ── Batch read arc: all tokens should appear ────────────────────────

    #[test]
    fn batch_read_arc_returns_all_tokens() {
        let executor = TransitionExecutor::new();
        let place_id = PlaceId::named("observations");
        let t_id = TransitionId::named("dispatch_fit");

        let transition = transition_with_ports(vec![Port::batch("obs")]);

        let arc = PetriArc::input(place_id.clone(), t_id, "obs").with_read(true);

        // Seed 5 tokens
        let mut marking = Marking::new();
        for i in 0..5 {
            marking.add_token(
                place_id.clone(),
                data_token(json!({ "a": i as f64 * 0.1, "d": 0.5, "z": i as f64 })),
            );
        }

        let arcs: Vec<&PetriArc> = vec![&arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed");

        // The batch port should contain ALL 5 tokens as a JSON array
        let obs = binding.port_inputs.get("obs").expect("obs port missing");
        let arr = obs.as_array().expect("obs should be an array");
        assert_eq!(arr.len(), 5, "batch read should return all 5 tokens");

        // Read arc: tokens should NOT be consumed
        assert!(
            binding.consumed_tokens.is_empty(),
            "read arc must not consume tokens"
        );

        // All 5 tokens tracked as read
        assert_eq!(
            binding.read_tokens.len(),
            5,
            "all 5 tokens should be tracked as read"
        );

        // Port should be in read_port_names
        assert!(binding.read_port_names.contains(&"obs".to_string()));
    }

    #[test]
    fn batch_read_arc_with_single_token() {
        let executor = TransitionExecutor::new();
        let place_id = PlaceId::named("observations");
        let t_id = TransitionId::named("dispatch_fit");

        let transition = transition_with_ports(vec![Port::batch("obs")]);
        let arc = PetriArc::input(place_id.clone(), t_id, "obs").with_read(true);

        let mut marking = Marking::new();
        marking.add_token(place_id.clone(), data_token(json!({ "x": 1 })));

        let arcs: Vec<&PetriArc> = vec![&arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed");

        let obs = binding.port_inputs.get("obs").unwrap();
        let arr = obs.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert!(binding.consumed_tokens.is_empty());
        assert_eq!(binding.read_tokens.len(), 1);
    }

    #[test]
    fn batch_read_arc_with_normal_input() {
        // Scenario: one batch-read arc + one normal single-consume arc
        // (mimics dispatch_fit: trigger consumed, observations read)
        let executor = TransitionExecutor::new();
        let obs_place = PlaceId::named("observation_log");
        let trigger_place = PlaceId::named("fit_trigger");
        let t_id = TransitionId::named("dispatch_fit");

        let transition = transition_with_ports(vec![
            Port::new("trigger"),
            Port::batch("obs"),
        ]);

        let trigger_arc = PetriArc::input(trigger_place.clone(), t_id.clone(), "trigger");
        let obs_arc = PetriArc::input(obs_place.clone(), t_id, "obs").with_read(true);

        let mut marking = Marking::new();

        // 1 trigger token
        marking.add_token(trigger_place.clone(), data_token(json!({ "iteration": 5 })));

        // 4 observation tokens
        for i in 0..4 {
            marking.add_token(
                obs_place.clone(),
                data_token(json!({ "a": i, "d": i, "z": i })),
            );
        }

        let arcs: Vec<&PetriArc> = vec![&trigger_arc, &obs_arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed");

        // Trigger consumed
        assert_eq!(binding.consumed_tokens.len(), 1);
        let trigger_data = binding.port_inputs.get("trigger").unwrap();
        assert_eq!(trigger_data["iteration"], 5);

        // All 4 observations read (not consumed)
        let obs = binding.port_inputs.get("obs").unwrap();
        assert_eq!(obs.as_array().unwrap().len(), 4);
        assert_eq!(binding.read_tokens.len(), 4);
    }

    #[test]
    fn batch_read_empty_place_returns_none() {
        let executor = TransitionExecutor::new();
        let place_id = PlaceId::named("observations");
        let t_id = TransitionId::named("dispatch_fit");

        let transition = transition_with_ports(vec![Port::batch("obs")]);
        let arc = PetriArc::input(place_id.clone(), t_id, "obs").with_read(true);

        let marking = Marking::new(); // empty — no tokens

        let arcs: Vec<&PetriArc> = vec![&arc];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None);
        assert!(binding.is_none(), "should not bind when place is empty");
    }

    // ── Reply routing merge tests ──────────────────────────────────────

    fn addr(net: &str, place: &str) -> BridgeReplyAddress {
        BridgeReplyAddress {
            net_id: net.to_string(),
            place_name: place.to_string(),
        }
    }

    #[test]
    fn merge_reply_routing_one_token_has_it() {
        let executor = TransitionExecutor::new();
        let p1 = PlaceId::named("a");
        let p2 = PlaceId::named("b");
        let t_id = TransitionId::named("t");

        let transition = transition_with_ports(vec![Port::new("a"), Port::new("b")]);
        let arc1 = PetriArc::input(p1.clone(), t_id.clone(), "a");
        let arc2 = PetriArc::input(p2.clone(), t_id, "b");

        let mut marking = Marking::new();
        // Token with reply routing
        let mut t1 = data_token(json!({"x": 1}));
        t1 = t1.with_reply_routing(ReplyRouting {
            reply_to: Some(addr("net-a", "reply_inbox")),
            reply_channels: None,
        });
        marking.add_token(p1, t1);
        // Token without reply routing
        marking.add_token(p2, data_token(json!({"y": 2})));

        let arcs: Vec<&PetriArc> = vec![&arc1, &arc2];
        let binding = find_valid_binding(&executor, &transition, &arcs, &marking, None)
            .expect("binding should succeed");
        let routing = binding.consumed_reply_routing.expect("should have routing");
        assert_eq!(routing.reply_to.unwrap().net_id, "net-a");
    }

    #[test]
    fn merge_reply_routing_compatible_channels() {
        let existing = ReplyRouting {
            reply_to: None,
            reply_channels: Some(HashMap::from([
                ("alpha".to_string(), addr("net-a", "alpha_inbox")),
            ])),
        };
        let incoming = ReplyRouting {
            reply_to: None,
            reply_channels: Some(HashMap::from([
                ("beta".to_string(), addr("net-a", "beta_inbox")),
            ])),
        };
        let merged = merge_reply_routing(existing, &incoming).expect("should merge");
        let channels = merged.reply_channels.unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels["alpha"].place_name, "alpha_inbox");
        assert_eq!(channels["beta"].place_name, "beta_inbox");
    }

    #[test]
    fn merge_reply_routing_conflicting_channel() {
        let existing = ReplyRouting {
            reply_to: None,
            reply_channels: Some(HashMap::from([
                ("result".to_string(), addr("net-a", "inbox_a")),
            ])),
        };
        let incoming = ReplyRouting {
            reply_to: None,
            reply_channels: Some(HashMap::from([
                ("result".to_string(), addr("net-b", "inbox_b")), // different address
            ])),
        };
        assert!(
            merge_reply_routing(existing, &incoming).is_none(),
            "conflicting channel key should fail"
        );
    }

    #[test]
    fn merge_reply_routing_conflicting_reply_to() {
        let existing = ReplyRouting {
            reply_to: Some(addr("net-a", "reply_a")),
            reply_channels: None,
        };
        let incoming = ReplyRouting {
            reply_to: Some(addr("net-b", "reply_b")),
            reply_channels: None,
        };
        assert!(
            merge_reply_routing(existing, &incoming).is_none(),
            "conflicting reply_to should fail"
        );
    }

    #[test]
    fn merge_reply_routing_identical_reply_to() {
        let existing = ReplyRouting {
            reply_to: Some(addr("net-a", "reply_inbox")),
            reply_channels: None,
        };
        let incoming = ReplyRouting {
            reply_to: Some(addr("net-a", "reply_inbox")),
            reply_channels: None,
        };
        let merged = merge_reply_routing(existing, &incoming).expect("identical should merge");
        assert_eq!(merged.reply_to.unwrap().place_name, "reply_inbox");
    }
}
