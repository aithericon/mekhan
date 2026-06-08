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
    pub created_at: DateTime<Utc>,
}
