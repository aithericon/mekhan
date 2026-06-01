use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use std::collections::HashMap;

use crate::{PetriNet, PlaceId, Token, TokenColor, TokenId, TransitionId};

/// Coarse-grained outcome kind for pre-dispatch hook event-log records
/// (see `pre-dispatch-hook.md` § 9). Lives in `domain` so the event log
/// is self-describing without taking a dependency on `application` types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreDispatchOutcomeKind {
    Continue,
    Reject,
    Defer,
}

/// Per-hook entry recorded in the `PreDispatchEvaluated` event so the audit
/// trail captures the full chain trace, not just the terminal outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PreDispatchHookOutcome {
    pub hook_name: String,
    pub kind: PreDispatchOutcomeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// True if the hook errored and was treated as Continue under `fail_open`,
    /// or as Reject under fail-closed.
    pub fail_open_applied: bool,
}

/// Domain events representing all possible state changes in the Petri Net.
/// These are the "facts" that get recorded in the event log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type")]
pub enum DomainEvent {
    /// The Petri Net topology was initialized
    NetInitialized { net: PetriNet },

    /// A new token was created and placed
    TokenCreated {
        token: Token,
        place_id: PlaceId,
        /// Human-readable place name (for resource subject routing)
        #[serde(skip_serializing_if = "Option::is_none")]
        place_name: Option<String>,
        /// Workflow ID this token belongs to (for resource subject routing)
        #[serde(skip_serializing_if = "Option::is_none")]
        workflow_id: Option<uuid::Uuid>,
        /// Signal key from the external signal that caused this token creation.
        /// Present when the token was injected via a signal (executor status,
        /// human task completion, etc.). Used by the causality consumer to link
        /// the token back to its originating process via cross-links.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal_key: Option<String>,
        /// Deterministic dedup identifier for this specific event.
        /// Set by publishers of one-shot events (e.g. slurm watcher: "slurm:{job}:{status}",
        /// human result: "human:{phase}:{task_id}"); left `None` for streaming events
        /// where every emit is a distinct legitimate token.
        /// Used by both NATS `Nats-Msg-Id` dedup and the engine-level `DedupIndex`
        /// to suppress redelivery duplicates without conflating with `signal_key`
        /// (which carries lineage and is intentionally shared across stream emits).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dedup_id: Option<String>,
    },

    /// A transition was fired, consuming and producing tokens
    TransitionFired {
        transition_id: TransitionId,
        /// Human-readable transition name (for debugging/logging)
        #[serde(skip_serializing_if = "Option::is_none")]
        transition_name: Option<String>,
        /// Tokens consumed from input places: (place_id, token_id)
        consumed_tokens: Vec<(PlaceId, TokenId)>,
        /// Tokens produced in output places: (place_id, token)
        produced_tokens: Vec<(PlaceId, Token)>,
        /// Tokens read via read arcs (full data for audit trail).
        /// These tokens were NOT removed from the marking.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        read_tokens: Vec<(PlaceId, Token)>,
        /// Process step breadcrumb: if set, this transition marks the start of this step.
        /// Read by the causality consumer to project step progress.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_step_started: Option<String>,
        /// Process step breadcrumb: if set, this transition marks the completion of this step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_step_completed: Option<String>,
    },

    /// A transition was deliberately bypassed because its `transition_id` was
    /// listed in the per-run `DispatchOptions.skip_mask` (sub-phase
    /// 2.5e-γ.mekhan additive surface for research-harness ablation studies).
    ///
    /// Distinct from `TransitionFired` with empty payload: explicit shape so
    /// the cloud-layer-side honest-absence citation pool can classify
    /// "transition produced no claims because skipped" vs "transition fired
    /// and produced no claims" (different semantic outcomes for ablation
    /// scoring + visualization correctness per
    /// `project_three_use_cases_and_visualization`).
    ///
    /// Tokens consumed from input places are still removed (the transition
    /// is enabled-and-fired structurally — the skip happens in the firing
    /// path AFTER input-binding selection). Output ports receive
    /// `Token::new_unit()` defaults so downstream transitions can proceed
    /// (or fail their own input-schema validation, which is the honest
    /// outcome for ablation).
    TransitionSkipped {
        transition_id: TransitionId,
        /// Human-readable transition name (for debugging/logging).
        #[serde(skip_serializing_if = "Option::is_none")]
        transition_name: Option<String>,
        /// Tokens consumed from input places: (place_id, token_id).
        consumed_tokens: Vec<(PlaceId, TokenId)>,
        /// Default tokens produced in output places: (place_id, Token).
        /// One `Token::new_unit()` per declared output port that resolves
        /// to a place.
        produced_tokens: Vec<(PlaceId, Token)>,
        /// Why the transition was skipped. Currently always `"skip_mask"`;
        /// extensible for future skip causes (e.g., budget exhaustion,
        /// guard pre-evaluation).
        skip_reason: String,
    },

    /// A token was produced by a transition but routed to a bridge-out place.
    /// Not added to local marking — forwarded to a remote net.
    TokenBridgedOut {
        token: Token,
        source_place_id: PlaceId,
        source_place_name: String,
        target_net_id: String,
        target_place_name: String,
        transition_id: TransitionId,
        signal_key: String,
        /// Event sequence of the TransitionFired/EffectCompleted that produced this token.
        /// Used by the causality consumer to inherit process tags from consumed tokens.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        produced_by_event: Option<u64>,
        /// If set, the remote net should send replies to this place on our net (default channel).
        #[serde(skip_serializing_if = "Option::is_none")]
        reply_to_place_name: Option<String>,
        /// Named reply channels: channel_name → local_place_name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_channels: Option<std::collections::HashMap<String, String>>,
    },

    /// A token was consumed (removed from the net entirely)
    TokenConsumed {
        token_id: TokenId,
        place_id: PlaceId,
    },

    /// A token was removed from a place by external command
    TokenRemoved {
        token_id: TokenId,
        place_id: PlaceId,
        /// Reason for removal (e.g., "job cancelled", "resource destroyed")
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Correlation ID for tracing
        #[serde(skip_serializing_if = "Option::is_none")]
        correlation_id: Option<String>,
    },

    /// A token was updated in place by external command
    TokenUpdated {
        token_id: TokenId,
        place_id: PlaceId,
        /// The new token data (replaces old color)
        new_color: TokenColor,
        /// Correlation ID for tracing
        #[serde(skip_serializing_if = "Option::is_none")]
        correlation_id: Option<String>,
    },

    /// An effect transition completed (side effect executed or replayed).
    /// Mirrors TransitionFired but includes the handler ID and stored result
    /// for deterministic replay.
    EffectCompleted {
        transition_id: TransitionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        transition_name: Option<String>,
        consumed_tokens: Vec<(PlaceId, TokenId)>,
        produced_tokens: Vec<(PlaceId, Token)>,
        effect_handler_id: String,
        effect_result: serde_json::Value,
        /// Tokens read via read arcs (full data for audit trail).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        read_tokens: Vec<(PlaceId, Token)>,
        /// Process step breadcrumb: if set, this transition marks the start of this step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_step_started: Option<String>,
        /// Process step breadcrumb: if set, this transition marks the completion of this step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_step_completed: Option<String>,
    },

    /// An effect transition failed (side effect returned an error).
    /// If `tokens_consumed` is true, the error was routed to an `_error` output port
    /// (tokens consumed, error token produced). If false, tokens remain in place
    /// (audit-only event, marking unchanged).
    EffectFailed {
        transition_id: TransitionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        transition_name: Option<String>,
        consumed_tokens: Vec<(PlaceId, TokenId)>,
        produced_tokens: Vec<(PlaceId, Token)>,
        effect_handler_id: String,
        error_message: String,
        /// True when `_error` port handled the failure (tokens consumed, error token produced).
        /// False when no error port exists (tokens left in place, audit-only event).
        tokens_consumed: bool,
        /// Original input data per port (for retry patterns and event log completeness).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_data: Option<HashMap<String, serde_json::Value>>,
        /// Whether the error is retryable. Defaults to true for backward compat with old events.
        #[serde(default = "default_retryable")]
        retryable: bool,
    },

    /// An error occurred during execution
    ErrorOccurred { message: String },

    /// A transition's script was updated (hot-reload)
    TransitionScriptUpdated {
        transition_id: TransitionId,
        script: String,
        guard: Option<String>,
    },

    /// Net was created (before topology loaded). Captures creation metadata.
    NetCreated {
        net_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        template_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parameters: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        created_by: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },

    /// Net reached a terminal state (quiescent + token at terminal place).
    NetCompleted {
        net_id: String,
        terminal_place_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<serde_json::Value>,
    },

    /// Net was externally cancelled/terminated.
    NetCancelled {
        net_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cancelled_by: Option<String>,
    },

    /// A transition failed permanently and the net cannot make progress.
    /// Emitted by the eval-loop driver after the firing layer consumed the
    /// offending tokens (see `firing.rs`); the net is torn down. Distinct from
    /// `NetCompleted` (success) and `NetCancelled` (external request).
    NetFailed {
        net_id: String,
        /// The transition whose firing failed permanently.
        transition_id: TransitionId,
        /// Human-readable failure reason (the `ServiceError` display string).
        reason: String,
        /// Whether the underlying error was classified retryable. Audit only:
        /// the net fails regardless — retry is authored via an `_error` port.
        retryable: bool,
    },

    /// A pre-dispatch hook chain was evaluated for an effect transition
    /// (see `pre-dispatch-hook.md` § 9). Emitted on every dispatch attempt
    /// regardless of outcome — one event per attempted dispatch.
    PreDispatchEvaluated {
        transition_id: TransitionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        transition_name: Option<String>,
        /// One entry per hook fired in the chain, in declaration order.
        hook_chain: Vec<PreDispatchHookOutcome>,
        /// Terminal outcome that determined whether dispatch proceeded.
        final_outcome: PreDispatchOutcomeKind,
        timestamp: DateTime<Utc>,
    },

    /// A pre-dispatch hook rejected the dispatch. Emitted IN ADDITION to
    /// `PreDispatchEvaluated` so downstream consumers can subscribe to the
    /// narrower rejection signal.
    PreDispatchRejected {
        transition_id: TransitionId,
        hook_name: String,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    /// A pre-dispatch hook deferred the dispatch. Emitted IN ADDITION to
    /// `PreDispatchEvaluated`.
    PreDispatchDeferred {
        transition_id: TransitionId,
        hook_name: String,
        retry_after_ms: u64,
        /// How many times this transition has been deferred (per-(net_id,
        /// transition_id) counter — see `pre-dispatch-hook.md` § 11 trip-wire 4).
        defer_count: u32,
        timestamp: DateTime<Utc>,
    },
}

/// Default for backward-compatible deserialization of old events without `retryable`.
fn default_retryable() -> bool {
    true
}

/// A persisted event with metadata and hash chaining for audit trail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PersistedEvent {
    /// Sequential event number (monotonically increasing)
    pub sequence: u64,
    /// When this event was recorded
    pub timestamp: DateTime<Utc>,
    /// The actual domain event
    pub event: DomainEvent,
    /// SHA-256 hash of this event's content
    pub hash: String,
    /// Hash of the previous event (None for first event)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_hash: Option<String>,
}

impl PersistedEvent {
    /// Create a new persisted event with hash chaining.
    pub fn new(sequence: u64, event: DomainEvent, previous_hash: Option<String>) -> Self {
        let timestamp = Utc::now();

        // Compute hash of event content
        let hash_input = serde_json::json!({
            "sequence": sequence,
            "timestamp": timestamp.to_rfc3339(),
            "event": &event,
            "previous_hash": &previous_hash,
        });

        let mut hasher = Sha256::new();
        hasher.update(hash_input.to_string().as_bytes());
        let hash = hex::encode(hasher.finalize());

        Self {
            sequence,
            timestamp,
            event,
            hash,
            previous_hash,
        }
    }

    /// Verify that this event's hash is valid.
    pub fn verify_hash(&self) -> bool {
        let hash_input = serde_json::json!({
            "sequence": self.sequence,
            "timestamp": self.timestamp.to_rfc3339(),
            "event": &self.event,
            "previous_hash": &self.previous_hash,
        });

        let mut hasher = Sha256::new();
        hasher.update(hash_input.to_string().as_bytes());
        let computed_hash = hex::encode(hasher.finalize());

        self.hash == computed_hash
    }
}

/// Verify the integrity of an event chain.
pub fn verify_event_chain(events: &[PersistedEvent]) -> bool {
    for (i, event) in events.iter().enumerate() {
        // Verify individual hash
        if !event.verify_hash() {
            return false;
        }

        // Verify chain linkage
        if i == 0 {
            if event.previous_hash.is_some() {
                return false; // First event should have no previous hash
            }
        } else {
            let expected_prev = &events[i - 1].hash;
            match &event.previous_hash {
                Some(prev) if prev == expected_prev => {}
                _ => return false,
            }
        }

        // Verify sequence numbers
        if event.sequence != i as u64 {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TokenColor;

    #[test]
    fn test_hash_chain_integrity() {
        let event1 = PersistedEvent::new(
            0,
            DomainEvent::ErrorOccurred {
                message: "test".to_string(),
            },
            None,
        );

        let event2 = PersistedEvent::new(
            1,
            DomainEvent::ErrorOccurred {
                message: "test2".to_string(),
            },
            Some(event1.hash.clone()),
        );

        let events = vec![event1, event2];
        assert!(verify_event_chain(&events));
    }

    #[test]
    fn test_hash_verification() {
        let event = PersistedEvent::new(
            0,
            DomainEvent::ErrorOccurred {
                message: "test".to_string(),
            },
            None,
        );

        assert!(event.verify_hash());
    }

    #[test]
    fn test_broken_chain_detected() {
        let event1 = PersistedEvent::new(
            0,
            DomainEvent::ErrorOccurred {
                message: "test".to_string(),
            },
            None,
        );

        // Create event with wrong previous hash
        let event2 = PersistedEvent::new(
            1,
            DomainEvent::ErrorOccurred {
                message: "test2".to_string(),
            },
            Some("wrong_hash".to_string()),
        );

        let events = vec![event1, event2];
        assert!(!verify_event_chain(&events));
    }

    #[test]
    fn test_wrong_sequence_detected() {
        let event1 = PersistedEvent::new(
            0,
            DomainEvent::ErrorOccurred {
                message: "test".to_string(),
            },
            None,
        );

        let event2 = PersistedEvent::new(
            5, // Wrong sequence - should be 1
            DomainEvent::ErrorOccurred {
                message: "test2".to_string(),
            },
            Some(event1.hash.clone()),
        );

        let events = vec![event1, event2];
        assert!(!verify_event_chain(&events));
    }

    #[test]
    fn test_token_created_event() {
        let place_id = PlaceId::new();
        let token = Token::new(TokenColor::Integer(42));

        let event = PersistedEvent::new(
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
        );

        assert!(event.verify_hash());
        match &event.event {
            DomainEvent::TokenCreated {
                token: t,
                place_id: p,
                ..
            } => {
                assert_eq!(t.id, token.id);
                assert_eq!(*p, place_id);
            }
            _ => panic!("Expected TokenCreated event"),
        }
    }

    #[test]
    fn test_transition_fired_event() {
        let transition_id = TransitionId::new();
        let place_a = PlaceId::new();
        let place_b = PlaceId::new();
        let consumed_token = TokenId::new();
        let produced_token = Token::new(TokenColor::Unit);

        let event = PersistedEvent::new(
            0,
            DomainEvent::TransitionFired {
                transition_id: transition_id.clone(),
                transition_name: None,
                consumed_tokens: vec![(place_a.clone(), consumed_token.clone())],
                produced_tokens: vec![(place_b.clone(), produced_token.clone())],
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            None,
        );

        assert!(event.verify_hash());
        match &event.event {
            DomainEvent::TransitionFired {
                transition_id: tid,
                consumed_tokens,
                produced_tokens,
                ..
            } => {
                assert_eq!(*tid, transition_id);
                assert_eq!(consumed_tokens.len(), 1);
                assert_eq!(produced_tokens.len(), 1);
            }
            _ => panic!("Expected TransitionFired event"),
        }
    }

    #[test]
    fn test_event_serialization_roundtrip() {
        let event = PersistedEvent::new(
            0,
            DomainEvent::ErrorOccurred {
                message: "test message".to_string(),
            },
            None,
        );

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: PersistedEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(event.sequence, deserialized.sequence);
        assert_eq!(event.hash, deserialized.hash);
        assert_eq!(event.event, deserialized.event);
    }

    #[test]
    fn test_effect_completed_event() {
        let transition_id = TransitionId::new();
        let place_a = PlaceId::new();
        let place_b = PlaceId::new();
        let consumed_token = TokenId::new();
        let produced_token = Token::new(TokenColor::Data(serde_json::json!({"result": 42})));

        let event = PersistedEvent::new(
            0,
            DomainEvent::EffectCompleted {
                transition_id: transition_id.clone(),
                transition_name: Some("call_api".to_string()),
                consumed_tokens: vec![(place_a.clone(), consumed_token.clone())],
                produced_tokens: vec![(place_b.clone(), produced_token.clone())],
                effect_handler_id: "http_handler".to_string(),
                effect_result: serde_json::json!({"status": 200, "body": "ok"}),
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            None,
        );

        assert!(event.verify_hash());

        // Verify serialization roundtrip
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: PersistedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event.event, deserialized.event);

        // Verify fields
        match &deserialized.event {
            DomainEvent::EffectCompleted {
                transition_id: tid,
                effect_handler_id,
                effect_result,
                consumed_tokens,
                produced_tokens,
                ..
            } => {
                assert_eq!(*tid, transition_id);
                assert_eq!(effect_handler_id, "http_handler");
                assert_eq!(
                    effect_result,
                    &serde_json::json!({"status": 200, "body": "ok"})
                );
                assert_eq!(consumed_tokens.len(), 1);
                assert_eq!(produced_tokens.len(), 1);
            }
            _ => panic!("Expected EffectCompleted event"),
        }
    }

    #[test]
    fn test_effect_failed_backward_compat_deserialization() {
        // Simulate an old stored EffectFailed JSON without input_data or retryable fields
        let old_json = serde_json::json!({
            "type": "EffectFailed",
            "transition_id": TransitionId::new(),
            "consumed_tokens": [],
            "produced_tokens": [],
            "effect_handler_id": "old_handler",
            "error_message": "old error",
            "tokens_consumed": true
        });

        let event: DomainEvent =
            serde_json::from_value(old_json).expect("Should deserialize old format");

        match event {
            DomainEvent::EffectFailed {
                input_data,
                retryable,
                error_message,
                ..
            } => {
                assert_eq!(
                    input_data, None,
                    "Missing input_data should default to None"
                );
                assert!(retryable, "Missing retryable should default to true");
                assert_eq!(error_message, "old error");
            }
            _ => panic!("Expected EffectFailed"),
        }
    }

    #[test]
    fn test_net_created_roundtrip() {
        let event = DomainEvent::NetCreated {
            net_id: "net-1".to_string(),
            template_id: Some("template-1".to_string()),
            parameters: Some(serde_json::json!({"gpu_count": 4})),
            created_by: Some("admin".to_string()),
            label: Some("My Net".to_string()),
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: DomainEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn test_net_created_minimal_roundtrip() {
        let event = DomainEvent::NetCreated {
            net_id: "net-1".to_string(),
            template_id: None,
            parameters: None,
            created_by: None,
            label: None,
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: DomainEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn test_net_completed_roundtrip() {
        let event = DomainEvent::NetCompleted {
            net_id: "net-1".to_string(),
            terminal_place_id: "done".to_string(),
            exit_code: Some(serde_json::json!(0)),
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: DomainEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn test_net_cancelled_roundtrip() {
        let event = DomainEvent::NetCancelled {
            net_id: "net-1".to_string(),
            reason: Some("timeout".to_string()),
            cancelled_by: Some("system".to_string()),
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: DomainEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn test_lifecycle_events_in_hash_chain() {
        let e1 = PersistedEvent::new(
            0,
            DomainEvent::NetCreated {
                net_id: "net-1".to_string(),
                template_id: None,
                parameters: None,
                created_by: None,
                label: None,
            },
            None,
        );
        assert!(e1.verify_hash());

        let e2 = PersistedEvent::new(
            1,
            DomainEvent::NetCompleted {
                net_id: "net-1".to_string(),
                terminal_place_id: "done".to_string(),
                exit_code: Some(serde_json::json!(42)),
            },
            Some(e1.hash.clone()),
        );
        assert!(e2.verify_hash());

        let e3 = PersistedEvent::new(
            2,
            DomainEvent::NetCancelled {
                net_id: "net-2".to_string(),
                reason: Some("user request".to_string()),
                cancelled_by: Some("admin".to_string()),
            },
            Some(e2.hash.clone()),
        );
        assert!(e3.verify_hash());

        assert!(verify_event_chain(&[e1, e2, e3]));
    }

    #[test]
    fn test_effect_failed_with_new_fields_roundtrip() {
        let mut input_data = std::collections::HashMap::new();
        input_data.insert(
            "request".to_string(),
            serde_json::json!({"url": "http://example.com"}),
        );

        let event = DomainEvent::EffectFailed {
            transition_id: TransitionId::new(),
            transition_name: Some("call_api".to_string()),
            consumed_tokens: vec![],
            produced_tokens: vec![],
            effect_handler_id: "http".to_string(),
            error_message: "timeout".to_string(),
            tokens_consumed: true,
            input_data: Some(input_data),
            retryable: false,
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: DomainEvent = serde_json::from_str(&json).unwrap();

        match deserialized {
            DomainEvent::EffectFailed {
                input_data,
                retryable,
                ..
            } => {
                assert!(!retryable, "retryable=false should survive roundtrip");
                let data = input_data.expect("input_data should survive roundtrip");
                assert_eq!(
                    data["request"],
                    serde_json::json!({"url": "http://example.com"})
                );
            }
            _ => panic!("Expected EffectFailed"),
        }
    }
}
