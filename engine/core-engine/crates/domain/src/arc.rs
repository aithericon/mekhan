use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ArcId, PlaceId, TransitionId};

/// Direction of an arc in the Petri Net.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArcDirection {
    /// Arc from a place to a transition (input arc)
    PlaceToTransition,
    /// Arc from a transition to a place (output arc)
    TransitionToPlace,
}

/// An arc connecting a place to a specific port on a transition.
///
/// Arcs define the wiring between places and transition ports:
/// - Input arcs (PlaceToTransition): Connect a place to an input port
/// - Output arcs (TransitionToPlace): Connect an output port to a place
///
/// The port_name determines which port on the transition this arc connects to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Arc {
    /// Unique identifier
    pub id: ArcId,

    /// The place endpoint
    pub place_id: PlaceId,

    /// The transition endpoint
    pub transition_id: TransitionId,

    /// Direction of the arc
    pub direction: ArcDirection,

    /// The name of the port on the transition that this arc connects to.
    /// For input arcs: the input port receiving tokens from this place.
    /// For output arcs: the output port sending tokens to this place.
    pub port_name: String,

    /// Number of tokens consumed/produced (default: 1)
    #[serde(default = "default_weight")]
    pub weight: usize,

    /// If true, this is a read arc (test arc): the token is consumed for
    /// evaluation but automatically produced back after firing. Only meaningful
    /// on PlaceToTransition arcs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub read: bool,

    /// Gather barrier: a producer-namespaced reference (e.g. `"expected.k"`)
    /// to a field on a bound coordinator token that supplies the count `K` of
    /// result tokens this Batch input arc must accumulate before the transition
    /// fires. `None` (the default) preserves today's non-barrier behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_from: Option<String>,

    /// Gather barrier: the field name on result tokens used to correlate them
    /// into a single gather group (e.g. `"iteration_id"`), so overlapping loop
    /// iterations don't mix. `None` (the default) preserves today's behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlate_on: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn default_weight() -> usize {
    1
}

impl Arc {
    /// Create an input arc (place -> transition port)
    pub fn input(
        place_id: PlaceId,
        transition_id: TransitionId,
        port_name: impl Into<String>,
    ) -> Self {
        Self {
            id: ArcId::new(),
            place_id,
            transition_id,
            direction: ArcDirection::PlaceToTransition,
            port_name: port_name.into(),
            weight: 1,
            read: false,
            count_from: None,
            correlate_on: None,
        }
    }

    /// Create an output arc (transition port -> place)
    pub fn output(
        transition_id: TransitionId,
        port_name: impl Into<String>,
        place_id: PlaceId,
    ) -> Self {
        Self {
            id: ArcId::new(),
            place_id,
            transition_id,
            direction: ArcDirection::TransitionToPlace,
            port_name: port_name.into(),
            weight: 1,
            read: false,
            count_from: None,
            correlate_on: None,
        }
    }

    /// Set this arc as a read arc (token consumed for evaluation, auto-produced back).
    pub fn with_read(mut self, read: bool) -> Self {
        self.read = read;
        self
    }

    pub fn with_weight(mut self, weight: usize) -> Self {
        self.weight = weight;
        self
    }

    /// Set the gather-barrier count source (producer-namespaced reference to a
    /// field on a bound coordinator token supplying `K`).
    pub fn with_count_from(mut self, count_from: impl Into<String>) -> Self {
        self.count_from = Some(count_from.into());
        self
    }

    /// Set the gather-barrier correlation field (the field on result tokens
    /// used to group them into a single gather).
    pub fn with_correlate_on(mut self, correlate_on: impl Into<String>) -> Self {
        self.correlate_on = Some(correlate_on.into());
        self
    }

    pub fn with_id(mut self, id: ArcId) -> Self {
        self.id = id;
        self
    }

    /// Check if this is an input arc (place -> transition)
    pub fn is_input(&self) -> bool {
        matches!(self.direction, ArcDirection::PlaceToTransition)
    }

    /// Check if this is an output arc (transition -> place)
    pub fn is_output(&self) -> bool {
        matches!(self.direction, ArcDirection::TransitionToPlace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arc_input() {
        let place_id = PlaceId::new();
        let transition_id = TransitionId::new();
        let arc = Arc::input(place_id.clone(), transition_id.clone(), "request");

        assert_eq!(arc.place_id, place_id);
        assert_eq!(arc.transition_id, transition_id);
        assert_eq!(arc.port_name, "request");
        assert!(arc.is_input());
        assert!(!arc.is_output());
        assert_eq!(arc.weight, 1);
    }

    #[test]
    fn test_arc_output() {
        let place_id = PlaceId::new();
        let transition_id = TransitionId::new();
        let arc = Arc::output(transition_id.clone(), "success", place_id.clone());

        assert_eq!(arc.place_id, place_id);
        assert_eq!(arc.transition_id, transition_id);
        assert_eq!(arc.port_name, "success");
        assert!(!arc.is_input());
        assert!(arc.is_output());
    }

    #[test]
    fn test_arc_with_weight() {
        let arc = Arc::input(PlaceId::new(), TransitionId::new(), "batch_items").with_weight(5);

        assert_eq!(arc.weight, 5);
    }

    #[test]
    fn test_arc_serialization() {
        let arc = Arc::input(PlaceId::new(), TransitionId::new(), "signal").with_weight(2);

        let json = serde_json::to_string(&arc).unwrap();
        let deserialized: Arc = serde_json::from_str(&json).unwrap();

        assert_eq!(arc.port_name, deserialized.port_name);
        assert_eq!(arc.weight, deserialized.weight);
        assert_eq!(arc.direction, deserialized.direction);
    }

    #[test]
    fn test_arc_without_gather_fields_omits_them_from_json() {
        // Byte-compat: an arc with no gather fields must not emit the keys, so
        // existing nets/AIR round-trip identically.
        let arc = Arc::input(PlaceId::new(), TransitionId::new(), "items");
        let json = serde_json::to_string(&arc).unwrap();

        assert!(!json.contains("count_from"), "json was: {json}");
        assert!(!json.contains("correlate_on"), "json was: {json}");

        assert!(arc.count_from.is_none());
        assert!(arc.correlate_on.is_none());

        let deserialized: Arc = serde_json::from_str(&json).unwrap();
        assert_eq!(arc, deserialized);
    }

    #[test]
    fn test_arc_with_gather_fields_round_trips() {
        let arc = Arc::input(PlaceId::new(), TransitionId::new(), "results")
            .with_count_from("expected.k")
            .with_correlate_on("iteration_id");

        assert_eq!(arc.count_from.as_deref(), Some("expected.k"));
        assert_eq!(arc.correlate_on.as_deref(), Some("iteration_id"));

        let json = serde_json::to_string(&arc).unwrap();
        assert!(json.contains("count_from"), "json was: {json}");
        assert!(json.contains("correlate_on"), "json was: {json}");

        let deserialized: Arc = serde_json::from_str(&json).unwrap();
        assert_eq!(arc, deserialized);
        assert_eq!(deserialized.count_from.as_deref(), Some("expected.k"));
        assert_eq!(deserialized.correlate_on.as_deref(), Some("iteration_id"));
    }

    #[test]
    fn test_arc_json_format() {
        let arc = Arc::output(TransitionId::new(), "result", PlaceId::new());
        let json = serde_json::to_string_pretty(&arc).unwrap();

        assert!(json.contains("\"port_name\": \"result\""));
        assert!(json.contains("\"direction\": \"transition_to_place\""));
    }
}
