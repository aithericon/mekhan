use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use serde_json::Value;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct TaskListQuery {
    pub status: Option<String>,
    pub search: Option<String>,
    pub process_id: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

impl TaskListQuery {
    fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(ref s) = self.status {
            params.push(format!("status={}", s));
        }
        if let Some(ref s) = self.search {
            params.push(format!("search={}", s));
        }
        if let Some(ref s) = self.process_id {
            params.push(format!("process_id={}", s));
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

/// GET /api/tasks — list tasks (proxied to HPI)
pub async fn list_tasks(
    State(state): State<AppState>,
    Query(query): Query<TaskListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = state.hpi.list_tasks(&query.to_query_string()).await;
    hpi_result(result)
}

/// GET /api/tasks/:task_id — get single task (proxied to HPI)
pub async fn get_task(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = state.hpi.get_task(&task_id).await;
    hpi_result(result)
}

#[derive(Debug, Deserialize)]
pub struct CompleteTaskBody {
    pub data: Value,
}

/// POST /api/tasks/:task_id/complete — complete a task (proxied to HPI)
pub async fn complete_task(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
    Json(body): Json<CompleteTaskBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = state.hpi.complete_task(&task_id, body.data).await;
    hpi_result(result)
}

#[derive(Debug, Deserialize)]
pub struct CancelTaskBody {
    pub reason: Option<String>,
}

/// POST /api/tasks/:task_id/cancel — cancel a task (proxied to HPI)
pub async fn cancel_task(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
    Json(body): Json<CancelTaskBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let result = state
        .hpi
        .cancel_task(&task_id, body.reason.as_deref())
        .await;
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
