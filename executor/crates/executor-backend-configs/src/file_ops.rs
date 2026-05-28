//! Wire-format config types for the file_ops backend.
//!
//! Deserialize-only mirrors of what `ExecutionSpec.config` carries. The
//! executor-file-ops crate consumes these for runtime execution; the compiler
//! consumes them for compile-time validation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use aithericon_executor_domain::{ExecutionSpec, ExecutorError};
use aithericon_executor_storage_types::StorageConfig;

/// Compression algorithm for streaming copy/move transfers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    /// Gzip (RFC 1952). Produces files with magic bytes `1f 8b`.
    Gzip,
    /// Zstandard (RFC 8878). Produces files with magic bytes `28 b5 2f fd`.
    Zstd,
}

/// Tagged enum of all file operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum FileOpsConfig {
    Probe(ProbeConfig),
    Copy(CopyConfig),
    Move(MoveConfig),
    Delete(DeleteConfig),
    Annotate(AnnotateConfig),
    List(ListConfig),
    Stat(StatConfig),
}

impl FileOpsConfig {
    /// Deserialize a FileOpsConfig from an ExecutionSpec's config field.
    pub fn from_spec(spec: &ExecutionSpec) -> Result<Self, ExecutorError> {
        serde_json::from_value(spec.config.clone())
            .map_err(|e| ExecutorError::Config(format!("invalid file_ops backend config: {e}")))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct ProbeConfig {
    pub path: String,
    #[serde(default)]
    pub include_statistics: bool,
    /// Optional. When omitted (e.g. compiler-injected probes against the
    /// platform's own object store), the executor falls back to its
    /// globally-configured default storage — mirroring
    /// `InputSource::StoragePath { storage: Option<_> }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct CopyConfig {
    pub source: String,
    pub destination: String,
    pub source_storage: StorageConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_storage: Option<StorageConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decompress: Option<Compression>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress: Option<Compression>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct MoveConfig {
    pub source: String,
    pub destination: String,
    pub source_storage: StorageConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_storage: Option<StorageConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decompress: Option<Compression>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress: Option<Compression>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct DeleteConfig {
    pub path: String,
    #[serde(default)]
    pub ignore_missing: bool,
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct AnnotateConfig {
    pub path: String,
    pub annotations: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub merge: bool,
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct ListConfig {
    pub prefix: String,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub include_stat: bool,
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct StatConfig {
    pub path: String,
    pub storage: StorageConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_storage_json() -> serde_json::Value {
        serde_json::json!({
            "backend": "local",
            "endpoint": "/tmp/test-storage"
        })
    }

    #[test]
    fn probe_config_roundtrip() {
        let json = serde_json::json!({
            "operation": "probe",
            "path": "data/train.parquet",
            "include_statistics": true,
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        assert!(
            matches!(config, FileOpsConfig::Probe(ref c) if c.path == "data/train.parquet" && c.include_statistics)
        );
    }

    #[test]
    fn stat_config_roundtrip() {
        let json = serde_json::json!({
            "operation": "stat",
            "path": "data/train.parquet",
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        assert!(matches!(config, FileOpsConfig::Stat(ref c) if c.path == "data/train.parquet"));
    }

    #[test]
    fn copy_missing_storage_fails() {
        let json = serde_json::json!({
            "operation": "copy",
            "source": "a.csv",
            "destination": "b.csv"
        });
        assert!(serde_json::from_value::<FileOpsConfig>(json).is_err());
    }

    #[test]
    fn from_spec_unknown_operation() {
        let spec = ExecutionSpec {
            backend: "file_ops".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({"operation": "unknown", "path": "test"}),
            config_ref: None,
        };
        assert!(FileOpsConfig::from_spec(&spec).is_err());
    }
}
