//! Typed response envelopes for handlers that previously returned
//! `serde_json::Value`. Wire format is preserved byte-for-byte; these structs
//! only carry the shape into the OpenAPI spec so frontend codegen can produce
//! real types instead of `unknown` bags.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::catalogue::model::CatalogueEntry;
use crate::handlers::process_live::LogRow;

/// Response shape for `GET /api/instances/{id}/events`.
///
/// Mirrors the literal `json!({ "net_id": ..., "events": [...], "event_count": ... })`
/// envelope the handler previously emitted. `events` stays `Vec<serde_json::Value>`
/// because the petri-lab event shape is heterogeneous (one of many event types).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct InstanceEventsResponse {
    pub net_id: String,
    pub events: Vec<serde_json::Value>,
    pub event_count: usize,
}

/// Response shape for `GET /api/processes/{process_id}/logs/tail`.
///
/// Frontend reads `body.logs[]` directly — keep the single-field envelope.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LogsTailResponse {
    pub logs: Vec<LogRow>,
}

/// Response shape for `GET /api/processes/{process_id}/artifacts/list`.
///
/// Frontend reads `body.entries[]` directly — keep the single-field envelope.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ArtifactsListResponse {
    pub entries: Vec<CatalogueEntry>,
}

/// Response shape for `GET /api/tasks`.
///
/// `tasks` is `Vec<serde_json::Value>` because each task is a
/// `HumanTask`-shaped JSON built by `to_human_task_json` from heterogeneous DB
/// rows — the right level of typing for this endpoint.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TaskListResponse {
    pub tasks: Vec<serde_json::Value>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
    pub has_next: bool,
    pub has_previous: bool,
}

/// Response shape for `POST /api/files/upload/{id}/{node_id}`.
///
/// The handler returns S3 metadata after a successful upload.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FileUploadResponse {
    pub key: String,
    pub filename: String,
    pub content_type: String,
    pub size: usize,
}
