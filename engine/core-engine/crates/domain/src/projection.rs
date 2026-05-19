//! Marking projection — replay domain events to compute token distribution.
//!
//! This is pure domain logic: it only depends on domain types (Marking,
//! DomainEvent, Token, PlaceId, etc.). Lives here so any crate that depends
//! on `petri-domain` can project markings without pulling in the full
//! application layer.

use crate::{DomainEvent, Marking, PersistedEvent};

/// Project a marking from a sequence of persisted events.
///
/// Replays events in order to compute the current token distribution.
pub fn project_marking(events: &[PersistedEvent]) -> Marking {
    let mut marking = Marking::new();
    for event in events {
        apply_event_to_marking(&mut marking, &event.event);
    }
    marking
}

/// Apply a single domain event to a marking.
///
/// This is the shared event-application logic used by both full replay
/// (`project_marking`) and incremental updates.
pub fn apply_event_to_marking(marking: &mut Marking, event: &DomainEvent) {
    match event {
        DomainEvent::TokenCreated {
            token, place_id, ..
        } => {
            marking.add_token(place_id.clone(), token.clone());
        }
        DomainEvent::TransitionFired {
            consumed_tokens,
            produced_tokens,
            ..
        }
        | DomainEvent::EffectCompleted {
            consumed_tokens,
            produced_tokens,
            ..
        } => {
            for (place_id, token_id) in consumed_tokens {
                marking.remove_token(place_id, token_id);
            }
            for (place_id, token) in produced_tokens {
                marking.add_token(place_id.clone(), token.clone());
            }
        }
        DomainEvent::TokenConsumed { place_id, token_id } => {
            marking.remove_token(place_id, token_id);
        }
        DomainEvent::TokenRemoved {
            place_id, token_id, ..
        } => {
            marking.remove_token(place_id, token_id);
        }
        DomainEvent::TokenUpdated {
            place_id,
            token_id,
            new_color,
            ..
        } => {
            marking.update_token(place_id, token_id, new_color.clone());
        }
        DomainEvent::EffectFailed {
            consumed_tokens,
            produced_tokens,
            tokens_consumed,
            ..
        } => {
            if *tokens_consumed {
                for (place_id, token_id) in consumed_tokens {
                    marking.remove_token(place_id, token_id);
                }
                for (place_id, token) in produced_tokens {
                    marking.add_token(place_id.clone(), token.clone());
                }
            }
            // !tokens_consumed → marking unchanged (audit-only event)
        }
        // These events don't affect marking
        DomainEvent::NetInitialized { .. }
        | DomainEvent::ErrorOccurred { .. }
        | DomainEvent::TransitionScriptUpdated { .. }
        | DomainEvent::TokenBridgedOut { .. }
        | DomainEvent::NetCreated { .. }
        | DomainEvent::NetCompleted { .. }
        | DomainEvent::NetCancelled { .. }
        | DomainEvent::NetFailed { .. }
        // Pre-dispatch hook events are audit-only — Reject/Defer outcomes
        // are non-destructive w.r.t. marking, and Continue's marking
        // effect is captured by the subsequent EffectCompleted event.
        | DomainEvent::PreDispatchEvaluated { .. }
        | DomainEvent::PreDispatchRejected { .. }
        | DomainEvent::PreDispatchDeferred { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PlaceId, Token, TokenColor, TransitionId};

    #[test]
    fn test_apply_token_created() {
        let mut marking = Marking::new();
        let place = PlaceId::new();
        let token = Token::new(TokenColor::Unit);

        apply_event_to_marking(
            &mut marking,
            &DomainEvent::TokenCreated {
                token: token.clone(),
                place_id: place.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            },
        );

        assert_eq!(marking.token_count(&place), 1);
    }

    #[test]
    fn test_apply_effect_failed_tokens_consumed() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();
        let place_b = PlaceId::new();

        let token = Token::new(TokenColor::Unit);
        let token_id = token.id.clone();
        marking.add_token(place_a.clone(), token);

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

        assert_eq!(marking.token_count(&place_a), 0);
        assert_eq!(marking.token_count(&place_b), 1);
    }

    #[test]
    fn test_apply_effect_failed_tokens_not_consumed() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();

        let token = Token::new(TokenColor::Unit);
        let token_id = token.id.clone();
        marking.add_token(place_a.clone(), token);

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

        assert_eq!(marking.token_count(&place_a), 1);
    }

    #[test]
    fn test_lifecycle_events_no_marking_change() {
        let mut marking = Marking::new();
        let place = PlaceId::new();
        marking.add_token(place.clone(), Token::new(TokenColor::Unit));

        for event in [
            DomainEvent::NetCreated {
                net_id: "test".into(),
                template_id: None,
                parameters: None,
                created_by: None,
                label: None,
            },
            DomainEvent::NetCompleted {
                net_id: "test".into(),
                terminal_place_id: "done".into(),
                exit_code: None,
            },
            DomainEvent::NetCancelled {
                net_id: "test".into(),
                reason: None,
                cancelled_by: None,
            },
        ] {
            apply_event_to_marking(&mut marking, &event);
            assert_eq!(marking.token_count(&place), 1);
        }
    }
}
