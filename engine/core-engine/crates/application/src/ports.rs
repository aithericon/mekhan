use petri_domain::{
    apply_event_to_marking, DomainEvent, Marking, PersistedEvent, PetriNet, TransitionId,
};
use thiserror::Error;

/// Error type for event store operations.
#[derive(Error, Debug, Clone)]
pub enum EventStoreError {
    #[error("Failed to persist event: {0}")]
    PersistFailed(String),
    #[error("Timeout waiting for event persistence")]
    Timeout,
}

/// Port for event storage (outbound).
/// Implementations provide persistence for the event log.
#[async_trait::async_trait]
pub trait EventRepository: Send + Sync {
    /// Append a new event to the log.
    /// Returns the persisted event with sequence number and hash.
    /// May fail if the underlying store is unavailable (e.g., NATS down).
    async fn append(&self, event: DomainEvent) -> Result<PersistedEvent, EventStoreError>;

    /// Get all events in storage order.
    async fn all_events(&self) -> Vec<PersistedEvent>;

    /// Get events whose `.sequence` field is `>= sequence`.
    ///
    /// Filters by the *content* of `PersistedEvent.sequence`. This is **not**
    /// safe to use for incremental cache cursoring when the log can contain
    /// events with non-monotonic `.sequence` (e.g. hydrated old sessions whose
    /// numbering overlaps with the current run). Prefer
    /// [`events_from`](Self::events_from) for cache/cursor use cases.
    async fn events_since(&self, sequence: u64) -> Vec<PersistedEvent>;

    /// Clear all events (for testing/reset).
    async fn reset(&self);

    /// Get the current sequence number that the next live append will use.
    ///
    /// Implementations backed only by an in-memory `Vec` return `len()` here,
    /// which coincides with "next sequence" only when sequences are monotonic
    /// 0..len. For cache/cursor logic, prefer [`len`](Self::len) which is
    /// always the storage-order count.
    async fn current_sequence(&self) -> u64;

    /// Number of events currently in the log (storage-order count).
    ///
    /// This is the correct cursor for incremental projection: pair it with
    /// [`events_from`](Self::events_from) to slice the events appended since
    /// a remembered position. It is always monotonic w.r.t. live appends,
    /// even if the cache was hydrated with events carrying overlapping
    /// `.sequence` fields.
    ///
    /// Default goes through `all_events().len()` — correct for any impl,
    /// but allocates. Override with a direct length read where possible.
    async fn len(&self) -> usize {
        self.all_events().await.len()
    }

    /// Slice the log from the given storage-order index to the end.
    ///
    /// Unlike [`events_since`](Self::events_since) this filters by *position*
    /// in the log, not by the `.sequence` field. Use this — paired with
    /// [`len`](Self::len) — to drive incremental marking cache updates: a
    /// remembered index `i` plus `events_from(i)` always yields exactly the
    /// events appended after `i`, regardless of whether their `.sequence`
    /// values overlap with earlier hydrated events.
    ///
    /// Default slices `all_events()` — correct for any impl, but copies the
    /// full log. Override with a direct positional slice where possible.
    async fn events_from(&self, idx: usize) -> Vec<PersistedEvent> {
        let all = self.all_events().await;
        let start = idx.min(all.len());
        all[start..].to_vec()
    }
}

/// Port for topology storage (outbound).
/// Implementations provide persistence for the Petri Net structure.
pub trait TopologyRepository: Send + Sync {
    /// Get the current topology.
    fn get_topology(&self) -> Option<PetriNet>;

    /// Set/replace the topology.
    fn set_topology(&self, net: PetriNet);

    /// Clear the topology.
    fn clear(&self);

    /// Update a transition's script and guard in-place.
    /// Returns true if the transition was found and updated.
    fn update_transition_script(
        &self,
        transition_id: &TransitionId,
        script: String,
        guard: Option<String>,
    ) -> bool;
}

/// Port for state projection (outbound).
/// Implementations compute current state from events.
pub trait StateProjection: Send + Sync {
    /// Compute the current marking by replaying all events.
    fn project(&self, events: &[PersistedEvent]) -> Marking;

    /// Apply a single event to an existing marking (incremental projection).
    ///
    /// Default implementation handles all standard event types. Override only
    /// if you need custom projection logic.
    fn apply_event(&self, marking: &mut Marking, event: &DomainEvent) {
        apply_event_to_marking(marking, event);
    }
}

// `apply_event_to_marking` is now in `petri_domain::projection` and
// re-exported via the `use` at the top of this file. Tests below
// continue to verify the behavior through the re-export.

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::{PlaceId, Token, TokenColor, TransitionId};

    #[test]
    fn test_apply_effect_failed_tokens_consumed() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();
        let place_b = PlaceId::new();

        // Add a token to place_a
        let token = Token::new(TokenColor::Unit);
        let token_id = token.id.clone();
        marking.add_token(place_a.clone(), token);
        assert_eq!(marking.token_count(&place_a), 1);

        // Apply EffectFailed with tokens_consumed=true
        let error_token = Token::new(TokenColor::Data(serde_json::json!({"error": "test"})));
        let event = DomainEvent::EffectFailed {
            transition_id: TransitionId::new(),
            transition_name: Some("t1".to_string()),
            consumed_tokens: vec![(place_a.clone(), token_id)],
            produced_tokens: vec![(place_b.clone(), error_token)],
            effect_handler_id: "handler".to_string(),
            error_message: "test error".to_string(),
            tokens_consumed: true,
            input_data: None,
            retryable: true,
        };

        apply_event_to_marking(&mut marking, &event);

        assert_eq!(
            marking.token_count(&place_a),
            0,
            "Token should be consumed from place_a"
        );
        assert_eq!(
            marking.token_count(&place_b),
            1,
            "Error token should be in place_b"
        );
    }

    #[test]
    fn test_apply_net_created_no_marking_change() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();
        let token = Token::new(TokenColor::Unit);
        marking.add_token(place_a.clone(), token);

        let event = DomainEvent::NetCreated {
            net_id: "test-net".to_string(),
            template_id: None,
            parameters: None,
            created_by: None,
            label: None,
        };

        apply_event_to_marking(&mut marking, &event);
        assert_eq!(marking.token_count(&place_a), 1, "NetCreated should not change marking");
    }

    #[test]
    fn test_apply_net_completed_no_marking_change() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();
        let token = Token::new(TokenColor::Unit);
        marking.add_token(place_a.clone(), token);

        let event = DomainEvent::NetCompleted {
            net_id: "test-net".to_string(),
            terminal_place_id: "done".to_string(),
            exit_code: Some(serde_json::json!(0)),
        };

        apply_event_to_marking(&mut marking, &event);
        assert_eq!(marking.token_count(&place_a), 1, "NetCompleted should not change marking");
    }

    #[test]
    fn test_apply_net_cancelled_no_marking_change() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();
        let token = Token::new(TokenColor::Unit);
        marking.add_token(place_a.clone(), token);

        let event = DomainEvent::NetCancelled {
            net_id: "test-net".to_string(),
            reason: Some("test".to_string()),
            cancelled_by: Some("admin".to_string()),
        };

        apply_event_to_marking(&mut marking, &event);
        assert_eq!(marking.token_count(&place_a), 1, "NetCancelled should not change marking");
    }

    #[test]
    fn test_apply_effect_failed_tokens_not_consumed() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();

        // Add a token to place_a
        let token = Token::new(TokenColor::Unit);
        let token_id = token.id.clone();
        marking.add_token(place_a.clone(), token);
        assert_eq!(marking.token_count(&place_a), 1);

        // Apply EffectFailed with tokens_consumed=false
        let event = DomainEvent::EffectFailed {
            transition_id: TransitionId::new(),
            transition_name: Some("t1".to_string()),
            consumed_tokens: vec![(place_a.clone(), token_id)],
            produced_tokens: vec![],
            effect_handler_id: "handler".to_string(),
            error_message: "test error".to_string(),
            tokens_consumed: false,
            input_data: None,
            retryable: true,
        };

        apply_event_to_marking(&mut marking, &event);

        assert_eq!(
            marking.token_count(&place_a),
            1,
            "Token should remain in place_a (not consumed)"
        );
    }
}
