use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use crate::catalogue::model::CatalogueEntry;
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
    /// Whether the backing server currently has at least one endpoint that can
    /// actually deliver bytes (`endpoint_servable`: healthy, not
    /// mismatch/conflict, transport-dispatchable). Drives the Data browser's
    /// Download affordance — false renders a disabled hint instead of a dead
    /// click.
    pub servable: bool,
}

/// A unified Data-browser row: the full catalogue entry (so the browser can
/// render the same rich artifact card, lineage/provenance/download, schema and
/// metadata the catalogue page did) PLUS its physical copies. The bridge
/// `content_hash` is surfaced navigably here — the whole point of consolidating
/// the catalogue + inventory split worlds into one surface.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DataEntry {
    /// The logical catalogue entry, flattened so the frontend `DataEntry` is a
    /// structural superset of `CatalogueEntry` (reuses `ArtifactCard` as-is).
    #[serde(flatten)]
    pub entry: CatalogueEntry,
    /// Physical copies (from `file_inventory`, joined by `content_hash`).
    pub copies: Vec<DataCopy>,
}

/// An index-only file: observed physically on a server but with no logical
/// catalogue identity yet (no matching `catalogue_entries` row). Surfaced so the
/// unified browser shows what's been crawled but not registered/hashed.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UncataloguedFile {
    pub name: String,
    pub content_hash: Option<String>,
    pub first_seen: DateTime<Utc>,
    /// The physical copy this file was observed as.
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
    pub uncatalogued: Vec<UncataloguedFile>,
    /// Total number of uncatalogued physical copies (not just the peek).
    pub uncatalogued_count: i64,
}
