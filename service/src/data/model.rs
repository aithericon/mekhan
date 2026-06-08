use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use crate::query::pagination::Paginated;

/// One physical copy of an entry's content, with its file server resolved.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataCopy {
    pub file_server_id: String,
    pub path: String,
    pub status: String,
    pub is_canonical: bool,
    /// Display name of the backing `file_servers` row, if the key is registered.
    pub server_display_name: Option<String>,
    /// Transport kind of the backing server (`object_store`/`s3`/`sftp`/…).
    pub server_kind: Option<String>,
}

/// A unified Data-browser row: the logical entry (catalogued content) plus its
/// physical copies. The bridge `content_hash` is surfaced navigably here — the
/// whole point of consolidating the catalogue + inventory split worlds.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataEntry {
    /// Catalogue surrogate id; `None` for an uncatalogued (index-only) row.
    pub entry_id: Option<uuid::Uuid>,
    pub content_hash: Option<String>,
    pub name: String,
    pub category: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub created_at: DateTime<Utc>,
    /// True when backed by a `catalogue_entries` row (logical identity exists).
    pub catalogued: bool,
    /// Physical copies (from `file_inventory`, joined by `content_hash`).
    pub copies: Vec<DataCopy>,
}

/// Response of `GET /api/v1/data/entries`: a page of catalogued entries (each
/// with copies), plus a capped peek at uncatalogued (index-only) files and the
/// total uncatalogued count.
#[derive(Debug, Serialize, ToSchema)]
pub struct DataEntriesResponse {
    #[serde(flatten)]
    pub page: Paginated<DataEntry>,
    /// Index-only files with no logical catalogue identity yet (capped peek).
    pub uncatalogued: Vec<DataEntry>,
    /// Total number of uncatalogued physical copies (not just the peek).
    pub uncatalogued_count: i64,
}
