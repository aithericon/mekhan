use std::convert::Infallible;
use std::time::Duration;

use async_nats::jetstream;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures::stream::Stream;
use futures::StreamExt;
use petri_domain::{DomainEvent, PersistedEvent};
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::instance::{
    CreateInstanceRequest, EngineStatus, InstanceListItem, InstanceStateResponse,
    ListInstancesQuery, WorkflowInstance,
};
use crate::models::responses::{InstanceEventsResponse, StepExecutionResponse};
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

    // Categorize the run. `test_run` is reserved for the template-test runner
    // and rejected from the public endpoint; anything else falls back to
    // `live`.
    let mode = match req.mode.as_deref() {
        None | Some("live") => "live",
        Some("draft") => "draft",
        Some("test_run") => {
            return Err(ApiError::bad_request(
                "mode 'test_run' is reserved for the template-test runner",
            ));
        }
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "unknown instance mode: {other}"
            )));
        }
    };

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
            mode: Some(mode),
            test_id: None,
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

    // Resolve the `mode` filter. Missing/empty ⇒ default to live-only (the
    // historical view). `any`/`all` returns everything. Anything else is an
    // explicit category filter and binds as-is.
    let mode_filter: Option<&str> = match params.mode.as_deref() {
        None | Some("") => Some("live"),
        Some("any") | Some("all") => None,
        Some(other) => Some(other),
    };

    // Build WHERE clause based on filter parameters. Bind index tracks the
    // next $N placeholder so each filter slots in without colliding.
    let mut conditions: Vec<String> = Vec::new();
    let mut bind_index: u8 = 1;
    if params.template_id.is_some() {
        conditions.push(format!("wi.template_id = ${bind_index}"));
        bind_index += 1;
    }
    if params.status.is_some() {
        conditions.push(format!("wi.status = ${bind_index}"));
        bind_index += 1;
    }
    if mode_filter.is_some() {
        conditions.push(format!("wi.mode = ${bind_index}"));
        bind_index += 1;
    }
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    let list_sql = format!(
        "SELECT wi.*, wt.name as template_name \
         FROM workflow_instances wi \
         JOIN workflow_templates wt ON wt.id = wi.template_id AND wt.version = wi.template_version \
         {} ORDER BY wi.created_at DESC LIMIT ${} OFFSET ${}",
        where_clause,
        bind_index,
        bind_index + 1
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
    if let Some(mode) = mode_filter {
        list_query = list_query.bind(mode);
        count_query = count_query.bind(mode);
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

/// GET /api/instances/{id}/stream
///
/// SSE stream of the instance's domain events, replayed from the start
/// (`DeliverPolicy::All`) then live, terminated by a final `result` event
/// carrying the structured envelope. Composes with FireAndForget: fire, get
/// the instance id, then open this stream. No per-instance ownership check —
/// consistent with `get_instance` (auth middleware gates the route).
#[utoipa::path(
    get,
    path = "/api/instances/{id}/stream",
    params(("id" = Uuid, Path, description = "Instance id")),
    responses(
        (status = 200, description = "SSE stream of domain events + final result", content_type = "text/event-stream"),
        (status = 404, description = "Instance not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "instances",
)]
pub async fn stream_instance(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    // Pre-stream existence check so 404 is a real HTTP status (not an SSE
    // error frame), and to short-circuit already-terminal / history-purged
    // instances without touching NATS.
    let row = sqlx::query_as::<_, (String, String, Option<serde_json::Value>)>(
        "SELECT net_id, status, result FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    let (net_id, status, db_result) =
        row.ok_or_else(|| ApiError::not_found("instance not found"))?;

    let already_terminal = matches!(
        status.as_str(),
        "completed" | "cancelled" | "failed" | "archived"
    );
    let nats = state.nats.clone();

    let stream = async_stream::stream! {
        yield Ok(Event::default().event("connected").data(net_id.clone()));

        // Already finished (or events purged by the cleanup sweep): emit the
        // persisted result envelope and close — no point replaying history.
        if already_terminal {
            let payload = db_result.unwrap_or(serde_json::Value::Null);
            yield Ok(Event::default().event("result").data(payload.to_string()));
            return;
        }

        let mut inner = Box::pin(instance_jetstream_events(nats, net_id));
        while let Some(ev) = inner.next().await {
            yield ev;
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}

/// Bare JetStream-backed event stream for a single instance — no `connected`
/// preamble, no DB existence check, no terminal short-circuit. Replays from
/// the beginning (`DeliverPolicy::All`) then follows live, emits one SSE
/// event per `PersistedEvent` (event name = domain event `type`), and closes
/// after emitting a final `result` envelope when it sees `NetCompleted` /
/// `NetCancelled`. Shared by `stream_instance` (GET) and the `Sse` arm of
/// `fire_trigger` (POST) — both want identical event semantics.
pub(crate) fn instance_jetstream_events(
    nats: crate::nats::MekhanNats,
    net_id: String,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    async_stream::stream! {
        let stream_h = match nats.jetstream().get_stream("PETRI_GLOBAL").await {
            Ok(s) => s,
            Err(e) => {
                yield Ok(Event::default().event("error").data(format!("stream: {e}")));
                return;
            }
        };
        // Ephemeral consumer (no durable name) so NATS auto-reaps it when the
        // client disconnects and this future is dropped.
        let consumer = match stream_h
            .create_consumer(jetstream::consumer::pull::Config {
                filter_subject: format!("petri.events.{net_id}.>"),
                deliver_policy: jetstream::consumer::DeliverPolicy::All,
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            })
            .await
        {
            Ok(c) => c,
            Err(e) => {
                yield Ok(Event::default().event("error").data(format!("consumer: {e}")));
                return;
            }
        };
        let mut messages = match consumer.messages().await {
            Ok(m) => m,
            Err(e) => {
                yield Ok(Event::default().event("error").data(format!("messages: {e}")));
                return;
            }
        };

        let mut ping = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                msg = messages.next() => {
                    match msg {
                        Some(Ok(m)) => {
                            if let Ok(ev) = serde_json::from_slice::<PersistedEvent>(&m.payload) {
                                let name = serde_json::to_value(&ev.event)
                                    .ok()
                                    .and_then(|v| {
                                        v.get("type")
                                            .and_then(|t| t.as_str().map(String::from))
                                    })
                                    .unwrap_or_else(|| "event".to_string());
                                let data =
                                    serde_json::to_string(&ev).unwrap_or_default();
                                yield Ok(Event::default().event(name).data(data));

                                // Terminal: derive the envelope straight from
                                // the event (race-free vs. the DB write the
                                // lifecycle consumer is doing concurrently),
                                // emit it, and close.
                                let terminal_envelope = match &ev.event {
                                    DomainEvent::NetCompleted { exit_code, .. } => {
                                        Some(exit_code.clone().unwrap_or(serde_json::Value::Null))
                                    }
                                    DomainEvent::NetCancelled { reason, .. } => Some(json!({
                                        "ok": false,
                                        "error": { "reason": reason, "value": serde_json::Value::Null }
                                    })),
                                    _ => None,
                                };
                                if let Some(env) = terminal_envelope {
                                    let _ = m.ack().await;
                                    yield Ok(Event::default().event("result").data(env.to_string()));
                                    return;
                                }
                            }
                            let _ = m.ack().await;
                        }
                        Some(Err(e)) => {
                            tracing::warn!("instance stream read error: {e}");
                        }
                        None => return,
                    }
                }
                _ = ping.tick() => {
                    yield Ok(Event::default().comment("ping"));
                }
            }
        }
    }
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

/// GET /api/instances/:id/step-executions
///
/// Returns one row per workflow node × execution iteration for an instance.
/// Materialized by the step-executions projection consumer; the frontend
/// overlays this data on the canvas node cards.
#[utoipa::path(
    get,
    path = "/api/instances/{id}/step-executions",
    params(("id" = Uuid, Path, description = "Instance id")),
    responses(
        (status = 200, description = "Per-step execution rows for this instance", body = Vec<StepExecutionResponse>),
        (status = 404, description = "Instance not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "instances",
)]
pub async fn list_step_executions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<StepExecutionResponse>>, ApiError> {
    // Existence check so the 404 path is honest (the projection may have no
    // rows for a brand-new instance that hasn't fired any transitions yet).
    let instance_exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM workflow_instances WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    if instance_exists.is_none() {
        return Err(ApiError::not_found("instance not found"));
    }

    let rows: Vec<(
        String,
        i32,
        String,
        String,
        Option<serde_json::Value>,
        Option<serde_json::Value>,
        Option<String>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<serde_json::Value>,
    )> = sqlx::query_as(
        "SELECT node_id, iteration_index, node_kind, status, \
                inputs, outputs, branch_taken, \
                started_at, completed_at, error \
         FROM step_execution \
         WHERE instance_id = $1 \
         ORDER BY started_at NULLS LAST, node_id, iteration_index",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;

    let response: Vec<StepExecutionResponse> = rows
        .into_iter()
        .map(
            |(
                node_id,
                iteration_index,
                node_kind,
                status,
                inputs,
                outputs,
                branch_taken,
                started_at,
                completed_at,
                error,
            )| {
                let duration_ms = match (started_at, completed_at) {
                    (Some(s), Some(c)) => Some((c - s).num_milliseconds()),
                    _ => None,
                };
                StepExecutionResponse {
                    node_id,
                    iteration_index,
                    node_kind,
                    status,
                    inputs,
                    outputs,
                    branch_taken,
                    started_at,
                    completed_at,
                    duration_ms,
                    error,
                }
            },
        )
        .collect();

    Ok(Json(response))
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
