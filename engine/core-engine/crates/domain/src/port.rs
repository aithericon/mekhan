use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Defines how many tokens a port expects and how they are passed to the script.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortCardinality {
    /// Expects exactly 1 token. Script receives the token data as a single object.
    #[default]
    Single,
    /// Expects weight >= 1 tokens. Script receives token data as an array.
    Batch,
}

/// A named port on a transition that defines an input or output connection point.
/// Ports are the "pins" on the transition "chip" that arcs connect to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Port {
    /// The name of the port (used in scripts and arc connections)
    pub name: String,

    /// Optional schema reference for type validation (e.g., "Signal", "Resource")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_ref: Option<String>,

    /// How many tokens this port expects
    #[serde(default)]
    pub cardinality: PortCardinality,
}

impl Port {
    /// Create a new port with the given name and default cardinality (Single).
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            schema_ref: None,
            cardinality: PortCardinality::Single,
        }
    }

    /// Create a new port with Batch cardinality.
    pub fn batch(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            schema_ref: None,
            cardinality: PortCardinality::Batch,
        }
    }

    /// Set the schema reference for this port.
    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema_ref = Some(schema.into());
        self
    }

    /// Set the cardinality for this port.
    pub fn with_cardinality(mut self, cardinality: PortCardinality) -> Self {
        self.cardinality = cardinality;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_new() {
        let port = Port::new("signal");
        assert_eq!(port.name, "signal");
        assert_eq!(port.cardinality, PortCardinality::Single);
        assert!(port.schema_ref.is_none());
    }

    #[test]
    fn test_port_batch() {
        let port = Port::batch("resources");
        assert_eq!(port.name, "resources");
        assert_eq!(port.cardinality, PortCardinality::Batch);
    }

    #[test]
    fn test_port_with_schema() {
        let port = Port::new("signal").with_schema("Signal");
        assert_eq!(port.schema_ref, Some("Signal".to_string()));
    }

    #[test]
    fn test_port_serialization() {
        let port = Port::new("request").with_schema("BookingRequest");
        let json = serde_json::to_string(&port).unwrap();
        let deserialized: Port = serde_json::from_str(&json).unwrap();
        assert_eq!(port, deserialized);
    }

    #[test]
    fn test_port_cardinality_default() {
        let cardinality = PortCardinality::default();
        assert_eq!(cardinality, PortCardinality::Single);
    }

    #[test]
    fn test_port_json_format() {
        let port = Port::batch("items").with_schema("Item");
        let json = serde_json::to_string_pretty(&port).unwrap();

        // Verify snake_case serialization
        assert!(json.contains("\"cardinality\": \"batch\""));
        assert!(json.contains("\"name\": \"items\""));
        assert!(json.contains("\"schema_ref\": \"Item\""));
    }
}
