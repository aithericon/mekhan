//! NATS subject naming conventions.
//!
//! Per ADR-09, every net-scoped subject carries a leading `{ws}` (workspace)
//! segment so a single engine process can host nets from many workspaces with
//! hard subject-level isolation. `{ws}` is a NATS-token-safe workspace UUID
//! string (like `{net}` ids); a net loaded without a workspace routes on the
//! reserved [`Subjects::DEFAULT_WORKSPACE`] sentinel.
//!
//! Subject hierarchy:
//! ```text
//! petri.{ws}.>                              # All events for a workspace (wildcard)
//! petri.{ws}.{net}.>                        # All subjects for one net
//! petri.{ws}.{net}.events.net.initialized   # Net topology loaded
//! petri.{ws}.{net}.events.token.created      # Token injected into place
//! petri.{ws}.{net}.events.token.consumed     # Token removed from place
//! petri.{ws}.{net}.events.transition.fired   # Transition executed
//! petri.{ws}.{net}.events.transition.updated # Script hot-reloaded
//! petri.{ws}.{net}.events.error              # Error occurred
//!
//! petri.{ws}.{net}.commands.inject.token     # Token injection requests
//!
//! petri.{ws}.{net}.bridge.{target_place_name}  # Cross-net bridge transfers (intra-workspace)
//! petri.{ws}.{net}.signal.{target_place_name}  # External system signals
//!
//! human.{ws}.request.{net}.{place}           # Human task requests (kept outside petri.>)
//! petri-dlq.{ws}.{class}                      # Dead-letter queue
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
    // ==================== Workspace (tenant) Segment ====================

    /// Root prefix shared by all petri subjects.
    pub const PETRI_ROOT: &'static str = "petri";

    /// Reserved workspace sentinel used when a net is loaded WITHOUT an explicit
    /// `workspace_id` (legacy/SDK/demo/dev). Subjects then route on
    /// `petri.default.{net}...`. NATS-token-safe by construction.
    pub const DEFAULT_WORKSPACE: &'static str = "default";

    /// Build a subscription filter for ALL subjects in a workspace:
    /// `petri.{ws}.>`.
    pub fn workspace_filter(ws: &str) -> String {
        format!("{}.{}.>", Self::PETRI_ROOT, ws)
    }

    /// Build a subscription filter for ALL subjects of a single net within a
    /// workspace: `petri.{ws}.{net}.>`.
    pub fn net_filter(ws: &str, net_id: &str) -> String {
        format!("{}.{}.{}.>", Self::PETRI_ROOT, ws, net_id)
    }

    // ==================== Event Subjects ====================

    /// Category segment for event subjects (follows `petri.{ws}.{net}.`).
    pub const EVENTS_CATEGORY: &'static str = "events";

    /// Wildcard for subscribing to all events across all workspaces/nets.
    pub const EVENTS_ALL: &'static str = "petri.>";

    // ==================== Command Subjects ====================

    /// Category segment for command subjects (follows `petri.{ws}.{net}.`).
    pub const COMMANDS_CATEGORY: &'static str = "commands";

    /// Token injection command suffix (appended after `commands` category).
    pub const COMMAND_INJECT_TOKEN_SUFFIX: &'static str = "inject.token";

    /// Token removal command suffix.
    pub const COMMAND_REMOVE_TOKEN_SUFFIX: &'static str = "remove.token";

    /// Token update command suffix.
    pub const COMMAND_UPDATE_TOKEN_SUFFIX: &'static str = "update.token";

    /// Create net command suffix.
    pub const COMMAND_CREATE_NET_SUFFIX: &'static str = "create_net";

    /// Build a net-scoped command subject:
    /// `petri.{ws}.{net}.commands.{suffix}`.
    pub fn command(ws: &str, net_id: &str, suffix: &str) -> String {
        format!(
            "{}.{}.{}.{}.{}",
            Self::PETRI_ROOT,
            ws,
            net_id,
            Self::COMMANDS_CATEGORY,
            suffix
        )
    }

    /// Token injection command subject for a net.
    pub fn command_inject_token(ws: &str, net_id: &str) -> String {
        Self::command(ws, net_id, Self::COMMAND_INJECT_TOKEN_SUFFIX)
    }

    /// Token removal command subject for a net.
    pub fn command_remove_token(ws: &str, net_id: &str) -> String {
        Self::command(ws, net_id, Self::COMMAND_REMOVE_TOKEN_SUFFIX)
    }

    /// Token update command subject for a net.
    pub fn command_update_token(ws: &str, net_id: &str) -> String {
        Self::command(ws, net_id, Self::COMMAND_UPDATE_TOKEN_SUFFIX)
    }

    /// Build the `create_net` command subject for a workspace.
    ///
    /// `create_net` has no target net yet, so it is workspace-scoped (no
    /// `{net}` segment): `petri.{ws}.commands.create_net`. The listener
    /// filters per-workspace.
    pub fn command_create_net(ws: &str) -> String {
        format!(
            "{}.{}.{}.{}",
            Self::PETRI_ROOT,
            ws,
            Self::COMMANDS_CATEGORY,
            Self::COMMAND_CREATE_NET_SUFFIX
        )
    }

    // ==================== Human Task Subjects ====================
    //
    // Human task subjects use a separate root (`human.`, not `petri.`) to avoid
    // JetStream subject overlap with the PETRI_GLOBAL stream that captures all
    // `petri.>` subjects. Per ADR-09 the workspace segment is inserted right
    // after the `human` root and BEFORE the category, so the shape is:
    //   human.{ws}.request.{net}.{place}
    //   human.{ws}.completed.{net}.{place}
    //   human.{ws}.cancel.{net}.{place}
    //   human.{ws}.cancelled.{net}.{place}
    //   human.{ws}.failed.{net}.{place}

    /// Root for human task subjects (workspace segment follows).
    pub const HUMAN_ROOT: &'static str = "human";

    /// Category segment for human task requests.
    pub const HUMAN_REQUEST_CATEGORY: &'static str = "request";

    /// Category segment for human task completions.
    pub const HUMAN_COMPLETED_CATEGORY: &'static str = "completed";

    /// Category segment for human task cancel requests (engine -> UI).
    pub const HUMAN_CANCEL_CATEGORY: &'static str = "cancel";

    /// Category segment for human task cancel confirmations (UI -> engine).
    pub const HUMAN_CANCELLED_CATEGORY: &'static str = "cancelled";

    /// Category segment for human task failure signals (UI -> engine).
    pub const HUMAN_FAILED_CATEGORY: &'static str = "failed";

    /// Stream name for human task cancel requests
    pub const STREAM_HUMAN_CANCEL: &'static str = "HUMAN_CANCEL";

    /// Stream name for human task cancel confirmations
    pub const STREAM_HUMAN_CANCELLED: &'static str = "HUMAN_CANCELLED";

    /// Stream name for human task failures
    pub const STREAM_HUMAN_FAILED: &'static str = "HUMAN_FAILED";

    /// Build a human task request subject: `human.{ws}.request.{net}.{place}`.
    pub fn human_request(ws: &str, net_id: &str, place_name: &str) -> String {
        format!(
            "{}.{}.{}.{}.{}",
            Self::HUMAN_ROOT,
            ws,
            Self::HUMAN_REQUEST_CATEGORY,
            net_id,
            place_name
        )
    }

    /// Build a human task completion filter: `human.{ws}.completed.{net}.>`.
    pub fn human_completed_filter(ws: &str, net_id: &str) -> String {
        format!(
            "{}.{}.{}.{}.>",
            Self::HUMAN_ROOT,
            ws,
            Self::HUMAN_COMPLETED_CATEGORY,
            net_id
        )
    }

    /// Build a human task cancel subject: `human.{ws}.cancel.{net}.{place}`.
    pub fn human_cancel(ws: &str, net_id: &str, place_name: &str) -> String {
        format!(
            "{}.{}.{}.{}.{}",
            Self::HUMAN_ROOT,
            ws,
            Self::HUMAN_CANCEL_CATEGORY,
            net_id,
            place_name
        )
    }

    /// Build a human task cancelled filter (for engine-side consumer):
    /// `human.{ws}.cancelled.{net}.>`.
    pub fn human_cancelled_filter(ws: &str, net_id: &str) -> String {
        format!(
            "{}.{}.{}.{}.>",
            Self::HUMAN_ROOT,
            ws,
            Self::HUMAN_CANCELLED_CATEGORY,
            net_id
        )
    }

    /// Build a human task failed filter (for engine-side consumer):
    /// `human.{ws}.failed.{net}.>`.
    pub fn human_failed_filter(ws: &str, net_id: &str) -> String {
        format!(
            "{}.{}.{}.{}.>",
            Self::HUMAN_ROOT,
            ws,
            Self::HUMAN_FAILED_CATEGORY,
            net_id
        )
    }

    /// Build a subscription filter for ALL human subjects of a workspace,
    /// for a given category: `human.{ws}.{category}.>`.
    pub fn human_workspace_filter(ws: &str, category: &str) -> String {
        format!("{}.{}.{}.>", Self::HUMAN_ROOT, ws, category)
    }

    /// Parse a `human.{ws}.completed.{net}.{place}` subject into
    /// `(ws, net_id, place_name)`.
    pub fn parse_human_completed_subject(subject: &str) -> Option<(&str, &str, &str)> {
        Self::parse_human_subject(subject, Self::HUMAN_COMPLETED_CATEGORY)
    }

    /// Parse a `human.{ws}.cancelled.{net}.{place}` subject into
    /// `(ws, net_id, place_name)`.
    pub fn parse_human_cancelled_subject(subject: &str) -> Option<(&str, &str, &str)> {
        Self::parse_human_subject(subject, Self::HUMAN_CANCELLED_CATEGORY)
    }

    /// Parse a `human.{ws}.failed.{net}.{place}` subject into
    /// `(ws, net_id, place_name)`.
    pub fn parse_human_failed_subject(subject: &str) -> Option<(&str, &str, &str)> {
        Self::parse_human_subject(subject, Self::HUMAN_FAILED_CATEGORY)
    }

    /// Shared parser for `human.{ws}.{category}.{net}.{place}` subjects.
    fn parse_human_subject<'a>(
        subject: &'a str,
        category: &str,
    ) -> Option<(&'a str, &'a str, &'a str)> {
        let parts: Vec<&str> = subject.split('.').collect();
        if parts.len() == 5 && parts[0] == Self::HUMAN_ROOT && parts[2] == category {
            Some((parts[1], parts[3], parts[4]))
        } else {
            None
        }
    }

    // ==================== External Signal Subjects ====================
    //
    // Signals from external systems (Nomad, Slurm, K8s, webhooks).
    //
    // Subject hierarchy (ADR-09, ws-segmented + category-after-net):
    //   petri.{ws}.{target_net_id}.signal.{target_place_name}

    /// Category segment for signal subjects (follows `petri.{ws}.{net}.`).
    pub const SIGNAL_CATEGORY: &'static str = "signal";

    /// Build a signal subject for publishing to a net's place.
    ///
    /// # Example
    /// ```
    /// use petri_api_types::subjects::Subjects;
    ///
    /// let subject = Subjects::signal_transfer("ws1", "gpu-resource", "status_inbox");
    /// assert_eq!(subject, "petri.ws1.gpu-resource.signal.status_inbox");
    /// ```
    pub fn signal_transfer(ws: &str, target_net_id: &str, target_place_name: &str) -> String {
        format!(
            "{}.{}.{}.{}.{}",
            Self::PETRI_ROOT,
            ws,
            target_net_id,
            Self::SIGNAL_CATEGORY,
            target_place_name
        )
    }

    /// Build a subscription filter for all signal messages targeting this net.
    ///
    /// # Example
    /// ```
    /// use petri_api_types::subjects::Subjects;
    ///
    /// let filter = Subjects::signal_inbox_filter("ws1", "gpu-resource");
    /// assert_eq!(filter, "petri.ws1.gpu-resource.signal.>");
    /// ```
    pub fn signal_inbox_filter(ws: &str, own_net_id: &str) -> String {
        format!(
            "{}.{}.{}.{}.>",
            Self::PETRI_ROOT,
            ws,
            own_net_id,
            Self::SIGNAL_CATEGORY
        )
    }

    /// Build a subscription filter for ALL signal messages in a workspace
    /// (across nets): `petri.{ws}.*.signal.>`.
    pub fn signal_workspace_filter(ws: &str) -> String {
        format!(
            "{}.{}.*.{}.>",
            Self::PETRI_ROOT,
            ws,
            Self::SIGNAL_CATEGORY
        )
    }

    /// Parse a signal subject into (ws, target_net_id, target_place_name).
    ///
    /// Returns `None` if the subject does not match the signal pattern.
    ///
    /// # Example
    /// ```
    /// use petri_api_types::subjects::Subjects;
    ///
    /// let parsed = Subjects::parse_signal_subject("petri.ws1.gpu-resource.signal.status_inbox");
    /// assert_eq!(parsed, Some(("ws1", "gpu-resource", "status_inbox")));
    /// ```
    pub fn parse_signal_subject(subject: &str) -> Option<(&str, &str, &str)> {
        let parts: Vec<&str> = subject.split('.').collect();
        if parts.len() == 5 && parts[0] == Self::PETRI_ROOT && parts[3] == Self::SIGNAL_CATEGORY {
            Some((parts[1], parts[2], parts[4]))
        } else {
            None
        }
    }

    // ==================== Cross-Net Bridge Subjects ====================
    //
    // Token transfer between separate Petri nets. Bridges are destination-
    // addressed and INTRA-workspace only (a net never bridges across tenants).
    //
    // Subject hierarchy (ADR-09, ws-segmented + category-after-net):
    //   petri.{ws}.{target_net_id}.bridge.{target_place_name}

    /// Category segment for bridge subjects (follows `petri.{ws}.{net}.`).
    pub const BRIDGE_CATEGORY: &'static str = "bridge";

    /// Build a bridge transfer subject for sending a token to a remote net's place.
    ///
    /// # Example
    /// ```
    /// use petri_api_types::subjects::Subjects;
    ///
    /// let subject = Subjects::bridge_transfer("ws1", "net-b", "inbox");
    /// assert_eq!(subject, "petri.ws1.net-b.bridge.inbox");
    /// ```
    pub fn bridge_transfer(ws: &str, target_net_id: &str, target_place_name: &str) -> String {
        format!(
            "{}.{}.{}.{}.{}",
            Self::PETRI_ROOT,
            ws,
            target_net_id,
            Self::BRIDGE_CATEGORY,
            target_place_name
        )
    }

    /// Build a subscription filter for all bridge messages targeting this net.
    ///
    /// # Example
    /// ```
    /// use petri_api_types::subjects::Subjects;
    ///
    /// let filter = Subjects::bridge_inbox_filter("ws1", "net-b");
    /// assert_eq!(filter, "petri.ws1.net-b.bridge.>");
    /// ```
    pub fn bridge_inbox_filter(ws: &str, own_net_id: &str) -> String {
        format!(
            "{}.{}.{}.{}.>",
            Self::PETRI_ROOT,
            ws,
            own_net_id,
            Self::BRIDGE_CATEGORY
        )
    }

    /// Build a subscription filter for ALL bridge messages in a workspace
    /// (across nets): `petri.{ws}.*.bridge.>`.
    pub fn bridge_workspace_filter(ws: &str) -> String {
        format!(
            "{}.{}.*.{}.>",
            Self::PETRI_ROOT,
            ws,
            Self::BRIDGE_CATEGORY
        )
    }

    /// Parse a bridge subject into (ws, target_net_id, target_place_name).
    ///
    /// Returns `None` if the subject does not match the bridge pattern.
    ///
    /// # Example
    /// ```
    /// use petri_api_types::subjects::Subjects;
    ///
    /// let parsed = Subjects::parse_bridge_subject("petri.ws1.net-b.bridge.inbox");
    /// assert_eq!(parsed, Some(("ws1", "net-b", "inbox")));
    /// ```
    pub fn parse_bridge_subject(subject: &str) -> Option<(&str, &str, &str)> {
        let parts: Vec<&str> = subject.split('.').collect();
        if parts.len() == 5 && parts[0] == Self::PETRI_ROOT && parts[3] == Self::BRIDGE_CATEGORY {
            Some((parts[1], parts[2], parts[4]))
        } else {
            None
        }
    }

    // ==================== Dead-Letter Queue Subjects ====================
    //
    // Terminally-failed messages from the NATS message loop are wrapped in
    // a DlqEntry envelope and published here instead of being dropped. The
    // prefix is deliberately NOT under `petri.` — the PETRI_GLOBAL stream
    // captures `petri.>` and JetStream rejects streams with overlapping
    // subjects (same reason the human task subjects live under `human.`).

    /// Prefix for dead-letter queue subjects
    pub const DLQ_PREFIX: &'static str = "petri-dlq";

    /// Wildcard for subscribing to all dead-lettered messages
    pub const DLQ_ALL: &'static str = "petri-dlq.>";

    /// Stream name for dead-lettered messages
    pub const STREAM_DLQ: &'static str = "PETRI_DLQ";

    /// Build a DLQ subject for a workspace + error class
    /// (`parse` | `business` | `internal`): `petri-dlq.{ws}.{class}`.
    ///
    /// # Example
    /// ```
    /// use petri_api_types::subjects::Subjects;
    ///
    /// assert_eq!(Subjects::dlq_subject("ws1", "parse"), "petri-dlq.ws1.parse");
    /// ```
    pub fn dlq_subject(ws: &str, error_class: &str) -> String {
        format!("{}.{}.{}", Self::DLQ_PREFIX, ws, error_class)
    }

    /// Build a subscription filter for ALL dead-lettered messages in a
    /// workspace: `petri-dlq.{ws}.>`.
    pub fn dlq_workspace_filter(ws: &str) -> String {
        format!("{}.{}.>", Self::DLQ_PREFIX, ws)
    }

    // ==================== JetStream Streams ====================

    /// Single global stream for ALL petri events (single stream architecture).
    /// All publishers write to this stream via `petri.>` subjects.
    /// Consumers use filter_subject to receive only relevant messages.
    pub const STREAM_GLOBAL: &'static str = "PETRI_GLOBAL";

    // ==================== Utilities ====================

    /// Get the appropriate subject for a domain event, scoped to a workspace
    /// and optionally a net.
    ///
    /// Per ADR-09 the category segment (`events`) follows `{ws}.{net}`:
    ///   - `Some(net)` → `petri.{ws}.{net}.events.{suffix}`
    ///   - `None`      → `petri.{ws}.events.{suffix}` (workspace-scoped, no net)
    pub fn for_event(event: &DomainEvent, ws: &str, net_id: Option<&str>) -> String {
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
            Some(id) => format!(
                "{}.{}.{}.{}.{}",
                Self::PETRI_ROOT,
                ws,
                id,
                Self::EVENTS_CATEGORY,
                suffix
            ),
            None => format!(
                "{}.{}.{}.{}",
                Self::PETRI_ROOT,
                ws,
                Self::EVENTS_CATEGORY,
                suffix
            ),
        }
    }

    /// Build a subscription filter for ALL events of a single net within a
    /// workspace: `petri.{ws}.{net}.events.>`.
    pub fn net_events_filter(ws: &str, net_id: &str) -> String {
        format!(
            "{}.{}.{}.{}.>",
            Self::PETRI_ROOT,
            ws,
            net_id,
            Self::EVENTS_CATEGORY
        )
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
            Subjects::for_event(&event, "ws1", None),
            "petri.ws1.events.token.created"
        );
        assert_eq!(
            Subjects::for_event(&event, "ws1", Some("net-a")),
            "petri.ws1.net-a.events.token.created"
        );
    }

    #[test]
    fn test_subject_for_error() {
        let event = DomainEvent::ErrorOccurred {
            message: "test".to_string(),
        };
        assert_eq!(
            Subjects::for_event(&event, "ws1", None),
            "petri.ws1.events.error"
        );
    }

    #[test]
    fn test_default_workspace_sentinel() {
        assert_eq!(Subjects::DEFAULT_WORKSPACE, "default");
        let event = DomainEvent::ErrorOccurred {
            message: "test".to_string(),
        };
        assert_eq!(
            Subjects::for_event(&event, Subjects::DEFAULT_WORKSPACE, Some("net-a")),
            "petri.default.net-a.events.error"
        );
    }

    #[test]
    fn test_workspace_and_net_filters() {
        assert_eq!(Subjects::workspace_filter("ws1"), "petri.ws1.>");
        assert_eq!(Subjects::net_filter("ws1", "net-a"), "petri.ws1.net-a.>");
        assert_eq!(
            Subjects::net_events_filter("ws1", "net-a"),
            "petri.ws1.net-a.events.>"
        );
    }

    // ==================== Command Subject Tests ====================

    #[test]
    fn test_command_subjects() {
        assert_eq!(
            Subjects::command_inject_token("ws1", "net-a"),
            "petri.ws1.net-a.commands.inject.token"
        );
        assert_eq!(
            Subjects::command_remove_token("ws1", "net-a"),
            "petri.ws1.net-a.commands.remove.token"
        );
        assert_eq!(
            Subjects::command_update_token("ws1", "net-a"),
            "petri.ws1.net-a.commands.update.token"
        );
        assert_eq!(
            Subjects::command_create_net("ws1"),
            "petri.ws1.commands.create_net"
        );
    }

    // ==================== Cross-Net Bridge Subject Tests ====================

    #[test]
    fn test_bridge_transfer_subject() {
        let subject = Subjects::bridge_transfer("ws1", "net-b", "inbox");
        assert_eq!(subject, "petri.ws1.net-b.bridge.inbox");
    }

    #[test]
    fn test_bridge_inbox_filter() {
        let filter = Subjects::bridge_inbox_filter("ws1", "net-b");
        assert_eq!(filter, "petri.ws1.net-b.bridge.>");
    }

    #[test]
    fn test_bridge_workspace_filter() {
        let filter = Subjects::bridge_workspace_filter("ws1");
        assert_eq!(filter, "petri.ws1.*.bridge.>");
    }

    #[test]
    fn test_parse_bridge_subject_valid() {
        let parsed = Subjects::parse_bridge_subject("petri.ws1.net-b.bridge.inbox");
        assert_eq!(parsed, Some(("ws1", "net-b", "inbox")));
    }

    #[test]
    fn test_parse_bridge_subject_invalid_prefix() {
        let parsed = Subjects::parse_bridge_subject("invalid.ws1.net-b.bridge.inbox");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_parse_bridge_subject_wrong_length() {
        let parsed = Subjects::parse_bridge_subject("petri.ws1.net-b.bridge");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_parse_bridge_subject_too_many_parts() {
        let parsed = Subjects::parse_bridge_subject("petri.ws1.net-b.bridge.inbox.extra");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_roundtrip_bridge_subject() {
        let subject = Subjects::bridge_transfer("ws1", "my-net", "my-place");
        let parsed = Subjects::parse_bridge_subject(&subject);
        assert_eq!(parsed, Some(("ws1", "my-net", "my-place")));
    }

    // ==================== External Signal Subject Tests ====================

    #[test]
    fn test_signal_transfer_subject() {
        let subject = Subjects::signal_transfer("ws1", "gpu-resource", "status_inbox");
        assert_eq!(subject, "petri.ws1.gpu-resource.signal.status_inbox");
    }

    #[test]
    fn test_signal_inbox_filter() {
        let filter = Subjects::signal_inbox_filter("ws1", "gpu-resource");
        assert_eq!(filter, "petri.ws1.gpu-resource.signal.>");
    }

    #[test]
    fn test_signal_workspace_filter() {
        let filter = Subjects::signal_workspace_filter("ws1");
        assert_eq!(filter, "petri.ws1.*.signal.>");
    }

    #[test]
    fn test_parse_signal_subject_valid() {
        let parsed = Subjects::parse_signal_subject("petri.ws1.gpu-resource.signal.status_inbox");
        assert_eq!(parsed, Some(("ws1", "gpu-resource", "status_inbox")));
    }

    #[test]
    fn test_parse_signal_subject_invalid_prefix() {
        let parsed = Subjects::parse_signal_subject("invalid.ws1.gpu-resource.signal.inbox");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_parse_signal_subject_wrong_length() {
        let parsed = Subjects::parse_signal_subject("petri.ws1.gpu-resource.signal");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_roundtrip_signal_subject() {
        let subject = Subjects::signal_transfer("ws1", "my-net", "my-place");
        let parsed = Subjects::parse_signal_subject(&subject);
        assert_eq!(parsed, Some(("ws1", "my-net", "my-place")));
    }

    // ==================== Human Task Subject Tests ====================

    #[test]
    fn test_human_request_subject() {
        let subject = Subjects::human_request("ws1", "net-a", "review");
        assert_eq!(subject, "human.ws1.request.net-a.review");
    }

    #[test]
    fn test_human_completed_filter() {
        let filter = Subjects::human_completed_filter("ws1", "net-a");
        assert_eq!(filter, "human.ws1.completed.net-a.>");
    }

    #[test]
    fn test_human_cancel_subject() {
        let subject = Subjects::human_cancel("ws1", "net-a", "review");
        assert_eq!(subject, "human.ws1.cancel.net-a.review");
    }

    #[test]
    fn test_human_cancelled_filter() {
        let filter = Subjects::human_cancelled_filter("ws1", "net-a");
        assert_eq!(filter, "human.ws1.cancelled.net-a.>");
    }

    #[test]
    fn test_human_failed_filter() {
        let filter = Subjects::human_failed_filter("ws1", "net-a");
        assert_eq!(filter, "human.ws1.failed.net-a.>");
    }

    #[test]
    fn test_human_workspace_filter() {
        let filter = Subjects::human_workspace_filter("ws1", Subjects::HUMAN_COMPLETED_CATEGORY);
        assert_eq!(filter, "human.ws1.completed.>");
    }

    #[test]
    fn test_parse_human_completed_subject_valid() {
        let parsed = Subjects::parse_human_completed_subject("human.ws1.completed.net-a.review");
        assert_eq!(parsed, Some(("ws1", "net-a", "review")));
    }

    #[test]
    fn test_parse_human_completed_subject_invalid() {
        assert!(Subjects::parse_human_completed_subject("human.ws1.request.net-a.review").is_none());
        assert!(Subjects::parse_human_completed_subject("human.ws1.completed.net-a").is_none());
        assert!(Subjects::parse_human_completed_subject(
            "human.ws1.completed.net-a.review.extra"
        )
        .is_none());
    }

    #[test]
    fn test_parse_human_cancelled_subject_valid() {
        let parsed = Subjects::parse_human_cancelled_subject("human.ws1.cancelled.net-a.review");
        assert_eq!(parsed, Some(("ws1", "net-a", "review")));
    }

    #[test]
    fn test_parse_human_cancelled_subject_invalid() {
        assert!(
            Subjects::parse_human_cancelled_subject("human.ws1.completed.net-a.review").is_none()
        );
        assert!(Subjects::parse_human_cancelled_subject("human.ws1.cancelled.net-a").is_none());
    }

    #[test]
    fn test_parse_human_failed_subject_valid() {
        let parsed = Subjects::parse_human_failed_subject("human.ws1.failed.net-a.review");
        assert_eq!(parsed, Some(("ws1", "net-a", "review")));
    }

    #[test]
    fn test_parse_human_failed_subject_invalid() {
        assert!(Subjects::parse_human_failed_subject("human.ws1.completed.net-a.review").is_none());
        assert!(Subjects::parse_human_failed_subject("human.ws1.failed.net-a").is_none());
    }

    #[test]
    fn test_roundtrip_human_completed_subject() {
        let subject = Subjects::human_request("ws1", "my-net", "my-place")
            .replace(".request.", ".completed.");
        let parsed = Subjects::parse_human_completed_subject(&subject);
        assert_eq!(parsed, Some(("ws1", "my-net", "my-place")));
    }

    #[test]
    fn test_roundtrip_human_cancelled_subject() {
        let subject = Subjects::human_cancel("ws1", "my-net", "my-place")
            .replace(".cancel.", ".cancelled.");
        let parsed = Subjects::parse_human_cancelled_subject(&subject);
        assert_eq!(parsed, Some(("ws1", "my-net", "my-place")));
    }

    // ==================== DLQ Subject Tests ====================

    #[test]
    fn test_dlq_subject() {
        assert_eq!(Subjects::dlq_subject("ws1", "parse"), "petri-dlq.ws1.parse");
        assert_eq!(Subjects::dlq_workspace_filter("ws1"), "petri-dlq.ws1.>");
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
            Subjects::for_event(&event, "ws1", Some("net-a")),
            "petri.ws1.net-a.events.net.created"
        );
        assert_eq!(
            Subjects::for_event(&event, "ws1", None),
            "petri.ws1.events.net.created"
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
            Subjects::for_event(&event, "ws1", Some("net-a")),
            "petri.ws1.net-a.events.net.completed"
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
            Subjects::for_event(&event, "ws1", Some("net-a")),
            "petri.ws1.net-a.events.net.cancelled"
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
            "{}.{}.{}.{}.{}",
            Subjects::HUMAN_ROOT,
            "ws1",
            Subjects::HUMAN_FAILED_CATEGORY,
            "my-net",
            "my-place"
        );
        let parsed = Subjects::parse_human_failed_subject(&subject);
        assert_eq!(parsed, Some(("ws1", "my-net", "my-place")));
    }
}
