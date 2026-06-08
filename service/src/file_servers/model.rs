use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::inventory::model::InventoryCount;

/// Allowed transport kinds for a file server (wire values).
///
/// `object_store` is the built-in platform S3 bucket (uses platform config, no
/// `resource_ref`). `s3` / `sftp` are external backends that reference a
/// workspace `resource` for connection + secrets. `nfs` / `local` are reserved
/// for the deferred co-located-runner transports.
pub const ALLOWED_KINDS: &[&str] = &["object_store", "s3", "sftp", "nfs", "local"];

/// A first-class storage-backend entity (maps 1:1 to the `file_servers` table).
///
/// Identity + topology only — **secrets never live here**. `resource_ref` points
/// at a workspace `resource` (by its `path`) that holds the connection +
/// credentials in Vault; it is NULL for the built-in `object_store` (which uses
/// platform S3 config). See docs/32 §4.1.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct FileServer {
    pub id: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    /// Stable slug; equals `file_inventory.file_server_id` (soft join, no FK).
    pub key: String,
    pub display_name: String,
    pub kind: String,
    /// Resource `path` holding connection + secrets. NULL for `object_store`.
    pub resource_ref: Option<String>,
    /// Root / prefix within the backend.
    pub base_path: Option<String>,
    pub status: String,
    pub last_seen: Option<DateTime<Utc>>,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A registered file server plus its DERIVED rollups (joined from
/// `file_inventory` by `key` at read time — never stored).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FileServerView {
    #[serde(flatten)]
    pub server: FileServer,
    /// Number of physical copies on this server.
    pub file_count: i64,
    /// Sum of logical sizes of those copies (via `catalogue_entries.size_bytes`
    /// joined on `content_hash`; copies of unhashed/uncatalogued files add 0).
    pub total_size_bytes: i64,
    /// Per-status breakdown of this server's copies.
    pub by_status: Vec<InventoryCount>,
    /// Whether `resource_ref` resolves to an existing, non-deleted resource in
    /// the same workspace (false when NULL or dangling).
    pub resource_resolves: bool,
}

/// An inventory `file_server_id` string observed in `file_inventory` that has
/// NO backing `file_servers` row yet — a candidate for `adopt`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UnregisteredServer {
    pub key: String,
    pub file_count: i64,
    pub total_size_bytes: i64,
}

/// Response of `GET /api/v1/file-servers`: registered servers (with rollups)
/// plus the unregistered inventory keys (so the UI can offer "adopt").
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FileServersResponse {
    pub servers: Vec<FileServerView>,
    pub unregistered: Vec<UnregisteredServer>,
}

/// Create / adopt body. `adopt` additionally requires `key` to exist in
/// `file_inventory`; otherwise the shape is identical.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateFileServerRequest {
    pub key: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub resource_ref: Option<String>,
    #[serde(default)]
    pub base_path: Option<String>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    /// Optional explicit workspace; falls back to the caller's workspace.
    #[serde(default)]
    pub workspace_id: Option<uuid::Uuid>,
}

/// Update body — all fields optional; `key` and `workspace_id` are immutable.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateFileServerRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    /// Set to `Some(Some(_))` to change, `Some(None)` to clear, omit to keep.
    #[serde(default, deserialize_with = "crate::file_servers::model::double_option")]
    pub resource_ref: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::file_servers::model::double_option")]
    pub base_path: Option<Option<String>>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

/// Deserialize that distinguishes "absent" (`None`) from "present and null"
/// (`Some(None)`) so PATCH-style updates can clear a column.
pub fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(de)?))
}
