use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::TokenId;

/// Reply routing context attached to tokens in request-reply bridge patterns.
///
/// Only present on tokens that are part of a request-reply exchange.
/// Provenance (correlation, source net) is tracked via the event log (ADR-18),
/// not on the token itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReplyRouting {
    /// Where to send the reply (if this is a request expecting a response).
    /// Used by unnamed `bridge_reply` places (default channel).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<BridgeReplyAddress>,
    /// Named reply channels for multi-address reply routing.
    /// Each key is a channel name (e.g., "result", "failure") and the value
    /// is the full reply address. Used by `bridge_reply_channel` places.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_channels: Option<HashMap<String, BridgeReplyAddress>>,
}

/// Full address for routing a reply token back to the requester net.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BridgeReplyAddress {
    /// Net ID to send the reply to (e.g., "net-a")
    pub net_id: String,
    /// Place name on the target net (e.g., "reply_inbox")
    pub place_name: String,
}

/// The "color" of a token in Colored Petri Net terminology.
/// Represents the data carried by a token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", content = "value")]
pub enum TokenColor {
    /// A simple presence/absence marker (classic Petri Net token)
    Unit,
    /// A numeric value (fungible resource)
    Integer(i64),
    /// Structured JSON data (non-fungible, complex state)
    Data(serde_json::Value),
}

impl TokenColor {
    pub fn unit() -> Self {
        Self::Unit
    }

    pub fn integer(value: i64) -> Self {
        Self::Integer(value)
    }

    pub fn data(value: serde_json::Value) -> Self {
        Self::Data(value)
    }
}

/// A token in the Petri Net.
/// Tokens are immutable - they are created and consumed, never modified.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Token {
    /// Unique identifier for this token
    pub id: TokenId,
    /// The data carried by this token
    pub color: TokenColor,
    /// When this token was created
    pub created_at: DateTime<Utc>,
    /// The event sequence number that created this token (for provenance)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_event: Option<u64>,
    /// Reply routing context from cross-net bridge transfer (for request-reply patterns)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_routing: Option<ReplyRouting>,
}

impl Token {
    pub fn new(color: TokenColor) -> Self {
        Self {
            id: TokenId::new(),
            color,
            created_at: Utc::now(),
            created_by_event: None,
            reply_routing: None,
        }
    }

    pub fn new_unit() -> Self {
        Self::new(TokenColor::Unit)
    }

    pub fn new_integer(value: i64) -> Self {
        Self::new(TokenColor::Integer(value))
    }

    pub fn new_data(value: serde_json::Value) -> Self {
        Self::new(TokenColor::Data(value))
    }

    pub fn with_provenance(mut self, event_sequence: u64) -> Self {
        self.created_by_event = Some(event_sequence);
        self
    }

    /// Create a token with a specific ID (for restoration purposes).
    pub fn with_id(id: TokenId, color: TokenColor) -> Self {
        Self {
            id,
            color,
            created_at: Utc::now(),
            created_by_event: None,
            reply_routing: None,
        }
    }

    /// Attach reply routing context (for request-reply bridge patterns).
    pub fn with_reply_routing(mut self, routing: ReplyRouting) -> Self {
        self.reply_routing = Some(routing);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_color_unit() {
        let color = TokenColor::unit();
        assert_eq!(color, TokenColor::Unit);
    }

    #[test]
    fn test_token_color_integer() {
        let color = TokenColor::integer(42);
        assert_eq!(color, TokenColor::Integer(42));
    }

    #[test]
    fn test_token_color_data() {
        let data = serde_json::json!({"key": "value"});
        let color = TokenColor::data(data.clone());
        assert_eq!(color, TokenColor::Data(data));
    }

    #[test]
    fn test_token_new_unit() {
        let token = Token::new_unit();
        assert_eq!(token.color, TokenColor::Unit);
        assert!(token.created_by_event.is_none());
    }

    #[test]
    fn test_token_new_integer() {
        let token = Token::new_integer(100);
        assert_eq!(token.color, TokenColor::Integer(100));
    }

    #[test]
    fn test_token_new_data() {
        let data = serde_json::json!({"foo": "bar"});
        let token = Token::new_data(data.clone());
        assert_eq!(token.color, TokenColor::Data(data));
    }

    #[test]
    fn test_token_with_provenance() {
        let token = Token::new_unit().with_provenance(5);
        assert_eq!(token.created_by_event, Some(5));
    }

    #[test]
    fn test_token_serialization() {
        let token = Token::new_integer(42);
        let json = serde_json::to_string(&token).unwrap();
        let deserialized: Token = serde_json::from_str(&json).unwrap();
        assert_eq!(token.id, deserialized.id);
        assert_eq!(token.color, deserialized.color);
    }

    #[test]
    fn test_token_color_serialization() {
        let colors = vec![
            TokenColor::Unit,
            TokenColor::Integer(123),
            TokenColor::Data(serde_json::json!({"nested": {"value": 1}})),
        ];

        for color in colors {
            let json = serde_json::to_string(&color).unwrap();
            let deserialized: TokenColor = serde_json::from_str(&json).unwrap();
            assert_eq!(color, deserialized);
        }
    }

    /// Regression: nested "type" key must survive TokenColor roundtrip.
    /// This is the key that serde(tag = "type") on InputSource (executor-domain) needs.
    #[test]
    fn test_token_color_nested_type_key_preserved() {
        let inner = serde_json::json!({
            "spec": {
                "inputs": [
                    {
                        "name": "script.py",
                        "source": { "type": "raw", "content": "print('hi')" }
                    },
                    {
                        "name": "params",
                        "source": { "type": "inline", "value": { "a": 0.5 } }
                    }
                ]
            }
        });

        let color = TokenColor::Data(inner.clone());
        let json = serde_json::to_string(&color).unwrap();
        let deserialized: TokenColor = serde_json::from_str(&json).unwrap();
        assert_eq!(color, deserialized, "TokenColor roundtrip failed");

        // Verify the nested "type" keys are still present
        if let TokenColor::Data(val) = &deserialized {
            let inputs = val["spec"]["inputs"].as_array().unwrap();
            for inp in inputs {
                assert!(
                    inp["source"].get("type").is_some(),
                    "nested 'type' key lost in source for input '{}': {}",
                    inp["name"],
                    inp["source"]
                );
            }
        } else {
            panic!("expected TokenColor::Data");
        }
    }
}
