//! Typed config values that may still hold a `{{ ... }}` placeholder in an
//! AUTHORED config.
//!
//! The platform's config placeholders (`{{ slug.field }}`, rewritten by the
//! service compiler to `{{input:NAME}}` and substituted **full-value, typed**
//! by the executor's `resolve_inputs` before the config is deserialized) are
//! string sites in the JSON tree — which silently restricted interpolation to
//! `String`-typed config fields: a placeholder sitting in a `usize` field
//! failed the compile-time `serde_json::from_value` shape check.
//!
//! [`Interpolable<T>`] makes a typed field honest about that lifecycle: it
//! deserializes a JSON value of type `T` as [`Interpolable::Value`] and a
//! string as [`Interpolable::Placeholder`]. By execution time substitution has
//! already happened, so [`Interpolable::get`] returns the typed value — and a
//! placeholder that *survived* to execution becomes a precise configuration
//! error instead of a serde type mismatch.

use serde::{Deserialize, Serialize};

/// A config field of type `T` that may, pre-substitution, hold a `{{ ... }}`
/// placeholder string. See the module docs for the lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Interpolable<T> {
    /// The resolved (or directly authored) typed value.
    Value(T),
    /// An unsubstituted placeholder, e.g. `"{{ start.batch_size }}"`.
    Placeholder(String),
}

impl<T: Clone> Interpolable<T> {
    /// Return the typed value, or a config error naming the field if a
    /// placeholder survived to the point of use (i.e. input staging never
    /// substituted it).
    pub fn get(&self, field: &str) -> Result<T, String> {
        match self {
            Self::Value(v) => Ok(v.clone()),
            Self::Placeholder(s) => Err(format!(
                "{field}: placeholder '{s}' was never resolved to a value \
                 (input substitution did not run or the reference was not staged)"
            )),
        }
    }
}

impl<T> From<T> for Interpolable<T> {
    fn from(v: T) -> Self {
        Self::Value(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_deserializes_as_value() {
        let v: Interpolable<usize> = serde_json::from_value(serde_json::json!(42)).unwrap();
        assert_eq!(v, Interpolable::Value(42));
        assert_eq!(v.get("f").unwrap(), 42);
    }

    #[test]
    fn placeholder_string_deserializes_and_errors_on_get() {
        let v: Interpolable<usize> =
            serde_json::from_value(serde_json::json!("{{ start.batch_size }}")).unwrap();
        assert!(matches!(v, Interpolable::Placeholder(_)));
        let err = v.get("crawl: batch_size").unwrap_err();
        assert!(err.contains("crawl: batch_size"), "{err}");
        assert!(err.contains("{{ start.batch_size }}"), "{err}");
    }

    #[test]
    fn serializes_value_as_bare_json() {
        let v: Interpolable<u64> = 7u64.into();
        assert_eq!(serde_json::to_value(&v).unwrap(), serde_json::json!(7));
    }
}
