//! NATS subject naming conventions.
//!
//! Subject hierarchy:
//! ```text
//! petri.events.>                    # All events (wildcard)
//! petri.events.net.initialized      # Net topology loaded
//! petri.events.token.created        # Token injected into place
//! petri.events.token.consumed       # Token removed from place
//! petri.events.transition.fired     # Transition executed
//! petri.events.transition.updated   # Script hot-reloaded
//! petri.events.error                # Error occurred
//!
//! petri.commands.inject.token       # Token injection requests
//!
//! petri.bridge.{target_net_id}.{target_place_name}  # Cross-net bridge transfers
//! petri.signal.{target_net_id}.{target_place_name}  # External system signals
//! ```

use petri_domain::DomainEvent;

/// Workflow context for dynamic subject routing.
///
/// Contains the workflow ID and metadata needed to route events
/// to the correct NATS subjects.
#[derive(Clone, Debug)]
pub struct WorkflowContext {
    /// Unique workflow identifier (UUID)
    pub workflow_id: uuid::Uuid,
    /// Human-readable workflow name (for logging/debugging)
    pub workflow_name: String,
}

impl WorkflowContext {
    /// Create a new workflow context.
    pub fn new(workflow_id: uuid::Uuid, workflow_name: impl Into<String>) -> Self {
        Self {
            workflow_id,
            workflow_name: workflow_name.into(),
        }
    }

    /// Generate a new workflow context with a random UUID.
    pub fn generate(workflow_name: impl Into<String>) -> Self {
        Self {
            workflow_id: uuid::Uuid::new_v4(),
            workflow_name: workflow_name.into(),
        }
    }
}

/// NATS subject constants and utilities.
pub struct Subjects;

impl Subjects {
    // ==================== Event Subjects ====================

    /// Base prefix for all event subjects
    pub const EVENTS_PREFIX: &'static str = "petri.events";

    /// Wildcard for subscribing to all events
    pub const EVENTS_ALL: &'static str = "petri.events.>";

    /// Net initialized event
    pub const EVENT_NET_INITIALIZED: &'static str = "petri.events.net.initialized";

    /// Token created event
    pub const EVENT_TOKEN_CREATED: &'static str = "petri.events.token.created";

    /// Token consumed event
    pub const EVENT_TOKEN_CONSUMED: &'static str = "petri.events.token.consumed";

    /// Token removed event (external removal)
    pub const EVENT_TOKEN_REMOVED: &'static str = "petri.events.token.removed";

    /// Token updated event (external update)
    pub const EVENT_TOKEN_UPDATED: &'static str = "petri.events.token.updated";

    /// Transition fired event
    pub const EVENT_TRANSITION_FIRED: &'static str = "petri.events.transition.fired";

    /// Transition script updated event
    pub const EVENT_TRANSITION_UPDATED: &'static str = "petri.events.transition.updated";

    /// Token bridged out event (forwarded to remote net)
    pub const EVENT_TOKEN_BRIDGED_OUT: &'static str = "petri.events.token.bridged_out";

    /// Effect transition completed event
    pub const EVENT_EFFECT_COMPLETED: &'static str = "petri.events.effect.completed";

    /// Effect transition failed event
    pub const EVENT_EFFECT_FAILED: &'static str = "petri.events.effect.failed";

    /// Error event
    pub const EVENT_ERROR: &'static str = "petri.events.error";

    // ==================== Lifecycle Event Subjects ====================

    /// Net created event
    pub const EVENT_NET_CREATED: &'static str = "petri.events.net.created";

    /// Net completed event (terminal state reached)
    pub const EVENT_NET_COMPLETED: &'static str = "petri.events.net.completed";

    /// Net cancelled event
    pub const EVENT_NET_CANCELLED: &'static str = "petri.events.net.cancelled";

    // ==================== Command Subjects ====================

    /// Base prefix for all command subjects
    pub const COMMANDS_PREFIX: &'static str = "petri.commands";

    /// Token injection command
    pub const COMMAND_INJECT_TOKEN: &'static str = "petri.commands.inject.token";

    /// Token removal command
    pub const COMMAND_REMOVE_TOKEN: &'static str = "petri.commands.remove.token";

    /// Token update command
    pub const COMMAND_UPDATE_TOKEN: &'static str = "petri.commands.update.token";

    /// Create net command
    pub const COMMAND_CREATE_NET: &'static str = "petri.commands.create_net";

    // ==================== Human Task Subjects ====================
    //
    // Human task subjects use a separate prefix (not petri.) to avoid
    // overlap with the PETRI_GLOBAL stream that captures all petri.> subjects.

    /// Prefix for human task requests
    pub const HUMAN_REQUEST_PREFIX: &'static str = "human.request";

    /// Prefix for human task completions
    pub const HUMAN_COMPLETED_PREFIX: &'static str = "human.completed";

    /// Prefix for human task cancel requests (engine -> UI)
    pub const HUMAN_CANCEL_PREFIX: &'static str = "human.cancel";

    /// Prefix for human task cancel confirmations (UI -> engine)
    pub const HUMAN_CANCELLED_PREFIX: &'static str = "human.cancelled";

    /// Prefix for human task failure signals (UI -> engine)
    pub const HUMAN_FAILED_PREFIX: &'static str = "human.failed";

    /// Stream name for human task cancel requests
    pub const STREAM_HUMAN_CANCEL: &'static str = "HUMAN_CANCEL";

    /// Stream name for human task cancel confirmations
    pub const STREAM_HUMAN_CANCELLED: &'static str = "HUMAN_CANCELLED";

    /// Stream name for human task failures
    pub const STREAM_HUMAN_FAILED: &'static str = "HUMAN_FAILED";

    /// Build a human task request subject.
    pub fn human_request(net_id: &str, place_name: &str) -> String {
        format!("{}.{}.{}", Self::HUMAN_REQUEST_PREFIX, net_id, place_name)
    }

    /// Build a human task completion filter.
    pub fn human_completed_filter(net_id: &str) -> String {
        format!("{}.{}.>", Self::HUMAN_COMPLETED_PREFIX, net_id)
    }

    /// Build a human task cancel subject.
    pub fn human_cancel(net_id: &str, place_name: &str) -> String {
        format!("{}.{}.{}", Self::HUMAN_CANCEL_PREFIX, net_id, place_name)
    }

    /// Build a human task cancelled filter (for engine-side consumer).
    pub fn human_cancelled_filter(net_id: &str) -> String {
        format!("{}.{}.>", Self::HUMAN_CANCELLED_PREFIX, net_id)
    }

    /// Build a human task failed filter (for engine-side consumer).
    pub fn human_failed_filter(net_id: &str) -> String {
        format!("{}.{}.>", Self::HUMAN_FAILED_PREFIX, net_id)
    }

    /// Parse a human.completed subject into (net_id, place_name).
    pub fn parse_human_completed_subject(subject: &str) -> Option<(&str, &str)> {
        let parts: Vec<&str> = subject.split('.').collect();
        if parts.len() == 4 && parts[0] == "human" && parts[1] == "completed" {
            Some((parts[2], parts[3]))
        } else {
            None
        }
    }

    /// Parse a human.cancelled subject into (net_id, place_name).
    pub fn parse_human_cancelled_subject(subject: &str) -> Option<(&str, &str)> {
        let parts: Vec<&str> = subject.split('.').collect();
        if parts.len() == 4 && parts[0] == "human" && parts[1] == "cancelled" {
            Some((parts[2], parts[3]))
        } else {
            None
        }
    }

    /// Parse a human.failed subject into (net_id, place_name).
    pub fn parse_human_failed_subject(subject: &str) -> Option<(&str, &str)> {
        let parts: Vec<&str> = subject.split('.').collect();
        if parts.len() == 4 && parts[0] == "human" && parts[1] == "failed" {
            Some((parts[2], parts[3]))
        } else {
            None
        }
    }

    // ==================== External Signal Subjects ====================
    //
    // Signals from external systems (Nomad, Slurm, K8s, webhooks).
    //
    // Subject hierarchy:
    //   petri.signal.{target_net_id}.{target_place_name}

    /// Prefix for external signal subjects
    pub const SIGNAL_PREFIX: &'static str = "petri.signal";

    /// Build a signal subject for publishing to a net's place.
    ///
    /// # Example
    /// ```
    /// use petri_nats::Subjects;
    ///
    /// let subject = Subjects::signal_transfer("gpu-resource", "status_inbox");
    /// assert_eq!(subject, "petri.signal.gpu-resource.status_inbox");
    /// ```
    pub fn signal_transfer(target_net_id: &str, target_place_name: &str) -> String {
        format!(
            "{}.{}.{}",
            Self::SIGNAL_PREFIX,
            target_net_id,
            target_place_name
        )
    }

    /// Build a subscription filter for all signal messages targeting this net.
    ///
    /// # Example
    /// ```
    /// use petri_nats::Subjects;
    ///
    /// let filter = Subjects::signal_inbox_filter("gpu-resource");
    /// assert_eq!(filter, "petri.signal.gpu-resource.>");
    /// ```
    pub fn signal_inbox_filter(own_net_id: &str) -> String {
        format!("{}.{}.>", Self::SIGNAL_PREFIX, own_net_id)
    }

    /// Parse a signal subject into (target_net_id, target_place_name).
    ///
    /// Returns `None` if the subject does not match the signal pattern.
    ///
    /// # Example
    /// ```
    /// use petri_nats::Subjects;
    ///
    /// let parsed = Subjects::parse_signal_subject("petri.signal.gpu-resource.status_inbox");
    /// assert_eq!(parsed, Some(("gpu-resource", "status_inbox")));
    /// ```
    pub fn parse_signal_subject(subject: &str) -> Option<(&str, &str)> {
        let parts: Vec<&str> = subject.split('.').collect();
        if parts.len() == 4 && parts[0] == "petri" && parts[1] == "signal" {
            Some((parts[2], parts[3]))
        } else {
            None
        }
    }

    // ==================== Cross-Net Bridge Subjects ====================
    //
    // Token transfer between separate Petri net engine instances.
    //
    // Subject hierarchy:
    //   petri.bridge.{target_net_id}.{target_place_name}

    /// Prefix for cross-net bridge subjects
    pub const BRIDGE_PREFIX: &'static str = "petri.bridge";

    /// Build a bridge transfer subject for sending a token to a remote net's place.
    ///
    /// # Example
    /// ```
    /// use petri_nats::Subjects;
    ///
    /// let subject = Subjects::bridge_transfer("net-b", "inbox");
    /// assert_eq!(subject, "petri.bridge.net-b.inbox");
    /// ```
    pub fn bridge_transfer(target_net_id: &str, target_place_name: &str) -> String {
        format!(
            "{}.{}.{}",
            Self::BRIDGE_PREFIX,
            target_net_id,
            target_place_name
        )
    }

    /// Build a subscription filter for all bridge messages targeting this net.
    ///
    /// # Example
    /// ```
    /// use petri_nats::Subjects;
    ///
    /// let filter = Subjects::bridge_inbox_filter("net-b");
    /// assert_eq!(filter, "petri.bridge.net-b.>");
    /// ```
    pub fn bridge_inbox_filter(own_net_id: &str) -> String {
        format!("{}.{}.>", Self::BRIDGE_PREFIX, own_net_id)
    }

    /// Parse a bridge subject into (target_net_id, target_place_name).
    ///
    /// Returns `None` if the subject does not match the bridge pattern.
    ///
    /// # Example
    /// ```
    /// use petri_nats::Subjects;
    ///
    /// let parsed = Subjects::parse_bridge_subject("petri.bridge.net-b.inbox");
    /// assert_eq!(parsed, Some(("net-b", "inbox")));
    /// ```
    pub fn parse_bridge_subject(subject: &str) -> Option<(&str, &str)> {
        let parts: Vec<&str> = subject.split('.').collect();
        if parts.len() == 4 && parts[0] == "petri" && parts[1] == "bridge" {
            Some((parts[2], parts[3]))
        } else {
            None
        }
    }

    // ==================== JetStream Streams ====================

    /// Single global stream for ALL petri events (single stream architecture).
    /// All publishers write to this stream via `petri.>` subjects.
    /// Consumers use filter_subject to receive only relevant messages.
    pub const STREAM_GLOBAL: &'static str = "PETRI_GLOBAL";

    // ==================== Utilities ====================

    /// Get the appropriate subject for a domain event, optionally scoped to a net.
    pub fn for_event(event: &DomainEvent, net_id: Option<&str>) -> String {
        let suffix = match event {
            DomainEvent::NetInitialized { .. } => "net.initialized",
            DomainEvent::TokenCreated { .. } => "token.created",
            DomainEvent::TokenConsumed { .. } => "token.consumed",
            DomainEvent::TokenRemoved { .. } => "token.removed",
            DomainEvent::TokenUpdated { .. } => "token.updated",
            DomainEvent::TransitionFired { .. } => "transition.fired",
            DomainEvent::TransitionSkipped { .. } => "transition.skipped",
            DomainEvent::TransitionScriptUpdated { .. } => "transition.updated",
            DomainEvent::ErrorOccurred { .. } => "error",
            DomainEvent::TokenBridgedOut { .. } => "token.bridged_out",
            DomainEvent::EffectCompleted { .. } => "effect.completed",
            DomainEvent::EffectFailed { .. } => "effect.failed",
            DomainEvent::NetCreated { .. } => "net.created",
            DomainEvent::NetCompleted { .. } => "net.completed",
            DomainEvent::NetCancelled { .. } => "net.cancelled",
            DomainEvent::NetFailed { .. } => "net.failed",
            DomainEvent::PreDispatchEvaluated { .. } => "pre_dispatch.evaluated",
            DomainEvent::PreDispatchRejected { .. } => "pre_dispatch.rejected",
            DomainEvent::PreDispatchDeferred { .. } => "pre_dispatch.deferred",
        };

        match net_id {
            Some(id) => format!("{}.{}.{}", Self::EVENTS_PREFIX, id, suffix),
            None => format!("{}.{}", Self::EVENTS_PREFIX, suffix),
        }
    }

    /// Get a human-readable event type name.
    pub fn event_type_name(event: &DomainEvent) -> &'static str {
        match event {
            DomainEvent::NetInitialized { .. } => "net.initialized",
            DomainEvent::TokenCreated { .. } => "token.created",
            DomainEvent::TokenConsumed { .. } => "token.consumed",
            DomainEvent::TokenRemoved { .. } => "token.removed",
            DomainEvent::TokenUpdated { .. } => "token.updated",
            DomainEvent::TransitionFired { .. } => "transition.fired",
            DomainEvent::TransitionSkipped { .. } => "transition.skipped",
            DomainEvent::TransitionScriptUpdated { .. } => "transition.updated",
            DomainEvent::ErrorOccurred { .. } => "error",
            DomainEvent::TokenBridgedOut { .. } => "token.bridged_out",
            DomainEvent::EffectCompleted { .. } => "effect.completed",
            DomainEvent::EffectFailed { .. } => "effect.failed",
            DomainEvent::NetCreated { .. } => "net.created",
            DomainEvent::NetCompleted { .. } => "net.completed",
            DomainEvent::NetCancelled { .. } => "net.cancelled",
            DomainEvent::NetFailed { .. } => "net.failed",
            DomainEvent::PreDispatchEvaluated { .. } => "pre_dispatch.evaluated",
            DomainEvent::PreDispatchRejected { .. } => "pre_dispatch.rejected",
            DomainEvent::PreDispatchDeferred { .. } => "pre_dispatch.deferred",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::{PlaceId, Token, TokenColor};

    #[test]
    fn test_subject_for_token_created() {
        let event = DomainEvent::TokenCreated {
            token: Token::new(TokenColor::Unit),
            place_id: PlaceId::new(),
            place_name: None,
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        };
        assert_eq!(
            Subjects::for_event(&event, None),
            "petri.events.token.created"
        );
        assert_eq!(
            Subjects::for_event(&event, Some("net-a")),
            "petri.events.net-a.token.created"
        );
    }

    #[test]
    fn test_subject_for_error() {
        let event = DomainEvent::ErrorOccurred {
            message: "test".to_string(),
        };
        assert_eq!(Subjects::for_event(&event, None), "petri.events.error");
    }

    // ==================== Cross-Net Bridge Subject Tests ====================

    #[test]
    fn test_bridge_transfer_subject() {
        let subject = Subjects::bridge_transfer("net-b", "inbox");
        assert_eq!(subject, "petri.bridge.net-b.inbox");
    }

    #[test]
    fn test_bridge_inbox_filter() {
        let filter = Subjects::bridge_inbox_filter("net-b");
        assert_eq!(filter, "petri.bridge.net-b.>");
    }

    #[test]
    fn test_parse_bridge_subject_valid() {
        let parsed = Subjects::parse_bridge_subject("petri.bridge.net-b.inbox");
        assert_eq!(parsed, Some(("net-b", "inbox")));
    }

    #[test]
    fn test_parse_bridge_subject_invalid_prefix() {
        let parsed = Subjects::parse_bridge_subject("invalid.bridge.net-b.inbox");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_parse_bridge_subject_wrong_length() {
        let parsed = Subjects::parse_bridge_subject("petri.bridge.net-b");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_parse_bridge_subject_too_many_parts() {
        let parsed = Subjects::parse_bridge_subject("petri.bridge.net-b.inbox.extra");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_roundtrip_bridge_subject() {
        let subject = Subjects::bridge_transfer("my-net", "my-place");
        let parsed = Subjects::parse_bridge_subject(&subject);
        assert_eq!(parsed, Some(("my-net", "my-place")));
    }

    // ==================== External Signal Subject Tests ====================

    #[test]
    fn test_signal_transfer_subject() {
        let subject = Subjects::signal_transfer("gpu-resource", "status_inbox");
        assert_eq!(subject, "petri.signal.gpu-resource.status_inbox");
    }

    #[test]
    fn test_signal_inbox_filter() {
        let filter = Subjects::signal_inbox_filter("gpu-resource");
        assert_eq!(filter, "petri.signal.gpu-resource.>");
    }

    #[test]
    fn test_parse_signal_subject_valid() {
        let parsed = Subjects::parse_signal_subject("petri.signal.gpu-resource.status_inbox");
        assert_eq!(parsed, Some(("gpu-resource", "status_inbox")));
    }

    #[test]
    fn test_parse_signal_subject_invalid_prefix() {
        let parsed = Subjects::parse_signal_subject("invalid.signal.gpu-resource.inbox");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_parse_signal_subject_wrong_length() {
        let parsed = Subjects::parse_signal_subject("petri.signal.gpu-resource");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_roundtrip_signal_subject() {
        let subject = Subjects::signal_transfer("my-net", "my-place");
        let parsed = Subjects::parse_signal_subject(&subject);
        assert_eq!(parsed, Some(("my-net", "my-place")));
    }

    // ==================== Human Task Subject Tests ====================

    #[test]
    fn test_human_request_subject() {
        let subject = Subjects::human_request("net-a", "review");
        assert_eq!(subject, "human.request.net-a.review");
    }

    #[test]
    fn test_human_completed_filter() {
        let filter = Subjects::human_completed_filter("net-a");
        assert_eq!(filter, "human.completed.net-a.>");
    }

    #[test]
    fn test_human_cancel_subject() {
        let subject = Subjects::human_cancel("net-a", "review");
        assert_eq!(subject, "human.cancel.net-a.review");
    }

    #[test]
    fn test_human_cancelled_filter() {
        let filter = Subjects::human_cancelled_filter("net-a");
        assert_eq!(filter, "human.cancelled.net-a.>");
    }

    #[test]
    fn test_human_failed_filter() {
        let filter = Subjects::human_failed_filter("net-a");
        assert_eq!(filter, "human.failed.net-a.>");
    }

    #[test]
    fn test_parse_human_completed_subject_valid() {
        let parsed = Subjects::parse_human_completed_subject("human.completed.net-a.review");
        assert_eq!(parsed, Some(("net-a", "review")));
    }

    #[test]
    fn test_parse_human_completed_subject_invalid() {
        assert!(Subjects::parse_human_completed_subject("human.request.net-a.review").is_none());
        assert!(Subjects::parse_human_completed_subject("human.completed.net-a").is_none());
        assert!(
            Subjects::parse_human_completed_subject("human.completed.net-a.review.extra").is_none()
        );
    }

    #[test]
    fn test_parse_human_cancelled_subject_valid() {
        let parsed = Subjects::parse_human_cancelled_subject("human.cancelled.net-a.review");
        assert_eq!(parsed, Some(("net-a", "review")));
    }

    #[test]
    fn test_parse_human_cancelled_subject_invalid() {
        assert!(Subjects::parse_human_cancelled_subject("human.completed.net-a.review").is_none());
        assert!(Subjects::parse_human_cancelled_subject("human.cancelled.net-a").is_none());
    }

    #[test]
    fn test_parse_human_failed_subject_valid() {
        let parsed = Subjects::parse_human_failed_subject("human.failed.net-a.review");
        assert_eq!(parsed, Some(("net-a", "review")));
    }

    #[test]
    fn test_parse_human_failed_subject_invalid() {
        assert!(Subjects::parse_human_failed_subject("human.completed.net-a.review").is_none());
        assert!(Subjects::parse_human_failed_subject("human.failed.net-a").is_none());
    }

    #[test]
    fn test_roundtrip_human_completed_subject() {
        let subject = format!(
            "{}.{}.{}",
            Subjects::HUMAN_COMPLETED_PREFIX,
            "my-net",
            "my-place"
        );
        let parsed = Subjects::parse_human_completed_subject(&subject);
        assert_eq!(parsed, Some(("my-net", "my-place")));
    }

    #[test]
    fn test_roundtrip_human_cancelled_subject() {
        let subject = format!(
            "{}.{}.{}",
            Subjects::HUMAN_CANCELLED_PREFIX,
            "my-net",
            "my-place"
        );
        let parsed = Subjects::parse_human_cancelled_subject(&subject);
        assert_eq!(parsed, Some(("my-net", "my-place")));
    }

    // ==================== Lifecycle Event Subject Tests ====================

    #[test]
    fn test_subject_for_net_created() {
        let event = DomainEvent::NetCreated {
            net_id: "net-a".to_string(),
            template_id: None,
            parameters: None,
            created_by: None,
            label: None,
        };
        assert_eq!(
            Subjects::for_event(&event, Some("net-a")),
            "petri.events.net-a.net.created"
        );
        assert_eq!(
            Subjects::for_event(&event, None),
            "petri.events.net.created"
        );
    }

    #[test]
    fn test_subject_for_net_completed() {
        let event = DomainEvent::NetCompleted {
            net_id: "net-a".to_string(),
            terminal_place_id: "done".to_string(),
            exit_code: None,
        };
        assert_eq!(
            Subjects::for_event(&event, Some("net-a")),
            "petri.events.net-a.net.completed"
        );
    }

    #[test]
    fn test_subject_for_net_cancelled() {
        let event = DomainEvent::NetCancelled {
            net_id: "net-a".to_string(),
            reason: None,
            cancelled_by: None,
        };
        assert_eq!(
            Subjects::for_event(&event, Some("net-a")),
            "petri.events.net-a.net.cancelled"
        );
    }

    #[test]
    fn test_event_type_name_lifecycle() {
        let created = DomainEvent::NetCreated {
            net_id: "x".to_string(),
            template_id: None,
            parameters: None,
            created_by: None,
            label: None,
        };
        assert_eq!(Subjects::event_type_name(&created), "net.created");

        let completed = DomainEvent::NetCompleted {
            net_id: "x".to_string(),
            terminal_place_id: "done".to_string(),
            exit_code: None,
        };
        assert_eq!(Subjects::event_type_name(&completed), "net.completed");

        let cancelled = DomainEvent::NetCancelled {
            net_id: "x".to_string(),
            reason: None,
            cancelled_by: None,
        };
        assert_eq!(Subjects::event_type_name(&cancelled), "net.cancelled");
    }

    #[test]
    fn test_roundtrip_human_failed_subject() {
        let subject = format!(
            "{}.{}.{}",
            Subjects::HUMAN_FAILED_PREFIX,
            "my-net",
            "my-place"
        );
        let parsed = Subjects::parse_human_failed_subject(&subject);
        assert_eq!(parsed, Some(("my-net", "my-place")));
    }
}
