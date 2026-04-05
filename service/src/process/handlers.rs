use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use crate::query::extractor::QueryParams;
use crate::AppState;
use super::model::{HpiTask, ProcessUpdateRequest};
use super::queries;

/// Convert a DB `HpiTask` row into the `HumanTask`-shaped JSON expected by the
/// Mekhan frontend (`@aithericon/hpi-ui` types). Merges fields projected into
/// `detail` by the causality consumer (steps, instructions_mdsvex, net_id,
/// place, response_subject, org_id, payload, hpi_process_id, hpi_process_step,
/// ...) with the top-level columns (`id` -> `task_id`, `status`, `title`,
/// timestamps).
///
/// The frontend keys list items on `task_id` and decides whether to render the
/// rich form by checking `steps?.length`, so these two fields must be present.
fn to_human_task_json(task: &HpiTask) -> JsonValue {
    // Start from detail so we inherit any extra fields the engine projected
    // (payload, sinks, metadata, ...). Column values override detail on conflict.
    let mut obj = match task.detail.clone() {
        JsonValue::Object(m) => m,
        _ => serde_json::Map::new(),
    };

    // Top-level column values (authoritative).
    obj.insert("task_id".to_string(), JsonValue::String(task.id.clone()));
    // Keep `id` too for backward compatibility with any callers that used it.
    obj.insert("id".to_string(), JsonValue::String(task.id.clone()));
    obj.insert(
        "process_id".to_string(),
        JsonValue::String(task.process_id.clone()),
    );
    obj.insert("title".to_string(), JsonValue::String(task.title.clone()));
    obj.insert("status".to_string(), JsonValue::String(task.status.clone()));
    obj.insert(
        "created_at".to_string(),
        JsonValue::String(task.created_at.to_rfc3339()),
    );
    if let Some(completed_at) = task.completed_at {
        obj.insert(
            "completed_at".to_string(),
            JsonValue::String(completed_at.to_rfc3339()),
        );
    }
    if let Some(ref assignee) = task.assignee {
        obj.insert(
            "assignee_id".to_string(),
            JsonValue::String(assignee.clone()),
        );
    }

    // Ensure the frontend's required fields exist with sensible defaults.
    obj.entry("steps".to_string())
        .or_insert_with(|| JsonValue::Array(vec![]));
    obj.entry("org_id".to_string())
        .or_insert(JsonValue::String(String::new()));

    JsonValue::Object(obj)
}

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
        Ok(tasks) => {
            let shaped: Vec<JsonValue> = tasks.iter().map(to_human_task_json).collect();
            Json(shaped).into_response()
        }
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
///
/// Returns `{ tasks, total, page, page_size, total_pages, has_next, has_previous }`
/// where each task is a `HumanTask`-shaped JSON object (see `to_human_task_json`).
/// The `tasks` key is what the Mekhan frontend's task store expects; the rest
/// of the pagination envelope is preserved for richer clients.
pub async fn list_tasks(
    State(state): State<AppState>,
    params: QueryParams,
) -> impl IntoResponse {
    match queries::list_tasks(&state.db, &params).await {
        Ok(response) => {
            let tasks: Vec<JsonValue> =
                response.items.iter().map(to_human_task_json).collect();
            Json(json!({
                "tasks": tasks,
                "total": response.total,
                "page": response.page,
                "page_size": response.page_size,
                "total_pages": response.total_pages,
                "has_next": response.has_next,
                "has_previous": response.has_previous,
            }))
            .into_response()
        }
        Err(e) => {
            tracing::warn!("task list: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /api/tasks/:id — get a single task.
///
/// Returns a `HumanTask`-shaped JSON object built from the DB row + `detail`
/// JSONB projected by the causality consumer. This includes `task_id`, `steps`,
/// `instructions_mdsvex`, `net_id`, `place`, etc. — everything the frontend
/// task form needs to render.
pub async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match queries::get_task(&state.db, &id).await {
        Ok(Some(task)) => Json(to_human_task_json(&task)).into_response(),
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

    Json(to_human_task_json(&updated)).into_response()
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

    Json(to_human_task_json(&updated)).into_response()
}
