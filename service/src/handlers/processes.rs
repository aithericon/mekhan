use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;

use crate::tasks::process_types::ProcessState;
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct ProcessListQuery {
    pub status: Option<String>,
}

/// GET /api/processes — list all processes
pub async fn list_processes(
    State(state): State<AppState>,
    Query(query): Query<ProcessListQuery>,
) -> Json<Vec<ProcessState>> {
    let processes = state.process_index.list(query.status.as_deref());
    Json(processes)
}

/// GET /api/processes/:process_id — get single process
pub async fn get_process(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
) -> Result<Json<ProcessState>, (StatusCode, String)> {
    match state.process_index.get(&process_id) {
        Some(p) => Ok(Json(p)),
        None => Err((StatusCode::NOT_FOUND, "Process not found".to_string())),
    }
}
