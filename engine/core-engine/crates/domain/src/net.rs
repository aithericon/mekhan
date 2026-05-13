use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use utoipa::ToSchema;

use crate::place::PlaceKind;
use crate::{Arc, Place, PlaceId, Port, Token, Transition, TransitionId};

/// Visualization group for hierarchical net layout.
///
/// Groups are structural metadata — they travel with the topology inside
/// `NetInitialized` events so they survive hydration from the event log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Group {
    /// Unique group identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Parent group ID for nested groups
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Optional metadata (e.g., {"image": "ffmpeg:latest"})
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// The complete Petri Net topology (structure).
/// This is the "board" on which the token game is played.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PetriNet {
    /// All places in the net, keyed by PlaceId
    #[serde(
        serialize_with = "serialize_places_as_vec",
        deserialize_with = "deserialize_places_from_vec"
    )]
    pub places: HashMap<PlaceId, Place>,
    /// All transitions in the net, keyed by TransitionId
    #[serde(
        serialize_with = "serialize_transitions_as_vec",
        deserialize_with = "deserialize_transitions_from_vec"
    )]
    pub transitions: HashMap<TransitionId, Transition>,
    /// All arcs connecting places and transitions
    pub arcs: Vec<Arc>,
    /// Visualization groups for hierarchical layout
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Group>,
}

// -- Serde helpers: serialize HashMap as Vec (JSON array), deserialize by keying on .id --

fn serialize_places_as_vec<S: Serializer>(
    map: &HashMap<PlaceId, Place>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let vec: Vec<&Place> = map.values().collect();
    vec.serialize(serializer)
}

fn deserialize_places_from_vec<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<PlaceId, Place>, D::Error> {
    let vec: Vec<Place> = Vec::deserialize(deserializer)?;
    Ok(vec.into_iter().map(|p| (p.id.clone(), p)).collect())
}

fn serialize_transitions_as_vec<S: Serializer>(
    map: &HashMap<TransitionId, Transition>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let vec: Vec<&Transition> = map.values().collect();
    vec.serialize(serializer)
}

fn deserialize_transitions_from_vec<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<HashMap<TransitionId, Transition>, D::Error> {
    let vec: Vec<Transition> = Vec::deserialize(deserializer)?;
    Ok(vec.into_iter().map(|t| (t.id.clone(), t)).collect())
}

impl PetriNet {
    pub fn new() -> Self {
        Self {
            places: HashMap::new(),
            transitions: HashMap::new(),
            arcs: Vec::new(),
            groups: Vec::new(),
        }
    }

    pub fn add_place(&mut self, place: Place) -> PlaceId {
        let id = place.id.clone();
        self.places.insert(id.clone(), place);
        id
    }

    pub fn add_transition(&mut self, transition: Transition) -> TransitionId {
        let id = transition.id.clone();
        self.transitions.insert(id.clone(), transition);
        id
    }

    pub fn add_arc(&mut self, arc: Arc) {
        self.arcs.push(arc);
    }

    pub fn get_place(&self, id: &PlaceId) -> Option<&Place> {
        self.places.get(id)
    }

    pub fn get_transition(&self, id: &TransitionId) -> Option<&Transition> {
        self.transitions.get(id)
    }

    /// Get all input arcs for a transition
    pub fn input_arcs(&self, transition_id: &TransitionId) -> Vec<&Arc> {
        self.arcs
            .iter()
            .filter(|a| &a.transition_id == transition_id && a.is_input())
            .collect()
    }

    /// Get all output arcs for a transition
    pub fn output_arcs(&self, transition_id: &TransitionId) -> Vec<&Arc> {
        self.arcs
            .iter()
            .filter(|a| &a.transition_id == transition_id && a.is_output())
            .collect()
    }

    /// Get the input arc for a specific port on a transition
    pub fn input_arc_for_port(
        &self,
        transition_id: &TransitionId,
        port_name: &str,
    ) -> Option<&Arc> {
        self.arcs
            .iter()
            .find(|a| &a.transition_id == transition_id && a.is_input() && a.port_name == port_name)
    }

    /// Get the output arc for a specific port on a transition
    pub fn output_arc_for_port(
        &self,
        transition_id: &TransitionId,
        port_name: &str,
    ) -> Option<&Arc> {
        self.arcs.iter().find(|a| {
            &a.transition_id == transition_id && a.is_output() && a.port_name == port_name
        })
    }

    /// Get all input arcs for a transition, grouped by port name
    pub fn input_arcs_by_port(&self, transition_id: &TransitionId) -> HashMap<String, Vec<&Arc>> {
        let mut by_port: HashMap<String, Vec<&Arc>> = HashMap::new();
        for arc in self.input_arcs(transition_id) {
            by_port.entry(arc.port_name.clone()).or_default().push(arc);
        }
        by_port
    }

    /// Get all output arcs for a transition, grouped by port name
    pub fn output_arcs_by_port(&self, transition_id: &TransitionId) -> HashMap<String, Vec<&Arc>> {
        let mut by_port: HashMap<String, Vec<&Arc>> = HashMap::new();
        for arc in self.output_arcs(transition_id) {
            by_port.entry(arc.port_name.clone()).or_default().push(arc);
        }
        by_port
    }

    /// Get an input port definition for a transition
    pub fn get_input_port(&self, transition_id: &TransitionId, port_name: &str) -> Option<&Port> {
        self.get_transition(transition_id)
            .and_then(|t| t.input_port(port_name))
    }

    /// Get all terminal place IDs.
    pub fn terminal_places(&self) -> Vec<PlaceId> {
        self.places
            .values()
            .filter(|p| matches!(p.kind, PlaceKind::Terminal))
            .map(|p| p.id.clone())
            .collect()
    }

    /// Get an output port definition for a transition
    pub fn get_output_port(&self, transition_id: &TransitionId, port_name: &str) -> Option<&Port> {
        self.get_transition(transition_id)
            .and_then(|t| t.output_port(port_name))
    }
}

impl Default for PetriNet {
    fn default() -> Self {
        Self::new()
    }
}

/// The current distribution of tokens across places.
/// This is the dynamic state of the Petri Net.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Marking {
    /// Map from place ID to tokens at that place
    pub tokens: HashMap<PlaceId, Vec<Token>>,
}

impl Marking {
    pub fn new() -> Self {
        Self {
            tokens: HashMap::new(),
        }
    }

    /// Add a token to a place
    pub fn add_token(&mut self, place_id: PlaceId, token: Token) {
        self.tokens.entry(place_id).or_default().push(token);
    }

    /// Remove a specific token from a place by ID
    pub fn remove_token(&mut self, place_id: &PlaceId, token_id: &crate::TokenId) -> Option<Token> {
        if let Some(tokens) = self.tokens.get_mut(place_id) {
            if let Some(pos) = tokens.iter().position(|t| &t.id == token_id) {
                return Some(tokens.remove(pos));
            }
        }
        None
    }

    /// Get all tokens at a place
    pub fn tokens_at(&self, place_id: &PlaceId) -> &[Token] {
        self.tokens
            .get(place_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Count tokens at a place
    pub fn token_count(&self, place_id: &PlaceId) -> usize {
        self.tokens.get(place_id).map(|v| v.len()).unwrap_or(0)
    }

    /// Update a token's data in place
    pub fn update_token(
        &mut self,
        place_id: &PlaceId,
        token_id: &crate::TokenId,
        new_color: crate::TokenColor,
    ) -> bool {
        if let Some(tokens) = self.tokens.get_mut(place_id) {
            if let Some(token) = tokens.iter_mut().find(|t| &t.id == token_id) {
                token.color = new_color;
                return true;
            }
        }
        false
    }
}

impl Default for Marking {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Port, Token, TokenColor, TokenId};

    #[test]
    fn test_petri_net_new() {
        let net = PetriNet::new();
        assert!(net.places.is_empty());
        assert!(net.transitions.is_empty());
        assert!(net.arcs.is_empty());
    }

    #[test]
    fn test_petri_net_add_place() {
        let mut net = PetriNet::new();
        let place = Place::internal("test_place");
        let id = net.add_place(place.clone());

        assert_eq!(net.places.len(), 1);
        assert_eq!(net.get_place(&id).unwrap().name, "test_place");
    }

    #[test]
    fn test_petri_net_add_transition() {
        let mut net = PetriNet::new();
        let transition = Transition::new("test_transition", "#{out: inp}")
            .with_input_port(Port::new("inp"))
            .with_output_port(Port::new("out"));
        let id = net.add_transition(transition);

        assert_eq!(net.transitions.len(), 1);
        assert_eq!(net.get_transition(&id).unwrap().name, "test_transition");
    }

    #[test]
    fn test_petri_net_input_output_arcs() {
        let mut net = PetriNet::new();

        let place1 = Place::internal("input");
        let place2 = Place::internal("output");
        let place1_id = net.add_place(place1);
        let place2_id = net.add_place(place2);

        let transition = Transition::new("t1", "#{out: inp}")
            .with_input_port(Port::new("inp"))
            .with_output_port(Port::new("out"));
        let transition_id = net.add_transition(transition);

        // Input arc: place1 -> transition (inp port)
        let input_arc = Arc::input(place1_id.clone(), transition_id.clone(), "inp");
        net.add_arc(input_arc);

        // Output arc: transition (out port) -> place2
        let output_arc = Arc::output(transition_id.clone(), "out", place2_id.clone());
        net.add_arc(output_arc);

        let inputs = net.input_arcs(&transition_id);
        let outputs = net.output_arcs(&transition_id);

        assert_eq!(inputs.len(), 1);
        assert_eq!(outputs.len(), 1);
        assert_eq!(inputs[0].place_id, place1_id);
        assert_eq!(inputs[0].port_name, "inp");
        assert_eq!(outputs[0].place_id, place2_id);
        assert_eq!(outputs[0].port_name, "out");
    }

    #[test]
    fn test_petri_net_port_lookup() {
        let mut net = PetriNet::new();

        let place_req = Place::internal("requests");
        let place_ctx = Place::internal("contexts");
        let place_success = Place::internal("success");
        let place_retry = Place::internal("retry");

        let place_req_id = net.add_place(place_req);
        let place_ctx_id = net.add_place(place_ctx);
        let place_success_id = net.add_place(place_success);
        let place_retry_id = net.add_place(place_retry);

        let transition = Transition::new("process", "#{success: req}")
            .with_input_port(Port::new("req"))
            .with_input_port(Port::new("ctx"))
            .with_output_port(Port::new("success"))
            .with_output_port(Port::new("retry"));
        let transition_id = net.add_transition(transition);

        // Wire up arcs
        net.add_arc(Arc::input(
            place_req_id.clone(),
            transition_id.clone(),
            "req",
        ));
        net.add_arc(Arc::input(
            place_ctx_id.clone(),
            transition_id.clone(),
            "ctx",
        ));
        net.add_arc(Arc::output(
            transition_id.clone(),
            "success",
            place_success_id.clone(),
        ));
        net.add_arc(Arc::output(
            transition_id.clone(),
            "retry",
            place_retry_id.clone(),
        ));

        // Test port lookup
        let req_arc = net.input_arc_for_port(&transition_id, "req");
        assert!(req_arc.is_some());
        assert_eq!(req_arc.unwrap().place_id, place_req_id);

        let success_arc = net.output_arc_for_port(&transition_id, "success");
        assert!(success_arc.is_some());
        assert_eq!(success_arc.unwrap().place_id, place_success_id);

        // Test port definitions
        let req_port = net.get_input_port(&transition_id, "req");
        assert!(req_port.is_some());
        assert_eq!(req_port.unwrap().name, "req");

        // Test arcs by port
        let inputs_by_port = net.input_arcs_by_port(&transition_id);
        assert_eq!(inputs_by_port.len(), 2);
        assert!(inputs_by_port.contains_key("req"));
        assert!(inputs_by_port.contains_key("ctx"));
    }

    #[test]
    fn test_marking_add_and_get_tokens() {
        let mut marking = Marking::new();
        let place_id = PlaceId::new();

        let token1 = Token::new(TokenColor::Unit);
        let token2 = Token::new(TokenColor::Integer(42));

        marking.add_token(place_id.clone(), token1.clone());
        marking.add_token(place_id.clone(), token2.clone());

        assert_eq!(marking.token_count(&place_id), 2);

        let tokens = marking.tokens_at(&place_id);
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].id, token1.id);
        assert_eq!(tokens[1].id, token2.id);
    }

    #[test]
    fn test_marking_remove_token() {
        let mut marking = Marking::new();
        let place_id = PlaceId::new();

        let token1 = Token::new(TokenColor::Unit);
        let token2 = Token::new(TokenColor::Unit);
        let token1_id = token1.id.clone();

        marking.add_token(place_id.clone(), token1);
        marking.add_token(place_id.clone(), token2);

        let removed = marking.remove_token(&place_id, &token1_id);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().id, token1_id);
        assert_eq!(marking.token_count(&place_id), 1);
    }

    #[test]
    fn test_marking_tokens_at_empty_place() {
        let marking = Marking::new();
        let place_id = PlaceId::new();

        let tokens = marking.tokens_at(&place_id);
        assert!(tokens.is_empty());
        assert_eq!(marking.token_count(&place_id), 0);
    }

    #[test]
    fn test_marking_remove_nonexistent_token() {
        let mut marking = Marking::new();
        let place_id = PlaceId::new();
        let fake_token_id = TokenId::new();

        let removed = marking.remove_token(&place_id, &fake_token_id);
        assert!(removed.is_none());
    }

    #[test]
    fn test_terminal_places_helper() {
        let mut net = PetriNet::new();
        net.add_place(Place::internal("start"));
        net.add_place(Place::terminal("done"));
        net.add_place(Place::signal("sig"));
        net.add_place(Place::terminal("fail"));

        let terminals = net.terminal_places();
        assert_eq!(terminals.len(), 2);
        assert!(terminals.contains(&PlaceId("done".to_string())));
        assert!(terminals.contains(&PlaceId("fail".to_string())));
    }

    #[test]
    fn test_terminal_places_empty_net() {
        let net = PetriNet::new();
        assert!(net.terminal_places().is_empty());
    }

    #[test]
    fn test_terminal_places_no_terminals() {
        let mut net = PetriNet::new();
        net.add_place(Place::internal("a"));
        net.add_place(Place::signal("b"));
        assert!(net.terminal_places().is_empty());
    }

    #[test]
    fn test_petri_net_serialization() {
        let mut net = PetriNet::new();
        let place = Place::internal("p1");
        let place_id = net.add_place(place);

        let json = serde_json::to_string(&net).unwrap();
        let deserialized: PetriNet = serde_json::from_str(&json).unwrap();

        assert_eq!(net.places.len(), deserialized.places.len());
        assert_eq!(
            net.get_place(&place_id).unwrap().name,
            deserialized.get_place(&place_id).unwrap().name
        );

        // Verify JSON uses array format (backward-compatible)
        let json_value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            json_value["places"].is_array(),
            "places should serialize as array"
        );
        assert!(
            json_value["transitions"].is_array(),
            "transitions should serialize as array"
        );
    }
}
