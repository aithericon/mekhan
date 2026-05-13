//! Mock adapter types for simulating external services.
//!
//! Mock adapters watch for tokens in trigger places and, after a configurable
//! latency, inject signal tokens into target places. This enables testing of
//! async workflows without actual external services.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::PlaceId;

/// Polymorphic adapter logic - supports Rhai (now), Wasm and JavaScript (future).
///
/// The script receives `token` (the triggering token's data) and must return:
/// ```rhai
/// #{ target_place: "place_id", data: { ... } }
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdapterLogic {
    /// Rhai script - evaluated by the engine.
    Rhai { source: String },
    /// JavaScript - for frontend-only simulation (deprecated, not evaluated by engine).
    #[serde(rename = "js")]
    JavaScript { source: String },
    /// Wasm module (future).
    Wasm { module: String, function: String },
}

impl AdapterLogic {
    /// Create Rhai adapter logic.
    pub fn rhai(source: impl Into<String>) -> Self {
        Self::Rhai {
            source: source.into(),
        }
    }

    /// Create JavaScript adapter logic (for legacy frontend compatibility).
    pub fn js(source: impl Into<String>) -> Self {
        Self::JavaScript {
            source: source.into(),
        }
    }

    /// Get the Rhai source if this is a Rhai adapter.
    pub fn as_rhai(&self) -> Option<&str> {
        match self {
            Self::Rhai { source } => Some(source),
            _ => None,
        }
    }
}

/// Configuration for a mock adapter.
///
/// Mock adapters simulate external services by watching for tokens in a trigger
/// place and injecting signal tokens after a latency delay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MockAdapterConfig {
    /// Human-readable name for this adapter.
    pub name: String,
    /// Place ID (scenario string ID) that triggers this adapter when a token arrives.
    pub trigger_place_id: String,
    /// Delay in milliseconds before injecting the response token.
    pub latency_ms: u64,
    /// Logic to evaluate when triggered.
    pub logic: AdapterLogic,
    /// If true, check that the triggering token still exists at the trigger place
    /// before executing the adapter logic. This enables timeout patterns where the
    /// adapter only fires if the token hasn't been consumed by another transition.
    /// Default: false (always execute after latency).
    #[serde(default)]
    pub check_token_exists: bool,
}

/// Registered adapter with resolved place ID.
///
/// This is the internal representation after scenario loading, where the
/// trigger_place_id has been resolved to an actual PlaceId UUID.
#[derive(Clone, Debug)]
pub struct RegisteredAdapter {
    /// Human-readable name for this adapter.
    pub name: String,
    /// Resolved place ID (UUID) that triggers this adapter.
    pub trigger_place_id: PlaceId,
    /// Delay in milliseconds before injecting the response token.
    pub latency_ms: u64,
    /// Logic to evaluate when triggered.
    pub logic: AdapterLogic,
    /// If true, verify the triggering token still exists before executing.
    pub check_token_exists: bool,
}

/// Result of adapter logic evaluation.
///
/// Contains the target place (scenario string ID) and the token data to inject.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdapterResult {
    /// Target place ID (scenario string ID) to inject the token into.
    pub target_place: String,
    /// Token data to inject.
    pub data: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_logic_rhai() {
        let logic = AdapterLogic::rhai("#{target_place: \"foo\", data: #{}}");
        assert!(matches!(logic, AdapterLogic::Rhai { .. }));
        assert_eq!(logic.as_rhai(), Some("#{target_place: \"foo\", data: #{}}"));
    }

    #[test]
    fn test_adapter_logic_js() {
        let logic = AdapterLogic::js("return { target_place: 'foo' }");
        assert!(matches!(logic, AdapterLogic::JavaScript { .. }));
        assert!(logic.as_rhai().is_none());
    }

    #[test]
    fn test_mock_adapter_config_serialization() {
        let config = MockAdapterConfig {
            name: "Test Adapter".to_string(),
            trigger_place_id: "some_place".to_string(),
            latency_ms: 500,
            logic: AdapterLogic::rhai("#{target_place: \"sig_ok\", data: #{}}"),
            check_token_exists: false,
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: MockAdapterConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn test_adapter_logic_rhai_serialization() {
        let logic = AdapterLogic::rhai("let x = 1; x");
        let json = serde_json::to_string(&logic).unwrap();
        assert!(json.contains("\"type\":\"rhai\""));

        let parsed: AdapterLogic = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, logic);
    }

    #[test]
    fn test_adapter_logic_js_serialization() {
        let logic = AdapterLogic::js("return {}");
        let json = serde_json::to_string(&logic).unwrap();
        assert!(json.contains("\"type\":\"js\""));

        let parsed: AdapterLogic = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, logic);
    }
}
