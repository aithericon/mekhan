# Storage

The `ArtifactStore` trait abstracts artifact storage. The executor ships with a local filesystem implementation; other backends (S3, GCS) can be added by implementing the trait.

## ArtifactStore Trait

```rust
#[async_trait]
pub trait ArtifactStore: Send + Sync + 'static {
    async fn upload(&self, execution_id: &str, artifact: &Artifact,
                    local_path: &Path, options: UploadOptions) -> Result<Artifact, StorageError>;
    async fn download(&self, storage_path: &StoragePath,
                      local_dest: &Path) -> Result<(), StorageError>;
    async fn exists(&self, storage_path: &StoragePath) -> Result<bool, StorageError>;
    async fn delete(&self, storage_path: &StoragePath) -> Result<(), StorageError>;
    async fn list(&self, execution_id: &str) -> Result<Vec<StoragePath>, StorageError>;
    async fn load_manifest(&self, execution_id: &str)
        -> Result<Option<ArtifactManifest>, StorageError>;
    async fn save_manifest(&self, execution_id: &str,
                           manifest: &ArtifactManifest) -> Result<(), StorageError>;
    fn name(&self) -> &'static str;
}
```

### Key types

**StoragePath** — Opaque string reference to a stored artifact. For the local store this is a relative path from the base directory.

**UploadOptions:**

| Field | Type | Default | Description |
|---|---|---|---|
| `extract_metadata` | `bool` | `true` | Auto-extract file metadata (feature-gated). |
| `overwrite` | `bool` | `false` | Allow overwriting an existing artifact. |

**StorageError** variants: `Io`, `NotFound`, `AlreadyExists`, `Serialization`, `Other`.

## Artifact Model

```rust
pub struct Artifact {
    pub id: String,                              // Unique artifact ID
    pub execution_id: String,                    // Which execution produced this
    pub name: String,                            // Human-readable name
    pub category: ArtifactCategory,              // Model, Dataset, Plot, Log, etc.
    pub filename: String,                        // Original filename
    pub mime_type: Option<String>,               // MIME type
    pub size_bytes: Option<u64>,                 // File size (set after upload)
    pub storage_path: Option<String>,            // Storage location (set after upload)
    pub file_metadata: Option<serde_json::Value>,// Extracted file metadata
    pub metadata: HashMap<String, String>,       // User-defined key-value pairs
    pub created_at: DateTime<Utc>,               // Creation timestamp
}
```

### ArtifactCategory

`Model`, `Dataset`, `Plot`, `Log`, `Checkpoint`, `Config`, `Metric`, `Other` (default).

### ArtifactManifest

```rust
pub struct ArtifactManifest {
    pub execution_id: String,
    pub artifacts: Vec<Artifact>,
    pub updated_at: DateTime<Utc>,
}
```

Collected after execution and included in the terminal status update.

## LocalArtifactStore

Filesystem layout:

```
{base_dir}/artifacts/{execution_id}/{artifact_id}/{filename}
{base_dir}/artifacts/{execution_id}/manifest.json
```

- `upload()` copies the local file to the store directory, sets `storage_path` and `size_bytes`.
- `download()` copies from the store to a local destination.
- `list()` returns all stored artifact paths for an execution.
- `load_manifest()` / `save_manifest()` reads/writes `manifest.json`.

## IPC Integration

When a child process sends `LogArtifactRequest` via IPC:

1. The sidecar creates an `Artifact` record from the request fields.
2. If an `ArtifactStore` is configured, the sidecar calls `upload()` with the local file path.
3. The upload sets `storage_path` and `size_bytes` on the artifact.
4. The artifact is added to the `SidecarResult.artifacts` list.
5. After execution, all artifacts are compiled into an `ArtifactManifest`.
