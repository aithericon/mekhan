use petri_application::{apply_event_to_marking, StateProjection};
use petri_domain::{Marking, PersistedEvent};

/// Computes the current marking by replaying events.
pub struct MarkingProjection;

impl MarkingProjection {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MarkingProjection {
    fn default() -> Self {
        Self::new()
    }
}

impl StateProjection for MarkingProjection {
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
    fn test_project_token_creation() {
        let projection = MarkingProjection::new();

        let place_id = PlaceId::new();
        let token = Token::new(TokenColor::Unit);

        let events = vec![PersistedEvent::new(
            0,
            DomainEvent::TokenCreated {
                token: token.clone(),
                place_id: place_id.clone(),
                place_name: None,
                workflow_id: None,
                    signal_key: None,
                    dedup_id: None,
            },
            None,
        )];

        let marking = projection.project(&events);

        assert_eq!(marking.token_count(&place_id), 1);
        assert_eq!(marking.tokens_at(&place_id)[0].id, token.id);
    }

    #[test]
    fn test_project_effect_completed() {
        let projection = MarkingProjection::new();

        let place_a = PlaceId::new();
        let place_b = PlaceId::new();
        let token_a = Token::new(TokenColor::Unit);
        let token_b = Token::new(TokenColor::Data(serde_json::json!({"result": "ok"})));

        let events = vec![
            PersistedEvent::new(
                0,
                DomainEvent::TokenCreated {
                    token: token_a.clone(),
                    place_id: place_a.clone(),
                    place_name: None,
                    workflow_id: None,
                    signal_key: None,
                    dedup_id: None,
                },
                None,
            ),
            PersistedEvent::new(
                1,
                DomainEvent::EffectCompleted {
                    transition_id: petri_domain::TransitionId::new(),
                    transition_name: Some("effect_t".to_string()),
                    consumed_tokens: vec![(place_a.clone(), token_a.id.clone())],
                    produced_tokens: vec![(place_b.clone(), token_b.clone())],
                    effect_handler_id: "test_handler".to_string(),
                    effect_result: serde_json::json!({"status": "ok"}),
                    read_tokens: vec![],
                    process_step_started: None,
                    process_step_completed: None,
                },
                Some("prev_hash".to_string()),
            ),
        ];

        let marking = projection.project(&events);

        assert_eq!(marking.token_count(&place_a), 0);
        assert_eq!(marking.token_count(&place_b), 1);
    }

    #[test]
    fn test_project_transition_firing() {
        let projection = MarkingProjection::new();

        let place_a = PlaceId::new();
        let place_b = PlaceId::new();
        let token_a = Token::new(TokenColor::Unit);
        let token_b = Token::new(TokenColor::Unit);

        let events = vec![
            PersistedEvent::new(
                0,
                DomainEvent::TokenCreated {
                    token: token_a.clone(),
                    place_id: place_a.clone(),
                    place_name: None,
                    workflow_id: None,
                    signal_key: None,
                    dedup_id: None,
                },
                None,
            ),
            PersistedEvent::new(
                1,
                DomainEvent::TransitionFired {
                    transition_id: petri_domain::TransitionId::new(),
                    transition_name: None,
                    consumed_tokens: vec![(place_a.clone(), token_a.id.clone())],
                    produced_tokens: vec![(place_b.clone(), token_b.clone())],
                    read_tokens: vec![],
                    process_step_started: None,
                    process_step_completed: None,
                },
                Some("prev_hash".to_string()),
            ),
        ];

        let marking = projection.project(&events);

        assert_eq!(marking.token_count(&place_a), 0);
        assert_eq!(marking.token_count(&place_b), 1);
    }
}
