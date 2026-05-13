//! Schema registry for JSON Schema validation of token data.
//!
//! Accepts definitions from AIR format, compiles validators at construction time,
//! and provides validation against `$ref` references on ports and places.

use std::collections::HashMap;

use serde_json::Value;
use thiserror::Error;

/// Errors from schema validation.
#[derive(Error, Debug, Clone)]
pub enum SchemaValidationError {
    #[error("Unknown schema reference: {0}")]
    UnknownSchemaRef(String),

    #[error("Schema validation failed for '{schema_ref}': {message}")]
    ValidationFailed { schema_ref: String, message: String },

    #[error("Schema compilation failed for '{schema_ref}': {message}")]
    CompilationFailed { schema_ref: String, message: String },
}

/// Registry of compiled JSON Schema validators.
///
/// Constructed from the `definitions` map in an AIR scenario. Each definition
/// is compiled into a `jsonschema::Validator` at construction time so that
/// runtime validation is a fast lookup + validate.
pub struct SchemaRegistry {
    /// Compiled validators keyed by definition name (without `#/definitions/` prefix).
    validators: HashMap<String, jsonschema::Validator>,
}

// Safety: jsonschema::Validator is Send + Sync
unsafe impl Send for SchemaRegistry {}
unsafe impl Sync for SchemaRegistry {}

impl SchemaRegistry {
    /// Build a registry from a definitions map.
    ///
    /// Each entry in `definitions` is a JSON Schema object. We construct a root
    /// schema document `{"definitions": {...}}` so that `$ref` resolution works,
    /// then compile a validator per definition using a schema that references it.
    pub fn new(definitions: HashMap<String, Value>) -> Result<Self, SchemaValidationError> {
        let mut validators = HashMap::with_capacity(definitions.len());

        // Build the root document that contains all definitions for $ref resolution
        let root_doc = serde_json::json!({
            "definitions": definitions,
        });

        for name in definitions.keys() {
            // Create a schema that references this definition within the root doc
            let schema = serde_json::json!({
                "definitions": root_doc["definitions"],
                "$ref": format!("#/definitions/{}", name),
            });

            let validator = jsonschema::validator_for(&schema).map_err(|e| {
                SchemaValidationError::CompilationFailed {
                    schema_ref: name.clone(),
                    message: e.to_string(),
                }
            })?;

            validators.insert(name.clone(), validator);
        }

        Ok(Self { validators })
    }

    /// Create an empty registry (no definitions = no validation).
    pub fn empty() -> Self {
        Self {
            validators: HashMap::new(),
        }
    }

    /// Returns true if this registry has no definitions.
    pub fn is_empty(&self) -> bool {
        self.validators.is_empty()
    }

    /// Check whether a schema reference is known.
    pub fn has_schema(&self, schema_ref: &str) -> bool {
        let name = strip_ref_prefix(schema_ref);
        self.validators.contains_key(name)
    }

    /// Validate data against a schema reference.
    ///
    /// `schema_ref` can be either `"#/definitions/Foo"` or just `"Foo"`.
    pub fn validate(&self, schema_ref: &str, data: &Value) -> Result<(), SchemaValidationError> {
        let name = strip_ref_prefix(schema_ref);

        let validator = self
            .validators
            .get(name)
            .ok_or_else(|| SchemaValidationError::UnknownSchemaRef(schema_ref.to_string()))?;

        if let Err(error) = validator.validate(data) {
            return Err(SchemaValidationError::ValidationFailed {
                schema_ref: schema_ref.to_string(),
                message: error.to_string(),
            });
        }

        Ok(())
    }
}

/// Strip the `#/definitions/` prefix if present.
fn strip_ref_prefix(schema_ref: &str) -> &str {
    schema_ref
        .strip_prefix("#/definitions/")
        .unwrap_or(schema_ref)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_definitions() -> HashMap<String, Value> {
        let mut defs = HashMap::new();
        defs.insert(
            "Task".to_string(),
            serde_json::json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string" },
                    "priority": { "type": "integer" }
                }
            }),
        );
        defs.insert(
            "Result".to_string(),
            serde_json::json!({
                "type": "object",
                "required": ["status"],
                "properties": {
                    "status": { "type": "string", "enum": ["success", "failure"] },
                    "value": { "type": "number" }
                }
            }),
        );
        defs
    }

    #[test]
    fn test_empty_registry() {
        let registry = SchemaRegistry::empty();
        assert!(registry.is_empty());
        assert!(!registry.has_schema("Task"));
    }

    #[test]
    fn test_construction_compiles_validators() {
        let registry = SchemaRegistry::new(sample_definitions()).unwrap();
        assert!(!registry.is_empty());
        assert!(registry.has_schema("Task"));
        assert!(registry.has_schema("#/definitions/Task"));
        assert!(registry.has_schema("Result"));
        assert!(!registry.has_schema("Unknown"));
    }

    #[test]
    fn test_validate_pass() {
        let registry = SchemaRegistry::new(sample_definitions()).unwrap();
        let data = serde_json::json!({"name": "build", "priority": 1});
        assert!(registry.validate("Task", &data).is_ok());
        assert!(registry.validate("#/definitions/Task", &data).is_ok());
    }

    #[test]
    fn test_validate_fail_missing_required() {
        let registry = SchemaRegistry::new(sample_definitions()).unwrap();
        let data = serde_json::json!({"priority": 1});
        let err = registry.validate("Task", &data).unwrap_err();
        assert!(matches!(
            err,
            SchemaValidationError::ValidationFailed { .. }
        ));
    }

    #[test]
    fn test_validate_fail_wrong_type() {
        let registry = SchemaRegistry::new(sample_definitions()).unwrap();
        let data = serde_json::json!({"name": 123});
        let err = registry.validate("Task", &data).unwrap_err();
        assert!(matches!(
            err,
            SchemaValidationError::ValidationFailed { .. }
        ));
    }

    #[test]
    fn test_validate_enum() {
        let registry = SchemaRegistry::new(sample_definitions()).unwrap();

        let good = serde_json::json!({"status": "success"});
        assert!(registry.validate("Result", &good).is_ok());

        let bad = serde_json::json!({"status": "unknown"});
        assert!(registry.validate("Result", &bad).is_err());
    }

    #[test]
    fn test_unknown_schema_ref() {
        let registry = SchemaRegistry::new(sample_definitions()).unwrap();
        let data = serde_json::json!({"name": "test"});
        let err = registry.validate("NonExistent", &data).unwrap_err();
        assert!(matches!(err, SchemaValidationError::UnknownSchemaRef(_)));
    }

    #[test]
    fn test_empty_definitions() {
        let registry = SchemaRegistry::new(HashMap::new()).unwrap();
        assert!(registry.is_empty());
    }
}
