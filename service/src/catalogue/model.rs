use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single catalogue entry (maps 1:1 to the `catalogue_entries` table).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CatalogueEntry {
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
    pub correlation_id: Option<String>,
    pub process_id: Option<String>,
    pub process_step: Option<String>,
    pub trace_id: Option<String>,
    pub file_metadata: serde_json::Value,
    pub user_metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub catalogued_at: DateTime<Utc>,
}

/// Aggregate statistics.
#[derive(Debug, Serialize)]
pub struct CatalogueStats {
    pub total_entries: i64,
    pub total_size_bytes: i64,
    pub by_category: Vec<CategoryStats>,
    pub latest_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CategoryStats {
    pub category: String,
    pub count: i64,
    pub total_bytes: i64,
}

/// Per-net summary.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct NetStats {
    pub source_net: Option<String>,
    pub total_artifacts: i64,
    pub total_bytes: i64,
    pub first_at: Option<DateTime<Utc>>,
    pub latest_at: Option<DateTime<Utc>>,
}

/// Lineage response: artifacts grouped by iteration/step.
#[derive(Debug, Serialize)]
pub struct LineageResponse {
    pub process_id: String,
    pub steps: Vec<LineageStep>,
    pub total_artifacts: i64,
}

#[derive(Debug, Serialize)]
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
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub process_id: Option<String>,
    #[serde(default)]
    pub process_step: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
    pub created_at: DateTime<Utc>,
}
