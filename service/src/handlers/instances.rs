use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::instance::{
    CreateInstanceRequest, EngineStatus, InstanceListItem, InstanceStateResponse,
    ListInstancesQuery, WorkflowInstance,
};
use crate::models::responses::InstanceEventsResponse;
use crate::models::template::{PaginatedResponse, WorkflowGraph, WorkflowTemplate};
use crate::petri::events::fetch_events;
use crate::petri::launcher::{InstanceLauncher, LaunchError, LaunchSpec};
use crate::AppState;

/// POST /api/instances
#[utoipa::path(
    post,
    path = "/api/instances",
    request_body = CreateInstanceRequest,
    responses(
        (status = 201, description = "Instance created and deployed to engine", body = WorkflowInstance),
        (status = 400, description = "Template not published", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 502, description = "Engine deploy failed", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "instances",
)]
pub async fn create_instance(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateInstanceRequest>,
) -> Result<(StatusCode, Json<WorkflowInstance>), ApiError> {
    let created_by = user.subject_as_uuid();
    // Fetch the template (must be published)
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(req.template_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    if !template.published {
        return Err(ApiError::bad_request("template is not published"));
    }

    let air_json = template
        .air_json
        .clone()
        .ok_or_else(|| ApiError::internal("published template has no AIR JSON"))?;

    // Deserialize the template's graph so parameterize_air can validate
    // start_tokens against each Start block's declared `initial` port.
    let graph: WorkflowGraph = serde_json::from_value(template.graph.clone())
        .map_err(|e| ApiError::internal(format!("template graph is invalid: {e}")))?;

    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{instance_id}");
    let metadata = req.metadata.clone().unwrap_or(json!({}));

    // Parameterize → insert row (before deploy, for the lifecycle listener) →
    // deploy → roll back the row on deploy failure. The launcher owns that
    // sequence; here we only translate its failures to HTTP statuses:
    // parameterize failures are caller error (400), a deploy failure is an
    // engine fault (502).
    let launcher = InstanceLauncher::new(&state.db, &state.petri);
    let instance = launcher
        .launch(LaunchSpec {
            instance_id,
            net_id,
            template_id: template.id,
            template_version: template.version,
            created_by,
            metadata,
            air_json: &air_json,
            graph: &graph,
            start_tokens: &req.start_tokens,
        })
        .await
        .map_err(|e| match e {
            LaunchError::Parameterize(pe) => ApiError::bad_request(pe.to_string()),
            LaunchError::Database(msg) => ApiError::internal(msg),
            LaunchError::Deploy(msg) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("failed to deploy to engine: {msg}"),
            ),
        })?;

    Ok((StatusCode::CREATED, Json(instance)))
}

/// GET /api/instances
#[utoipa::path(
    get,
    path = "/api/instances",
    params(ListInstancesQuery),
    responses(
        (status = 200, description = "Paginated list of instances", body = PaginatedResponse<InstanceListItem>),
    ),
    tag = "instances",
)]
pub async fn list_instances(
    State(state): State<AppState>,
    Query(params): Query<ListInstancesQuery>,
) -> Json<PaginatedResponse<InstanceListItem>> {
    let offset = (params.page - 1) * params.per_page;

    // Build WHERE clause based on filter parameters
    let mut conditions = Vec::new();
    if params.template_id.is_some() {
        conditions.push("wi.template_id = $1");
    }
    if params.status.is_some() {
        conditions.push(if params.template_id.is_some() {
            "wi.status = $2"
        } else {
            "wi.status = $1"
        });
    }
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let next_param = 1 + params.template_id.is_some() as u8 + params.status.is_some() as u8;

    let list_sql = format!(
        "SELECT wi.*, wt.name as template_name \
         FROM workflow_instances wi \
         JOIN workflow_templates wt ON wt.id = wi.template_id AND wt.version = wi.template_version \
         {} ORDER BY wi.created_at DESC LIMIT ${} OFFSET ${}",
        where_clause,
        next_param,
        next_param + 1
    );
    let count_sql = format!(
        "SELECT COUNT(*) FROM workflow_instances wi {}",
        where_clause
    );

    let mut list_query = sqlx::query_as::<_, InstanceListItem>(&list_sql);
    let mut count_query = sqlx::query_as::<_, (i64,)>(&count_sql);

    if let Some(tid) = params.template_id {
        list_query = list_query.bind(tid);
        count_query = count_query.bind(tid);
    }
    if let Some(ref status) = params.status {
        list_query = list_query.bind(status);
        count_query = count_query.bind(status);
    }
    list_query = list_query.bind(params.per_page).bind(offset);

    let items = list_query.fetch_all(&state.db).await.unwrap_or_default();
    let total = count_query
        .fetch_one(&state.db)
        .await
        .unwrap_or((0,))
        .0;

    Json(PaginatedResponse {
        items,
        total,
        page: params.page,
        per_page: params.per_page,
    })
}

/// GET /api/instances/:id
#[utoipa::path(
    get,
    path = "/api/instances/{id}",
    params(("id" = Uuid, Path, description = "Instance id")),
    responses(
        (status = 200, description = "Instance", body = WorkflowInstance),
        (status = 404, description = "Instance not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "instances",
)]
pub async fn get_instance(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowInstance>, ApiError> {
    let instance = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("instance not found"))?;

    Ok(Json(instance))
}

/// GET /api/instances/:id/state
///
/// Returns instance state with marking projected from JetStream events (source
/// of truth) and best-effort engine status for enabled transitions / run mode.
#[utoipa::path(
    get,
    path = "/api/instances/{id}/state",
    params(("id" = Uuid, Path, description = "Instance id")),
    responses(
        (status = 200, description = "Instance state with marking + engine status", body = InstanceStateResponse),
        (status = 404, description = "Instance not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "instances",
)]
pub async fn get_instance_state(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<InstanceStateResponse>, ApiError> {
    let instance = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("instance not found"))?;

    // 1. Fetch events from JetStream (source of truth)
    let events = fetch_events(&state.nats, &instance.net_id)
        .await
        .map_err(|e| {
            tracing::error!("failed to fetch events from JetStream: {e}");
            ApiError::internal(format!("event fetch failed: {e}"))
        })?;

    // 2. Project marking from events
    let marking = petri_domain::project_marking(&events);
    let marking_json = serde_json::to_value(&marking).unwrap_or(json!({}));

    // 3. Serialize events as JSON values
    let event_count = events.len();
    let events_json: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|e| serde_json::to_value(e).ok())
        .collect();

    // 4. Best-effort engine query for status + enabled transitions + run mode
    let (engine, enabled_transitions) = match state.petri.try_get_state(&instance.net_id).await {
        Some(engine_state) => {
            let transitions: Vec<String> = engine_state
                .enabled_transitions
                .iter()
                .map(|t| t.to_string())
                .collect();
            (
                EngineStatus {
                    available: true,
                    run_mode: Some(engine_state.run_mode),
                },
                transitions,
            )
        }
        None => (
            EngineStatus {
                available: false,
                run_mode: None,
            },
            vec![],
        ),
    };

    Ok(Json(InstanceStateResponse {
        instance_id: instance.id,
        net_id: instance.net_id,
        status: instance.status,
        events: events_json,
        event_count,
        marking: marking_json,
        engine,
        enabled_transitions,
        current_step: instance.current_step,
    }))
}

/// GET /api/instances/:id/events
///
/// Returns the full event log for an instance from JetStream.
#[utoipa::path(
    get,
    path = "/api/instances/{id}/events",
    params(("id" = Uuid, Path, description = "Instance id")),
    responses(
        (status = 200, description = "JetStream events for this instance", body = InstanceEventsResponse),
        (status = 404, description = "Instance not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "instances",
)]
pub async fn get_instance_events(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<InstanceEventsResponse>, ApiError> {
    let instance = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("instance not found"))?;

    let events = fetch_events(&state.nats, &instance.net_id)
        .await
        .map_err(|e| {
            tracing::error!("failed to fetch events from JetStream: {e}");
            ApiError::internal(format!("event fetch failed: {e}"))
        })?;

    let events_json: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|e| serde_json::to_value(e).ok())
        .collect();
    let event_count = events_json.len();

    Ok(Json(InstanceEventsResponse {
        net_id: instance.net_id,
        events: events_json,
        event_count,
    }))
}

/// DELETE /api/instances/:id
#[utoipa::path(
    delete,
    path = "/api/instances/{id}",
    params(("id" = Uuid, Path, description = "Instance id")),
    responses(
        (status = 200, description = "Instance cancelled", body = WorkflowInstance),
        (status = 404, description = "Instance not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "instances",
)]
pub async fn cancel_instance(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowInstance>, ApiError> {
    let instance = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("instance not found"))?;

    if instance.status == "completed" || instance.status == "cancelled" {
        return Err(ApiError::conflict(format!(
            "instance is already {}",
            instance.status
        )));
    }

    // Terminate the net in petri-lab (pause + delete)
    if let Err(e) = state.petri.terminate_net(&instance.net_id).await {
        tracing::warn!("failed to terminate net in petri-lab: {e}");
    }

    // Update instance status
    let instance = sqlx::query_as::<_, WorkflowInstance>(
        r#"
        UPDATE workflow_instances
        SET status = 'cancelled', completed_at = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(Json(instance))
}
