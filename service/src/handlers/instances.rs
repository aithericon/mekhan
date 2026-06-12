use std::convert::Infallible;
use std::time::Duration;

use async_nats::jetstream;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures::stream::{self, Stream, StreamExt};
use petri_domain::{DomainEvent, PersistedEvent};
use serde_json::json;
use uuid::Uuid;

use crate::auth::{
    annotate_roles_keep_all, map_to_api_error, require_object_role, AuthUser, ObjectKind,
    ObjectRef, Role,
};
use crate::handlers::require_template;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::instance::{
    CreateInstanceRequest, EngineStatus, InstanceListItem, InstanceStateResponse,
    ListInstancesQuery, WorkflowInstance,
};
use crate::models::responses::{
    AllocationResponse, InstanceChild, InstanceEventsResponse, StepExecutionResponse,
};
use crate::models::template::{PaginatedResponse, WorkflowGraph};
use crate::petri::events::fetch_events;
use crate::petri::launcher::{InstanceLauncher, LaunchError, LaunchSpec};
use crate::AppState;

/// POST /api/v1/instances
#[utoipa::path(
    post,
    path = "/api/v1/instances",
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
    let template = require_template(&state.db, req.template_id).await?;

    // Object-ACL gate: the launched instance doesn't exist yet, so key the
    // check on the TEMPLATE (+ its folder). Requires Editor on the template —
    // a workspace member's floor satisfies this; a non-member (even of a
    // public template) is now rejected. Behavior change vs. the prior
    // published-only check; covered by a non-member-launch regression test.
    require_object_role(
        &state.db,
        &user,
        ObjectRef::template(template.id),
        Role::Editor,
    )
    .await
    .map_err(map_to_api_error)?;

    if !template.published {
        return Err(ApiError::bad_request("template is not published"));
    }

    if template.visibility == "private" {
        return Err(ApiError::bad_request(
            "private sub-workflows can't run standalone; they run embedded in their owning workflow",
        ));
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
        .launch(LaunchSpec::Templated {
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
            // User POST path does not surface ablation today; #126.2's
            // ablation surface lives at the trigger boundary (research
            // harness drives via fire_trigger). Plain create-instance →
            // empty dispatch options.
            dispatch_options: petri_api_types::DispatchOptions::default(),
            // Tenant propagation (D1-A) is surfaced at the trigger-fire
            // boundary; the user POST create-instance path does not carry
            // a net-parameter bag today.
            net_parameters: None,
        })
        .await
        .map_err(|e| match e {
            LaunchError::Parameterize(pe) => ApiError::bad_request(pe.to_string()),
            LaunchError::ParameterizeForPlace(pe) => ApiError::bad_request(pe.to_string()),
            LaunchError::Database(msg) => ApiError::internal(msg),
            LaunchError::Deploy(msg) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("failed to deploy to engine: {msg}"),
            ),
        })?;

    Ok((StatusCode::CREATED, Json(instance)))
}

/// GET /api/v1/instances
#[utoipa::path(
    get,
    path = "/api/v1/instances",
    params(ListInstancesQuery),
    responses(
        (status = 200, description = "Paginated list of instances", body = PaginatedResponse<InstanceListItem>),
    ),
    tag = "instances",
)]
pub async fn list_instances(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListInstancesQuery>,
) -> Result<Json<PaginatedResponse<InstanceListItem>>, ApiError> {
    let offset = (params.page - 1) * params.per_page;
    // Workspace scope (closes the pre-Phase-3 leak: this list had no auth and
    // returned instances across ALL workspaces). Instances carry no
    // workspace_id, so scope through the joined template's workspace + public
    // visibility — mirroring `list_templates`.
    let workspace_id = user.workspace_id.unwrap_or_else(Uuid::nil);

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
    // Sub-workflow child instances are sub-runs of a parent instance (one per
    // Loop/Map iteration). They are reachable via the parent's drill-in
    // (`GET /instances/{parent}/children`) and must not clutter the top-level
    // instances list or inflate its pagination count. This is a static
    // predicate (no bind), so it doesn't consume a `$N` slot.
    conditions.push("wi.parent_instance_id IS NULL".to_string());
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
    // Workspace + public gate on the joined template (bound last among filters).
    let ws_bind = bind_index;
    conditions.push(format!(
        "(wt.workspace_id = ${ws_bind} OR wt.visibility = 'public')"
    ));
    bind_index += 1;

    let where_clause = format!("WHERE {}", conditions.join(" AND "));

    // Both COUNT and SELECT JOIN the version-pinned template so the workspace
    // predicate resolves identically.
    let join =
        "JOIN workflow_templates wt ON wt.id = wi.template_id AND wt.version = wi.template_version";
    let list_sql = format!(
        "SELECT wi.*, wt.name as template_name \
         FROM workflow_instances wi {join} \
         {} ORDER BY wi.created_at DESC LIMIT ${} OFFSET ${}",
        where_clause,
        bind_index,
        bind_index + 1
    );
    let count_sql = format!("SELECT COUNT(*) FROM workflow_instances wi {join} {where_clause}");

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
    list_query = list_query.bind(workspace_id);
    count_query = count_query.bind(workspace_id);
    list_query = list_query.bind(params.per_page).bind(offset);

    let mut items = list_query.fetch_all(&state.db).await?;
    let total = count_query.fetch_one(&state.db).await?.0;

    // Annotate each row with the caller's effective role (one query for the
    // whole page) so the SPA can hide stale edit affordances; the backend still
    // enforces on every mutate path.
    // Keep-all on purpose: an instance only becomes restricted via the
    // template's ancestor folder, and detail access is gated by
    // `require_object_role`.
    annotate_roles_keep_all(
        &state.db,
        &user,
        ObjectKind::Instance,
        workspace_id,
        &mut items,
    )
    .await
    .map_err(map_to_api_error)?;

    Ok(Json(PaginatedResponse {
        items,
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

/// Object-ACL gate for a single instance. Call AFTER the handler's existence
/// check so a genuine member hitting a vanished instance still gets 404; a
/// non-member gets 403 (`require_object_role` resolves the instance → template
/// → folder and applies the workspace floor / grant elevation).
async fn gate_instance(
    state: &AppState,
    user: &AuthUser,
    id: Uuid,
    need: Role,
) -> Result<Role, ApiError> {
    require_object_role(&state.db, user, ObjectRef::instance(id), need)
        .await
        .map_err(map_to_api_error)
}

/// GET /api/v1/instances/:id
#[utoipa::path(
    get,
    path = "/api/v1/instances/{id}",
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
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowInstance>, ApiError> {
    let mut instance =
        sqlx::query_as::<_, WorkflowInstance>("SELECT * FROM workflow_instances WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::not_found("instance not found"))?;

    let role = gate_instance(&state, &user, id, Role::Viewer).await?;
    instance.my_effective_role = Some(role.as_label().to_string());
    Ok(Json(instance))
}

/// GET /api/v1/instances/{id}/stream
///
/// SSE stream of the instance's domain events, replayed from the start
/// (`DeliverPolicy::All`) then live, terminated by a final `result` event
/// carrying the structured envelope. Composes with FireAndForget: fire, get
/// the instance id, then open this stream. No per-instance ownership check —
/// consistent with `get_instance` (auth middleware gates the route).
#[utoipa::path(
    get,
    path = "/api/v1/instances/{id}/stream",
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
    user: AuthUser,
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
    .await?;
    let (net_id, status, db_result) =
        row.ok_or_else(|| ApiError::not_found("instance not found"))?;

    gate_instance(&state, &user, id, Role::Viewer).await?;

    let already_terminal = matches!(
        status.as_str(),
        "completed" | "cancelled" | "failed" | "archived"
    );

    // Compose with plain stream::chain instead of wrapping in another
    // async_stream — nested generator state machines blow the test thread
    // stack and aren't free in prod either.
    let prelude = stream::iter(vec![Ok::<_, Infallible>(
        Event::default().event("connected").data(net_id.clone()),
    )]);
    let body: futures::stream::BoxStream<'static, Result<Event, Infallible>> = if already_terminal {
        // Already finished (or events purged by the cleanup sweep): emit
        // the persisted result envelope and close — no point replaying.
        let payload = db_result.unwrap_or(serde_json::Value::Null);
        Box::pin(stream::iter(vec![Ok(Event::default()
            .event("result")
            .data(payload.to_string()))]))
    } else {
        Box::pin(instance_jetstream_events(state.nats.clone(), net_id))
    };
    let stream = prelude.chain(body);

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
        let stream_h = match nats
            .jetstream()
            .get_stream(crate::nats::subjects::Subjects::STREAM_GLOBAL)
            .await
        {
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
                filter_subject: crate::nats::subjects::net_events_filter(&net_id),
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

/// GET /api/v1/instances/:id/state
///
/// Returns instance state with marking projected from JetStream events (source
/// of truth) and best-effort engine status for enabled transitions / run mode.
#[utoipa::path(
    get,
    path = "/api/v1/instances/{id}/state",
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
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<InstanceStateResponse>, ApiError> {
    let instance =
        sqlx::query_as::<_, WorkflowInstance>("SELECT * FROM workflow_instances WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::not_found("instance not found"))?;

    gate_instance(&state, &user, id, Role::Viewer).await?;

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

/// GET /api/v1/instances/:id/events
///
/// Returns the full event log for an instance from JetStream.
#[utoipa::path(
    get,
    path = "/api/v1/instances/{id}/events",
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
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<InstanceEventsResponse>, ApiError> {
    let instance =
        sqlx::query_as::<_, WorkflowInstance>("SELECT * FROM workflow_instances WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::not_found("instance not found"))?;

    gate_instance(&state, &user, id, Role::Viewer).await?;

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

/// GET /api/v1/instances/:id/step-executions
///
/// Returns one row per workflow node × execution iteration for an instance.
/// Materialized by the step-executions projection consumer; the frontend
/// overlays this data on the canvas node cards.
#[utoipa::path(
    get,
    path = "/api/v1/instances/{id}/step-executions",
    params(("id" = Uuid, Path, description = "Instance id")),
    responses(
        (status = 200, description = "Per-step execution rows for this instance", body = Vec<StepExecutionResponse>),
        (status = 404, description = "Instance not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "instances",
)]
#[allow(clippy::type_complexity)]
pub async fn list_step_executions(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<StepExecutionResponse>>, ApiError> {
    // Existence check so the 404 path is honest (the projection may have no
    // rows for a brand-new instance that hasn't fired any transitions yet).
    let instance_exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM workflow_instances WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    if instance_exists.is_none() {
        return Err(ApiError::not_found("instance not found"));
    }
    gate_instance(&state, &user, id, Role::Viewer).await?;

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
        Option<String>,
    )> = sqlx::query_as(
        "SELECT node_id, iteration_index, node_kind, status, \
                inputs, outputs, branch_taken, \
                started_at, completed_at, error, execution_id \
         FROM step_execution \
         WHERE instance_id = $1 \
         ORDER BY started_at NULLS LAST, node_id, iteration_index",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

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
                execution_id,
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
                    execution_id,
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

/// GET /api/v1/instances/:id/children
///
/// Lists the sub-workflow child instances this instance spawned. A SubWorkflow
/// node runs its child as a separate engine net; the causality ingest
/// registers each spawn as a first-class child `workflow_instances` row
/// (parent_instance_id = this instance). A SubWorkflow inside a Loop/Map spawns
/// one child per iteration, so multiple rows can share `parent_node_id` —
/// ordered by `spawn_seq` (spawn/iteration order). The instance graph view
/// groups these by `parent_node_id` to offer an "Enter sub-workflow" drill-in
/// on each SubWorkflow node.
#[utoipa::path(
    get,
    path = "/api/v1/instances/{id}/children",
    params(("id" = Uuid, Path, description = "Parent instance id")),
    responses(
        (status = 200, description = "Sub-workflow child instances spawned by this instance", body = Vec<InstanceChild>),
        (status = 404, description = "Instance not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "instances",
)]
#[allow(clippy::type_complexity)]
pub async fn list_instance_children(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<InstanceChild>>, ApiError> {
    let instance_exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM workflow_instances WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    if instance_exists.is_none() {
        return Err(ApiError::not_found("instance not found"));
    }
    gate_instance(&state, &user, id, Role::Viewer).await?;

    let rows: Vec<(
        Uuid,
        Option<String>,
        Option<i64>,
        Uuid,
        i32,
        String,
        String,
        chrono::DateTime<chrono::Utc>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    )> = sqlx::query_as(
        "SELECT wi.id, wi.parent_node_id, wi.spawn_seq, wi.template_id, \
                wi.template_version, wt.name, wi.status, wi.created_at, \
                wi.started_at, wi.completed_at \
         FROM workflow_instances wi \
         JOIN workflow_templates wt ON wt.id = wi.template_id \
         WHERE wi.parent_instance_id = $1 \
         ORDER BY wi.parent_node_id NULLS LAST, wi.spawn_seq NULLS LAST, wi.created_at",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let response: Vec<InstanceChild> = rows
        .into_iter()
        .map(
            |(
                id,
                parent_node_id,
                spawn_seq,
                template_id,
                template_version,
                template_name,
                status,
                created_at,
                started_at,
                completed_at,
            )| InstanceChild {
                id,
                parent_node_id,
                spawn_seq,
                template_id,
                template_version,
                template_name,
                status,
                created_at,
                started_at,
                completed_at,
            },
        )
        .collect();

    Ok(Json(response))
}

/// GET /api/v1/instances/:id/allocations
///
/// Lists the resource grants (datacenter leases + token-pool admissions) this
/// instance held over its lifetime, from the `allocations` projection table.
/// Each row is one `(net_id, grant_id, kind)` grant: a LeaseScope / Loop body
/// holding a Slurm/Nomad/HTTP allocation (`datacenter_lease`), or an admission
/// against one of our own worker pools (`concurrency_limit_grant`). The instance view
/// surfaces these to show "what did this run hold, for how long, and at what
/// cost" — `duration_ms` is computed (`released_at - acquired_at`, or live for
/// a still-`held` grant). Ordered by `acquired_at` (acquisition order).
#[utoipa::path(
    get,
    path = "/api/v1/instances/{id}/allocations",
    params(("id" = Uuid, Path, description = "Instance id")),
    responses(
        (status = 200, description = "Resource grants held by this instance", body = Vec<AllocationResponse>),
        (status = 404, description = "Instance not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "instances",
)]
pub async fn list_instance_allocations(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AllocationResponse>>, ApiError> {
    // Existence check so the 404 path is honest (the projection may have no
    // rows for an instance that never held a lease or pool grant).
    let instance_exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM workflow_instances WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    if instance_exists.is_none() {
        return Err(ApiError::not_found("instance not found"));
    }
    gate_instance(&state, &user, id, Role::Viewer).await?;

    let rows: Vec<AllocationResponse> = sqlx::query_as(
        "SELECT id, kind, net_id, instance_id, node_id, grant_id, \
                cluster_resource_id, scheduler_flavor, alloc_id, node, \
                executor_namespace, status, requested_at, acquired_at, \
                released_at, expiry, exit_code, queue_wait_ms, elapsed_ms, \
                cpu_seconds, gpu_seconds, peak_rss_bytes, requested_tres, \
                allocated_tres, last_error, last_sequence \
         FROM allocations \
         WHERE instance_id = $1 \
         ORDER BY acquired_at NULLS LAST, requested_at NULLS LAST",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let response: Vec<AllocationResponse> = rows
        .into_iter()
        .map(AllocationResponse::with_duration)
        .collect();

    Ok(Json(response))
}

/// DELETE /api/v1/instances/:id
#[utoipa::path(
    delete,
    path = "/api/v1/instances/{id}",
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
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowInstance>, ApiError> {
    let instance =
        sqlx::query_as::<_, WorkflowInstance>("SELECT * FROM workflow_instances WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::not_found("instance not found"))?;

    // Cancelling is a state-changing operation → Editor on the instance.
    gate_instance(&state, &user, id, Role::Editor).await?;

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

    // Cancel any in-flight executor jobs for this instance. terminate_net stops
    // the net from firing new transitions, but AutomatedSteps already dispatched
    // to the executor run on a separate process (NATS-decoupled) and never see
    // NetCancelled — they'd otherwise run to completion. Publish a best-effort
    // cancel per running execution_id; the executor's `executor.cancel.*`
    // listener flips the job's CancellationToken. Cooperative: cancellation only
    // takes effect for backends that observe the token mid-run.
    let running_executions: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT execution_id FROM step_execution
        WHERE instance_id = $1
          AND status IN ('running', 'pending')
          AND execution_id IS NOT NULL
        "#,
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    for execution_id in running_executions {
        let subject = aithericon_executor_domain::cancel_subject(&execution_id);
        if let Err(e) = state
            .nats
            .client()
            .publish(subject, Vec::new().into())
            .await
        {
            tracing::warn!(%execution_id, "failed to publish executor cancel: {e}");
        }
    }

    // Update instance status
    let instance = sqlx::query_as::<_, WorkflowInstance>(
        r#"
        UPDATE workflow_instances
        SET status = 'cancelled', completed_at = NOW(), updated_at = NOW(), updated_by = $2
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await?;

    Ok(Json(instance))
}
