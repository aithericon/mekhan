//! Wire-format config types for the file_ops backend.
//!
//! Deserialize-only mirrors of what `ExecutionSpec.config` carries. The
//! executor-file-ops crate consumes these for runtime execution; the compiler
//! consumes them for compile-time validation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use aithericon_executor_domain::{ExecutionSpec, ExecutorError};
use aithericon_executor_storage_types::StorageConfig;
use aithericon_file_metadata::ChecksumAlgorithm;

use crate::interp::Interpolable;

/// Default crawl batch size — number of `{path,size,mtime}` entries accumulated
/// before a streaming `item` event is emitted. Sized to keep per-batch JSON
/// payloads well under the NATS max-payload while still amortizing emit overhead
/// across the ~4M-file corpus the crawler targets.
fn default_crawl_batch_size() -> Interpolable<usize> {
    Interpolable::Value(5000)
}

fn default_crawl_probe_concurrency() -> Interpolable<usize> {
    Interpolable::Value(8)
}

/// Default for `CrawlConfig.stat` — crawl `stat()`s each entry by default
/// because the OpenDAL `fs` lister returns entries without
/// `content_length`/`last_modified`, so size+mtime require a per-entry stat.
fn default_crawl_stat() -> bool {
    true
}

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
    Crawl(CrawlConfig),
}

impl FileOpsConfig {
    /// Deserialize a FileOpsConfig from an ExecutionSpec's config field.
    pub fn from_spec(spec: &ExecutionSpec) -> Result<Self, ExecutorError> {
        crate::from_spec(spec, "file_ops")
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
    /// Optional checksum algorithm override. When set, probe forces this
    /// algorithm for the emitted `checksum`/`checksum_digest` instead of the
    /// `extract_metadata_async` default (SHA-256). Set to `sha256` for the
    /// legacy-migration reconcile path, whose join key is the bare-lowercase-hex
    /// SHA-256 digest (`legacy_file_index.hash`, `"SHA256:"` stripped). The
    /// emitted `checksum_digest` is always bare lowercase hex so it can be
    /// compared directly against catalogue content hashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schema(value_type = Option<String>))]
    pub checksum_algo: Option<ChecksumAlgorithm>,
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

/// Recursive, streaming directory walk — `list`'s checkpointable sibling.
///
/// Drives an OpenDAL recursive lister as a stream (never buffering the whole
/// listing), `stat()`ing each file for `{path, size, mtime}` and emitting
/// fixed-size batches over the job's [`EventStream`](aithericon_executor_backend)
/// `item()`/`close()` channel. Mandatory at the ~4M-file scale of the legacy
/// migration: `list` buffers the entire `Vec` and the `fs` lister returns no
/// size/mtime, so crawl streams + per-entry stats instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct CrawlConfig {
    /// Prefix (relative to the storage root) to walk recursively.
    pub prefix: String,
    /// Storage the walk targets — typically an `fs://` mount co-located with a
    /// file server.
    pub storage: StorageConfig,
    /// Number of `{path,size,mtime}` entries per emitted `item` batch.
    /// Interpolation-capable: may be authored as a `{{ ... }}` placeholder
    /// resolving to a number (e.g. a Start field carrying campaign sizing).
    #[serde(default = "default_crawl_batch_size")]
    #[cfg_attr(feature = "schema", schema(value_type = usize))]
    pub batch_size: Interpolable<usize>,
    /// Optional resume cursor: the walk resumes *after* this path. Native
    /// `start_after` on backends that support it (S3); elsewhere a
    /// client-side skip-until-cursor (readdir-cheap, assumes stable
    /// enumeration order on an unchanged tree). An empty string counts as
    /// absent (interpolated campaign configs deliver `""` on iteration 0).
    /// True idempotency comes from the inventory
    /// `UNIQUE(file_server_id, path)` upsert downstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from: Option<String>,
    /// Whether to `stat()` each entry for size+mtime. Defaults to `true` because
    /// the OpenDAL `fs` lister omits `content_length`/`last_modified`.
    #[serde(default = "default_crawl_stat")]
    pub stat: bool,
    /// Optional cap on the number of *filled* batches per invocation — the
    /// chunking knob for cursor-loop campaigns (`resume_from` carries the
    /// cursor between invocations). `None` walks to exhaustion.
    /// Interpolation-capable like `batch_size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schema(value_type = Option<u64>))]
    pub max_batches: Option<Interpolable<u64>>,
    /// Opt-in batch-fold sink (docs/32): when set, each batch is published
    /// durably to the `INVENTORY_FOLD` stream for set-based folding into the
    /// inventory, and NO per-file channel items are emitted. When `None`
    /// (default), batches ride the `crawl` EventStream channel as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink: Option<CrawlSinkConfig>,
    /// Opt-in per-entry content probing during the walk:
    /// * `"hash"` — read each file once and emit its SHA-256 (bare lowercase
    ///   hex — the catalogue `content_hash` / reconcile join shape);
    /// * `"full"` — hash PLUS `fmeta` metadata extraction (format, mime,
    ///   tabular stats); unsupported formats degrade to checksum-only.
    ///
    /// Absent / empty string = metadata-only walk (the default — integrity
    /// hashing then remains the separate `probe` op's job). A file that fails
    /// to probe is emitted hashless and counted in the `probe_errors` output
    /// instead of failing the walk. With `"full"`, keep `batch_size` modest
    /// (≤ ~500): each item carries its metadata blob inside one sink publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<String>,
    /// Max number of files probed CONCURRENTLY during the walk (`probe` =
    /// `hash`/`full`). The walk lists + `stat`s sequentially (cheap), but the
    /// per-file content read + SHA-256 + `fmeta` parse is the expensive,
    /// frequently latency-bound step — running several in flight overlaps disk
    /// seeks (especially many small files on a RAID array) and CPU parse.
    /// Results are still consumed in listing order, so `last_path` (the resume
    /// cursor) stays exact. `1` restores the historical one-at-a-time walk.
    /// Default 8. Lower it for huge-file corpora — each in-flight `full` probe
    /// holds its own read/parse buffers, so N concurrent multi-GB probes cost
    /// ~N× the per-probe memory. Has no effect when `probe` is off.
    /// Interpolation-capable like `batch_size`.
    #[serde(default = "default_crawl_probe_concurrency")]
    #[cfg_attr(feature = "schema", schema(value_type = usize))]
    pub probe_concurrency: Interpolable<usize>,
}

/// Where (and how) sink-mode crawl batches are folded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct CrawlSinkConfig {
    /// Fold discipline: `"reconcile"` (classify against the legacy baseline)
    /// or `"index"` (plain inventory upsert).
    pub mode: String,
    /// Inventory server key the crawled paths belong to
    /// (`file_inventory.file_server_id`).
    pub file_server_id: String,
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
    fn probe_config_checksum_algo() {
        let json = serde_json::json!({
            "operation": "probe",
            "path": "data/file.bin",
            "checksum_algo": "sha256",
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        assert!(matches!(
            config,
            FileOpsConfig::Probe(ref c) if c.checksum_algo == Some(ChecksumAlgorithm::Sha256)
        ));
    }

    #[test]
    fn probe_config_checksum_algo_defaults_none() {
        let json = serde_json::json!({
            "operation": "probe",
            "path": "data/file.bin",
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        assert!(matches!(
            config,
            FileOpsConfig::Probe(ref c) if c.checksum_algo.is_none()
        ));
    }

    #[test]
    fn crawl_config_roundtrip_defaults() {
        let json = serde_json::json!({
            "operation": "crawl",
            "prefix": "Data/",
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        match config {
            FileOpsConfig::Crawl(c) => {
                assert_eq!(c.prefix, "Data/");
                assert_eq!(c.batch_size, 5000.into());
                assert!(c.stat);
                assert!(c.resume_from.is_none());
                assert!(c.max_batches.is_none());
                assert!(c.sink.is_none());
            }
            other => panic!("expected Crawl, got {other:?}"),
        }
    }

    #[test]
    fn crawl_config_sink_and_max_batches_roundtrip() {
        let json = serde_json::json!({
            "operation": "crawl",
            "prefix": "Data/",
            "max_batches": 50,
            "sink": { "mode": "reconcile", "file_server_id": "legacy-nas-2" },
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        match config {
            FileOpsConfig::Crawl(c) => {
                assert_eq!(c.max_batches, Some(50.into()));
                let sink = c.sink.expect("sink");
                assert_eq!(sink.mode, "reconcile");
                assert_eq!(sink.file_server_id, "legacy-nas-2");
                // Optional fields stay off the wire when unset (events-mode
                // configs remain byte-identical to pre-sink ones).
                let back = serde_json::to_value(FileOpsConfig::Crawl(CrawlConfig {
                    sink: None,
                    probe: None,
                    max_batches: None,
                    prefix: "Data/".into(),
                    storage: serde_json::from_value(local_storage_json()).unwrap(),
                    batch_size: 5000.into(),
                    resume_from: None,
                    stat: true,
                    probe_concurrency: 8.into(),
                }))
                .unwrap();
                assert!(back.get("sink").is_none());
                assert!(back.get("max_batches").is_none());
            }
            other => panic!("expected Crawl, got {other:?}"),
        }
    }

    #[test]
    fn crawl_config_roundtrip_explicit() {
        let json = serde_json::json!({
            "operation": "crawl",
            "prefix": "Data/",
            "batch_size": 100,
            "resume_from": "Data/x/last.txt",
            "stat": false,
            "storage": local_storage_json()
        });
        let config: FileOpsConfig = serde_json::from_value(json).unwrap();
        match config {
            FileOpsConfig::Crawl(c) => {
                assert_eq!(c.batch_size, 100.into());
                assert_eq!(c.resume_from.as_deref(), Some("Data/x/last.txt"));
                assert!(!c.stat);
            }
            other => panic!("expected Crawl, got {other:?}"),
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
