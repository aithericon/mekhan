use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::PlaceId;

/// Bridge target configuration for cross-net token transfer.
/// When a place has a bridge_out target, tokens produced there are
/// not added to local marking — they are forwarded to the remote net.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BridgeTarget {
    pub target_net_id: String,
    pub target_place_name: String,
    /// Local place name to receive replies (default channel).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Named reply channels: channel_name → local_place_name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_channels: Option<HashMap<String, String>>,
}

/// How a place interacts with the world outside its net.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlaceKind {
    /// Regular place — tokens flow only within the net.
    Internal,
    /// Receives external signals from adapters/timers.
    Signal,
    /// Receives tokens from other nets via bridge.
    BridgeIn {
        /// Source net ID (visualization metadata, no runtime effect)
        #[serde(skip_serializing_if = "Option::is_none")]
        source_net_id: Option<String>,
        /// Source place name in the remote net (visualization metadata)
        #[serde(skip_serializing_if = "Option::is_none")]
        source_place_name: Option<String>,
    },
    /// Forwards produced tokens to a place on another net.
    BridgeOut {
        target_net_id: String,
        target_place_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
        /// Named reply channels: channel_name → local_place_name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_channels: Option<HashMap<String, String>>,
        /// Display name for UI grouping (used instead of target_net_id when present).
        /// Useful when target_net_id is a dynamic reference like `$result.child_net_id`.
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Routes produced tokens back to the sender's reply address.
    /// If `channel` is None, uses the default `reply_to` address.
    /// If `channel` is Some, looks up the named channel in `reply_channels`.
    BridgeReply {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
    },
    /// Terminal sink — tokens here signal net completion.
    /// No outgoing arcs by convention. The first token's data may
    /// contain an `exit_code` field read on completion.
    Terminal,
}

/// A place (location) in the Petri Net where tokens can reside.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Place {
    /// Unique identifier
    pub id: PlaceId,
    /// Human-readable name
    pub name: String,
    /// How this place interacts with the world outside its net.
    #[serde(flatten)]
    pub kind: PlaceKind,
    /// Maximum number of tokens allowed (None = unlimited)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<usize>,
    /// Group ID for visualization (hierarchical components)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// JSON Schema reference for tokens at this place (e.g., "#/definitions/Task")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_schema: Option<String>,
}

impl Place {
    /// Create a regular internal place.
    pub fn internal(name: impl Into<String>) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::Internal,
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    /// Create a signal place (receives external triggers).
    pub fn signal(name: impl Into<String>) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::Signal,
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    /// Create a bridge-in place (receives tokens from other nets).
    pub fn bridge_in(name: impl Into<String>) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::BridgeIn {
                source_net_id: None,
                source_place_name: None,
            },
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    /// Create a bridge-in place with source annotation (for visualization).
    pub fn bridge_in_from(
        name: impl Into<String>,
        source_net_id: impl Into<String>,
        source_place_name: impl Into<String>,
    ) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::BridgeIn {
                source_net_id: Some(source_net_id.into()),
                source_place_name: Some(source_place_name.into()),
            },
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    /// Create a bridge-out place that forwards tokens to a remote net.
    pub fn bridge_out(
        name: impl Into<String>,
        net_id: impl Into<String>,
        place_name: impl Into<String>,
    ) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::BridgeOut {
                target_net_id: net_id.into(),
                target_place_name: place_name.into(),
                reply_to: None,
                reply_channels: None,
                label: None,
            },
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    /// Create a bridge-out place with a reply-to address for request-reply.
    pub fn bridge_out_reply(
        name: impl Into<String>,
        net_id: impl Into<String>,
        place_name: impl Into<String>,
        reply_to: impl Into<String>,
    ) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::BridgeOut {
                target_net_id: net_id.into(),
                target_place_name: place_name.into(),
                reply_to: Some(reply_to.into()),
                reply_channels: None,
                label: None,
            },
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    /// Create a bridge-out place with named reply channels.
    /// Each channel maps a name (e.g., "result") to a local place name
    /// (e.g., "result_inbox") where replies for that channel should land.
    pub fn bridge_out_reply_channels(
        name: impl Into<String>,
        net_id: impl Into<String>,
        place_name: impl Into<String>,
        channels: HashMap<String, String>,
    ) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::BridgeOut {
                target_net_id: net_id.into(),
                target_place_name: place_name.into(),
                reply_to: None,
                reply_channels: Some(channels),
                label: None,
            },
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    /// Create a bridge-out place with label, reply-to, and target info.
    /// Used for dynamic bridges (e.g., spawn) where target_net_id is a
    /// runtime reference like `$result.child_net_id` but the UI needs
    /// a human-readable label for grouping.
    pub fn bridge_out_labeled(
        name: impl Into<String>,
        net_id: impl Into<String>,
        place_name: impl Into<String>,
        reply_to: Option<String>,
        label: impl Into<String>,
    ) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::BridgeOut {
                target_net_id: net_id.into(),
                target_place_name: place_name.into(),
                reply_to,
                reply_channels: None,
                label: Some(label.into()),
            },
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    /// Create a terminal place (sink that signals net completion).
    pub fn terminal(name: impl Into<String>) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::Terminal,
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    /// Create a bridge-reply place (routes tokens back via consumed reply_routing's reply_to).
    pub fn bridge_reply(name: impl Into<String>) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::BridgeReply { channel: None },
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    /// Create a bridge-reply place that reads a named channel from reply_routing's reply_channels.
    pub fn bridge_reply_channel(name: impl Into<String>, channel: impl Into<String>) -> Self {
        let name_str: String = name.into();
        Self {
            id: PlaceId(name_str.clone()),
            name: name_str,
            kind: PlaceKind::BridgeReply {
                channel: Some(channel.into()),
            },
            capacity: None,
            group_id: None,
            token_schema: None,
        }
    }

    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = Some(capacity);
        self
    }

    pub fn with_id(mut self, id: PlaceId) -> Self {
        self.id = id;
        self
    }

    pub fn with_group_id(mut self, group_id: impl Into<String>) -> Self {
        self.group_id = Some(group_id.into());
        self
    }

    pub fn with_token_schema(mut self, schema: impl Into<String>) -> Self {
        self.token_schema = Some(schema.into());
        self
    }

    /// Extract BridgeTarget for event emission (used by firing.rs).
    pub fn bridge_target(&self) -> Option<BridgeTarget> {
        match &self.kind {
            PlaceKind::BridgeOut {
                target_net_id,
                target_place_name,
                reply_to,
                reply_channels,
                ..
            } => Some(BridgeTarget {
                target_net_id: target_net_id.clone(),
                target_place_name: target_place_name.clone(),
                reply_to: reply_to.clone(),
                reply_channels: reply_channels.clone(),
            }),
            _ => None,
        }
    }

    /// Check if this place is a bridge-out place.
    pub fn is_bridge_out(&self) -> bool {
        matches!(self.kind, PlaceKind::BridgeOut { .. })
    }

    /// Check if this place is externally fed (skip UNREACHABLE checks).
    pub fn is_externally_fed(&self) -> bool {
        matches!(
            self.kind,
            PlaceKind::Signal | PlaceKind::BridgeIn { .. } | PlaceKind::BridgeReply { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_place_terminal_constructor() {
        let place = Place::terminal("done");
        assert_eq!(place.name, "done");
        assert_eq!(place.id, PlaceId("done".to_string()));
        assert!(matches!(place.kind, PlaceKind::Terminal));
    }

    #[test]
    fn test_terminal_serialization_roundtrip() {
        let place = Place::terminal("done");
        let json = serde_json::to_string(&place).unwrap();
        let parsed: Place = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "done");
        assert!(matches!(parsed.kind, PlaceKind::Terminal));
    }
}
