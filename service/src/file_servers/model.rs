use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::inventory::model::InventoryCount;

/// Allowed access methods for a file-server endpoint (wire values).
///
/// `object_store` is the built-in platform S3 bucket (uses platform config, no
/// `resource_ref`). `s3` / `sftp` are external backends that reference a
/// workspace `resource` for connection + secrets. `local_mount` is a filesystem
/// path reachable from a capacity group's co-located runners (`group_id`).
pub const ALLOWED_ACCESS_METHODS: &[&str] = &["object_store", "s3", "sftp", "local_mount"];

/// A first-class storage-backend entity (maps 1:1 to the `file_servers` table).
///
/// Identity + topology only — **no transport, no secrets**. The ways to reach
/// the backend live in N child [`FileServerEndpoint`] rows. See docs/32 §4.1.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct FileServer {
    pub id: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    /// Stable slug; equals `file_inventory.file_server_id` (soft join, no FK).
    pub key: String,
    pub display_name: String,
    pub status: String,
    pub last_seen: Option<DateTime<Utc>>,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One access method (transport) onto a [`FileServer`] (maps 1:1 to the
/// `file_server_endpoints` table). Secrets never live here — `resource_ref`
/// points at a workspace `resource` holding connection + credentials in Vault.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct FileServerEndpoint {
    pub id: uuid::Uuid,
    pub file_server_id: uuid::Uuid,
    /// `object_store` | `s3` | `sftp` | `local_mount`.
    pub access_method: String,
    /// Prefix in this namespace mapping to the server's canonical root.
    pub root: String,
    /// Resource `path` holding connection + secrets. NULL for `object_store`.
    pub resource_ref: Option<String>,
    /// Capacity-group UUID for `local_mount` dispatch; NULL otherwise.
    pub group_id: Option<String>,
    pub status: String,
    /// `unverified` | `verified` | `mismatch` | `conflict`.
    pub verification_status: String,
    pub last_verified: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
    /// Operator routing override; higher = preferred.
    pub priority: i32,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A registered file server plus its endpoints + DERIVED rollups (joined from
/// `file_inventory` by `key` at read time — never stored).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FileServerView {
    #[serde(flatten)]
    pub server: FileServer,
    /// The access methods onto this backend.
    pub endpoints: Vec<FileServerEndpoint>,
    /// Number of physical copies on this server.
    pub file_count: i64,
    /// Sum of logical sizes of those copies (via `catalogue_entries.size_bytes`
    /// joined on `content_hash`; copies of unhashed/uncatalogued files add 0).
    pub total_size_bytes: i64,
    /// Per-status breakdown of this server's copies.
    pub by_status: Vec<InventoryCount>,
    /// Whether every endpoint's `resource_ref` resolves to an existing,
    /// non-deleted resource in the same workspace (NULL refs are treated as
    /// resolved — the built-in object_store needs none).
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

/// Response of `GET /api/v1/file-servers`: registered servers (with endpoints +
/// rollups) plus the unregistered inventory keys (so the UI can offer "adopt").
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FileServersResponse {
    pub servers: Vec<FileServerView>,
    pub unregistered: Vec<UnregisteredServer>,
}

/// Create / adopt body. A new server may carry an optional first endpoint
/// inline; otherwise it is created identity-only and endpoints are added via the
/// `/endpoints` sub-resource. `adopt` additionally requires `key` to exist in
/// `file_inventory`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateFileServerRequest {
    pub key: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    /// Optional explicit workspace; falls back to the caller's workspace.
    #[serde(default)]
    pub workspace_id: Option<uuid::Uuid>,
    /// Optional first endpoint to create alongside the server.
    #[serde(default)]
    pub endpoint: Option<CreateEndpointRequest>,
}

/// Update body for the identity-only parent — all fields optional; `key` and
/// `workspace_id` are immutable.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateFileServerRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

/// Create an endpoint under a server.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateEndpointRequest {
    pub access_method: String,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub resource_ref: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

/// Update an endpoint — all fields optional. `access_method` is mutable but
/// still validated. `resource_ref`/`group_id` use double-option so they can be
/// explicitly cleared (`Some(None)`) vs left alone (`None`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateEndpointRequest {
    #[serde(default)]
    pub access_method: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default, deserialize_with = "crate::file_servers::model::double_option")]
    pub resource_ref: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::file_servers::model::double_option")]
    pub group_id: Option<Option<String>>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub verification_status: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
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
