use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A single catalogue entry (maps 1:1 to the `catalogue_entries` table).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct CatalogueEntry {
    /// Surrogate primary key (content-addressed reshape, docs/32).
    #[serde(default)]
    pub entry_id: Option<uuid::Uuid>,
    /// Logical content identity (bare-hex SHA-256). NULL for job-net
    /// artifacts; populated for legacy / by-reference rows.
    #[serde(default)]
    pub content_hash: Option<String>,
    // NOTE: since the content-addressed reshape (docs/32) the columns below are
    // nullable in the DB (legacy logical rows carry only a content_hash). The
    // catalogue read path projects them with COALESCE(...,'') in `queries.rs`
    // so the existing job-net consumers keep a non-Option `String` view; the
    // legacy/inventory surface reads through `file_inventory` instead.
    pub id: String,
    pub execution_id: String,
    pub job_id: Option<String>,
    pub name: String,
    pub category: String,
    pub filename: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub storage_path: Option<String>,
    pub source_net: Option<String>,
    pub source_place: Option<String>,
    pub signal_key: Option<String>,
    pub process_id: Option<String>,
    pub process_step: Option<String>,
    pub source_event_sequence: Option<i64>,
    pub file_metadata: serde_json::Value,
    pub user_metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub catalogued_at: DateTime<Utc>,
    /// Normalized, UI-facing projection of `file_metadata` (see
    /// [`crate::catalogue::metadata_view`]). Computed at read time via
    /// [`CatalogueEntry::hydrate_view`] — NOT a DB column (`#[sqlx(default)]`).
    /// `None` for rows whose `file_metadata` can't be parsed as
    /// `fmeta::FileMetadata` (empty/legacy/pre-probe).
    #[sqlx(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_view: Option<super::metadata_view::FileMetadataView>,
}

impl CatalogueEntry {
    /// Populate [`Self::metadata_view`] from `file_metadata`. Every read path
    /// that fetches entries for the client MUST call this (the value defaults to
    /// `None` after `FromRow`). New `query_as::<_, CatalogueEntry>` fetch sites
    /// need the same `.hydrate_view()` pass — grep for it in `queries.rs`.
    #[must_use]
    pub fn hydrate_view(mut self) -> Self {
        self.metadata_view = super::metadata_view::FileMetadataView::from_raw(&self.file_metadata);
        self
    }
}

/// Aggregate statistics.
#[derive(Debug, Serialize, ToSchema)]
pub struct CatalogueStats {
    pub total_entries: i64,
    pub total_size_bytes: i64,
    pub by_category: Vec<CategoryStats>,
    pub latest_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct CategoryStats {
    pub category: String,
    pub count: i64,
    pub total_bytes: i64,
}

/// Per-net summary.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct NetStats {
    pub source_net: Option<String>,
    pub total_artifacts: i64,
    pub total_bytes: i64,
    pub first_at: Option<DateTime<Utc>>,
    pub latest_at: Option<DateTime<Utc>>,
}

/// Lineage response: artifacts grouped by iteration/step.
#[derive(Debug, Serialize, ToSchema)]
pub struct LineageResponse {
    pub process_id: String,
    pub steps: Vec<LineageStep>,
    pub total_artifacts: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LineageStep {
    pub step: String,
    pub iteration: Option<i64>,
    pub artifacts: Vec<CatalogueEntry>,
}

/// The register command published by the petri-lab effect handler.
/// Mirrors `petri_domain::catalogue::CatalogueRegisterCommand`.
#[derive(Debug, Deserialize)]
pub struct CatalogueRegisterCommand {
    pub execution_id: String,
    pub job_id: String,
    pub artifact_id: String,
    pub name: String,
    pub category: String,
    pub filename: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub storage_path: Option<String>,
    #[serde(default)]
    pub file_metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub user_metadata: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub source_net: Option<String>,
    #[serde(default)]
    pub source_place: Option<String>,
    #[serde(default)]
    pub signal_key: Option<String>,
    #[serde(default)]
    pub process_id: Option<String>,
    #[serde(default)]
    pub process_step: Option<String>,
    /// Logical content identity (bare-hex SHA-256). The job path leaves this
    /// `None`; legacy / by-reference registration sets it.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// By-reference artifact (`log_artifact(upload=False)`): the bytes were not
    /// uploaded to the object store; they stay put and the inventory copy is
    /// located by `(file_server_id, reference_path)` instead of `storage_path`.
    #[serde(default)]
    pub by_reference: bool,
    /// File server holding a by-reference artifact's physical bytes.
    #[serde(default)]
    pub file_server_id: Option<String>,
    /// Path on `file_server_id` where a by-reference artifact physically lives.
    #[serde(default)]
    pub reference_path: Option<String>,
    /// Canonical (server-relative) root the `reference_path` is anchored to —
    /// the `endpoint_root` a `crawl` recorded. Stamped into the inventory row's
    /// `provenance` so an `adopt` can promote it onto the file-server endpoint's
    /// `root`. Optional: absent for artifacts not sourced from a rooted crawl.
    #[serde(default)]
    pub endpoint_root: Option<String>,
    /// Fileserve dispatch group of the executor that registered the artifact
    /// (`fileserve.<group>.read`: its runner id or pool routing partition).
    /// Stamped into inventory `provenance` so an `adopt` can promote it onto
    /// the endpoint's `group_id` — making the adopted endpoint dispatchable
    /// (and auto-verifiable) without manual configuration.
    #[serde(default)]
    pub serve_group: Option<String>,
    pub created_at: DateTime<Utc>,
}
