//! Mock StateProjection implementation for testing.

use petri_application::{apply_event_to_marking, StateProjection};
use petri_domain::{Marking, PersistedEvent};

/// Simple state projection that rebuilds marking from events.
///
/// This is a working implementation (not just a stub) that can be used
/// in integration tests where a real projection is needed.
pub struct MockStateProjection;

impl MockStateProjection {
    /// Create a new mock projection.
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockStateProjection {
    fn default() -> Self {
        Self::new()
    }
}

impl StateProjection for MockStateProjection {
    fn project(&self, events: &[PersistedEvent]) -> Marking {
        let mut marking = Marking::new();
        for persisted in events {
            apply_event_to_marking(&mut marking, &persisted.event);
        }
        marking
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::{DomainEvent, PlaceId, Token, TokenColor};

    #[test]
    fn test_empty_events_empty_marking() {
        let projection = MockStateProjection::new();
        let marking = projection.project(&[]);
        assert!(marking.tokens.is_empty());
    }

    #[test]
    fn test_token_created() {
        let projection = MockStateProjection::new();
        let place_id = PlaceId::new();
        let token = Token::new(TokenColor::Unit);

        let event = PersistedEvent::new(
            0,
            DomainEvent::TokenCreated {
                place_id: place_id.clone(),
                token: token.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            },
            None,
        );

        let marking = projection.project(&[event]);
        assert_eq!(marking.token_count(&place_id), 1);
    }

    #[test]
    fn test_token_consumed() {
        let projection = MockStateProjection::new();
        let place_id = PlaceId::new();
        let token = Token::new(TokenColor::Unit);
        let token_id = token.id.clone();

        let create_event = PersistedEvent::new(
            0,
            DomainEvent::TokenCreated {
                place_id: place_id.clone(),
                token,
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            },
            None,
        );

        let consume_event = PersistedEvent::new(
            1,
            DomainEvent::TokenConsumed {
                place_id: place_id.clone(),
                token_id,
            },
            Some(create_event.hash.clone()),
        );

        let marking = projection.project(&[create_event, consume_event]);
        assert_eq!(marking.token_count(&place_id), 0);
    }
}
