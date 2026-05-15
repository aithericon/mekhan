use petri_domain::{PlaceId, TokenId, TransitionId};
use thiserror::Error;

use crate::EventStoreError;

#[derive(Error, Debug, Clone)]
pub enum ServiceError {
    #[error("Transition not found: {0}")]
    TransitionNotFound(TransitionId),

    #[error("Place not found: {0}")]
    PlaceNotFound(PlaceId),

    #[error("Token not found: {0} in place {1}")]
    TokenNotFound(TokenId, PlaceId),

    #[error("Invalid operation: {0}")]
    InvalidOperation(String),

    #[error("Transition {id} is not enabled: {reason}")]
    TransitionNotEnabled { id: TransitionId, reason: String },

    #[error("Insufficient tokens at place {place_id}: required {required}, available {available}")]
    InsufficientTokens {
        place_id: PlaceId,
        required: usize,
        available: usize,
    },

    #[error("Guard condition not satisfied for transition {0}")]
    GuardNotSatisfied(TransitionId),

    #[error("Script error in {script_type}: {message}")]
    ScriptError {
        script_type: String,
        message: String,
    },

    #[error("Unknown output port '{port_name}' returned by script")]
    UnknownOutputPort { port_name: String },

    #[error("No arc connected to output port '{port_name}'")]
    NoArcForPort { port_name: String },

    #[error("No topology loaded")]
    NoTopology,

    #[error("Effect handler '{handler_id}' failed for transition {transition_id}: {message}")]
    EffectFailed {
        transition_id: TransitionId,
        handler_id: String,
        message: String,
    },

    #[error(
        "Schema validation failed on port '{port_name}' of transition {transition_id}: {error}"
    )]
    SchemaValidationFailed {
        port_name: String,
        transition_id: TransitionId,
        error: String,
    },

    #[error("Bridge reply routing failed at place '{place_name}': no matching reply address{}", channel.as_ref().map(|c| format!(" for channel '{}'", c)).unwrap_or_default())]
    BridgeReplyMissing {
        place_name: String,
        channel: Option<String>,
    },

    #[error("Effect handler contract mismatch: {0}")]
    EffectContractError(String),

    #[error("Secret resolution failed for transition {transition_id}: {message}")]
    SecretResolutionFailed {
        transition_id: TransitionId,
        message: String,
    },

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Event store error: {0}")]
    EventStore(#[from] EventStoreError),

    /// Pre-dispatch hook rejected the dispatch — marking unchanged, transition
    /// will be retried on the next eval pass (see `pre-dispatch-hook.md` § 4).
    #[error("Pre-dispatch rejected transition {transition_id} by hook '{hook_name}': {reason}")]
    PreDispatchRejected {
        transition_id: TransitionId,
        hook_name: String,
        reason: String,
    },

    /// Pre-dispatch hook deferred the dispatch — marking unchanged, transition
    /// will be retried after `retry_after_ms` (subject to the defer budget).
    #[error(
        "Pre-dispatch deferred transition {transition_id} by hook '{hook_name}' for {retry_after_ms}ms"
    )]
    PreDispatchDeferred {
        transition_id: TransitionId,
        hook_name: String,
        retry_after_ms: u64,
    },
}

impl ServiceError {
    /// Returns the appropriate HTTP status code for this error variant.
    pub fn status_code(&self) -> u16 {
        match self {
            // Not found
            Self::TransitionNotFound(_) | Self::PlaceNotFound(_) | Self::TokenNotFound(_, _) => 404,
            // Bad request (client errors)
            Self::InsufficientTokens { .. }
            | Self::GuardNotSatisfied(_)
            | Self::TransitionNotEnabled { .. }
            | Self::InvalidOperation(_)
            | Self::UnknownOutputPort { .. }
            | Self::NoArcForPort { .. }
            | Self::ScriptError { .. }
            | Self::SchemaValidationFailed { .. }
            | Self::EffectContractError(_)
            | Self::BridgeReplyMissing { .. } => 400,
            // Conflict / precondition
            Self::NoTopology => 409,
            // Pre-dispatch soft outcomes: 409 (conflict-style, non-destructive,
            // retry-eligible) keeps them distinct from 4xx user-input errors.
            Self::PreDispatchRejected { .. } | Self::PreDispatchDeferred { .. } => 409,
            // Internal server errors
            Self::EffectFailed { .. }
            | Self::SecretResolutionFailed { .. }
            | Self::Internal(_)
            | Self::EventStore(_) => 500,
        }
    }
}
