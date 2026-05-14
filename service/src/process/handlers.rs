use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use crate::catalogue::model::CatalogueEntry;
use crate::models::error::ErrorResponse;
use crate::models::responses::TaskListResponse;
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;
use crate::AppState;
use super::model::{
    HpiLog, HpiMetric, HpiMetricSummary, HpiProcess, HpiTask, ProcessDetail, ProcessStats,
    ProcessUpdateRequest,
};
use super::queries;

/// Convert a DB `HpiTask` row into the `HumanTask`-shaped JSON expected by the
/// Mekhan frontend (`@aithericon/hpi-ui` types). Merges fields projected into
/// `detail` by the causality consumer (steps, instructions_mdsvex, net_id,
/// place, response_subject, org_id, payload, process_id, process_step,
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
///
/// Query parameters use a custom DSL (see `query/extractor.rs`): `filter`,
/// `sort`, `page`, `page_size`. Response shape is paginated.
#[utoipa::path(
    get,
    path = "/api/processes",
    responses(
        (status = 200, description = "Paginated list of processes", body = Paginated<HpiProcess>),
        (status = 400, description = "Invalid query", body = ErrorResponse),
    ),
    tag = "processes",
)]
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
#[utoipa::path(
    get,
    path = "/api/processes/stats",
    responses(
        (status = 200, description = "Process counts by status", body = ProcessStats),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "processes",
)]
pub async fn process_stats(State(state): State<AppState>) -> impl IntoResponse {
    match queries::process_stats(&state.db).await {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => {
            tracing::error!("process stats: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /api/processes/{process_id} — get process detail (with tasks, metrics, logs, artifact count).
#[utoipa::path(
    get,
    path = "/api/processes/{process_id}",
    params(("process_id" = String, Path, description = "Process id")),
    responses(
        (status = 200, description = "Process detail with tasks, metrics, logs", body = ProcessDetail),
        (status = 404, description = "Process not found"),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "processes",
)]
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

/// PUT /api/processes/{process_id} — partial update of a process.
#[utoipa::path(
    put,
    path = "/api/processes/{process_id}",
    params(("process_id" = String, Path, description = "Process id")),
    request_body = ProcessUpdateRequest,
    responses(
        (status = 200, description = "Updated process", body = HpiProcess),
        (status = 404, description = "Process not found"),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "processes",
)]
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

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct MetricQueryParams {
    pub key: Option<String>,
    pub limit: Option<i64>,
}

/// GET /api/processes/{process_id}/metrics/summary — aggregated metric stats per key.
#[utoipa::path(
    get,
    path = "/api/processes/{process_id}/metrics/summary",
    params(("process_id" = String, Path, description = "Process id")),
    responses(
        (status = 200, description = "Per-key min/max/avg/last", body = Vec<HpiMetricSummary>),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "processes",
)]
pub async fn get_process_metrics_summary(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
) -> impl IntoResponse {
    match queries::summarize_metrics(&state.db, &process_id).await {
        Ok(summary) => Json(summary).into_response(),
        Err(e) => {
            tracing::error!("process metrics summary: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /api/processes/{process_id}/metrics — list metrics for a process.
#[utoipa::path(
    get,
    path = "/api/processes/{process_id}/metrics",
    params(
        ("process_id" = String, Path, description = "Process id"),
        MetricQueryParams,
    ),
    responses(
        (status = 200, description = "Recent metric rows", body = Vec<HpiMetric>),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "processes",
)]
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

/// GET /api/processes/{process_id}/logs — list logs for a process with filter/pagination.
#[utoipa::path(
    get,
    path = "/api/processes/{process_id}/logs",
    params(("process_id" = String, Path, description = "Process id")),
    responses(
        (status = 200, description = "Paginated logs", body = Paginated<HpiLog>),
        (status = 400, description = "Invalid query", body = ErrorResponse),
    ),
    tag = "processes",
)]
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

/// GET /api/processes/{process_id}/tasks — list tasks for a process.
#[utoipa::path(
    get,
    path = "/api/processes/{process_id}/tasks",
    params(("process_id" = String, Path, description = "Process id")),
    responses(
        (status = 200, description = "Tasks (HumanTask-shaped JSON)", body = Vec<serde_json::Value>),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "processes",
)]
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

/// GET /api/processes/{process_id}/artifacts — list catalogue entries for a process.
#[utoipa::path(
    get,
    path = "/api/processes/{process_id}/artifacts",
    params(("process_id" = String, Path, description = "Process id")),
    responses(
        (status = 200, description = "Paginated catalogue entries", body = Paginated<CatalogueEntry>),
        (status = 400, description = "Invalid query", body = ErrorResponse),
    ),
    tag = "processes",
)]
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
#[utoipa::path(
    get,
    path = "/api/tasks",
    responses(
        (status = 200, description = "Paginated tasks (HumanTask-shaped) in `tasks` envelope", body = TaskListResponse),
        (status = 400, description = "Invalid query", body = ErrorResponse),
    ),
    tag = "tasks",
)]
pub async fn list_tasks(
    State(state): State<AppState>,
    params: QueryParams,
) -> impl IntoResponse {
    match queries::list_tasks(&state.db, &params).await {
        Ok(response) => {
            let tasks: Vec<JsonValue> =
                response.items.iter().map(to_human_task_json).collect();
            Json(TaskListResponse {
                tasks,
                total: response.total,
                page: response.page,
                page_size: response.page_size,
                total_pages: response.total_pages,
                has_next: response.has_next,
                has_previous: response.has_previous,
            })
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
#[utoipa::path(
    get,
    path = "/api/tasks/{id}",
    params(("id" = String, Path, description = "Task id")),
    responses(
        (status = 200, description = "HumanTask-shaped JSON object", body = serde_json::Value),
        (status = 404, description = "Task not found"),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "tasks",
)]
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

/// POST /api/tasks/{id}/complete — complete a task and publish NATS signal.
#[utoipa::path(
    post,
    path = "/api/tasks/{id}/complete",
    params(("id" = String, Path, description = "Task id")),
    request_body(content = serde_json::Value, description = "Completion payload — `data` field is forwarded as the task result"),
    responses(
        (status = 200, description = "Task completed", body = serde_json::Value),
        (status = 404, description = "Task not found"),
        (status = 409, description = "Task is not pending", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "tasks",
)]
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
    // The engine's GlobalHumanResultListener expects HumanTaskCompletion shape:
    // { task_id, data, completed_at, corr_id? }
    let net_id = task.detail.get("net_id").and_then(|v| v.as_str());
    let place = task.detail.get("place").and_then(|v| v.as_str());

    if let (Some(net_id), Some(place)) = (net_id, place) {
        let subject = format!("human.completed.{net_id}.{place}");
        let completion = serde_json::json!({
            "task_id": id,
            "data": body.get("data").cloned().unwrap_or(body.clone()),
            "completed_at": Utc::now().to_rfc3339(),
        });
        let payload = serde_json::to_vec(&completion).unwrap_or_default();
        if let Err(e) = state.nats.client().publish(subject.clone(), payload.into()).await {
            tracing::error!(subject = %subject, "failed to publish task completion: {e}");
        } else {
            tracing::info!(task_id = %id, subject = %subject, "published task completion");
        }
    }

    Json(to_human_task_json(&updated)).into_response()
}

/// POST /api/tasks/{id}/cancel — cancel a task and publish NATS signal.
#[utoipa::path(
    post,
    path = "/api/tasks/{id}/cancel",
    params(("id" = String, Path, description = "Task id")),
    request_body(content = serde_json::Value, description = "Optional `reason` field"),
    responses(
        (status = 200, description = "Task cancelled", body = serde_json::Value),
        (status = 404, description = "Task not found"),
        (status = 409, description = "Task is not pending", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "tasks",
)]
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
    // The engine's GlobalHumanResultListener expects HumanTaskCancellation shape:
    // { task_id, reason?, cancelled_at }
    let net_id = task.detail.get("net_id").and_then(|v| v.as_str());
    let place = task.detail.get("place").and_then(|v| v.as_str());

    if let (Some(net_id), Some(place)) = (net_id, place) {
        let subject = format!("human.cancelled.{net_id}.{place}");
        let cancellation = serde_json::json!({
            "task_id": id,
            "reason": body.get("reason").and_then(|v| v.as_str()),
            "cancelled_at": Utc::now().to_rfc3339(),
        });
        let payload = serde_json::to_vec(&cancellation).unwrap_or_default();
        if let Err(e) = state.nats.client().publish(subject.clone(), payload.into()).await {
            tracing::error!(subject = %subject, "failed to publish task cancellation: {e}");
        } else {
            tracing::info!(task_id = %id, subject = %subject, "published task cancellation");
        }
    }

    Json(to_human_task_json(&updated)).into_response()
}
