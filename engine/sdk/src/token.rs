//! Token trait and built-in token types.
//!
//! Tokens are the data that flows through the Petri net. Every place holds tokens
//! of a specific type, and transitions consume/produce tokens.
//!
//! ## Token Trait
//!
//! The [`Token`] trait is automatically implemented for any type that implements
//! `JsonSchema + Serialize + 'static`. This enables:
//!
//! - Compile-time type safety for wiring places to ports
//! - Automatic JSON Schema extraction for runtime validation
//! - Serialization to JSON for the engine
//!
//! ## Defining Custom Tokens
//!
//! Use the `#[token]` attribute macro to define custom token types:
//!
//! ```ignore
//! use aithericon_sdk::prelude::*;
//!
//! #[token]
//! struct Task {
//!     id: String,
//!     name: String,
//!     priority: i32,
//! }
//! ```
//!
//! The macro automatically derives `Clone`, `Debug`, `Serialize`, and `JsonSchema`.
//!
//! ## Built-in Token Types
//!
//! | Type | Purpose |
//! |------|---------|
//! | [`UnitToken`] | Simple marker with no data (classic Petri net dot) |
//! | [`IntegerToken`] | Fungible resource counter |
//! | [`DynamicToken`] | Untyped JSON for flexibility |

use schemars::schema_for;
use schemars::JsonSchema;
use serde::Serialize;

/// Marker trait for token payloads.
///
/// Enables:
/// - Compile-time type safety for wiring places to ports
/// - Automatic JSON Schema extraction for Wasm validation
///
/// # Example
/// ```ignore
/// use serde::Serialize;
/// use schemars::JsonSchema;
///
/// #[derive(Clone, Debug, Serialize, JsonSchema)]
/// struct Task {
///     id: String,
///     priority: u32,
/// }
///
/// // Task automatically implements Token
/// ```
pub trait Token: JsonSchema + Serialize + 'static {
    /// Get the simple type name for schema references
    fn type_name() -> &'static str {
        std::any::type_name::<Self>()
            .rsplit("::")
            .next()
            .unwrap_or("Unknown")
    }

    /// Extract JSON Schema for this token type as a serde_json::Value
    fn extract_schema() -> serde_json::Value {
        let schema = schema_for!(Self);
        serde_json::to_value(schema).unwrap_or_default()
    }

    /// Get schema reference string (e.g., "#/definitions/Task")
    fn schema_ref() -> String {
        format!("#/definitions/{}", Self::type_name())
    }
}

// Blanket implementation for any type satisfying the bounds
impl<T: JsonSchema + Serialize + 'static> Token for T {}

/// Unit token - classic Petri net marker with no data.
///
/// Use for simple presence/absence marking.
#[derive(Clone, Debug, Default, Serialize, JsonSchema)]
pub struct UnitToken;

/// Integer token - fungible resource counter.
///
/// Use for countable resources like credits, permits, etc.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct IntegerToken(pub i64);

impl IntegerToken {
    pub fn new(value: i64) -> Self {
        Self(value)
    }

    pub fn value(&self) -> i64 {
        self.0
    }
}

/// Dynamic token - untyped JSON data.
///
/// Use when you need flexibility and don't want compile-time type checking.
/// Rhai scripts can manipulate these freely.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DynamicToken(pub serde_json::Value);

impl DynamicToken {
    pub fn new(value: serde_json::Value) -> Self {
        Self(value)
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json).map(Self)
    }

    pub fn value(&self) -> &serde_json::Value {
        &self.0
    }
}

impl From<serde_json::Value> for DynamicToken {
    fn from(value: serde_json::Value) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Serialize, JsonSchema)]
    struct TestTask {
        id: String,
        name: String,
    }

    #[test]
    fn test_type_name() {
        assert_eq!(TestTask::type_name(), "TestTask");
        assert_eq!(UnitToken::type_name(), "UnitToken");
    }

    #[test]
    fn test_schema_ref() {
        assert_eq!(TestTask::schema_ref(), "#/definitions/TestTask");
    }

    #[test]
    fn test_json_schema_extraction() {
        let schema = TestTask::extract_schema();
        assert!(schema.is_object());
        // Schema should have a "properties" field for structs
        let obj = schema.as_object().unwrap();
        assert!(
            obj.contains_key("$schema") || obj.contains_key("title") || obj.contains_key("type")
        );
    }
}
