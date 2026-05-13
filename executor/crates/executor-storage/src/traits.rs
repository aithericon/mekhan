use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use aithericon_executor_domain::{Artifact, ArtifactManifest};

/// Opaque storage path referencing an artifact in the store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoragePath(pub String);

impl StoragePath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for StoragePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Options for uploading an artifact.
#[derive(Debug, Clone)]
pub struct UploadOptions {
    /// Whether to extract file metadata (e.g., via file-metadata crate).
    pub extract_metadata: bool,

    /// Whether to overwrite if a file already exists at the storage path.
    pub overwrite: bool,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            extract_metadata: true,
            overwrite: false,
        }
    }
}

/// Errors from artifact storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("artifact not found: {0}")]
    NotFound(String),

    #[error("artifact already exists: {0}")]
    AlreadyExists(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("storage error: {0}")]
    Other(String),
}

/// Trait for artifact storage backends.
///
/// Implementations handle uploading, downloading, and managing artifacts
/// produced during execution.
#[async_trait]
pub trait ArtifactStore: Send + Sync + 'static {
    /// Upload a local file as an artifact.
    ///
    /// Returns the artifact with `storage_path` set.
    async fn upload(
        &self,
        execution_id: &str,
        artifact: &Artifact,
        local_path: &Path,
        options: UploadOptions,
    ) -> Result<Artifact, StorageError>;

    /// Download an artifact to a local destination.
    async fn download(
        &self,
        storage_path: &StoragePath,
        local_dest: &Path,
    ) -> Result<(), StorageError>;

    /// Check if an artifact exists at the given storage path.
    async fn exists(&self, storage_path: &StoragePath) -> Result<bool, StorageError>;

    /// Delete an artifact at the given storage path.
    async fn delete(&self, storage_path: &StoragePath) -> Result<(), StorageError>;

    /// List all artifact storage paths for an execution.
    async fn list(&self, execution_id: &str) -> Result<Vec<StoragePath>, StorageError>;

    /// Load the artifact manifest for an execution.
    async fn load_manifest(
        &self,
        execution_id: &str,
    ) -> Result<Option<ArtifactManifest>, StorageError>;

    /// Save the artifact manifest for an execution.
    async fn save_manifest(
        &self,
        execution_id: &str,
        manifest: &ArtifactManifest,
    ) -> Result<(), StorageError>;

    /// Human-readable name of this store implementation.
    fn name(&self) -> &'static str;
}
