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

/// One item in a batched by-reference **register** request.
///
/// `content_hash` is REQUIRED: register fills both halves of the equation (a
/// logical `catalogue_entries` row keyed on the hash AND a physical
/// `file_inventory` row on `(file_server_id, path)`), so a row with no content
/// identity is rejected (400). To record a file you've *seen* but not yet
/// hashed, use [`InventoryIndexItem`] (`POST /api/v1/inventory/index`) instead.
/// Optional content metadata (`name`/`size_bytes`/`mime_type`) enriches the
/// catalogue row. No bytes are transferred — this is the online crawl/reconcile
/// path, not the 4M offline load (that goes through the importer).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct InventoryRegisterItem {
    /// Bare-hex SHA-256 of the content. REQUIRED — supplied by a `probe`. An
    /// item missing this is rejected; observe-only goes through `/index`.
    #[serde(default)]
    pub content_hash: Option<String>,
    pub file_server_id: String,
    pub path: String,
    pub status: String,
    #[serde(default = "default_provenance")]
    pub provenance: serde_json::Value,
    // Optional catalogue-enrichment fields.
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

fn default_indexed_status() -> String {
    "indexed".to_string()
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

/// One item in a batched **index** request — a hashless physical observation.
///
/// Index records that a file exists at `path` on a server, WITHOUT a content
/// identity. It writes `file_inventory` only (never `catalogue_entries`),
/// because we haven't hashed the bytes yet. This is the landing zone for
/// `crawl` output; promote to a coupled catalogue row later via `/register`
/// once a `probe` supplies the hash. There is deliberately no `content_hash`
/// field here — claiming an identity is what `register` is for.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct InventoryIndexItem {
    pub path: String,
    /// Physical-observation status — defaults to `indexed`.
    #[serde(default = "default_indexed_status")]
    pub status: String,
    #[serde(default = "default_provenance")]
    pub provenance: serde_json::Value,
}

/// Batched index request body — all items share one `file_server_id`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct InventoryIndexRequest {
    pub file_server_id: String,
    pub items: Vec<InventoryIndexItem>,
}

/// Result count of a batched index call.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InventoryIndexResponse {
    /// `file_inventory` rows inserted or updated. No catalogue rows are written.
    pub inventory_upserted: i64,
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
