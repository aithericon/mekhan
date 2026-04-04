use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use crate::query::extractor::QueryParams;
use crate::AppState;
use super::model::ProcessUpdateRequest;
use super::queries;

/// GET /api/processes — list processes with filter/sort/pagination.
pub async fn list_processes(
    State(state): State<AppState>,
    params: QueryParams,
) -> impl IntoResponse {
    match queries::list_processes(&state.db, &params).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            tracing::warn!("process list: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /api/processes/stats — aggregate process statistics.
pub async fn process_stats(State(state): State<AppState>) -> impl IntoResponse {
    match queries::process_stats(&state.db).await {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => {
            tracing::error!("process stats: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /api/processes/:process_id — get process detail (with tasks, metrics, logs, artifact count).
pub async fn get_process(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
) -> impl IntoResponse {
    match queries::get_process_detail(&state.db, &process_id).await {
        Ok(Some(detail)) => Json(detail).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("process get: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// PUT /api/processes/:process_id — partial update of a process.
pub async fn update_process(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
    Json(body): Json<ProcessUpdateRequest>,
) -> impl IntoResponse {
    match queries::update_process(&state.db, &process_id, &body).await {
        Ok(Some(process)) => Json(process).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("process update: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct MetricQueryParams {
    pub key: Option<String>,
    pub limit: Option<i64>,
}

/// GET /api/processes/:process_id/metrics — list metrics for a process.
pub async fn get_process_metrics(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
    Query(params): Query<MetricQueryParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(500);
    match queries::list_metrics(&state.db, &process_id, params.key.as_deref(), limit).await {
        Ok(metrics) => Json(metrics).into_response(),
        Err(e) => {
            tracing::error!("process metrics: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /api/processes/:process_id/logs — list logs for a process with filter/pagination.
pub async fn get_process_logs(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
    params: QueryParams,
) -> impl IntoResponse {
    match queries::list_logs(&state.db, &process_id, &params).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            tracing::warn!("process logs: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /api/processes/:process_id/tasks — list tasks for a process.
pub async fn get_process_tasks(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
) -> impl IntoResponse {
    match queries::list_process_tasks(&state.db, &process_id).await {
        Ok(tasks) => Json(tasks).into_response(),
        Err(e) => {
            tracing::error!("process tasks: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /api/processes/:process_id/artifacts — list catalogue entries for a process.
pub async fn get_process_artifacts(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
    params: QueryParams,
) -> impl IntoResponse {
    match queries::list_process_artifacts(&state.db, &process_id, &params).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            tracing::warn!("process artifacts: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /api/tasks — list all tasks with filter/sort/pagination.
pub async fn list_tasks(
    State(state): State<AppState>,
    params: QueryParams,
) -> impl IntoResponse {
    match queries::list_tasks(&state.db, &params).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => {
            tracing::warn!("task list: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /api/tasks/:id — get a single task.
pub async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match queries::get_task(&state.db, &id).await {
        Ok(Some(task)) => Json(task).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("task get: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /api/tasks/:id/complete — complete a task and publish NATS signal.
pub async fn complete_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // First get the task to extract net_id and place from detail
    let task = match queries::get_task(&state.db, &id).await {
        Ok(Some(t)) => t,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("task complete lookup: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    if task.status != "pending" {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": format!("task is already {}", task.status) })),
        )
            .into_response();
    }

    // Update task status in DB
    let updated = match queries::update_task_status(&state.db, &id, "completed", Some(&body)).await
    {
        Ok(Some(t)) => t,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("task complete update: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Publish NATS signal: human.completed.{net_id}.{place}
    let net_id = task.detail.get("net_id").and_then(|v| v.as_str());
    let place = task.detail.get("place").and_then(|v| v.as_str());

    if let (Some(net_id), Some(place)) = (net_id, place) {
        let subject = format!("human.completed.{net_id}.{place}");
        let payload = serde_json::to_vec(&body).unwrap_or_default();
        if let Err(e) = state.nats.client().publish(subject.clone(), payload.into()).await {
            tracing::error!(subject = %subject, "failed to publish task completion: {e}");
        } else {
            tracing::info!(task_id = %id, subject = %subject, "published task completion");
        }
    }

    Json(updated).into_response()
}

/// POST /api/tasks/:id/cancel — cancel a task and publish NATS signal.
pub async fn cancel_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // First get the task to extract net_id and place from detail
    let task = match queries::get_task(&state.db, &id).await {
        Ok(Some(t)) => t,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("task cancel lookup: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    if task.status != "pending" {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": format!("task is already {}", task.status) })),
        )
            .into_response();
    }

    // Update task status in DB
    let updated = match queries::update_task_status(&state.db, &id, "cancelled", Some(&body)).await
    {
        Ok(Some(t)) => t,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("task cancel update: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Publish NATS signal: human.cancelled.{net_id}.{place}
    let net_id = task.detail.get("net_id").and_then(|v| v.as_str());
    let place = task.detail.get("place").and_then(|v| v.as_str());

    if let (Some(net_id), Some(place)) = (net_id, place) {
        let subject = format!("human.cancelled.{net_id}.{place}");
        let payload = serde_json::to_vec(&body).unwrap_or_default();
        if let Err(e) = state.nats.client().publish(subject.clone(), payload.into()).await {
            tracing::error!(subject = %subject, "failed to publish task cancellation: {e}");
        } else {
            tracing::info!(task_id = %id, subject = %subject, "published task cancellation");
        }
    }

    Json(updated).into_response()
}
