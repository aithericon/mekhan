use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A single physical-copy row (maps 1:1 to the `file_inventory` table).
///
/// One row per *physical* copy of a file on a file server. `content_hash` is a
/// logical link to `catalogue_entries.content_hash` (index only — no hard FK,
/// since a physical file can be observed by `crawl` before its catalogue row
/// exists). See docs/32 §4.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct InventoryEntry {
    pub id: uuid::Uuid,
    pub content_hash: Option<String>,
    pub file_server_id: String,
    pub path: String,
    pub status: String,
    pub is_canonical: bool,
    pub copy_of: Option<uuid::Uuid>,
    pub migration_target: Option<String>,
    pub provenance: serde_json::Value,
    pub first_seen: DateTime<Utc>,
    pub last_seen: Option<DateTime<Utc>>,
    pub last_verified: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// One item in a batched by-reference register request.
///
/// Optional content metadata (`name`/`size_bytes`/`mime_type`) is used to
/// UPSERT a logical `catalogue_entries` row keyed on `content_hash`; the
/// `file_inventory` row is always upserted on `(file_server_id, path)`. No
/// bytes are transferred — this is the online crawl/reconcile path, not the 4M
/// offline load (that goes through the importer).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct InventoryRegisterItem {
    /// Bare-hex SHA-256, if known. NULL until a `probe` populates it.
    #[serde(default)]
    pub content_hash: Option<String>,
    pub file_server_id: String,
    pub path: String,
    pub status: String,
    #[serde(default = "default_provenance")]
    pub provenance: serde_json::Value,
    // Optional catalogue upsert fields — only used when `content_hash` is set.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<i64>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

fn default_provenance() -> serde_json::Value {
    serde_json::json!({})
}

/// Batched register request body.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct InventoryRegisterRequest {
    pub entries: Vec<InventoryRegisterItem>,
}

/// Result counts of a batched register call.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InventoryRegisterResponse {
    /// `file_inventory` rows inserted or updated.
    pub inventory_upserted: i64,
    /// `catalogue_entries` logical rows newly inserted (ON CONFLICT skips
    /// pre-existing hashes).
    pub catalogue_inserted: i64,
}

/// Per-status / per-server inventory counts.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InventoryStats {
    pub total: i64,
    pub by_status: Vec<InventoryCount>,
    pub by_server: Vec<InventoryCount>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct InventoryCount {
    pub key: String,
    pub count: i64,
}
