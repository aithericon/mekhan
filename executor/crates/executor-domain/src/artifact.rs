use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Category of an artifact produced during execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ArtifactCategory {
    Model,
    Dataset,
    Plot,
    Log,
    Checkpoint,
    Config,
    Metric,
    #[default]
    Other,
}

/// A single artifact produced by an execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Artifact {
    /// Unique artifact identifier.
    pub id: String,

    /// The execution that produced this artifact.
    pub execution_id: String,

    /// Human-readable name.
    pub name: String,

    /// Artifact category.
    #[serde(default)]
    pub category: ArtifactCategory,

    /// Original filename.
    pub filename: String,

    /// MIME type if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,

    /// File size in bytes if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,

    /// Path in the artifact store (set after upload).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_path: Option<String>,

    /// Registered by reference instead of uploaded. When the SDK calls
    /// `log_artifact(..., upload=False)`, the bytes are NOT copied into the
    /// object store; the artifact is still hashed and catalogued, but its
    /// physical location is `(file_server_id, reference_path)` rather than a
    /// `storage_path`.
    #[serde(default)]
    pub by_reference: bool,

    /// Identifier of the file server holding a by-reference artifact's bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_server_id: Option<String>,

    /// Physical path of a by-reference artifact on its file server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_path: Option<String>,

    /// Extracted file metadata as JSON (avoids hard dependency on file-metadata crate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_metadata: Option<serde_json::Value>,

    /// User-defined key-value metadata.
    #[serde(default)]
    pub metadata: HashMap<String, String>,

    /// When this artifact was created.
    pub created_at: DateTime<Utc>,
}

/// Manifest of all artifacts for an execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArtifactManifest {
    /// The execution this manifest belongs to.
    pub execution_id: String,

    /// All artifacts produced.
    pub artifacts: Vec<Artifact>,

    /// When this manifest was last updated.
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_serde_roundtrip() {
        let artifact = Artifact {
            id: "art-001".into(),
            execution_id: "exec-123".into(),
            name: "model.pt".into(),
            category: ArtifactCategory::Model,
            filename: "model.pt".into(),
            mime_type: Some("application/octet-stream".into()),
            size_bytes: Some(1024),
            storage_path: Some("artifacts/exec-123/art-001/model.pt".into()),
            by_reference: false,
            file_server_id: None,
            reference_path: None,
            file_metadata: None,
            metadata: HashMap::from([("epoch".into(), "10".into())]),
            created_at: Utc::now(),
        };

        let json = serde_json::to_string(&artifact).unwrap();
        let deserialized: Artifact = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "art-001");
        assert_eq!(deserialized.category, ArtifactCategory::Model);
    }

    #[test]
    fn manifest_serde_roundtrip() {
        let manifest = ArtifactManifest {
            execution_id: "exec-123".into(),
            artifacts: vec![],
            updated_at: Utc::now(),
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: ArtifactManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.execution_id, "exec-123");
    }

    #[test]
    fn category_default_is_other() {
        assert_eq!(ArtifactCategory::default(), ArtifactCategory::Other);
    }
}
