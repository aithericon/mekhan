use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use serde_json::Value;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct ProcessListQuery {
    pub status: Option<String>,
    pub namespace: Option<String>,
    pub search: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

impl ProcessListQuery {
    fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(ref s) = self.status {
            params.push(format!("status={}", s));
        }
        if let Some(ref s) = self.namespace {
            params.push(format!("namespace={}", s));
        }
        if let Some(ref s) = self.search {
            params.push(format!("search={}", s));
        }
        if let Some(l) = self.limit {
            params.push(format!("limit={}", l));
        }
        if let Some(o) = self.offset {
            params.push(format!("offset={}", o));
        }
        params.join("&")
    }
}

/// GET /api/processes — list processes (proxied to HPI)
pub async fn list_processes(
    State(state): State<AppState>,
    Query(query): Query<ProcessListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = state.hpi.list_processes(&query.to_query_string()).await;
    hpi_result(result)
}

/// GET /api/processes/:process_id — get single process (proxied to HPI)
pub async fn get_process(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = state.hpi.get_process(&process_id).await;
    hpi_result(result)
}

fn hpi_result(result: Result<Value, crate::hpi::HpiError>) -> Result<Json<Value>, (StatusCode, String)> {
    match result {
        Ok(v) => Ok(Json(v)),
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            Err((status, e.to_string()))
        }
    }
}
