//! Operation dispatch, shared error types, and validation.
//!
//! The [`dispatch()`] function is the main entry point: it builds OpenDAL
//! operators from the inline [`StorageConfig`] in each operation config, then
//! delegates to the appropriate `<op>::execute()` function.

pub mod annotate;
pub mod copy;
pub mod delete;
pub mod list;
pub mod move_op;
pub mod probe;
pub mod stat;
pub mod streaming;

use std::collections::HashMap;
use std::path::Path;

use opendal::Operator;
use thiserror::Error;

use aithericon_executor_storage::StorageConfig;

use crate::config::FileOpsConfig;

/// Errors specific to file operations.
///
/// In the backend lifecycle, these are caught by `execute()` and surfaced as
/// [`ExecutionOutcome::BackendError`](aithericon_executor_domain::ExecutionOutcome::BackendError)
/// rather than propagated as `Err`.
#[derive(Debug, Error)]
pub enum FileOpsError {
    /// An OpenDAL storage operation failed (network, permissions, etc.).
    #[error("storage error: {0}")]
    Storage(#[from] opendal::Error),

    /// The target path does not exist in storage.
    #[error("file not found: {0}")]
    NotFound(String),

    /// JSON serialization or deserialization failed (e.g. sidecar parsing).
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// The `fmeta` metadata extraction library returned an error.
    #[error("metadata extraction failed: {0}")]
    Metadata(String),

    /// A local filesystem I/O operation failed (e.g. temp file for probe).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The operation config is invalid (caught during `validate()`).
    #[error("configuration error: {0}")]
    Config(String),

    /// An input reference (`{{input:NAME}}`) could not be resolved.
    #[error("input resolution failed: {0}")]
    InputResolution(String),
}

/// Result alias for file operations.
///
/// On success, the `HashMap` contains operation-specific output fields
/// documented on each operation's `execute()` function.
pub type FileOpsResult = Result<HashMap<String, serde_json::Value>, FileOpsError>;

/// Build an OpenDAL Operator from an inline StorageConfig.
fn build_op(config: &StorageConfig) -> Result<(Operator, String), FileOpsError> {
    let op = aithericon_executor_storage::build_operator(config)
        .map_err(|e| FileOpsError::Config(format!("failed to build operator: {e}")))?;
    Ok((op, config.prefix.clone()))
}

/// Dispatch a validated config to the appropriate operation handler.
///
/// Operators are built on-the-fly from the inline StorageConfig in each
/// operation config. For copy/move, separate source and destination
/// operators may be constructed for cross-backend transfers.
pub async fn dispatch(
    config: &FileOpsConfig,
    run_dir: &Path,
    default_storage: Option<&StorageConfig>,
) -> FileOpsResult {
    match config {
        FileOpsConfig::Probe(c) => {
            // Probe storage is optional: a compiler-injected probe against
            // the platform's own object store omits it and relies on the
            // executor's globally-configured default storage.
            let storage = c.storage.as_ref().or(default_storage).ok_or_else(|| {
                FileOpsError::Config(
                    "probe: no storage config — operation omitted 'storage' and \
                     the executor has no default storage configured"
                        .into(),
                )
            })?;
            let (op, pfx) = build_op(storage)?;
            probe::execute(c, &op, &pfx, run_dir).await
        }
        FileOpsConfig::Copy(c) => {
            let (src_op, src_pfx) = build_op(&c.source_storage)?;
            let dst_storage = c.destination_storage.as_ref().unwrap_or(&c.source_storage);
            let (dst_op, dst_pfx) = build_op(dst_storage)?;
            copy::execute(c, &src_op, &src_pfx, &dst_op, &dst_pfx).await
        }
        FileOpsConfig::Move(c) => {
            let (src_op, src_pfx) = build_op(&c.source_storage)?;
            let dst_storage = c.destination_storage.as_ref().unwrap_or(&c.source_storage);
            let (dst_op, dst_pfx) = build_op(dst_storage)?;
            move_op::execute(c, &src_op, &src_pfx, &dst_op, &dst_pfx).await
        }
        FileOpsConfig::Delete(c) => {
            let (op, pfx) = build_op(&c.storage)?;
            delete::execute(c, &op, &pfx).await
        }
        FileOpsConfig::Annotate(c) => {
            let (op, pfx) = build_op(&c.storage)?;
            annotate::execute(c, &op, &pfx).await
        }
        FileOpsConfig::List(c) => {
            let (op, pfx) = build_op(&c.storage)?;
            list::execute(c, &op, &pfx).await
        }
        FileOpsConfig::Stat(c) => {
            let (op, pfx) = build_op(&c.storage)?;
            stat::execute(c, &op, &pfx).await
        }
    }
}

/// Validate a config before execution.
pub fn validate(config: &FileOpsConfig) -> Result<(), String> {
    match config {
        FileOpsConfig::Probe(c) => {
            if c.path.is_empty() {
                return Err("probe: path must not be empty".into());
            }
            // Storage is optional; when omitted the default store is used and
            // dispatch surfaces a clear error if none is configured.
            if let Some(ref s) = c.storage {
                validate_storage(s, "probe.storage")?;
            }
        }
        FileOpsConfig::Copy(c) => {
            if c.source.is_empty() {
                return Err("copy: source must not be empty".into());
            }
            if c.destination.is_empty() {
                return Err("copy: destination must not be empty".into());
            }
            validate_storage(&c.source_storage, "copy.source_storage")?;
            if let Some(ref dst) = c.destination_storage {
                validate_storage(dst, "copy.destination_storage")?;
            }
        }
        FileOpsConfig::Move(c) => {
            if c.source.is_empty() {
                return Err("move: source must not be empty".into());
            }
            if c.destination.is_empty() {
                return Err("move: destination must not be empty".into());
            }
            validate_storage(&c.source_storage, "move.source_storage")?;
            if let Some(ref dst) = c.destination_storage {
                validate_storage(dst, "move.destination_storage")?;
            }
        }
        FileOpsConfig::Delete(c) => {
            if c.path.is_empty() {
                return Err("delete: path must not be empty".into());
            }
            validate_storage(&c.storage, "delete.storage")?;
        }
        FileOpsConfig::Annotate(c) => {
            if c.path.is_empty() {
                return Err("annotate: path must not be empty".into());
            }
            if c.annotations.is_empty() {
                return Err("annotate: annotations must not be empty".into());
            }
            validate_storage(&c.storage, "annotate.storage")?;
        }
        FileOpsConfig::List(c) => {
            if c.prefix.is_empty() {
                return Err("list: prefix must not be empty".into());
            }
            validate_storage(&c.storage, "list.storage")?;
        }
        FileOpsConfig::Stat(c) => {
            if c.path.is_empty() {
                return Err("stat: path must not be empty".into());
            }
            validate_storage(&c.storage, "stat.storage")?;
        }
    }
    Ok(())
}

/// Validate an inline StorageConfig.
fn validate_storage(config: &StorageConfig, field: &str) -> Result<(), String> {
    use aithericon_executor_storage::StorageBackend;

    if config.endpoint.is_empty() {
        return Err(format!("{field}: endpoint must not be empty"));
    }
    match config.backend {
        StorageBackend::S3 | StorageBackend::Gcs | StorageBackend::AzBlob => {
            if config.bucket.is_empty() {
                return Err(format!(
                    "{field}: bucket must not be empty for cloud backends"
                ));
            }
        }
        StorageBackend::Local => {}
    }
    Ok(())
}

/// Resolve a user-facing path to the full storage path with prefix.
fn resolve_path(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_string()
    } else {
        format!("{}{}", prefix, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use aithericon_executor_storage::{StorageBackend, StorageCredentials};

    fn memory_operator() -> Operator {
        let builder = opendal::services::Memory::default();
        Operator::new(builder).unwrap().finish()
    }

    /// Dummy local storage config — used only for struct initialization in unit
    /// tests that call execute functions directly (not through dispatch).
    fn dummy_storage() -> StorageConfig {
        StorageConfig {
            backend: StorageBackend::Local,
            endpoint: "/tmp/dummy".into(),
            bucket: String::new(),
            region: None,
            prefix: String::new(),
            credentials: StorageCredentials::default(),
            retry: Default::default(),
            resource_alias: None,
        }
    }

    // -- stat tests --

    #[tokio::test]
    async fn stat_existing_file() {
        let op = memory_operator();
        op.write("data/test.csv", "hello").await.unwrap();

        let config = StatConfig {
            path: "data/test.csv".into(),
            storage: dummy_storage(),
        };
        let result = stat::execute(&config, &op, "").await.unwrap();
        assert_eq!(result["exists"], serde_json::json!(true));
        assert_eq!(result["content_length"], serde_json::json!(5));
        assert_eq!(result["path"], serde_json::json!("data/test.csv"));
    }

    #[tokio::test]
    async fn stat_missing_file() {
        let op = memory_operator();

        let config = StatConfig {
            path: "nonexistent.csv".into(),
            storage: dummy_storage(),
        };
        let result = stat::execute(&config, &op, "").await.unwrap();
        assert_eq!(result["exists"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn stat_with_prefix() {
        let op = memory_operator();
        op.write("pfx/data/test.csv", "hello").await.unwrap();

        let config = StatConfig {
            path: "data/test.csv".into(),
            storage: dummy_storage(),
        };
        let result = stat::execute(&config, &op, "pfx/").await.unwrap();
        assert_eq!(result["exists"], serde_json::json!(true));
    }

    // -- delete tests --

    #[tokio::test]
    async fn delete_existing_file() {
        let op = memory_operator();
        op.write("to_delete.csv", "data").await.unwrap();

        let config = DeleteConfig {
            path: "to_delete.csv".into(),
            ignore_missing: false,
            storage: dummy_storage(),
        };
        let result = delete::execute(&config, &op, "").await.unwrap();
        assert_eq!(result["deleted"], serde_json::json!(true));
        assert!(!op.exists("to_delete.csv").await.unwrap());
    }

    #[tokio::test]
    async fn delete_missing_file_errors() {
        let op = memory_operator();

        let config = DeleteConfig {
            path: "nonexistent.csv".into(),
            ignore_missing: false,
            storage: dummy_storage(),
        };
        let result = delete::execute(&config, &op, "").await;
        assert!(matches!(result, Err(FileOpsError::NotFound(_))));
    }

    #[tokio::test]
    async fn delete_missing_file_ignore() {
        let op = memory_operator();

        let config = DeleteConfig {
            path: "nonexistent.csv".into(),
            ignore_missing: true,
            storage: dummy_storage(),
        };
        let result = delete::execute(&config, &op, "").await.unwrap();
        assert_eq!(result["deleted"], serde_json::json!(true));
    }

    // -- copy tests --

    #[tokio::test]
    async fn copy_file() {
        let op = memory_operator();
        op.write("src.csv", "data").await.unwrap();

        let config = CopyConfig {
            source: "src.csv".into(),
            destination: "dst.csv".into(),
            source_storage: dummy_storage(),
            destination_storage: None,
            decompress: None,
            compress: None,
        };
        let result = copy::execute(&config, &op, "", &op, "").await.unwrap();
        assert_eq!(result["copied"], serde_json::json!(true));
        assert_eq!(result["cross_backend"], serde_json::json!(false));

        // Both files should exist
        assert!(op.exists("src.csv").await.unwrap());
        assert!(op.exists("dst.csv").await.unwrap());
        let content = op.read("dst.csv").await.unwrap();
        assert_eq!(&content.to_vec(), b"data");
    }

    #[tokio::test]
    async fn copy_cross_backend() {
        let src_op = memory_operator();
        let dst_op = memory_operator();
        src_op.write("src.csv", "cross-data").await.unwrap();

        let config = CopyConfig {
            source: "src.csv".into(),
            destination: "dst.csv".into(),
            source_storage: dummy_storage(),
            destination_storage: Some(dummy_storage()),
            decompress: None,
            compress: None,
        };
        let result = copy::execute(&config, &src_op, "", &dst_op, "")
            .await
            .unwrap();
        assert_eq!(result["copied"], serde_json::json!(true));
        assert_eq!(result["cross_backend"], serde_json::json!(true));

        // Source still on src_op, destination on dst_op
        assert!(src_op.exists("src.csv").await.unwrap());
        assert!(dst_op.exists("dst.csv").await.unwrap());
        let content = dst_op.read("dst.csv").await.unwrap();
        assert_eq!(&content.to_vec(), b"cross-data");
    }

    #[tokio::test]
    async fn copy_missing_source() {
        let op = memory_operator();

        let config = CopyConfig {
            source: "nonexistent.csv".into(),
            destination: "dst.csv".into(),
            source_storage: dummy_storage(),
            destination_storage: None,
            decompress: None,
            compress: None,
        };
        let result = copy::execute(&config, &op, "", &op, "").await;
        assert!(matches!(result, Err(FileOpsError::NotFound(_))));
    }

    // -- move tests --

    #[tokio::test]
    async fn move_file() {
        let op = memory_operator();
        op.write("src.csv", "data").await.unwrap();

        let config = MoveConfig {
            source: "src.csv".into(),
            destination: "dst.csv".into(),
            source_storage: dummy_storage(),
            destination_storage: None,
            decompress: None,
            compress: None,
        };
        let result = move_op::execute(&config, &op, "", &op, "").await.unwrap();
        assert_eq!(result["moved"], serde_json::json!(true));
        assert_eq!(result["cross_backend"], serde_json::json!(false));

        // Source should be gone, destination should exist
        assert!(!op.exists("src.csv").await.unwrap());
        assert!(op.exists("dst.csv").await.unwrap());
        let content = op.read("dst.csv").await.unwrap();
        assert_eq!(&content.to_vec(), b"data");
    }

    #[tokio::test]
    async fn move_cross_backend() {
        let src_op = memory_operator();
        let dst_op = memory_operator();
        src_op.write("src.csv", "cross-move").await.unwrap();

        let config = MoveConfig {
            source: "src.csv".into(),
            destination: "dst.csv".into(),
            source_storage: dummy_storage(),
            destination_storage: Some(dummy_storage()),
            decompress: None,
            compress: None,
        };
        let result = move_op::execute(&config, &src_op, "", &dst_op, "")
            .await
            .unwrap();
        assert_eq!(result["moved"], serde_json::json!(true));
        assert_eq!(result["cross_backend"], serde_json::json!(true));

        // Source gone from src_op, destination on dst_op
        assert!(!src_op.exists("src.csv").await.unwrap());
        assert!(dst_op.exists("dst.csv").await.unwrap());
        let content = dst_op.read("dst.csv").await.unwrap();
        assert_eq!(&content.to_vec(), b"cross-move");
    }

    #[tokio::test]
    async fn move_missing_source() {
        let op = memory_operator();

        let config = MoveConfig {
            source: "nonexistent.csv".into(),
            destination: "dst.csv".into(),
            source_storage: dummy_storage(),
            destination_storage: None,
            decompress: None,
            compress: None,
        };
        let result = move_op::execute(&config, &op, "", &op, "").await;
        assert!(matches!(result, Err(FileOpsError::NotFound(_))));
    }

    // -- list tests --

    #[tokio::test]
    async fn list_files() {
        let op = memory_operator();
        op.write("datasets/a.csv", "a").await.unwrap();
        op.write("datasets/b.csv", "bb").await.unwrap();

        let config = ListConfig {
            prefix: "datasets/".into(),
            limit: None,
            include_stat: false,
            storage: dummy_storage(),
        };
        let result = list::execute(&config, &op, "").await.unwrap();
        assert_eq!(result["count"], serde_json::json!(2));

        let files = result["files"].as_array().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
        assert!(paths.contains(&"datasets/a.csv"));
        assert!(paths.contains(&"datasets/b.csv"));
    }

    #[tokio::test]
    async fn list_with_limit() {
        let op = memory_operator();
        op.write("data/a.csv", "a").await.unwrap();
        op.write("data/b.csv", "b").await.unwrap();
        op.write("data/c.csv", "c").await.unwrap();

        let config = ListConfig {
            prefix: "data/".into(),
            limit: Some(2),
            include_stat: false,
            storage: dummy_storage(),
        };
        let result = list::execute(&config, &op, "").await.unwrap();
        assert_eq!(result["count"], serde_json::json!(2));
        assert_eq!(result["truncated"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn list_with_stat() {
        let op = memory_operator();
        op.write("data/file.csv", "hello").await.unwrap();

        let config = ListConfig {
            prefix: "data/".into(),
            limit: None,
            include_stat: true,
            storage: dummy_storage(),
        };
        let result = list::execute(&config, &op, "").await.unwrap();
        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);

        let entry = &files[0];
        assert!(entry.get("content_length").is_some());
    }

    // -- annotate tests --

    #[tokio::test]
    async fn annotate_create_sidecar() {
        let op = memory_operator();
        op.write("data/file.parquet", "parquet-data").await.unwrap();

        let config = AnnotateConfig {
            path: "data/file.parquet".into(),
            annotations: HashMap::from([("owner".into(), serde_json::json!("ml-team"))]),
            merge: false,
            storage: dummy_storage(),
        };
        let result = annotate::execute(&config, &op, "").await.unwrap();
        assert_eq!(
            result["sidecar_path"],
            serde_json::json!("data/file.parquet.meta.json")
        );

        // Verify sidecar was written
        let sidecar = op.read("data/file.parquet.meta.json").await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&sidecar.to_vec()).unwrap();
        assert_eq!(parsed["owner"], serde_json::json!("ml-team"));
    }

    #[tokio::test]
    async fn annotate_merge_sidecar() {
        let op = memory_operator();
        op.write("data/file.parquet", "parquet-data").await.unwrap();

        // Write an initial sidecar
        let initial = serde_json::json!({"owner": "old-team", "version": 1});
        op.write(
            "data/file.parquet.meta.json",
            serde_json::to_vec(&initial).unwrap(),
        )
        .await
        .unwrap();

        // Merge in new annotations
        let config = AnnotateConfig {
            path: "data/file.parquet".into(),
            annotations: HashMap::from([
                ("owner".into(), serde_json::json!("new-team")),
                ("description".into(), serde_json::json!("training data")),
            ]),
            merge: true,
            storage: dummy_storage(),
        };
        let result = annotate::execute(&config, &op, "").await.unwrap();
        let annotations = &result["annotations"];

        // owner should be overwritten, version preserved, description added
        assert_eq!(annotations["owner"], serde_json::json!("new-team"));
        assert_eq!(annotations["version"], serde_json::json!(1));
        assert_eq!(
            annotations["description"],
            serde_json::json!("training data")
        );
    }

    #[tokio::test]
    async fn annotate_missing_target_errors() {
        let op = memory_operator();

        let config = AnnotateConfig {
            path: "nonexistent.parquet".into(),
            annotations: HashMap::from([("key".into(), serde_json::json!("value"))]),
            merge: false,
            storage: dummy_storage(),
        };
        let result = annotate::execute(&config, &op, "").await;
        assert!(matches!(result, Err(FileOpsError::NotFound(_))));
    }

    // -- probe tests --

    #[tokio::test]
    async fn probe_csv_file() {
        let op = memory_operator();
        let csv_content = "name,age\nAlice,30\nBob,25\n";
        op.write("data/people.csv", csv_content).await.unwrap();

        let tmp_dir =
            std::env::temp_dir().join(format!("file_ops_probe_test_{}", std::process::id()));
        tokio::fs::create_dir_all(&tmp_dir).await.unwrap();

        let config = ProbeConfig {
            path: "data/people.csv".into(),
            include_statistics: false,
            storage: Some(dummy_storage()),
        };
        let result = probe::execute(&config, &op, "", &tmp_dir).await.unwrap();

        assert_eq!(result["path"], serde_json::json!("data/people.csv"));
        assert!(result.contains_key("metadata"));
        assert!(result.contains_key("format"));

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn probe_missing_file() {
        let op = memory_operator();

        let tmp_dir =
            std::env::temp_dir().join(format!("file_ops_probe_miss_test_{}", std::process::id()));

        let config = ProbeConfig {
            path: "nonexistent.csv".into(),
            include_statistics: false,
            storage: Some(dummy_storage()),
        };
        let result = probe::execute(&config, &op, "", &tmp_dir).await;
        assert!(matches!(result, Err(FileOpsError::NotFound(_))));
    }

    // -- validation tests --

    #[test]
    fn validate_rejects_empty_path() {
        let config = FileOpsConfig::Stat(StatConfig {
            path: String::new(),
            storage: dummy_storage(),
        });
        assert!(validate(&config).is_err());
    }

    #[test]
    fn validate_rejects_empty_source() {
        let config = FileOpsConfig::Copy(CopyConfig {
            source: String::new(),
            destination: "dst".into(),
            source_storage: dummy_storage(),
            destination_storage: None,
            decompress: None,
            compress: None,
        });
        assert!(validate(&config).is_err());
    }

    #[test]
    fn validate_rejects_empty_annotations() {
        let config = FileOpsConfig::Annotate(AnnotateConfig {
            path: "file.parquet".into(),
            annotations: HashMap::new(),
            merge: false,
            storage: dummy_storage(),
        });
        assert!(validate(&config).is_err());
    }

    #[test]
    fn validate_accepts_valid_config() {
        let config = FileOpsConfig::Stat(StatConfig {
            path: "data/file.csv".into(),
            storage: dummy_storage(),
        });
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn validate_rejects_empty_endpoint() {
        let mut storage = dummy_storage();
        storage.endpoint = String::new();
        let config = FileOpsConfig::Stat(StatConfig {
            path: "data/file.csv".into(),
            storage,
        });
        assert!(validate(&config).is_err());
    }

    #[test]
    fn validate_rejects_empty_bucket_for_s3() {
        let storage = StorageConfig {
            backend: StorageBackend::S3,
            endpoint: "https://s3.amazonaws.com".into(),
            bucket: String::new(),
            region: None,
            prefix: String::new(),
            credentials: StorageCredentials::default(),
            retry: Default::default(),
            resource_alias: None,
        };
        let config = FileOpsConfig::Stat(StatConfig {
            path: "data/file.csv".into(),
            storage,
        });
        assert!(validate(&config).is_err());
    }

    // -- resolve_path tests --

    #[test]
    fn resolve_path_no_prefix() {
        assert_eq!(resolve_path("", "data/file.csv"), "data/file.csv");
    }

    #[test]
    fn resolve_path_with_prefix() {
        assert_eq!(
            resolve_path("executor/", "data/file.csv"),
            "executor/data/file.csv"
        );
    }
}
