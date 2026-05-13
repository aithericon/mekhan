//! Typed signal token structs for external signal payloads.
//!
//! These structs document the shapes of tokens injected by external signal
//! sources. They are **not** enforced at the engine level — the engine always
//! uses `serde_json::Value`. Instead, these provide:
//!
//! 1. **Documentation** — exact field names and types, with Rhai access patterns
//! 2. **Deserialization** — `serde::Deserialize` for type-safe signal handling in Rust
//! 3. **Schema generation** — `JsonSchema` for use as `token_schema` on signal places
//!
//! # Important: payload becomes the token
//!
//! `ExternalSignal.payload` becomes the token color directly — there is no
//! wrapping. In Rhai, you access `signal.artifact`, **not** `signal.payload.artifact`.
//!
//! # Catalogue subscription signals
//!
//! Mekhan publishes catalogue signals when a new artifact matches a subscription
//! filter. The outer NATS envelope contains `{ source, signal_key, payload, timestamp }`,
//! but the engine extracts `payload` and injects it as the token. So the token
//! shape is [`CatalogueSignalToken`].
//!
//! ```ignore
//! let catalogue_events = ctx.signal::<CatalogueSignalToken>(
//!     "catalogue_events",
//!     "Catalogue Events",
//! );
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ─── Catalogue signal tokens ─────────────────────────────────────────────────

/// Top-level token for catalogue subscription signals.
///
/// Published by Mekhan when a newly catalogued artifact matches a subscription
/// filter. The engine unwraps `ExternalSignal.payload` so this struct **is**
/// the token color — no outer wrapper.
///
/// # Rhai access pattern
///
/// ```rhai
/// let path = signal.artifact.storage_path;
/// let name = signal.artifact.name;
/// let sub_id = signal.subscription_id;
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CatalogueSignalToken {
    /// Always `"catalogue"`.
    pub source: String,
    /// Subscription that matched this artifact.
    pub subscription_id: String,
    /// The matching catalogue artifact.
    pub artifact: CatalogueArtifact,
}

/// Catalogue entry metadata as delivered in signal tokens.
///
/// Mirrors the fields of Mekhan's `CatalogueEntry` that are serialized into
/// the signal payload. Nullable database columns are `Option<T>`.
///
/// # Rhai access pattern
///
/// ```rhai
/// let id = signal.artifact.id;
/// let path = signal.artifact.storage_path;
/// let category = signal.artifact.category;
/// let trace = signal.artifact.trace_id;
/// let meta = signal.artifact.file_metadata;
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CatalogueArtifact {
    /// Unique artifact identifier.
    pub id: String,
    /// Execution that produced this artifact.
    pub execution_id: String,
    /// Optional job identifier.
    #[serde(default)]
    pub job_id: Option<String>,
    /// Human-readable artifact name.
    pub name: String,
    /// Artifact category (e.g., `"model"`, `"dataset"`, `"plot"`).
    pub category: String,
    /// Original filename.
    pub filename: String,
    /// MIME type (e.g., `"application/octet-stream"`).
    #[serde(default)]
    pub mime_type: Option<String>,
    /// File size in bytes.
    #[serde(default)]
    pub size_bytes: Option<i64>,
    /// Storage path (S3 key or local path).
    #[serde(default)]
    pub storage_path: Option<String>,
    /// Source net identifier.
    #[serde(default)]
    pub source_net: Option<String>,
    /// Source place within the net.
    #[serde(default)]
    pub source_place: Option<String>,
    /// Correlation identifier for cross-net tracing.
    #[serde(default)]
    pub correlation_id: Option<String>,
    /// HPI process identifier.
    #[serde(default)]
    pub process_id: Option<String>,
    /// Process step label.
    #[serde(default)]
    pub process_step: Option<String>,
    /// Lineage trace identifier.
    #[serde(default)]
    pub trace_id: Option<String>,
    /// Extracted file metadata (content-dependent).
    #[serde(default)]
    pub file_metadata: serde_json::Value,
    /// User-supplied metadata key-value pairs.
    #[serde(default)]
    pub user_metadata: serde_json::Value,
    /// ISO 8601 creation timestamp.
    #[serde(default)]
    pub created_at: Option<String>,
    /// ISO 8601 cataloguing timestamp.
    #[serde(default)]
    pub catalogued_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> serde_json::Value {
        serde_json::json!({
            "source": "catalogue",
            "subscription_id": "sub-001",
            "artifact": {
                "id": "art-123",
                "execution_id": "exec-456",
                "job_id": "job-789",
                "name": "trained_model.pt",
                "category": "model",
                "filename": "trained_model.pt",
                "mime_type": "application/octet-stream",
                "size_bytes": 104857600,
                "storage_path": "s3://bucket/models/trained_model.pt",
                "source_net": "training-net",
                "source_place": "p_output",
                "correlation_id": "corr-abc",
                "process_id": "proc-def",
                "process_step": "training",
                "trace_id": "trace-ghi",
                "file_metadata": { "format": "pytorch", "version": "2.0" },
                "user_metadata": { "experiment": "baseline" },
                "created_at": "2026-04-04T12:00:00Z",
                "catalogued_at": "2026-04-04T12:00:01Z"
            }
        })
    }

    #[test]
    fn deserialize_full_payload() {
        let val = sample_payload();
        let token: CatalogueSignalToken =
            serde_json::from_value(val).expect("should deserialize");

        assert_eq!(token.source, "catalogue");
        assert_eq!(token.subscription_id, "sub-001");
        assert_eq!(token.artifact.id, "art-123");
        assert_eq!(token.artifact.execution_id, "exec-456");
        assert_eq!(token.artifact.job_id.as_deref(), Some("job-789"));
        assert_eq!(token.artifact.name, "trained_model.pt");
        assert_eq!(token.artifact.category, "model");
        assert_eq!(token.artifact.filename, "trained_model.pt");
        assert_eq!(token.artifact.mime_type.as_deref(), Some("application/octet-stream"));
        assert_eq!(token.artifact.size_bytes, Some(104857600));
        assert_eq!(
            token.artifact.storage_path.as_deref(),
            Some("s3://bucket/models/trained_model.pt")
        );
        assert_eq!(token.artifact.source_net.as_deref(), Some("training-net"));
        assert_eq!(token.artifact.trace_id.as_deref(), Some("trace-ghi"));
        assert_eq!(token.artifact.process_step.as_deref(), Some("training"));
    }

    #[test]
    fn deserialize_minimal_payload() {
        let val = serde_json::json!({
            "source": "catalogue",
            "subscription_id": "sub-002",
            "artifact": {
                "id": "art-minimal",
                "execution_id": "exec-min",
                "name": "output.csv",
                "category": "dataset",
                "filename": "output.csv"
            }
        });

        let token: CatalogueSignalToken =
            serde_json::from_value(val).expect("should deserialize with optional fields missing");

        assert_eq!(token.artifact.id, "art-minimal");
        assert!(token.artifact.mime_type.is_none());
        assert!(token.artifact.size_bytes.is_none());
        assert!(token.artifact.storage_path.is_none());
        assert!(token.artifact.source_net.is_none());
        assert!(token.artifact.trace_id.is_none());
        assert!(token.artifact.job_id.is_none());
        assert!(token.artifact.process_step.is_none());
        assert_eq!(token.artifact.file_metadata, serde_json::Value::Null);
        assert_eq!(token.artifact.user_metadata, serde_json::Value::Null);
    }

    #[test]
    fn roundtrip_serialization() {
        let val = sample_payload();
        let token: CatalogueSignalToken =
            serde_json::from_value(val.clone()).expect("deserialize");
        let reserialized = serde_json::to_value(&token).expect("serialize");

        // Verify key fields survive the roundtrip
        assert_eq!(reserialized["source"], "catalogue");
        assert_eq!(reserialized["subscription_id"], "sub-001");
        assert_eq!(reserialized["artifact"]["id"], "art-123");
        assert_eq!(reserialized["artifact"]["storage_path"], "s3://bucket/models/trained_model.pt");
    }

    #[test]
    fn schema_generation() {
        let schema = schemars::schema_for!(CatalogueSignalToken);
        let schema_json = serde_json::to_value(&schema).expect("schema should serialize");

        // Top-level type is object
        assert_eq!(schema_json["type"], "object");

        // Required fields present
        let required = schema_json["required"].as_array().expect("required array");
        let required_names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_names.contains(&"source"));
        assert!(required_names.contains(&"subscription_id"));
        assert!(required_names.contains(&"artifact"));

        // Artifact definition exists somewhere in the schema
        let schema_str = serde_json::to_string(&schema).expect("stringify");
        assert!(schema_str.contains("CatalogueArtifact"));
    }
}
