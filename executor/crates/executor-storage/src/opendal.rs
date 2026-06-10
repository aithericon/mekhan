use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use opendal::layers::RetryLayer;
use opendal::Operator;
use tracing::debug;

use aithericon_executor_domain::{Artifact, ArtifactManifest};

use crate::config::{StorageBackend, StorageConfig};
use crate::local::enrich_artifact_metadata;
use crate::traits::{ArtifactStore, StorageError, StoragePath, UploadOptions};

/// Build a `RetryLayer` from `StorageConfig.retry`. Only retries operations
/// whose error returns `is_temporary() == true` (504, connection drops,
/// throttling) — non-transient failures (404, 403) are not retried.
fn retry_layer(config: &StorageConfig) -> RetryLayer {
    RetryLayer::new()
        .with_max_times(config.retry.max_attempts as usize)
        .with_min_delay(Duration::from_millis(config.retry.min_delay_ms))
        .with_max_delay(Duration::from_millis(config.retry.max_delay_ms))
        .with_jitter()
}

/// Build an OpenDAL `Operator` from a `StorageConfig`.
///
/// Each cloud backend requires its corresponding feature flag:
/// - S3: `opendal-s3`
/// - GCS: `opendal-gcs`
/// - Azure Blob: `opendal-azblob`
///
/// The `Local` backend is always available when `opendal` is enabled.
pub fn build_operator(config: &StorageConfig) -> Result<Operator, StorageError> {
    match config.backend {
        #[cfg(feature = "opendal-s3")]
        StorageBackend::S3 => {
            let mut builder = opendal::services::S3::default()
                .endpoint(&config.endpoint)
                .bucket(&config.bucket)
                // Prevent OpenDAL from loading AWS credentials/config from the
                // host environment (~/.aws/, AWS_* env vars, EC2 IMDS). Without
                // this, stale AWS config can silently override the credentials
                // we set explicitly — causing auth failures against non-AWS S3
                // stores like RustFS or MinIO.
                .disable_config_load()
                .disable_ec2_metadata();
            if !config.credentials.access_key.is_empty() {
                builder = builder
                    .access_key_id(&config.credentials.access_key)
                    .secret_access_key(&config.credentials.secret_key);
            }
            if let Some(region) = &config.region {
                builder = builder.region(region);
            }
            Ok(Operator::new(builder)
                .map_err(|e| StorageError::Other(format!("S3 operator init: {e}")))?
                .layer(retry_layer(config))
                .finish())
        }
        #[cfg(not(feature = "opendal-s3"))]
        StorageBackend::S3 => Err(StorageError::Other(
            "S3 backend requires the 'opendal-s3' feature".into(),
        )),
        #[cfg(feature = "opendal-gcs")]
        StorageBackend::Gcs => {
            let mut builder = opendal::services::Gcs::default()
                .endpoint(&config.endpoint)
                .bucket(&config.bucket);
            if !config.credentials.access_key.is_empty() {
                builder = builder.credential_path(&config.credentials.access_key);
            }
            Ok(Operator::new(builder)
                .map_err(|e| StorageError::Other(format!("GCS operator init: {e}")))?
                .layer(retry_layer(config))
                .finish())
        }
        #[cfg(not(feature = "opendal-gcs"))]
        StorageBackend::Gcs => Err(StorageError::Other(
            "GCS backend requires the 'opendal-gcs' feature".into(),
        )),
        #[cfg(feature = "opendal-azblob")]
        StorageBackend::AzBlob => {
            let mut builder = opendal::services::Azblob::default()
                .endpoint(&config.endpoint)
                .container(&config.bucket);
            if !config.credentials.access_key.is_empty() {
                builder = builder
                    .account_name(&config.credentials.access_key)
                    .account_key(&config.credentials.secret_key);
            }
            Ok(Operator::new(builder)
                .map_err(|e| StorageError::Other(format!("AzBlob operator init: {e}")))?
                .layer(retry_layer(config))
                .finish())
        }
        #[cfg(not(feature = "opendal-azblob"))]
        StorageBackend::AzBlob => Err(StorageError::Other(
            "Azure Blob backend requires the 'opendal-azblob' feature".into(),
        )),
        StorageBackend::Local => {
            let builder = opendal::services::Fs::default().root(&config.endpoint);
            Ok(Operator::new(builder)
                .map_err(|e| StorageError::Other(format!("Fs operator init: {e}")))?
                .layer(retry_layer(config))
                .finish())
        }
        #[cfg(feature = "opendal-sftp")]
        StorageBackend::Sftp => {
            // opendal's Sftp wants a key *path*, not inline PEM — materialize the
            // secret to a 0600 temp file (content-addressed so concurrent builds
            // reuse one file). Mirrors `Datacenter::Slurm`'s "0600 temp file at
            // use". `endpoint` is the SSH endpoint, `access_key` the username,
            // `secret_key` the PEM; `prefix` carries the base path so root is "/".
            let key_path = write_sftp_key(&config.credentials.secret_key)
                .map_err(|e| StorageError::Other(format!("sftp key materialize: {e}")))?;
            let strategy = config
                .region
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("Accept");
            let builder = opendal::services::Sftp::default()
                .endpoint(&config.endpoint)
                .user(&config.credentials.access_key)
                .key(&key_path)
                .root("/")
                .known_hosts_strategy(strategy);
            Ok(Operator::new(builder)
                .map_err(|e| StorageError::Other(format!("Sftp operator init: {e}")))?
                .layer(retry_layer(config))
                .finish())
        }
        #[cfg(not(feature = "opendal-sftp"))]
        StorageBackend::Sftp => Err(StorageError::Other(
            "SFTP backend requires the 'opendal-sftp' feature".into(),
        )),
    }
}

/// Write an inline PEM private key to a 0600, content-addressed temp file and
/// return its path. Idempotent: reuses an existing file with identical bytes,
/// so concurrent operator builds never race on distinct paths. opendal's Sftp
/// `key()` takes a filesystem path, not inline content — this bridges that.
#[cfg(feature = "opendal-sftp")]
fn write_sftp_key(pem: &str) -> std::io::Result<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    pem.hash(&mut h);
    let path = std::env::temp_dir().join(format!("aithericon-sftp-key-{:016x}.pem", h.finish()));
    if !path.exists() {
        std::fs::write(&path, pem)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(path.to_string_lossy().into_owned())
}

/// Build an OpenDAL `Operator` and prefix from a `StorageConfig`.
///
/// This is a convenience wrapper around [`build_operator`] that also returns
/// the prefix, matching the pattern used by the file-ops backend.
pub fn build_operator_with_prefix(
    config: &StorageConfig,
) -> Result<(Operator, String), StorageError> {
    let op = build_operator(config)?;
    Ok((op, config.prefix.clone()))
}

/// Artifact store backed by Apache OpenDAL.
///
/// Supports any storage service that OpenDAL supports (S3, GCS, Azure Blob,
/// local filesystem, etc.) through a single `Operator` abstraction.
pub struct OpenDalArtifactStore {
    operator: Operator,
    prefix: String,
}

impl OpenDalArtifactStore {
    /// Create from an already-configured OpenDAL `Operator`.
    ///
    /// `prefix` is prepended to all storage paths (e.g. `"executor/"`)
    /// to isolate executor data within a shared bucket.
    pub fn new(operator: Operator, prefix: String) -> Self {
        Self { operator, prefix }
    }

    /// Build from a `StorageConfig`.
    ///
    /// Each cloud backend requires its corresponding feature flag:
    /// - S3: `opendal-s3`
    /// - GCS: `opendal-gcs`
    /// - Azure Blob: `opendal-azblob`
    ///
    /// The `Local` backend is always available when `opendal` is enabled.
    pub fn from_config(config: &StorageConfig) -> Result<Self, StorageError> {
        let operator = build_operator(config)?;
        Ok(Self::new(operator, config.prefix.clone()))
    }

    fn artifact_path(&self, execution_id: &str, artifact_id: &str, filename: &str) -> String {
        format!(
            "{}artifacts/{}/{}/{}",
            self.prefix, execution_id, artifact_id, filename
        )
    }

    fn manifest_path(&self, execution_id: &str) -> String {
        format!("{}artifacts/{}/manifest.json", self.prefix, execution_id)
    }

    fn artifacts_prefix(&self, execution_id: &str) -> String {
        format!("{}artifacts/{}/", self.prefix, execution_id)
    }

    fn storage_path_to_remote(&self, storage_path: &StoragePath) -> String {
        format!("{}{}", self.prefix, storage_path.0)
    }
}

#[async_trait]
impl ArtifactStore for OpenDalArtifactStore {
    async fn upload(
        &self,
        execution_id: &str,
        artifact: &Artifact,
        local_path: &Path,
        options: UploadOptions,
    ) -> Result<Artifact, StorageError> {
        let remote_path = self.artifact_path(execution_id, &artifact.id, &artifact.filename);
        let storage_path = format!(
            "artifacts/{}/{}/{}",
            execution_id, artifact.id, artifact.filename
        );

        if !options.overwrite {
            let exists = self
                .operator
                .exists(&remote_path)
                .await
                .map_err(opendal_err)?;
            if exists {
                return Err(StorageError::AlreadyExists(storage_path));
            }
        }

        let data = tokio::fs::read(local_path)
            .await
            .map_err(StorageError::Io)?;
        let file_size = data.len() as u64;

        self.operator
            .write(&remote_path, data)
            .await
            .map_err(opendal_err)?;

        let mut uploaded = artifact.clone();
        uploaded.storage_path = Some(storage_path);
        uploaded.size_bytes = Some(file_size);

        if options.extract_metadata {
            enrich_artifact_metadata(&mut uploaded, local_path).await;
        }

        debug!(
            execution_id,
            artifact_id = %artifact.id,
            filename = %artifact.filename,
            size_bytes = file_size,
            "artifact uploaded via opendal"
        );

        Ok(uploaded)
    }

    async fn download(
        &self,
        storage_path: &StoragePath,
        local_dest: &Path,
    ) -> Result<(), StorageError> {
        let remote_path = self.storage_path_to_remote(storage_path);

        let data = self
            .operator
            .read(&remote_path)
            .await
            .map_err(opendal_err)?;

        if let Some(parent) = local_dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(StorageError::Io)?;
        }

        tokio::fs::write(local_dest, data.to_vec())
            .await
            .map_err(StorageError::Io)?;
        Ok(())
    }

    async fn put(&self, storage_path: &StoragePath, data: Vec<u8>) -> Result<(), StorageError> {
        let remote_path = self.storage_path_to_remote(storage_path);
        self.operator
            .write(&remote_path, data)
            .await
            .map_err(opendal_err)?;
        Ok(())
    }

    async fn exists(&self, storage_path: &StoragePath) -> Result<bool, StorageError> {
        let remote_path = self.storage_path_to_remote(storage_path);
        self.operator
            .exists(&remote_path)
            .await
            .map_err(opendal_err)
    }

    async fn delete(&self, storage_path: &StoragePath) -> Result<(), StorageError> {
        let remote_path = self.storage_path_to_remote(storage_path);
        self.operator
            .delete(&remote_path)
            .await
            .map_err(opendal_err)
    }

    async fn list(&self, execution_id: &str) -> Result<Vec<StoragePath>, StorageError> {
        let prefix = self.artifacts_prefix(execution_id);
        let entries = self.operator.list(&prefix).await.map_err(opendal_err)?;

        let paths: Vec<StoragePath> = entries
            .into_iter()
            .filter(|e| {
                let path = e.path();
                !path.ends_with('/') && !path.ends_with("manifest.json")
            })
            .filter_map(|e| {
                let path = e.path();
                path.strip_prefix(&self.prefix)
                    .map(|rel| StoragePath(rel.to_string()))
            })
            .collect();

        Ok(paths)
    }

    async fn load_manifest(
        &self,
        execution_id: &str,
    ) -> Result<Option<ArtifactManifest>, StorageError> {
        let path = self.manifest_path(execution_id);

        match self.operator.read(&path).await {
            Ok(data) => {
                let manifest: ArtifactManifest = serde_json::from_slice(&data.to_vec())
                    .map_err(|e| StorageError::Serialization(e.to_string()))?;
                Ok(Some(manifest))
            }
            Err(e) if e.kind() == opendal::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(opendal_err(e)),
        }
    }

    async fn save_manifest(
        &self,
        execution_id: &str,
        manifest: &ArtifactManifest,
    ) -> Result<(), StorageError> {
        let path = self.manifest_path(execution_id);
        let data = serde_json::to_vec_pretty(manifest)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;

        self.operator
            .write(&path, data)
            .await
            .map_err(opendal_err)?;

        debug!(execution_id, "artifact manifest saved via opendal");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "opendal"
    }
}

fn opendal_err(e: opendal::Error) -> StorageError {
    match e.kind() {
        opendal::ErrorKind::NotFound => StorageError::NotFound(e.to_string()),
        opendal::ErrorKind::AlreadyExists => StorageError::AlreadyExists(e.to_string()),
        _ => StorageError::Other(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_domain::ArtifactCategory;
    use chrono::Utc;
    use std::collections::HashMap;

    fn test_artifact(execution_id: &str) -> Artifact {
        Artifact {
            id: "art-001".into(),
            execution_id: execution_id.into(),
            name: "test-file.txt".into(),
            category: ArtifactCategory::Other,
            filename: "test-file.txt".into(),
            mime_type: None,
            size_bytes: None,
            storage_path: None,
            file_metadata: None,
            by_reference: false,
            file_server_id: None,
            reference_path: None,
            endpoint_root: None,
            metadata: HashMap::new(),
            created_at: Utc::now(),
        }
    }

    fn memory_store() -> OpenDalArtifactStore {
        let builder = opendal::services::Memory::default();
        let op = Operator::new(builder).unwrap().finish();
        OpenDalArtifactStore::new(op, String::new())
    }

    /// The SFTP transport (docs/32 §4.1) builds an `Operator` from a
    /// `StorageConfig` — endpoint/user/inline-PEM-key/prefix — WITHOUT
    /// connecting (opendal connects lazily on first op), so this exercises the
    /// builder + the 0600 key-file materialization off the live-NAS path.
    #[cfg(feature = "opendal-sftp")]
    #[test]
    fn build_sftp_operator_without_connecting() {
        use crate::config::{StorageBackend, StorageCredentials};
        let config = StorageConfig {
            backend: StorageBackend::Sftp,
            endpoint: "ssh://nas.example:22".into(),
            bucket: String::new(),
            region: Some("Accept".into()),
            prefix: "legacy/".into(),
            credentials: StorageCredentials {
                access_key: "svc".into(),
                secret_key: "-----BEGIN OPENSSH PRIVATE KEY-----\nDUMMY\n-----END OPENSSH PRIVATE KEY-----\n".into(),
            },
            retry: Default::default(),
            resource_alias: None,
        };
        let op = build_operator(&config).expect("sftp operator builds");
        assert_eq!(op.info().scheme().to_string(), "sftp");

        // The inline PEM was materialized to a 0600 file.
        let key_path = write_sftp_key(&config.credentials.secret_key).unwrap();
        let meta = std::fs::metadata(&key_path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
        let _ = meta;
    }

    #[tokio::test]
    async fn upload_and_download() {
        let store = memory_store();

        // Write a local file to upload
        let tmp = std::env::temp_dir().join(format!("opendal-test-upload-{}", std::process::id()));
        tokio::fs::write(&tmp, b"hello opendal").await.unwrap();

        let artifact = test_artifact("exec-1");
        let uploaded = store
            .upload(
                "exec-1",
                &artifact,
                &tmp,
                UploadOptions {
                    extract_metadata: false,
                    overwrite: true,
                },
            )
            .await
            .unwrap();

        assert!(uploaded.storage_path.is_some());
        assert_eq!(uploaded.size_bytes, Some(13));

        // Download
        let dest =
            std::env::temp_dir().join(format!("opendal-test-download-{}", std::process::id()));
        let sp = StoragePath(uploaded.storage_path.unwrap());
        store.download(&sp, &dest).await.unwrap();

        let content = tokio::fs::read_to_string(&dest).await.unwrap();
        assert_eq!(content, "hello opendal");

        // Cleanup
        let _ = tokio::fs::remove_file(&tmp).await;
        let _ = tokio::fs::remove_file(&dest).await;
    }

    #[tokio::test]
    async fn exists_and_delete() {
        let store = memory_store();

        let tmp = std::env::temp_dir().join(format!("opendal-test-exists-{}", std::process::id()));
        tokio::fs::write(&tmp, b"data").await.unwrap();

        let artifact = test_artifact("exec-2");
        let uploaded = store
            .upload(
                "exec-2",
                &artifact,
                &tmp,
                UploadOptions {
                    extract_metadata: false,
                    overwrite: true,
                },
            )
            .await
            .unwrap();

        let sp = StoragePath(uploaded.storage_path.unwrap());
        assert!(store.exists(&sp).await.unwrap());

        store.delete(&sp).await.unwrap();
        assert!(!store.exists(&sp).await.unwrap());

        let _ = tokio::fs::remove_file(&tmp).await;
    }

    #[tokio::test]
    async fn manifest_roundtrip() {
        let store = memory_store();

        let manifest = ArtifactManifest {
            execution_id: "exec-3".into(),
            artifacts: vec![test_artifact("exec-3")],
            updated_at: Utc::now(),
        };

        store.save_manifest("exec-3", &manifest).await.unwrap();

        let loaded = store.load_manifest("exec-3").await.unwrap().unwrap();
        assert_eq!(loaded.execution_id, "exec-3");
        assert_eq!(loaded.artifacts.len(), 1);
    }

    #[tokio::test]
    async fn load_missing_manifest_returns_none() {
        let store = memory_store();
        let result = store.load_manifest("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn overwrite_protection() {
        let store = memory_store();

        let tmp =
            std::env::temp_dir().join(format!("opendal-test-overwrite-{}", std::process::id()));
        tokio::fs::write(&tmp, b"v1").await.unwrap();

        let artifact = test_artifact("exec-4");
        store
            .upload(
                "exec-4",
                &artifact,
                &tmp,
                UploadOptions {
                    extract_metadata: false,
                    overwrite: true,
                },
            )
            .await
            .unwrap();

        // Second upload without overwrite should fail
        let result = store
            .upload(
                "exec-4",
                &artifact,
                &tmp,
                UploadOptions {
                    extract_metadata: false,
                    overwrite: false,
                },
            )
            .await;

        assert!(matches!(result, Err(StorageError::AlreadyExists(_))));

        let _ = tokio::fs::remove_file(&tmp).await;
    }

    #[test]
    fn build_operator_with_prefix_returns_prefix() {
        let config = StorageConfig {
            backend: StorageBackend::Local,
            endpoint: std::env::temp_dir().to_string_lossy().into_owned(),
            bucket: String::new(),
            region: None,
            prefix: "my/prefix/".into(),
            credentials: Default::default(),
            retry: Default::default(),
            resource_alias: None,
        };
        let (op, prefix) = build_operator_with_prefix(&config).unwrap();
        assert_eq!(prefix, "my/prefix/");
        // Operator should be usable (local fs)
        assert!(op.info().full_capability().read);
    }

    #[test]
    fn build_operator_honours_custom_retry_config() {
        use aithericon_executor_storage_types::RetryConfig;
        let config = StorageConfig {
            backend: StorageBackend::Local,
            endpoint: std::env::temp_dir().to_string_lossy().into_owned(),
            bucket: String::new(),
            region: None,
            prefix: String::new(),
            credentials: Default::default(),
            retry: RetryConfig {
                max_attempts: 7,
                min_delay_ms: 50,
                max_delay_ms: 500,
            },
            resource_alias: None,
        };
        // The RetryLayer wraps successfully — operator builds without panic
        // even with non-default retry config. (Behaviour assertions about the
        // retry policy live in OpenDAL's own test suite.)
        let op = build_operator(&config).expect("operator should build");
        assert!(op.info().full_capability().read);
    }

    #[test]
    fn build_operator_with_prefix_empty_prefix() {
        let config = StorageConfig {
            backend: StorageBackend::Local,
            endpoint: std::env::temp_dir().to_string_lossy().into_owned(),
            bucket: String::new(),
            region: None,
            prefix: String::new(),
            credentials: Default::default(),
            retry: Default::default(),
            resource_alias: None,
        };
        let (_, prefix) = build_operator_with_prefix(&config).unwrap();
        assert_eq!(prefix, "");
    }
}
