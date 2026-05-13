//! Configuration types for file-ops job specifications.
//!
//! Each operation is represented as a JSON object with an `"operation"` key
//! that selects the variant. The remaining fields are operation-specific.
//! All operations carry their own inline [`StorageConfig`] — there is no
//! default storage backend.
//!
//! These types are deserialized from
//! [`ExecutionSpec::config`](aithericon_executor_domain::ExecutionSpec) at
//! runtime by [`FileOpsBackend`](crate::FileOpsBackend).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use aithericon_executor_domain::{ExecutionSpec, ExecutorError};
use aithericon_executor_storage::StorageConfig;

/// Compression algorithm for streaming copy/move transfers.
///
/// When specified on a copy or move config, the transfer uses a
/// constant-memory streaming pipeline instead of buffered read+write.
///
/// Both `decompress` and `compress` can be set on the same operation to
/// transcode between formats (e.g. gzip → zstd).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    /// Gzip (RFC 1952). Produces files with magic bytes `1f 8b`.
    Gzip,
    /// Zstandard (RFC 8878). Produces files with magic bytes `28 b5 2f fd`.
    Zstd,
}

/// Tagged enum of all file operations.
///
/// Deserialized from `ExecutionSpec.config` at runtime by `FileOpsBackend`.
/// The `operation` field selects the variant.
///
/// Every operation carries its own `StorageConfig` — there is no default
/// operator. For copy/move, `source_storage` is required and
/// `destination_storage` defaults to `source_storage` when omitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        serde_json::from_value(spec.config.clone()).map_err(|e| {
            ExecutorError::Config(format!("invalid file_ops backend config: {e}"))
        })
    }
}

/// Extract file metadata and checksum via the `fmeta` library.
///
/// Downloads the file to a temp location, runs format detection and metadata
/// extraction, then cleans up. Supports CSV, Parquet, JSON, Excel, and more.
///
/// # Job specification
///
/// ```json
/// {
///   "operation": "probe",
///   "path": "datasets/train.parquet",
///   "include_statistics": true,
///   "storage": {
///     "backend": "s3",
///     "endpoint": "https://s3.amazonaws.com",
///     "bucket": "ml-data"
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeConfig {
    /// Storage path to probe.
    pub path: String,

    /// Whether to include column-level statistics in output.
    #[serde(default)]
    pub include_statistics: bool,

    /// Storage backend to use.
    pub storage: StorageConfig,
}

/// Copy a file within or across storage backends.
///
/// Uses constant-memory streaming. Same-backend copies attempt the native
/// `copy()` operation first and fall back to streaming if unsupported.
/// Cross-backend copies and copies with compression always stream.
///
/// # Job specifications
///
/// **Same-backend copy:**
/// ```json
/// {
///   "operation": "copy",
///   "source": "raw/data.csv",
///   "destination": "processed/data.csv",
///   "source_storage": { "backend": "local", "endpoint": "/data" }
/// }
/// ```
///
/// **Cross-backend copy (S3 → GCS):**
/// ```json
/// {
///   "operation": "copy",
///   "source": "exports/dump.parquet",
///   "destination": "imports/dump.parquet",
///   "source_storage": { "backend": "s3", "endpoint": "https://s3.amazonaws.com", "bucket": "src" },
///   "destination_storage": { "backend": "gcs", "endpoint": "https://storage.googleapis.com", "bucket": "dst" }
/// }
/// ```
///
/// **Copy with gzip compression:**
/// ```json
/// {
///   "operation": "copy",
///   "source": "raw/data.csv",
///   "destination": "archive/data.csv.gz",
///   "source_storage": { "backend": "s3", "endpoint": "https://s3.amazonaws.com", "bucket": "data" },
///   "compress": "gzip"
/// }
/// ```
///
/// **Transcode (decompress gzip, recompress as zstd):**
/// ```json
/// {
///   "operation": "copy",
///   "source": "archive/data.csv.gz",
///   "destination": "warehouse/data.csv.zst",
///   "source_storage": { "backend": "s3", "endpoint": "https://s3.amazonaws.com", "bucket": "data" },
///   "decompress": "gzip",
///   "compress": "zstd"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyConfig {
    /// Source storage path.
    pub source: String,

    /// Destination storage path.
    pub destination: String,

    /// Storage backend for the source file.
    pub source_storage: StorageConfig,

    /// Storage backend for the destination file.
    /// Defaults to `source_storage` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_storage: Option<StorageConfig>,

    /// Decompress the source stream during transfer.
    /// Forces the streaming path even for same-backend copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decompress: Option<Compression>,

    /// Compress the destination stream during transfer.
    /// Forces the streaming path even for same-backend copies.
    /// Set both `decompress` and `compress` to transcode between formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress: Option<Compression>,
}

/// Move (rename) a file within or across storage backends.
///
/// Semantically equivalent to copy + delete source. Same-backend moves
/// attempt atomic `rename()` first, then `copy()` + delete, then streaming
/// + delete. Cross-backend moves and moves with compression always stream.
///
/// # Job specifications
///
/// **Same-backend move:**
/// ```json
/// {
///   "operation": "move",
///   "source": "staging/upload.csv",
///   "destination": "final/upload.csv",
///   "source_storage": { "backend": "local", "endpoint": "/data" }
/// }
/// ```
///
/// **Move with compression:**
/// ```json
/// {
///   "operation": "move",
///   "source": "staging/data.csv",
///   "destination": "archive/data.csv.zst",
///   "source_storage": { "backend": "s3", "endpoint": "https://s3.amazonaws.com", "bucket": "data" },
///   "compress": "zstd"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveConfig {
    /// Source storage path.
    pub source: String,

    /// Destination storage path.
    pub destination: String,

    /// Storage backend for the source file.
    pub source_storage: StorageConfig,

    /// Storage backend for the destination file.
    /// Defaults to `source_storage` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_storage: Option<StorageConfig>,

    /// Decompress the source stream during transfer.
    /// Forces the streaming path even for same-backend moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decompress: Option<Compression>,

    /// Compress the destination stream during transfer.
    /// Forces the streaming path even for same-backend moves.
    /// Set both `decompress` and `compress` to transcode between formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress: Option<Compression>,
}

/// Delete a file from storage.
///
/// Returns [`FileOpsError::NotFound`](crate::ops::FileOpsError::NotFound) if
/// the file does not exist and `ignore_missing` is false.
///
/// # Job specification
///
/// ```json
/// {
///   "operation": "delete",
///   "path": "temp/scratch.csv",
///   "ignore_missing": true,
///   "storage": { "backend": "local", "endpoint": "/data" }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteConfig {
    /// Storage path to delete.
    pub path: String,

    /// If true, do not error when the file does not exist.
    #[serde(default)]
    pub ignore_missing: bool,

    /// Storage backend to use.
    pub storage: StorageConfig,
}

/// Write or merge a `.meta.json` sidecar file next to the target.
///
/// The sidecar is stored at `<path>.meta.json`. With `merge: true`, existing
/// sidecar keys are preserved and new keys overwrite on conflict. With
/// `merge: false`, the sidecar is replaced entirely.
///
/// The target file must exist; otherwise
/// [`FileOpsError::NotFound`](crate::ops::FileOpsError::NotFound) is returned.
///
/// # Job specification
///
/// ```json
/// {
///   "operation": "annotate",
///   "path": "datasets/train.parquet",
///   "annotations": { "owner": "ml-team", "version": 3 },
///   "merge": true,
///   "storage": { "backend": "s3", "endpoint": "https://s3.amazonaws.com", "bucket": "data" }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotateConfig {
    /// Storage path of the target file.
    pub path: String,

    /// Key-value annotations to write.
    pub annotations: HashMap<String, serde_json::Value>,

    /// If true, deep-merge with existing sidecar; if false, overwrite.
    #[serde(default)]
    pub merge: bool,

    /// Storage backend to use.
    pub storage: StorageConfig,
}

/// List files under a storage prefix.
///
/// Directory markers (paths ending with `/`) are automatically skipped.
/// With `include_stat: true`, each entry becomes an object with `path`,
/// `content_length`, and optionally `last_modified`; otherwise entries are
/// plain path strings.
///
/// # Job specification
///
/// ```json
/// {
///   "operation": "list",
///   "prefix": "datasets/",
///   "limit": 100,
///   "include_stat": true,
///   "storage": { "backend": "s3", "endpoint": "https://s3.amazonaws.com", "bucket": "data" }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListConfig {
    /// Storage prefix to list under.
    pub prefix: String,

    /// Maximum number of entries to return.
    #[serde(default)]
    pub limit: Option<usize>,

    /// Whether to include stat info (size, last_modified) per entry.
    #[serde(default)]
    pub include_stat: bool,

    /// Storage backend to use.
    pub storage: StorageConfig,
}

/// Get file metadata (existence, size, last modified, content type, etag).
///
/// Stat of a non-existent file is **not** an error — it returns
/// `{ "exists": false }` in the output. This makes stat safe to use as a
/// pre-flight check.
///
/// # Job specification
///
/// ```json
/// {
///   "operation": "stat",
///   "path": "datasets/train.parquet",
///   "storage": { "backend": "s3", "endpoint": "https://s3.amazonaws.com", "bucket": "data" }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatConfig {
    /// Storage path to stat.
    pub path: String,

    /// Storage backend to use.
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

    fn s3_storage_json() -> serde_json::Value {
        serde_json::json!({
            "backend": "s3",
            "endpoint": "https://s3.amazonaws.com",
            "bucket": "data-lake",
            "region": "us-east-1",
            "credentials": {
                "access_key": "AKIA...",
                "secret_key": "wJa..."
            }
        })
    }

    fn gcs_storage_json() -> serde_json::Value {
        serde_json::json!({
            "backend": "gcs",
            "endpoint": "https://storage.googleapis.com",
            "bucket": "ml-staging",
            "prefix": "v2/"
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
        assert!(matches!(config, FileOpsConfig::Probe(ref c) if c.path == "data/train.parquet" && c.include_statistics));
        let reserialized = serde_json::to_string(&config).unwrap();
        let roundtripped: FileOpsConfig = serde_json::from_str(&reserialized).unwrap();
        assert!(matches!(roundtripped, FileOpsConfig::Probe(_)));
    }

    #[test]
    fn copy_config_same_backend() {
        let json = serde_json::json!({
            "operation": "copy",
            "source": "raw/data.csv",
            "destination": "processed/data.csv",
            "source_storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        match &config {
            FileOpsConfig::Copy(c) => {
                assert_eq!(c.source, "raw/data.csv");
                assert!(c.destination_storage.is_none());
            }
            _ => panic!("expected Copy"),
        }
    }

    #[test]
    fn copy_config_cross_backend() {
        let json = serde_json::json!({
            "operation": "copy",
            "source": "raw/data.csv",
            "destination": "imported/data.csv",
            "source_storage": s3_storage_json(),
            "destination_storage": gcs_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        match &config {
            FileOpsConfig::Copy(c) => {
                assert!(c.destination_storage.is_some());
            }
            _ => panic!("expected Copy"),
        }
    }

    #[test]
    fn move_config_roundtrip() {
        let json = serde_json::json!({
            "operation": "move",
            "source": "staging/upload.parquet",
            "destination": "final/upload.parquet",
            "source_storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        assert!(matches!(config, FileOpsConfig::Move(ref c) if c.source == "staging/upload.parquet"));
    }

    #[test]
    fn delete_config_roundtrip() {
        let json = serde_json::json!({
            "operation": "delete",
            "path": "temp/scratch.csv",
            "ignore_missing": true,
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        assert!(matches!(config, FileOpsConfig::Delete(ref c) if c.ignore_missing));
    }

    #[test]
    fn delete_config_defaults() {
        let json = serde_json::json!({
            "operation": "delete",
            "path": "temp/scratch.csv",
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        assert!(matches!(config, FileOpsConfig::Delete(ref c) if !c.ignore_missing));
    }

    #[test]
    fn annotate_config_roundtrip() {
        let json = serde_json::json!({
            "operation": "annotate",
            "path": "data/train.parquet",
            "annotations": {"owner": "ml-team"},
            "merge": true,
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        assert!(matches!(config, FileOpsConfig::Annotate(ref c) if c.merge));
    }

    #[test]
    fn list_config_roundtrip() {
        let json = serde_json::json!({
            "operation": "list",
            "prefix": "datasets/",
            "limit": 100,
            "include_stat": true,
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        assert!(matches!(config, FileOpsConfig::List(ref c) if c.limit == Some(100) && c.include_stat));
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
    fn from_spec_valid() {
        let spec = ExecutionSpec {
            backend: "file_ops".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({
                "operation": "stat",
                "path": "test.csv",
                "storage": local_storage_json()
            }),
        };
        let config = FileOpsConfig::from_spec(&spec).unwrap();
        assert!(matches!(config, FileOpsConfig::Stat(_)));
    }

    #[test]
    fn from_spec_invalid() {
        let spec = ExecutionSpec {
            backend: "file_ops".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({"bad": "config"}),
        };
        assert!(FileOpsConfig::from_spec(&spec).is_err());
    }

    #[test]
    fn from_spec_unknown_operation() {
        let spec = ExecutionSpec {
            backend: "file_ops".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({"operation": "unknown", "path": "test"}),
        };
        assert!(FileOpsConfig::from_spec(&spec).is_err());
    }

    #[test]
    fn copy_config_with_compress() {
        let json = serde_json::json!({
            "operation": "copy",
            "source": "raw/data.csv",
            "destination": "archive/data.csv.gz",
            "source_storage": local_storage_json(),
            "compress": "gzip"
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        match &config {
            FileOpsConfig::Copy(c) => {
                assert_eq!(c.compress, Some(Compression::Gzip));
                assert_eq!(c.decompress, None);
            }
            _ => panic!("expected Copy"),
        }
        // roundtrip
        let reserialized = serde_json::to_string(&config).unwrap();
        let rt: FileOpsConfig = serde_json::from_str(&reserialized).unwrap();
        assert!(matches!(rt, FileOpsConfig::Copy(ref c) if c.compress == Some(Compression::Gzip)));
    }

    #[test]
    fn copy_config_with_decompress() {
        let json = serde_json::json!({
            "operation": "copy",
            "source": "archive/data.csv.gz",
            "destination": "raw/data.csv",
            "source_storage": local_storage_json(),
            "decompress": "gzip"
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        match &config {
            FileOpsConfig::Copy(c) => {
                assert_eq!(c.decompress, Some(Compression::Gzip));
                assert_eq!(c.compress, None);
            }
            _ => panic!("expected Copy"),
        }
    }

    #[test]
    fn copy_config_transcode() {
        let json = serde_json::json!({
            "operation": "copy",
            "source": "archive/data.csv.gz",
            "destination": "warehouse/data.csv.zst",
            "source_storage": s3_storage_json(),
            "destination_storage": gcs_storage_json(),
            "decompress": "gzip",
            "compress": "zstd"
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        match &config {
            FileOpsConfig::Copy(c) => {
                assert_eq!(c.decompress, Some(Compression::Gzip));
                assert_eq!(c.compress, Some(Compression::Zstd));
                assert!(c.destination_storage.is_some());
            }
            _ => panic!("expected Copy"),
        }
    }

    #[test]
    fn move_config_with_compress() {
        let json = serde_json::json!({
            "operation": "move",
            "source": "staging/upload.csv",
            "destination": "archive/upload.csv.zst",
            "source_storage": local_storage_json(),
            "compress": "zstd"
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        match &config {
            FileOpsConfig::Move(c) => {
                assert_eq!(c.compress, Some(Compression::Zstd));
                assert_eq!(c.decompress, None);
            }
            _ => panic!("expected Move"),
        }
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
    fn stat_missing_storage_fails() {
        let json = serde_json::json!({
            "operation": "stat",
            "path": "test.csv"
        });
        assert!(serde_json::from_value::<FileOpsConfig>(json).is_err());
    }
}
