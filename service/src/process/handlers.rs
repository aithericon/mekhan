use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use super::model::{
    HpiLog, HpiMetric, HpiMetricSummary, HpiProcess, HpiTask, ProcessDetail, ProcessStats,
    ProcessUpdateRequest,
};
use super::queries;
use crate::auth::AuthUser;
use crate::catalogue::model::CatalogueEntry;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::responses::TaskListResponse;
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;
use crate::AppState;

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

/// GET /api/v1/processes — list processes with filter/sort/pagination.
///
/// Query parameters use a custom DSL (see `query/extractor.rs`): `filter`,
/// `sort`, `page`, `page_size`. Response shape is paginated.
#[utoipa::path(
    get,
    path = "/api/v1/processes",
    responses(
        (status = 200, description = "Paginated list of processes", body = Paginated<HpiProcess>),
        (status = 400, description = "Invalid query", body = ErrorResponse),
    ),
    tag = "processes",
)]
pub async fn list_processes(
    State(state): State<AppState>,
    params: QueryParams,
) -> Result<Json<serde_json::Value>, ApiError> {
    let response = queries::list_processes(&state.db, &params)
        .await
        .map_err(|e| {
            tracing::warn!("process list: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(serde_json::to_value(response).unwrap_or(json!({}))))
}

/// GET /api/v1/processes/stats — aggregate process statistics.
#[utoipa::path(
    get,
    path = "/api/v1/processes/stats",
    responses(
        (status = 200, description = "Process counts by status", body = ProcessStats),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "processes",
)]
pub async fn process_stats(State(state): State<AppState>) -> Result<Json<ProcessStats>, ApiError> {
    let stats = queries::process_stats(&state.db).await.map_err(|e| {
        tracing::error!("process stats: {e}");
        ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
    })?;
    Ok(Json(stats))
}

/// GET /api/v1/processes/{process_id} — get process detail (with tasks, metrics, logs, artifact count).
#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}",
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
) -> Result<Json<ProcessDetail>, ApiError> {
    let detail = queries::get_process_detail(&state.db, &process_id)
        .await
        .map_err(|e| {
            tracing::error!("process get: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;
    Ok(Json(detail))
}

/// PUT /api/v1/processes/{process_id} — partial update of a process.
#[utoipa::path(
    put,
    path = "/api/v1/processes/{process_id}",
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
) -> Result<Json<HpiProcess>, ApiError> {
    let process = queries::update_process(&state.db, &process_id, &body)
        .await
        .map_err(|e| {
            tracing::error!("process update: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;
    Ok(Json(process))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct MetricQueryParams {
    pub key: Option<String>,
    pub limit: Option<i64>,
}

/// GET /api/v1/processes/{process_id}/metrics/summary — aggregated metric stats per key.
#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/metrics/summary",
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
) -> Result<Json<Vec<HpiMetricSummary>>, ApiError> {
    let summary = queries::summarize_metrics(&state.db, &process_id)
        .await
        .map_err(|e| {
            tracing::error!("process metrics summary: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?;
    Ok(Json(summary))
}

/// GET /api/v1/processes/{process_id}/metrics — list metrics for a process.
#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/metrics",
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
) -> Result<Json<Vec<HpiMetric>>, ApiError> {
    let limit = params.limit.unwrap_or(500);
    let metrics = queries::list_metrics(&state.db, &process_id, params.key.as_deref(), limit)
        .await
        .map_err(|e| {
            tracing::error!("process metrics: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?;
    Ok(Json(metrics))
}

/// GET /api/v1/processes/{process_id}/logs — list logs for a process with filter/pagination.
#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/logs",
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
) -> Result<Json<serde_json::Value>, ApiError> {
    let response = queries::list_logs(&state.db, &process_id, &params)
        .await
        .map_err(|e| {
            tracing::warn!("process logs: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(serde_json::to_value(response).unwrap_or(json!({}))))
}

/// GET /api/v1/processes/{process_id}/tasks — list tasks for a process.
#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/tasks",
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
) -> Result<Json<Vec<JsonValue>>, ApiError> {
    let tasks = queries::list_process_tasks(&state.db, &process_id)
        .await
        .map_err(|e| {
            tracing::error!("process tasks: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?;
    let shaped: Vec<JsonValue> = tasks.iter().map(to_human_task_json).collect();
    Ok(Json(shaped))
}

/// GET /api/v1/processes/{process_id}/artifacts — list catalogue entries for a process.
#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/artifacts",
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
) -> Result<Json<serde_json::Value>, ApiError> {
    let response = queries::list_process_artifacts(&state.db, &process_id, &params)
        .await
        .map_err(|e| {
            tracing::warn!("process artifacts: {e}");
            ApiError::bad_request(e.to_string())
        })?;
    Ok(Json(serde_json::to_value(response).unwrap_or(json!({}))))
}

/// GET /api/v1/tasks — list all tasks with filter/sort/pagination.
///
/// Returns `{ tasks, total, page, page_size, total_pages, has_next, has_previous }`
/// where each task is a `HumanTask`-shaped JSON object (see `to_human_task_json`).
/// The `tasks` key is what the Mekhan frontend's task store expects; the rest
/// of the pagination envelope is preserved for richer clients.
#[utoipa::path(
    get,
    path = "/api/v1/tasks",
    responses(
        (status = 200, description = "Paginated tasks (HumanTask-shaped) in `tasks` envelope", body = TaskListResponse),
        (status = 400, description = "Invalid query", body = ErrorResponse),
    ),
    tag = "tasks",
)]
pub async fn list_tasks(
    State(state): State<AppState>,
    params: QueryParams,
) -> Result<Json<TaskListResponse>, ApiError> {
    let response = queries::list_tasks(&state.db, &params).await.map_err(|e| {
        tracing::warn!("task list: {e}");
        ApiError::bad_request(e.to_string())
    })?;
    let tasks: Vec<JsonValue> = response.items.iter().map(to_human_task_json).collect();
    Ok(Json(TaskListResponse {
        tasks,
        total: response.total,
        page: response.page,
        page_size: response.page_size,
        total_pages: response.total_pages,
        has_next: response.has_next,
        has_previous: response.has_previous,
    }))
}

/// GET /api/v1/tasks/inbox — the caller's eligibility-filtered human-task inbox
/// (docs/33 P4).
///
/// Returns `{ tasks: [...] }` where each task is a `HumanTask`-shaped JSON object
/// (same shape as `GET /api/v1/tasks`). The set is the union of (a) `offered`
/// tasks whose backing human capacity the caller is *enrolled in* — the offers
/// they may claim — and (b) `claimed` tasks the caller already holds (their work
/// in flight). Workspace-scoped; see [`queries::inbox_tasks`] for the eligibility
/// contract (membership now; caps-vs-`requirements` matching deferred).
///
/// Mounted BEFORE `/tasks/{id}` so matchit routes the literal `inbox` here.
#[utoipa::path(
    get,
    path = "/api/v1/tasks/inbox",
    responses(
        (status = 200, description = "The caller's inbox (offered-to-me + claimed-by-me), HumanTask-shaped, in a `tasks` envelope", body = serde_json::Value),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "tasks",
)]
pub async fn inbox(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<JsonValue>, ApiError> {
    let workspace_id = user.workspace_id.unwrap_or_else(uuid::Uuid::nil);
    let member = user.subject_as_uuid();
    let rows = queries::inbox_tasks(&state.db, workspace_id, member)
        .await
        .map_err(|e| {
            tracing::error!("task inbox: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?;
    let tasks: Vec<JsonValue> = rows.iter().map(to_human_task_json).collect();
    Ok(Json(json!({ "tasks": tasks })))
}

/// GET /api/v1/tasks/:id — get a single task.
///
/// Returns a `HumanTask`-shaped JSON object built from the DB row + `detail`
/// JSONB projected by the causality consumer. This includes `task_id`, `steps`,
/// `instructions_mdsvex`, `net_id`, `place`, etc. — everything the frontend
/// task form needs to render.
#[utoipa::path(
    get,
    path = "/api/v1/tasks/{id}",
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
) -> Result<Json<JsonValue>, ApiError> {
    let task = queries::get_task(&state.db, &id)
        .await
        .map_err(|e| {
            tracing::error!("task get: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;
    Ok(Json(to_human_task_json(&task)))
}

/// Backstop coercion of human-task form submissions to the JSON shape their
/// declared `TaskFieldKind` implies. The task-form UI already coerces (where
/// it can also surface per-field errors before submit), but the `aithericon`
/// CLI, raw API callers, and trigger paths post completions without it — and
/// the compiler's enforced `Data__*` schema types Number/Bool strictly, so an
/// uncoerced `"23"`/`"true"` wedges the net at `t_*_yield`. Coercing at this
/// single completion ingress keeps the strict schema honest for every caller
/// without loosening it (the reverted "lenient scalar schemas" approach).
///
/// Only `number`/`range`/`rating` (→ JSON number) and `checkbox` (→ JSON
/// bool) are rewritten; other kinds are already strings. Non-empty input that
/// won't parse as a number is left untouched so the schema surfaces a clear
/// failure rather than this silently corrupting data; a blank numeric value
/// is dropped (an unfilled optional number is "not provided", which the
/// open-`additionalProperties` schema accepts — a present `""` would not).
fn coerce_form_data(detail: &JsonValue, data: JsonValue) -> JsonValue {
    let JsonValue::Object(mut map) = data else {
        return data;
    };
    for (name, kind) in form_field_kinds(detail) {
        if !map.contains_key(&name) {
            continue;
        }
        match kind.as_str() {
            "number" | "range" | "rating" => match &map[&name] {
                JsonValue::Number(_) => {}
                JsonValue::String(s) => {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        map.remove(&name);
                    } else if let Some(n) = parse_json_number(trimmed) {
                        map.insert(name, n);
                    }
                }
                _ => {}
            },
            "checkbox" => {
                let b = coerce_bool(&map[&name]);
                map.insert(name, JsonValue::Bool(b));
            }
            _ => {}
        }
    }
    JsonValue::Object(map)
}

/// `(field name, declared kind)` for every Input block across the task's form
/// steps, read from the engine `HumanTaskRequest` projected into the task
/// `detail`. Tolerates absent/missing `steps` (returns empty → no-op), so a
/// task created by a path that doesn't carry a form simply isn't touched.
fn form_field_kinds(detail: &JsonValue) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(steps) = detail.get("steps").and_then(|v| v.as_array()) else {
        return out;
    };
    for step in steps {
        let Some(blocks) = step.get("blocks").and_then(|v| v.as_array()) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(|v| v.as_str()) != Some("input") {
                continue;
            }
            let Some(field) = block.get("field") else {
                continue;
            };
            if let (Some(name), Some(kind)) = (
                field.get("name").and_then(|v| v.as_str()),
                field.get("kind").and_then(|v| v.as_str()),
            ) {
                out.push((name.to_string(), kind.to_string()));
            }
        }
    }
    out
}

/// Parse a numeric string into the narrowest JSON number (`i64`/`u64`/`f64`),
/// or `None` if it isn't a finite number.
fn parse_json_number(s: &str) -> Option<JsonValue> {
    if let Ok(i) = s.parse::<i64>() {
        return Some(JsonValue::from(i));
    }
    if let Ok(u) = s.parse::<u64>() {
        return Some(JsonValue::from(u));
    }
    let f = s.parse::<f64>().ok()?;
    if !f.is_finite() {
        return None;
    }
    serde_json::Number::from_f64(f).map(JsonValue::Number)
}

/// Best-effort string/number → bool for checkbox fields posted by non-UI
/// callers. The Svelte form already sends a real boolean; this only kicks in
/// for the CLI / raw API.
fn coerce_bool(v: &JsonValue) -> bool {
    match v {
        JsonValue::Bool(b) => *b,
        JsonValue::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "true" | "on" | "1" | "yes" | "y"
        ),
        JsonValue::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        _ => false,
    }
}

/// POST /api/v1/tasks/{id}/complete — complete a task and publish NATS signal.
#[utoipa::path(
    post,
    path = "/api/v1/tasks/{id}/complete",
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
) -> Result<Json<JsonValue>, ApiError> {
    // First get the task to extract net_id and place from detail
    let task = queries::get_task(&state.db, &id)
        .await
        .map_err(|e| {
            tracing::error!("task complete lookup: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;

    // An unpooled task is `pending`; a capacity-bound (offer) task is `claimed`
    // once a member has bound a slot (docs/34). Both are completable/cancelable;
    // `offered` (unclaimed) / terminal states are not.
    if !matches!(task.status.as_str(), "pending" | "claimed") {
        return Err(ApiError::conflict(format!(
            "task is already {}",
            task.status
        )));
    }

    // Coerce form submissions to their declared field kinds before this
    // completion is persisted and signalled (backstop for non-UI callers;
    // see `coerce_form_data`). Rebuild the `{ data: ... }` envelope so the
    // persisted task detail (rendered by the completed-task panel) and the
    // engine signal carry identical typed values.
    let raw_data = body.get("data").cloned().unwrap_or_else(|| body.clone());
    let coerced_data = coerce_form_data(&task.detail, raw_data);
    let body = match body {
        JsonValue::Object(mut m) if m.contains_key("data") => {
            m.insert("data".to_string(), coerced_data.clone());
            JsonValue::Object(m)
        }
        _ => coerced_data.clone(),
    };

    // Update task status in DB
    let updated = queries::update_task_status(&state.db, &id, "completed", Some(&body))
        .await
        .map_err(|e| {
            tracing::error!("task complete update: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;

    // Publish NATS signal: human.completed.{net_id}.{place}
    // The engine's GlobalHumanResultListener expects HumanTaskCompletion shape:
    // { task_id, data, completed_at, corr_id? }
    let net_id = task.detail.get("net_id").and_then(|v| v.as_str());
    let place = task.detail.get("place").and_then(|v| v.as_str());

    if let (Some(net_id), Some(place)) = (net_id, place) {
        let subject = format!("human.completed.{net_id}.{place}");
        let completion = serde_json::json!({
            "task_id": id,
            "data": coerced_data,
            "completed_at": Utc::now().to_rfc3339(),
        });
        let payload = serde_json::to_vec(&completion).unwrap_or_default();
        if let Err(e) = state
            .nats
            .client()
            .publish(subject.clone(), payload.into())
            .await
        {
            tracing::error!(subject = %subject, "failed to publish task completion: {e}");
        } else {
            tracing::info!(task_id = %id, subject = %subject, "published task completion");
        }
    }

    Ok(Json(to_human_task_json(&updated)))
}

/// POST /api/v1/tasks/{id}/cancel — cancel a task and publish NATS signal.
#[utoipa::path(
    post,
    path = "/api/v1/tasks/{id}/cancel",
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
) -> Result<Json<JsonValue>, ApiError> {
    // First get the task to extract net_id and place from detail
    let task = queries::get_task(&state.db, &id)
        .await
        .map_err(|e| {
            tracing::error!("task cancel lookup: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;

    // An unpooled task is `pending`; a capacity-bound (offer) task is `claimed`
    // once a member has bound a slot (docs/34). Both are completable/cancelable;
    // `offered` (unclaimed) / terminal states are not.
    if !matches!(task.status.as_str(), "pending" | "claimed") {
        return Err(ApiError::conflict(format!(
            "task is already {}",
            task.status
        )));
    }

    // Update task status in DB
    let updated = queries::update_task_status(&state.db, &id, "cancelled", Some(&body))
        .await
        .map_err(|e| {
            tracing::error!("task cancel update: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;

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
        if let Err(e) = state
            .nats
            .client()
            .publish(subject.clone(), payload.into())
            .await
        {
            tracing::error!(subject = %subject, "failed to publish task cancellation: {e}");
        } else {
            tracing::info!(task_id = %id, subject = %subject, "published task cancellation");
        }
    }

    Ok(Json(to_human_task_json(&updated)))
}

/// POST /api/v1/tasks/{id}/claim — claim an offered human task (docs/34 §6).
///
/// A pooled `HumanTask` is *offered* (not assigned) to eligible available
/// members: the offer parks in the capacity `pool-<id>` net and a member binds
/// it by claiming. This endpoint publishes that claim and returns **202
/// Accepted immediately** (docs/33 §11) — it is a pure, projection-confirmed
/// fire-and-forget: the authoritative `claimed` status (and `assignee`) arrives
/// asynchronously via the causality projection of the pool net's `in_use`
/// token, NOT from this handler. We deliberately take no optimistic local lock
/// and do NOT write `status` here; the engine `t_claim` guard is authoritative
/// (an ineligible member's claim simply fails to bind and the row stays
/// `offered`).
///
/// The caller IS the claiming member: their id is `subject_as_uuid()`, carried
/// as the offer net's `runner_id` correlation (docs/34 §3 — bind ANY free slot
/// of the member, not an exact unit).
///
/// Pool-net resolution contract: the offered row's `id` IS the `grant_id`, and
/// the backing pool net is `pool-<capacity_id>`. We read the net id from the
/// offered projection's `detail` — `detail->>'pool_net_id'` is the canonical
/// field the offered projection (`causality/ingest.rs` §4.1) writes; we fall
/// back to deriving `pool-<capacity_id>` from `detail->>'capacity_id'` if only
/// the bare capacity id was projected. If neither is present the offer cannot
/// be routed → 422.
#[utoipa::path(
    post,
    path = "/api/v1/tasks/{id}/claim",
    params(("id" = String, Path, description = "Task id (= offer grant_id)")),
    responses(
        (status = 200, description = "Unpooled task soft-claimed (assigned) synchronously", body = serde_json::Value),
        (status = 202, description = "Pooled claim published; `claimed` status follows via projection", body = serde_json::Value),
        (status = 404, description = "Task not found"),
        (status = 409, description = "Task is not claimable (already claimed or wrong state)", body = ErrorResponse),
        (status = 422, description = "Offered row carries no resolvable pool net id", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "tasks",
)]
pub async fn claim_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<JsonValue>), ApiError> {
    // The caller is the claiming member; the offer net correlates on this id as
    // `runner_id` (docs/34 §3).
    let member_id = user.subject_as_uuid();

    let task = queries::get_task(&state.db, &id)
        .await
        .map_err(|e| {
            tracing::error!("task claim lookup: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;

    // An UNPOOLED task (`pending`, no `capacity_id`) has no offer pool to route a
    // claim through — claiming it is a soft, control-plane assign (docs/33 surface
    // unification): set assignee + flip to `claimed` so it moves from the inbox's
    // "open to anyone" bucket into the claimer's "in progress". Advisory only;
    // anyone can still complete it (the engine net just awaits the signal).
    if task.status == "pending" && task.detail.get("capacity_id").is_none() {
        let updated = queries::soft_claim_task(&state.db, &id, &member_id.to_string())
            .await
            .map_err(|e| {
                tracing::error!("task soft-claim: {e}");
                ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
            })?
            // Lost the race (already claimed/completed between lookup and update).
            .ok_or_else(|| ApiError::conflict("task is no longer open to claim"))?;
        tracing::info!(task_id = %id, member = %member_id, "soft-claimed unpooled task");
        return Ok((StatusCode::OK, Json(to_human_task_json(&updated))));
    }

    if task.status != "offered" {
        return Err(ApiError::conflict(format!(
            "task is not offered (status is {})",
            task.status
        )));
    }

    // Resolve the pool net id from the offered projection's `detail`. Prefer the
    // explicit `pool_net_id`; fall back to deriving `pool-<capacity_id>` from a
    // bare `capacity_id`. (Contract with `causality/ingest.rs` §4.1.)
    let pool_net_id = task
        .detail
        .get("pool_net_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            task.detail
                .get("capacity_id")
                .and_then(|v| v.as_str())
                .map(|cap| format!("pool-{cap}"))
        })
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "offered task carries no pool_net_id/capacity_id to route the claim",
            )
        })?;

    // Publish the claim onto the capacity pool's existing cross-net bridge. This
    // is fire-and-forget; the `claimed` status is projected from the pool net's
    // `in_use` token (docs/34 §4.2), not set here.
    crate::runners_presence::inject_claim(&state.nats, &pool_net_id, &id, &member_id.to_string())
        .await;

    tracing::info!(
        task_id = %id,
        member = %member_id,
        pool_net_id = %pool_net_id,
        "published human task claim"
    );

    // 202: the row is still `offered` here; the projection will flip it to
    // `claimed`. Return the current shape so the caller can render optimistically
    // and re-poll.
    Ok((StatusCode::ACCEPTED, Json(to_human_task_json(&task))))
}

#[cfg(test)]
mod coerce_tests {
    use super::*;
    use serde_json::json;

    fn detail_with_fields(fields: &[(&str, &str)]) -> JsonValue {
        let blocks: Vec<JsonValue> = fields
            .iter()
            .map(|(name, kind)| {
                json!({ "type": "input", "field": { "name": name, "label": name, "kind": kind } })
            })
            .collect();
        json!({ "steps": [ { "id": "s1", "title": "Step", "blocks": blocks } ] })
    }

    #[test]
    fn coerces_number_and_checkbox_strings_to_typed_json() {
        let detail = detail_with_fields(&[
            ("age", "number"),
            ("ratio", "number"),
            ("agree", "checkbox"),
            ("note", "text"),
        ]);
        let data = json!({
            "age": "23",
            "ratio": "23.5",
            "agree": "true",
            "note": "hello"
        });
        let out = coerce_form_data(&detail, data);
        assert_eq!(out["age"], json!(23));
        assert_eq!(out["ratio"], json!(23.5));
        assert_eq!(out["agree"], json!(true));
        // Non-numeric/bool kinds pass through untouched.
        assert_eq!(out["note"], json!("hello"));
    }

    #[test]
    fn range_and_rating_are_numeric_real_bool_passes_through() {
        let detail =
            detail_with_fields(&[("vol", "range"), ("stars", "rating"), ("ok", "checkbox")]);
        let out = coerce_form_data(&detail, json!({ "vol": "4", "stars": "5", "ok": true }));
        assert_eq!(out["vol"], json!(4));
        assert_eq!(out["stars"], json!(5));
        assert_eq!(out["ok"], json!(true));
    }

    #[test]
    fn blank_optional_number_is_dropped_unparseable_is_left_for_the_schema() {
        let detail = detail_with_fields(&[("a", "number"), ("b", "number")]);
        let out = coerce_form_data(&detail, json!({ "a": "  ", "b": "not-a-number" }));
        assert!(
            out.get("a").is_none(),
            "blank optional number must be omitted so the open schema accepts its absence"
        );
        assert_eq!(
            out["b"],
            json!("not-a-number"),
            "unparseable input is left so the strict schema yields a clear error"
        );
    }

    #[test]
    fn missing_steps_is_a_no_op() {
        let out = coerce_form_data(&json!({}), json!({ "age": "23" }));
        assert_eq!(out["age"], json!("23"));
    }

    #[test]
    fn parse_json_number_prefers_integers() {
        assert_eq!(parse_json_number("23"), Some(json!(23)));
        assert_eq!(parse_json_number("-4"), Some(json!(-4)));
        assert_eq!(parse_json_number("23.5"), Some(json!(23.5)));
        assert_eq!(parse_json_number("1e3"), Some(json!(1000.0)));
        assert_eq!(parse_json_number("nan"), None);
        assert_eq!(parse_json_number("abc"), None);
    }

    #[test]
    fn coerce_bool_handles_strings_numbers_and_bools() {
        assert!(coerce_bool(&json!(true)));
        assert!(coerce_bool(&json!("true")));
        assert!(coerce_bool(&json!("On")));
        assert!(coerce_bool(&json!("1")));
        assert!(coerce_bool(&json!(2)));
        assert!(!coerce_bool(&json!(false)));
        assert!(!coerce_bool(&json!("false")));
        assert!(!coerce_bool(&json!("")));
        assert!(!coerce_bool(&json!(0)));
        assert!(!coerce_bool(&json!(null)));
    }
}
