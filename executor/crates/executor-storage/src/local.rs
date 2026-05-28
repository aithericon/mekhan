use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tracing::debug;

use aithericon_executor_domain::{Artifact, ArtifactManifest};

use crate::traits::{ArtifactStore, StorageError, StoragePath, UploadOptions};

/// Enrich an artifact with file metadata extracted from the local file.
///
/// Extracts format-specific metadata and computes a SHA-256 checksum via `fmeta`,
/// then promotes the commonly-queried scalars (`mime_type`, `size_bytes`) onto the
/// artifact itself so the API, live renderers, and download Content-Type don't have
/// to reach into the `file_metadata` JSON blob. The full extracted metadata is
/// always stored in `artifact.file_metadata`. Silently skips unsupported formats.
pub async fn enrich_artifact_metadata(artifact: &mut Artifact, local_path: &Path) {
    match aithericon_file_metadata::extract_metadata_async(local_path).await {
        Ok(meta) => {
            // Backfill scalars fmeta detected, but never clobber a value the
            // producer set explicitly (e.g. an SDK-supplied mime_type).
            if artifact.mime_type.is_none() {
                artifact.mime_type = meta.mime_type.clone();
            }
            if artifact.size_bytes.is_none() {
                artifact.size_bytes = meta.file_size_bytes;
            }
            debug!(
                artifact_id = %artifact.id,
                format = ?meta.format,
                mime_type = ?artifact.mime_type,
                checksum = ?meta.checksum,
                "file metadata extracted"
            );
            artifact.file_metadata = serde_json::to_value(&meta).ok();
        }
        Err(e) => {
            debug!(
                artifact_id = %artifact.id,
                error = %e,
                "file-metadata extraction skipped"
            );
        }
    }
}

/// Local filesystem artifact store.
///
/// Layout: `{base_dir}/artifacts/{execution_id}/{artifact_id}/{filename}`
/// Manifest: `{base_dir}/artifacts/{execution_id}/manifest.json`
pub struct LocalArtifactStore {
    base_dir: PathBuf,
}

impl LocalArtifactStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn artifacts_dir(&self, execution_id: &str) -> PathBuf {
        self.base_dir.join("artifacts").join(execution_id)
    }

    fn artifact_dir(&self, execution_id: &str, artifact_id: &str) -> PathBuf {
        self.artifacts_dir(execution_id).join(artifact_id)
    }

    fn manifest_path(&self, execution_id: &str) -> PathBuf {
        self.artifacts_dir(execution_id).join("manifest.json")
    }

    fn storage_path_to_local(&self, storage_path: &StoragePath) -> PathBuf {
        self.base_dir.join(&storage_path.0)
    }
}

#[async_trait]
impl ArtifactStore for LocalArtifactStore {
    async fn upload(
        &self,
        execution_id: &str,
        artifact: &Artifact,
        local_path: &Path,
        options: UploadOptions,
    ) -> Result<Artifact, StorageError> {
        let dest_dir = self.artifact_dir(execution_id, &artifact.id);
        tokio::fs::create_dir_all(&dest_dir).await?;

        let dest_path = dest_dir.join(&artifact.filename);
        let storage_path = format!(
            "artifacts/{}/{}/{}",
            execution_id, artifact.id, artifact.filename
        );

        if !options.overwrite && dest_path.exists() {
            return Err(StorageError::AlreadyExists(storage_path));
        }

        tokio::fs::copy(local_path, &dest_path).await?;

        let file_size = tokio::fs::metadata(&dest_path).await?.len();

        let mut uploaded = artifact.clone();
        uploaded.storage_path = Some(storage_path);
        uploaded.size_bytes = Some(file_size);

        if options.extract_metadata {
            enrich_artifact_metadata(&mut uploaded, &dest_path).await;
        }

        debug!(
            execution_id,
            artifact_id = %artifact.id,
            filename = %artifact.filename,
            size_bytes = file_size,
            "artifact uploaded to local store"
        );

        Ok(uploaded)
    }

    async fn download(
        &self,
        storage_path: &StoragePath,
        local_dest: &Path,
    ) -> Result<(), StorageError> {
        let source = self.storage_path_to_local(storage_path);
        if !source.exists() {
            return Err(StorageError::NotFound(storage_path.to_string()));
        }

        if let Some(parent) = local_dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::copy(&source, local_dest).await?;
        Ok(())
    }

    async fn put(&self, storage_path: &StoragePath, data: Vec<u8>) -> Result<(), StorageError> {
        let dest = self.storage_path_to_local(storage_path);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&dest, data).await?;
        Ok(())
    }

    async fn exists(&self, storage_path: &StoragePath) -> Result<bool, StorageError> {
        let path = self.storage_path_to_local(storage_path);
        Ok(path.exists())
    }

    async fn delete(&self, storage_path: &StoragePath) -> Result<(), StorageError> {
        let path = self.storage_path_to_local(storage_path);
        if path.exists() {
            tokio::fs::remove_file(&path).await?;
        }
        Ok(())
    }

    async fn list(&self, execution_id: &str) -> Result<Vec<StoragePath>, StorageError> {
        let dir = self.artifacts_dir(execution_id);
        if !dir.exists() {
            return Ok(vec![]);
        }

        let mut paths = Vec::new();
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                // Each subdirectory is an artifact_id
                let mut files = tokio::fs::read_dir(&path).await?;
                while let Some(file) = files.next_entry().await? {
                    let file_path = file.path();
                    if file_path.is_file()
                        && file_path.file_name().is_none_or(|n| n != "manifest.json")
                    {
                        if let Ok(relative) = file_path.strip_prefix(&self.base_dir) {
                            paths.push(StoragePath(relative.to_string_lossy().into_owned()));
                        }
                    }
                }
            }
        }
        Ok(paths)
    }

    async fn load_manifest(
        &self,
        execution_id: &str,
    ) -> Result<Option<ArtifactManifest>, StorageError> {
        let path = self.manifest_path(execution_id);
        if !path.exists() {
            return Ok(None);
        }

        let data = tokio::fs::read(&path).await?;
        let manifest: ArtifactManifest = serde_json::from_slice(&data)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        Ok(Some(manifest))
    }

    async fn save_manifest(
        &self,
        execution_id: &str,
        manifest: &ArtifactManifest,
    ) -> Result<(), StorageError> {
        let dir = self.artifacts_dir(execution_id);
        tokio::fs::create_dir_all(&dir).await?;

        let path = self.manifest_path(execution_id);
        let data = serde_json::to_vec_pretty(manifest)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        tokio::fs::write(&path, data).await?;

        debug!(execution_id, "artifact manifest saved");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "local"
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
            metadata: HashMap::new(),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn upload_and_download() {
        let tmp = tempdir();
        let store = LocalArtifactStore::new(tmp.clone());

        // Create a source file
        let source = tmp.join("source.txt");
        tokio::fs::write(&source, b"hello world").await.unwrap();

        let artifact = test_artifact("exec-1");
        let uploaded = store
            .upload("exec-1", &artifact, &source, UploadOptions::default())
            .await
            .unwrap();

        assert!(uploaded.storage_path.is_some());
        assert_eq!(uploaded.size_bytes, Some(11));

        // Download
        let dest = tmp.join("downloaded.txt");
        let sp = StoragePath(uploaded.storage_path.unwrap());
        store.download(&sp, &dest).await.unwrap();

        let content = tokio::fs::read_to_string(&dest).await.unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn exists_and_delete() {
        let tmp = tempdir();
        let store = LocalArtifactStore::new(tmp.clone());

        let source = tmp.join("src.txt");
        tokio::fs::write(&source, b"data").await.unwrap();

        let artifact = test_artifact("exec-2");
        let uploaded = store
            .upload("exec-2", &artifact, &source, UploadOptions::default())
            .await
            .unwrap();

        let sp = StoragePath(uploaded.storage_path.unwrap());
        assert!(store.exists(&sp).await.unwrap());

        store.delete(&sp).await.unwrap();
        assert!(!store.exists(&sp).await.unwrap());
    }

    #[tokio::test]
    async fn manifest_roundtrip() {
        let tmp = tempdir();
        let store = LocalArtifactStore::new(tmp);

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
        let tmp = tempdir();
        let store = LocalArtifactStore::new(tmp);

        let result = store.load_manifest("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("executor-storage-test-{}", uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn uuid() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        format!("{}-{}", d.as_secs(), d.subsec_nanos())
    }
}
