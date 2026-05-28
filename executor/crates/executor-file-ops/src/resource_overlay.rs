//! Workspace-resource overlay for file-ops storage configs.
//!
//! Each variant of [`FileOpsConfig`] carries one or two [`StorageConfig`]
//! blocks; any block whose `resource_alias` names a workspace `s3`
//! (or future `gcs` / `azblob`) resource has its endpoint / region /
//! bucket / credentials filled from the resource envelope at run time.
//!
//! Precedence: per-step inline values WIN. We only fill a target field
//! when the inline value is empty (`""`) or `None`. Mirrors the SMTP +
//! LLM convention so step authors can still override one credential
//! without unbinding the rest.
//!
//! The S3 workspace resource type uses `access_key_id` /
//! `secret_access_key` field names; `StorageConfig::credentials` uses
//! `access_key` / `secret_key`. The rename is encoded here as explicit
//! calls — there's only one resource type → storage mapping today.

use aithericon_executor_backend::load_resource_envelope;
use aithericon_executor_domain::{ExecutorError, RunContext};
use aithericon_executor_storage::StorageConfig;
use serde_json::Map;
use serde_json::Value;

use crate::config::FileOpsConfig;

/// Walk the parsed config and overlay each [`StorageConfig`] block.
pub(crate) fn overlay_file_ops_resources(
    config: &mut FileOpsConfig,
    run_context: &RunContext,
) -> Result<(), ExecutorError> {
    match config {
        FileOpsConfig::Probe(c) => {
            if let Some(storage) = c.storage.as_mut() {
                overlay_storage(storage, run_context)?;
            }
        }
        FileOpsConfig::Copy(c) => {
            overlay_storage(&mut c.source_storage, run_context)?;
            if let Some(storage) = c.destination_storage.as_mut() {
                overlay_storage(storage, run_context)?;
            }
        }
        FileOpsConfig::Move(c) => {
            overlay_storage(&mut c.source_storage, run_context)?;
            if let Some(storage) = c.destination_storage.as_mut() {
                overlay_storage(storage, run_context)?;
            }
        }
        FileOpsConfig::Delete(c) => overlay_storage(&mut c.storage, run_context)?,
        FileOpsConfig::Annotate(c) => overlay_storage(&mut c.storage, run_context)?,
        FileOpsConfig::List(c) => overlay_storage(&mut c.storage, run_context)?,
        FileOpsConfig::Stat(c) => overlay_storage(&mut c.storage, run_context)?,
    }
    Ok(())
}

/// Overlay a single [`StorageConfig`] from its bound resource envelope,
/// if any. No-op when `resource_alias` is `None` / empty.
fn overlay_storage(
    storage: &mut StorageConfig,
    run_context: &RunContext,
) -> Result<(), ExecutorError> {
    let Some(alias) = storage.resource_alias.as_deref() else {
        return Ok(());
    };
    if alias.is_empty() {
        return Ok(());
    }
    let envelope = load_resource_envelope(run_context, alias)?;
    let obj = envelope.as_object().ok_or_else(|| {
        ExecutorError::Config(format!(
            "file_ops backend: resource '{alias}' envelope must be a JSON object"
        ))
    })?;

    // Public fields — straight names.
    fill_string_if_empty(obj, "endpoint", &mut storage.endpoint);
    fill_string_if_empty(obj, "bucket", &mut storage.bucket);
    fill_opt_string_if_none(obj, "region", &mut storage.region);

    // Credential rename: `s3` resource uses `*_id` / `*_key` suffixes,
    // StorageCredentials does not.
    fill_string_if_empty(obj, "access_key_id", &mut storage.credentials.access_key);
    fill_string_if_empty(
        obj,
        "secret_access_key",
        &mut storage.credentials.secret_key,
    );

    Ok(())
}

fn fill_string_if_empty(obj: &Map<String, Value>, key: &str, target: &mut String) {
    if !target.is_empty() {
        return;
    }
    if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
        *target = v.to_string();
    }
}

fn fill_opt_string_if_none(obj: &Map<String, Value>, key: &str, target: &mut Option<String>) {
    if target.is_some() {
        return;
    }
    if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
        *target = Some(v.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_domain::{ExecutionSpec, RunDirectory};
    use aithericon_executor_storage::{StorageBackend, StorageCredentials};
    use std::collections::HashMap;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;

    fn ctx_with_inputs_dir(td: &TempDir) -> RunContext {
        RunContext {
            execution_id: "test".into(),
            spec: ExecutionSpec {
                backend: "file_ops".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({}),
                config_ref: None,
            },
            run_dir: RunDirectory::new(td.path(), "test"),
            timeout: Duration::from_secs(60),
            env: HashMap::new(),
            resolved_env: HashMap::new(),
            resolved_config: None,
            resolved_input_storage: HashMap::new(),
            resolved_output_storage: HashMap::new(),
            resolved_inline_inputs: HashMap::new(),
            metadata: HashMap::new(),
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: vec![],
            backend_state: serde_json::Value::Null,
        }
    }

    fn write_envelope(td: &TempDir, alias: &str, body: &str) {
        let inputs = td.path().join("runs/test/inputs");
        fs::create_dir_all(&inputs).unwrap();
        fs::write(inputs.join(format!("{alias}.json")), body).unwrap();
    }

    fn empty_storage(backend: StorageBackend, alias: &str) -> StorageConfig {
        StorageConfig {
            backend,
            endpoint: String::new(),
            bucket: String::new(),
            region: None,
            prefix: String::new(),
            credentials: StorageCredentials::default(),
            retry: Default::default(),
            resource_alias: Some(alias.into()),
        }
    }

    #[test]
    fn missing_alias_is_no_op() {
        let td = TempDir::new().unwrap();
        let ctx = ctx_with_inputs_dir(&td);
        let mut s = StorageConfig {
            backend: StorageBackend::S3,
            endpoint: "inline-endpoint".into(),
            bucket: "inline-bucket".into(),
            region: None,
            prefix: String::new(),
            credentials: StorageCredentials::default(),
            retry: Default::default(),
            resource_alias: None,
        };
        overlay_storage(&mut s, &ctx).unwrap();
        assert_eq!(s.endpoint, "inline-endpoint");
        assert_eq!(s.bucket, "inline-bucket");
    }

    #[test]
    fn empty_alias_is_no_op() {
        let td = TempDir::new().unwrap();
        let ctx = ctx_with_inputs_dir(&td);
        let mut s = empty_storage(StorageBackend::S3, "");
        overlay_storage(&mut s, &ctx).unwrap();
        assert_eq!(s.endpoint, "");
    }

    #[test]
    fn overlays_all_fields_when_inline_is_empty() {
        let td = TempDir::new().unwrap();
        let ctx = ctx_with_inputs_dir(&td);
        write_envelope(
            &td,
            "minio_dev",
            r#"{
                "endpoint": "https://minio.local:9000",
                "bucket": "demo",
                "region": "us-east-1",
                "access_key_id": "AKIA",
                "secret_access_key": "secret"
            }"#,
        );
        let mut s = empty_storage(StorageBackend::S3, "minio_dev");
        overlay_storage(&mut s, &ctx).unwrap();
        assert_eq!(s.endpoint, "https://minio.local:9000");
        assert_eq!(s.bucket, "demo");
        assert_eq!(s.region.as_deref(), Some("us-east-1"));
        assert_eq!(s.credentials.access_key, "AKIA");
        assert_eq!(s.credentials.secret_key, "secret");
    }

    #[test]
    fn per_step_values_win_over_resource() {
        let td = TempDir::new().unwrap();
        let ctx = ctx_with_inputs_dir(&td);
        write_envelope(
            &td,
            "minio_dev",
            r#"{
                "endpoint": "from-resource",
                "bucket": "from-resource",
                "access_key_id": "FROM-RESOURCE",
                "secret_access_key": "FROM-RESOURCE"
            }"#,
        );
        let mut s = StorageConfig {
            backend: StorageBackend::S3,
            endpoint: "inline-endpoint".into(),
            bucket: String::new(),
            region: Some("inline-region".into()),
            prefix: String::new(),
            credentials: StorageCredentials {
                access_key: "INLINE-AK".into(),
                secret_key: String::new(),
            },
            retry: Default::default(),
            resource_alias: Some("minio_dev".into()),
        };
        overlay_storage(&mut s, &ctx).unwrap();
        // Per-step inline wins
        assert_eq!(s.endpoint, "inline-endpoint");
        assert_eq!(s.region.as_deref(), Some("inline-region"));
        assert_eq!(s.credentials.access_key, "INLINE-AK");
        // Resource fills the empties
        assert_eq!(s.bucket, "from-resource");
        assert_eq!(s.credentials.secret_key, "FROM-RESOURCE");
    }

    #[test]
    fn missing_envelope_errors() {
        let td = TempDir::new().unwrap();
        let ctx = ctx_with_inputs_dir(&td);
        let mut s = empty_storage(StorageBackend::S3, "ghost");
        let err = overlay_storage(&mut s, &ctx).unwrap_err();
        match err {
            ExecutorError::Config(msg) => assert!(msg.contains("ghost")),
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[test]
    fn copy_variant_overlays_both_storages() {
        use crate::config::{CopyConfig, FileOpsConfig};
        let td = TempDir::new().unwrap();
        let ctx = ctx_with_inputs_dir(&td);
        write_envelope(
            &td,
            "src",
            r#"{ "endpoint": "src-endpoint", "bucket": "src-bucket" }"#,
        );
        write_envelope(
            &td,
            "dst",
            r#"{ "endpoint": "dst-endpoint", "bucket": "dst-bucket" }"#,
        );
        let mut config = FileOpsConfig::Copy(CopyConfig {
            source: "a".into(),
            destination: "b".into(),
            source_storage: empty_storage(StorageBackend::S3, "src"),
            destination_storage: Some(empty_storage(StorageBackend::S3, "dst")),
            decompress: None,
            compress: None,
        });
        overlay_file_ops_resources(&mut config, &ctx).unwrap();
        match config {
            FileOpsConfig::Copy(c) => {
                assert_eq!(c.source_storage.endpoint, "src-endpoint");
                assert_eq!(c.source_storage.bucket, "src-bucket");
                let dst = c.destination_storage.unwrap();
                assert_eq!(dst.endpoint, "dst-endpoint");
                assert_eq!(dst.bucket, "dst-bucket");
            }
            _ => panic!("expected Copy variant"),
        }
    }
}
