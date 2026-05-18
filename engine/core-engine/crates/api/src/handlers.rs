use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use petri_application::{
    token_color_to_json, validate_topology, EventRepository, StateProjection, TopologyRepository,
    TransitionStatusDetail,
};
use petri_domain::{verify_event_chain, DomainEvent, PlaceId, TokenColor, TokenId, TransitionId};

use crate::dto::{
    AnalysisReport, AnalysisSummary, CommandResponse, CreateTokenRequest, ErrorResponse,
    EvaluateFinalState, EvaluateRequest, EvaluateResponse, EventsResponse, FiredTransition,
    IssueLevel, LoadScenarioRequest, LoadScenarioResponse, RunMode, RunModeResponse,
    ScenarioDefinition, SetRunModeRequest, StateResponse, TopologyResponse, TransitionStatus,
    UpdateTransitionRequest, ValidationIssue,
};
use crate::router::{AppState, SseSignal};
use crate::scenario_bridge::ScenarioBridge;
use petri_application::EvaluateFinalState as ServiceFinalState;

use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use petri_domain::PersistedEvent;
use std::convert::Infallible;

/// Broadcast a persisted event to all SSE clients. Silently ignored if no receivers.
fn broadcast_event<E, T, S>(app_state: &AppState<E, T, S>, event: &PersistedEvent)
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    let _ = app_state.event_tx.send(SseSignal::Event(Box::new(event.clone())));
}

/// Broadcast a reset signal to all SSE clients.
fn broadcast_reset<E, T, S>(app_state: &AppState<E, T, S>)
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    let _ = app_state.event_tx.send(SseSignal::Reset);
}

/// GET /api/topology
/// Returns the Petri Net topology.
#[utoipa::path(
    get,
    path = "/api/topology",
    responses(
        (status = 200, description = "Topology retrieved", body = TopologyResponse)
    ),
    tag = "Topology"
)]
pub async fn get_topology<E, T, S>(State(app_state): State<AppState<E, T, S>>) -> impl IntoResponse
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    let topology = app_state.service.get_topology();
    Json(TopologyResponse { topology })
}

/// Query parameters for the events endpoint
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Only return events with sequence >= from_sequence
    pub from_sequence: Option<u64>,
}

/// GET /api/events
/// Returns events in the log, optionally filtered by sequence number.
#[utoipa::path(
    get,
    path = "/api/events",
    params(
        ("from_sequence" = Option<u64>, Query, description = "Only return events with sequence >= this value")
    ),
    responses(
        (status = 200, description = "Events retrieved", body = EventsResponse)
    ),
    tag = "Events"
)]
pub async fn get_events<E, T, S>(
    State(app_state): State<AppState<E, T, S>>,
    Query(query): Query<EventsQuery>,
) -> impl IntoResponse
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    let all_events = app_state.service.get_events().await;

    // Filter events if from_sequence is provided
    let events: Vec<_> = match query.from_sequence {
        Some(from_seq) => all_events
            .into_iter()
            .filter(|e| e.sequence >= from_seq)
            .collect(),
        None => all_events,
    };

    let chain_valid = verify_event_chain(&events);
    Json(EventsResponse {
        events,
        chain_valid,
    })
}

/// GET /api/state
/// Returns the current marking and enabled transitions.
#[utoipa::path(
    get,
    path = "/api/state",
    responses(
        (status = 200, description = "State retrieved", body = StateResponse)
    ),
    tag = "State"
)]
pub async fn get_state<E, T, S>(State(app_state): State<AppState<E, T, S>>) -> impl IntoResponse
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    let marking = app_state.service.get_marking().await;
    let enabled_transitions = app_state
        .service
        .enabled_transitions()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to compute enabled transitions");
            vec![]
        });

    // Get detailed statuses and convert to DTO format
    let statuses = app_state
        .service
        .transition_statuses()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to compute transition statuses");
            std::collections::HashMap::new()
        });
    let transition_statuses: HashMap<String, TransitionStatus> = statuses
        .into_iter()
        .map(|(tid, status)| {
            let dto_status = match status {
                TransitionStatusDetail::Enabled => TransitionStatus::Enabled,
                TransitionStatusDetail::DisabledNoTokens { missing_place } => {
                    TransitionStatus::DisabledNoTokens { missing_place }
                }
                TransitionStatusDetail::DisabledGuardFailed { guard } => {
                    TransitionStatus::DisabledGuardFailed { guard }
                }
                TransitionStatusDetail::DisabledGuardError { error } => {
                    TransitionStatus::DisabledGuardError { error }
                }
            };
            (tid.to_string(), dto_status)
        })
        .collect();

    let run_mode = *app_state.run_mode.read();

    Json(StateResponse {
        marking,
        enabled_transitions,
        transition_statuses,
        run_mode,
    })
}

/// POST /api/command/fire/{transition_id}
/// Fire a transition.
#[utoipa::path(
    post,
    path = "/api/command/fire/{transition_id}",
    params(
        ("transition_id" = String, Path, description = "Transition ID to fire")
    ),
    responses(
        (status = 200, description = "Transition fired", body = CommandResponse),
        (status = 400, description = "Transition cannot fire", body = ErrorResponse)
    ),
    tag = "Commands"
)]
pub async fn fire_transition<E, T, S>(
    State(app_state): State<AppState<E, T, S>>,
    Path(transition_id): Path<String>,
) -> impl IntoResponse
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    let tid = TransitionId(transition_id);

    match app_state.service.fire_transition(tid).await {
        Ok(event) => {
            // Notify adapter scheduler about produced tokens
            if let DomainEvent::TransitionFired {
                produced_tokens, ..
            } = &event.event
            {
                for (place_id, token) in produced_tokens {
                    let token_data = token_color_to_json(&token.color);
                    let token_created_at_ms = token.created_at.timestamp_millis();
                    notify_adapters(
                        &app_state,
                        place_id,
                        token.id.clone(),
                        token_data,
                        token_created_at_ms,
                    );
                }
            }

            broadcast_event(&app_state, &event);
            (StatusCode::OK, Json(CommandResponse::success(event)))
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(CommandResponse::error(e.to_string())))
        }
    }
}

/// POST /api/command/create-token
/// Create a new token at a place.
#[utoipa::path(
    post,
    path = "/api/command/create-token",
    request_body = CreateTokenRequest,
    responses(
        (status = 200, description = "Token created", body = CommandResponse),
        (status = 400, description = "Cannot create token", body = ErrorResponse)
    ),
    tag = "Commands"
)]
pub async fn create_token<E, T, S>(
    State(app_state): State<AppState<E, T, S>>,
    Json(request): Json<CreateTokenRequest>,
) -> impl IntoResponse
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    let place_id = request.place_id.clone();
    let color = request.color.clone();

    match app_state
        .service
        .create_token(place_id.clone(), color.clone())
        .await
    {
        Ok(event) => {
            // Notify adapter scheduler about the new token
            if let DomainEvent::TokenCreated {
                token,
                place_id: pid,
                ..
            } = &event.event
            {
                let token_data = token_color_to_json(&token.color);
                let token_created_at_ms = token.created_at.timestamp_millis();
                notify_adapters(
                    &app_state,
                    pid,
                    token.id.clone(),
                    token_data,
                    token_created_at_ms,
                );
            }

            broadcast_event(&app_state, &event);
            (StatusCode::OK, Json(CommandResponse::success(event)))
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(CommandResponse::error(e.to_string())))
        }
    }
}

/// POST /api/command/reset
/// Reset the event log.
#[utoipa::path(
    post,
    path = "/api/command/reset",
    responses(
        (status = 200, description = "Event log reset", body = CommandResponse)
    ),
    tag = "Commands"
)]
pub async fn reset<E, T, S>(State(app_state): State<AppState<E, T, S>>) -> impl IntoResponse
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    if let Err(e) = app_state.service.reset().await {
        tracing::error!(error = %e, "Failed to reset service");
    }
    broadcast_reset(&app_state);
    Json(CommandResponse {
        success: true,
        event: None,
        error: None,
    })
}

/// PATCH /api/topology/transition/{transition_id}
/// Update a transition's script and/or guard (hot-reload).
#[utoipa::path(
    patch,
    path = "/api/topology/transition/{transition_id}",
    params(
        ("transition_id" = String, Path, description = "Transition ID to update")
    ),
    request_body = UpdateTransitionRequest,
    responses(
        (status = 200, description = "Transition updated", body = CommandResponse),
        (status = 400, description = "Invalid script or transition not found", body = ErrorResponse)
    ),
    tag = "Topology"
)]
pub async fn update_transition_script<E, T, S>(
    State(app_state): State<AppState<E, T, S>>,
    Path(transition_id): Path<String>,
    Json(request): Json<UpdateTransitionRequest>,
) -> impl IntoResponse
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    let tid = TransitionId(transition_id);

    match app_state
        .service
        .update_transition_script(tid, request.script, request.guard)
        .await
    {
        Ok(event) => {
            broadcast_event(&app_state, &event);
            (StatusCode::OK, Json(CommandResponse::success(event)))
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(CommandResponse::error(e.to_string())))
        }
    }
}

/// POST /api/scenario
/// Load a new scenario from JSON definition.
#[utoipa::path(
    post,
    path = "/api/scenario",
    request_body = LoadScenarioRequest,
    responses(
        (status = 200, description = "Scenario loaded", body = LoadScenarioResponse),
        (status = 400, description = "Invalid scenario", body = ErrorResponse)
    ),
    tag = "Scenario"
)]
pub async fn load_scenario<E, T, S>(
    State(app_state): State<AppState<E, T, S>>,
    Json(envelope): Json<LoadScenarioRequest>,
) -> impl IntoResponse
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    // Sub-phase 2.5e-γ.mekhan: unwrap envelope, then validate the per-run
    // dispatch options against the scenario's declared transitions. Failures
    // here MUST fail-closed BEFORE any service mutation (no half-loaded
    // state on bad input).
    let LoadScenarioRequest {
        scenario,
        skip_mask,
        stage_overrides,
    } = envelope;

    if let Err((status, message)) = validate_dispatch_options(&scenario, &skip_mask, &stage_overrides) {
        tracing::error!(reason = %message, "Dispatch options validation failed");
        return Err(status);
    }
    let dispatch_options = petri_domain::DispatchOptions {
        skip_mask,
        stage_overrides,
    };

    // Validate infrastructure requirements before loading
    if !scenario.requirements.is_empty() {
        let registered = app_state.service.registered_handler_ids();
        for req in &scenario.requirements {
            for handler_id in &req.handler_ids {
                if !registered.contains(handler_id) {
                    tracing::error!(
                        handler_id = %handler_id,
                        category = %req.category,
                        "Required effect handler not registered"
                    );
                    return Err(StatusCode::PRECONDITION_FAILED);
                }
            }
        }
    }

    // Parse scenario using the bridge
    let parsed = match ScenarioBridge::parse(
        &scenario.places,
        &scenario.transitions,
        scenario.definitions.clone(),
    ) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Failed to parse scenario");
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    let mut net = parsed.net;
    let place_ids = parsed.place_ids;
    let initial_tokens = parsed.initial_tokens.clone();
    let tokens_count = parsed.initial_tokens.len();

    // Attach groups to the topology so they survive event-sourced hydration
    net.groups = scenario.groups.clone();

    // Clear any existing events/state before initializing
    app_state.service.clear().await;

    // Initialize the service with the new topology
    app_state
        .service
        .initialize(net)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to initialize service");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    app_state.service.set_initial_tokens(initial_tokens.clone());

    // Build and register schema registry if definitions are present
    if !parsed.definitions.is_empty() {
        match petri_application::SchemaRegistry::new(parsed.definitions) {
            Ok(registry) => {
                app_state.service.set_schema_registry(registry);
                tracing::info!("Schema registry loaded with definitions");
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to compile schema definitions");
                return Err(StatusCode::BAD_REQUEST);
            }
        }
    }

    // Register mock adapters with the scheduler BEFORE creating tokens,
    // so adapter notifications fire for initial seed tokens.
    app_state
        .adapter_scheduler
        .register_adapters(&scenario.mock_adapters, &place_ids);

    // Sub-phase 2.5e-γ.mekhan: install the per-run dispatch options BEFORE
    // initial-token creation so an eval-running net picks them up
    // immediately. Mirror to both AppState (for ergonomic admin-read
    // surface) and PetriNetService (the canonical source consulted by the
    // firing path). Lock is short-held (writing one assignment); no .await
    // inside the critical section.
    {
        let mut guard = app_state.dispatch_options.write();
        *guard = dispatch_options.clone();
    }
    app_state.service.set_dispatch_options(dispatch_options);

    // Create initial tokens and notify adapters for each
    for (place_id, color) in initial_tokens {
        if let Ok(event) = app_state.service.create_token(place_id, color).await {
            if let DomainEvent::TokenCreated {
                token,
                place_id: pid,
                ..
            } = &event.event
            {
                let token_data = token_color_to_json(&token.color);
                let token_created_at_ms = token.created_at.timestamp_millis();
                notify_adapters(
                    &app_state,
                    pid,
                    token.id.clone(),
                    token_data,
                    token_created_at_ms,
                );
            }
            broadcast_event(&app_state, &event);
        }
    }

    Ok(Json(LoadScenarioResponse {
        success: true,
        places_count: scenario.places.len(),
        transitions_count: scenario.transitions.len(),
        tokens_count,
        error: None,
    }))
}

/// Sub-phase 2.5e-γ.mekhan: validate per-run dispatch options against the
/// scenario's declared transitions BEFORE any engine mutation.
///
/// Returns `Err((StatusCode::BAD_REQUEST, message))` on:
/// - `skip_mask` entry that doesn't reference a declared transition_id
/// - `stage_overrides` key that doesn't reference a declared transition_id
/// - `stage_overrides` value that carries a non-null `model` key when the
///   target transition's original `effect_config.model` is unset / empty /
///   whitespace-only (per `feedback_no_default_model`).
fn validate_dispatch_options(
    scenario: &ScenarioDefinition,
    skip_mask: &[String],
    stage_overrides: &std::collections::HashMap<String, serde_json::Value>,
) -> Result<(), (StatusCode, String)> {
    use petri_api_types::TransitionLogic;

    if skip_mask.is_empty() && stage_overrides.is_empty() {
        return Ok(());
    }
    let declared: std::collections::HashSet<&str> = scenario
        .transitions
        .iter()
        .map(|t| t.id.as_str())
        .collect();

    for skip_id in skip_mask {
        if !declared.contains(skip_id.as_str()) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("skip_mask references unknown transition_id: {}", skip_id),
            ));
        }
    }

    for (override_id, patch_value) in stage_overrides {
        if !declared.contains(override_id.as_str()) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "stage_overrides references unknown transition_id: {}",
                    override_id
                ),
            ));
        }
        // Per feedback_no_default_model: reject model-injection where the
        // target transition has no declared model.
        if let Some(patch_obj) = patch_value.as_object() {
            if let Some(model_patch) = patch_obj.get("model") {
                if !model_patch.is_null() {
                    let transition = scenario
                        .transitions
                        .iter()
                        .find(|t| t.id == *override_id)
                        .expect("transition existence verified above");
                    let original_model = match &transition.logic {
                        TransitionLogic::Effect { config, .. } => config
                            .as_ref()
                            .and_then(|c| c.get("model"))
                            .and_then(|m| m.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty()),
                        _ => None,
                    };
                    if original_model.is_none() {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            format!(
                                "stage_overrides for transition '{}' injects a 'model' key but the transition has no declared model (per feedback_no_default_model)",
                                override_id
                            ),
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

/// GET /api/analyze
/// Perform static analysis on the loaded topology.
#[utoipa::path(
    get,
    path = "/api/analyze",
    responses(
        (status = 200, description = "Analysis complete", body = AnalysisReport)
    ),
    tag = "Analysis"
)]
pub async fn analyze_topology_handler<E, T, S>(
    State(app_state): State<AppState<E, T, S>>,
) -> impl IntoResponse
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    match app_state.service.get_topology() {
        Some(net) => Json(validate_topology(&net)),
        None => Json(AnalysisReport {
            is_valid: false,
            issues: vec![ValidationIssue {
                node_id: String::new(),
                node_type: "system".to_string(),
                level: IssueLevel::Error,
                code: "NO_TOPOLOGY".to_string(),
                message: "No topology loaded".to_string(),
                remote_net_id: None,
            }],
            summary: AnalysisSummary {
                error_count: 1,
                warning_count: 0,
                info_count: 0,
            },
        }),
    }
}

/// Helper function to notify adapter scheduler about new tokens.
pub fn notify_adapters<E, T, S>(
    app_state: &AppState<E, T, S>,
    place_id: &PlaceId,
    token_id: TokenId,
    token_data: JsonValue,
    token_created_at_ms: i64,
) where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    let scheduler = app_state.adapter_scheduler.clone();
    let service = app_state.service.clone();
    let pid = place_id.clone();
    let eval_notify = app_state.eval_notify.clone();
    let run_mode = app_state.run_mode.clone();

    // Create a closure that injects tokens via the service
    let inject_fn: Arc<dyn Fn(PlaceId, TokenColor) + Send + Sync> = {
        let svc = service.clone();
        let notify = eval_notify.clone();
        let mode = run_mode.clone();
        Arc::new(move |target_place: PlaceId, color: TokenColor| {
            let svc = svc.clone();
            let notify = notify.clone();
            let mode = mode.clone();
            tokio::spawn(async move {
                let _ = svc.create_token(target_place, color).await;
                if *mode.read() == RunMode::Running {
                    notify.notify_one();
                }
            });
        })
    };

    // Create a closure that checks if a token still exists in a place.
    #[allow(clippy::type_complexity)]
    let check_token_fn: Arc<dyn Fn(&PlaceId, &TokenId) -> bool + Send + Sync> = {
        let svc = service.clone();
        Arc::new(move |place_id: &PlaceId, token_id: &TokenId| {
            let svc = svc.clone();
            let pid = place_id.clone();
            let tid = token_id.clone();
            tokio::task::block_in_place(move || {
                tokio::runtime::Handle::current()
                    .block_on(async move { svc.token_exists_in_place(&pid, &tid).await })
            })
        })
    };

    scheduler.process_token_created(
        &pid,
        token_id,
        token_data,
        token_created_at_ms,
        inject_fn,
        check_token_fn,
    );

    // Also notify evaluation loop for the current token creation
    if *run_mode.read() == RunMode::Running {
        eval_notify.notify_one();
    }
}

/// Helper function to notify adapters about all produced tokens from a batch of events.
pub fn notify_adapters_for_events<E, T, S>(
    app_state: &AppState<E, T, S>,
    events: &[petri_domain::PersistedEvent],
) where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    for persisted in events {
        if let DomainEvent::TransitionFired {
            produced_tokens, ..
        } = &persisted.event
        {
            for (place_id, token) in produced_tokens {
                let token_data = token_color_to_json(&token.color);
                let token_created_at_ms = token.created_at.timestamp_millis();
                notify_adapters(
                    app_state,
                    place_id,
                    token.id.clone(),
                    token_data,
                    token_created_at_ms,
                );
            }
        }
    }
}

// =============================================================================
// Evaluate and Run Mode Handlers
// =============================================================================

/// POST /api/command/evaluate
/// Fire all enabled transitions until quiescence or max_steps reached.
#[utoipa::path(
    post,
    path = "/api/command/evaluate",
    request_body = EvaluateRequest,
    responses(
        (status = 200, description = "Evaluation complete", body = EvaluateResponse)
    ),
    tag = "Commands"
)]
pub async fn evaluate<E, T, S>(
    State(app_state): State<AppState<E, T, S>>,
    Json(request): Json<EvaluateRequest>,
) -> impl IntoResponse
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    let max_steps = if request.max_steps == 0 {
        1000
    } else {
        request.max_steps
    };

    match app_state.service.evaluate_until_quiescent(max_steps).await {
        Ok(result) => {
            // Notify adapters about all produced tokens from fired transitions
            notify_adapters_for_events(&app_state, &result.events);

            // Broadcast each event to SSE clients
            for ev in &result.events {
                broadcast_event(&app_state, ev);
            }

            // Convert service result to DTO
            let final_state = match result.final_state {
                ServiceFinalState::Quiescent => EvaluateFinalState::Quiescent,
                ServiceFinalState::LimitReached => EvaluateFinalState::LimitReached,
            };

            let transitions_fired: Vec<FiredTransition> = result
                .transitions_fired
                .into_iter()
                .map(|(tid, seq)| FiredTransition {
                    transition_id: tid.to_string(),
                    sequence: seq,
                })
                .collect();

            (
                StatusCode::OK,
                Json(EvaluateResponse::success(
                    result.steps_executed,
                    final_state,
                    transitions_fired,
                )),
            )
        }
        Err(e) => {
            let status =
                StatusCode::from_u16(e.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, Json(EvaluateResponse::error(e.to_string())))
        }
    }
}

/// GET /api/run-mode
/// Get the current run mode.
#[utoipa::path(
    get,
    path = "/api/run-mode",
    responses(
        (status = 200, description = "Run mode retrieved", body = RunModeResponse)
    ),
    tag = "RunMode"
)]
pub async fn get_run_mode<E, T, S>(State(app_state): State<AppState<E, T, S>>) -> impl IntoResponse
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    let current_mode = *app_state.run_mode.read();
    Json(RunModeResponse {
        success: true,
        previous_mode: None,
        current_mode,
    })
}

/// PUT /api/run-mode
/// Set the engine run mode.
#[utoipa::path(
    put,
    path = "/api/run-mode",
    request_body = SetRunModeRequest,
    responses(
        (status = 200, description = "Run mode updated", body = RunModeResponse)
    ),
    tag = "RunMode"
)]
pub async fn set_run_mode<E, T, S>(
    State(app_state): State<AppState<E, T, S>>,
    Json(request): Json<SetRunModeRequest>,
) -> impl IntoResponse
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    let previous_mode = {
        let mut mode = app_state.run_mode.write();
        let prev = *mode;
        *mode = request.mode;
        prev
    };

    // If switching to Running mode, notify the evaluation loop
    if request.mode == RunMode::Running && previous_mode != RunMode::Running {
        app_state.eval_notify.notify_one();
    }

    Json(RunModeResponse {
        success: true,
        previous_mode: Some(previous_mode),
        current_mode: request.mode,
    })
}

// =============================================================================
// Service introspection
// =============================================================================

/// GET /api/services -- List registered effect handlers and their categories.
pub async fn get_services<E, T, S>(
    State(app_state): State<AppState<E, T, S>>,
) -> impl IntoResponse
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    let handler_ids = app_state.service.registered_handler_ids();

    let mut categories: HashMap<String, Vec<String>> = HashMap::new();
    for id in &handler_ids {
        let category = match id.as_str() {
            "scheduler_submit" | "scheduler_cancel" => "scheduler",
            "executor_submit" | "executor_cancel" => "executor",
            "timer_schedule" | "timer_cancel" => "timer",
            "human_task" | "human_cancel" => "human",
            _ => "custom",
        };
        categories
            .entry(category.to_string())
            .or_default()
            .push(id.clone());
    }

    Json(crate::dto::ServicesResponse {
        handlers: handler_ids,
        categories,
    })
}

// =============================================================================
// Server-Sent Events handler
// =============================================================================

/// GET /api/events/stream -- Server-Sent Events stream for real-time event delivery.
///
/// Clients may pass `?from_sequence=N` to backfill any events they missed
/// before the live stream begins.
pub async fn event_stream<E, T, S>(
    State(app_state): State<AppState<E, T, S>>,
    Query(query): Query<EventsQuery>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    use futures::StreamExt;

    let rx = app_state.event_tx.subscribe();

    // Phase 1: backfill missed events as an immediate stream
    let backfill: Vec<Result<SseEvent, Infallible>> = if let Some(from_seq) = query.from_sequence {
        app_state
            .service
            .get_events()
            .await
            .into_iter()
            .filter(|e| e.sequence >= from_seq)
            .map(|event| {
                let data = serde_json::to_string(&event).unwrap_or_default();
                Ok(SseEvent::default().event("update").data(data))
            })
            .collect()
    } else {
        vec![]
    };

    // Phase 2: live stream from broadcast receiver
    let live = futures::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(SseSignal::Event(event)) => {
                let data = serde_json::to_string(&event).unwrap_or_default();
                Some((Ok(SseEvent::default().event("update").data(data)), rx))
            }
            Ok(SseSignal::Reset) => {
                Some((Ok(SseEvent::default().event("reset").data("{}")), rx))
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                let data = serde_json::json!({ "missed": n }).to_string();
                Some((Ok(SseEvent::default().event("resync").data(data)), rx))
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
        }
    });

    // Chain: emit backfill immediately, then block on live events
    let stream = futures::stream::iter(backfill).chain(live);

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(5))
            .text("ping"),
    )
}

// =============================================================================
// Executor SSE endpoint
// =============================================================================

/// Shared state for the executor SSE endpoint (broadcast + backfill buffer).
#[cfg(feature = "executor")]
#[derive(Clone)]
pub struct ExecutorSseState {
    pub tx: std::sync::Arc<tokio::sync::broadcast::Sender<petri_executor::ExecutorSseEvent>>,
    pub buffer: petri_executor::ExecutorSseBuffer,
}

/// SSE endpoint for streaming executor events (metrics, progress, phases, status).
///
/// On connect, replays all buffered events (backfill), then switches to live broadcast.
/// This ensures dashboards survive page refreshes — all historic events are replayed.
#[cfg(feature = "executor")]
pub async fn executor_event_stream(
    State(state): State<ExecutorSseState>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
    // Subscribe BEFORE reading buffer so we don't miss events between read and subscribe.
    let rx = state.tx.subscribe();

    // Phase 1: backfill from buffer.
    let (backfill, max_backfill_seq) = {
        let buf = state.buffer.read().unwrap();
        let events: Vec<Result<SseEvent, Infallible>> = buf
            .iter()
            .map(|e| {
                let data = serde_json::to_string(&e.payload).unwrap_or_default();
                Ok(SseEvent::default()
                    .event("executor")
                    .data(data)
                    .id(e.seq.to_string()))
            })
            .collect();
        let max_seq = buf.last().map(|e| e.seq).unwrap_or(0);
        (events, max_seq)
    };

    // Phase 2: live stream, skipping events already sent in backfill.
    let live = futures::stream::unfold(
        (rx, max_backfill_seq),
        |(mut rx, skip_until)| async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if event.seq <= skip_until {
                            continue;
                        }
                        let data =
                            serde_json::to_string(&event.payload).unwrap_or_default();
                        return Some((
                            Ok(SseEvent::default()
                                .event("executor")
                                .data(data)
                                .id(event.seq.to_string())),
                            (rx, event.seq),
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        let data = serde_json::json!({ "missed": n }).to_string();
                        return Some((
                            Ok(SseEvent::default().event("resync").data(data)),
                            (rx, skip_until),
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );

    // Chain: emit backfill immediately, then block on live events.
    use futures::StreamExt;
    let stream = futures::stream::iter(backfill).chain(live);

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(5))
            .text("ping"),
    )
}

// =============================================================================
// Net-scoped handler wrappers
// =============================================================================

/// Thin wrappers that extract `net_id` from path, look up `NetInstance` in the
/// registry, build `AppState`, and delegate to the existing handler functions.
/// GET /api/bridges/check — validate all cross-net bridge connections.
pub async fn check_all_bridges<E, T, S>(
    State(registry): State<Arc<crate::net_registry::NetRegistry<E, T, S>>>,
) -> Json<petri_application::AnalysisReport>
where
    E: petri_application::EventRepository + 'static,
    T: petri_application::TopologyRepository + 'static,
    S: petri_application::StateProjection + 'static,
{
    Json(petri_application::validate_all_bridges(registry.as_ref()))
}

pub mod net_scoped {
    use std::sync::Arc;

    use axum::{
        extract::{Path, Query, State},
        http::StatusCode,
        response::IntoResponse,
        Json,
    };

    use petri_application::{EventRepository, StateProjection, TopologyRepository};

    use super::EventsQuery;
    use crate::dto::{
        CreateTokenRequest, EvaluateRequest, LoadScenarioRequest, RunMode, SetRunModeRequest,
        UpdateTransitionRequest,
    };
    use crate::net_registry::NetRegistry;

    /// Helper: get an existing net instance or return 404.
    ///
    /// If the net is hibernated (not in memory but known to the registry),
    /// it is automatically rehydrated. After a cold engine boot the in-process
    /// `known_nets` set is empty, so we fall back to an external
    /// [`MetadataLookup`](crate::net_registry::MetadataLookup) (typically the
    /// `KV_NET_METADATA` JetStream bucket) to distinguish a hibernated net
    /// from one that was never deployed or has been tombstoned.
    ///
    /// Returns:
    /// - `Ok(instance)` if the net is loaded or successfully rehydrated.
    /// - `Err(404)` if the net was never deployed.
    /// - `Err(409)` if the net is tombstoned (completed/cancelled).
    #[allow(clippy::type_complexity)]
    async fn get_instance<E, T, S>(
        registry: &NetRegistry<E, T, S>,
        net_id: &str,
    ) -> Result<Arc<crate::net_registry::NetInstance<E, T, S>>, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        // Fast path: net is in memory (hot)
        if let Some(instance) = registry.get(net_id) {
            return Ok(instance);
        }

        // Warm path: net hibernated within this engine's lifetime.
        if registry.is_known(net_id) {
            return Ok(registry.get_or_create(net_id));
        }

        // Cold path: in-process registry has no record. Consult external
        // metadata (KV bucket) so we can rehydrate nets that survived an
        // engine restart, while still refusing tombstoned/unknown nets.
        if let Some(lookup) = registry.metadata_lookup() {
            use crate::net_registry::MetadataStatus;
            match lookup.lookup(net_id).await {
                MetadataStatus::Known => Ok(registry.get_or_create(net_id)),
                MetadataStatus::Tombstoned => Err((
                    StatusCode::CONFLICT,
                    "Net is completed or cancelled",
                )),
                MetadataStatus::Unknown => Err((StatusCode::NOT_FOUND, "Net not found")),
            }
        } else {
            Err((StatusCode::NOT_FOUND, "Net not found"))
        }
    }

    /// GET /api/nets — list all net IDs.
    pub async fn list_nets<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
    ) -> impl IntoResponse
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        Json(registry.list())
    }

    /// DELETE /api/nets/{net_id} — tear down a net instance.
    pub async fn delete_net<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
    ) -> impl IntoResponse
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        match registry.remove(&net_id) {
            Some(_) => StatusCode::NO_CONTENT,
            None => StatusCode::NOT_FOUND,
        }
    }

    /// GET /api/nets/{net_id}/topology
    pub async fn net_get_topology<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::get_topology(State(instance.as_app_state())).await)
    }

    /// GET /api/nets/{net_id}/events
    pub async fn net_get_events<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
        query: Query<EventsQuery>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::get_events(State(instance.as_app_state()), query).await)
    }

    /// GET /api/nets/{net_id}/events/stream
    pub async fn net_event_stream<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
        query: Query<super::EventsQuery>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::event_stream(State(instance.as_app_state()), query).await)
    }

    /// GET /api/nets/{net_id}/state
    pub async fn net_get_state<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::get_state(State(instance.as_app_state())).await)
    }

    /// POST /api/nets/{net_id}/command/fire/{transition_id}
    pub async fn net_fire_transition<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path((net_id, transition_id)): Path<(String, String)>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::fire_transition(State(instance.as_app_state()), Path(transition_id)).await)
    }

    /// POST /api/nets/{net_id}/command/create-token
    pub async fn net_create_token<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
        body: Json<CreateTokenRequest>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::create_token(State(instance.as_app_state()), body).await)
    }

    /// POST /api/nets/{net_id}/command/reset
    pub async fn net_reset<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::reset(State(instance.as_app_state())).await)
    }

    /// POST /api/nets/{net_id}/command/evaluate
    pub async fn net_evaluate<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
        body: Json<EvaluateRequest>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::evaluate(State(instance.as_app_state()), body).await)
    }

    /// POST /api/nets/{net_id}/scenario — auto-creates net if it doesn't exist.
    pub async fn net_load_scenario<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
        body: Json<LoadScenarioRequest>,
    ) -> impl IntoResponse
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        // Use get_or_create so loading into a new net_id auto-creates it
        let instance = registry.get_or_create(&net_id);

        // Advance signal epoch BEFORE loading scenario so stale signals
        // from a previous scenario instance are ACK'd without processing.
        instance.notify_scenario_loaded().await;

        let response = super::load_scenario(State(instance.as_app_state()), body).await;

        // Run bridge validation in warn mode after deploy.
        // Runs against whatever topology is loaded — if load_scenario failed,
        // get_topology() returns None and we skip gracefully.
        if let Some(topology) = instance.service.get_topology() {
            let report = petri_application::validate_bridges(
                &net_id,
                &topology,
                registry.as_ref(),
                petri_application::BridgeValidationMode::Warn,
            );
            for issue in &report.issues {
                match issue.level {
                    petri_application::IssueLevel::Error => {
                        tracing::error!(
                            net_id = %net_id,
                            code = %issue.code,
                            "Bridge validation: {}",
                            issue.message
                        );
                    }
                    petri_application::IssueLevel::Warning => {
                        tracing::warn!(
                            net_id = %net_id,
                            code = %issue.code,
                            "Bridge validation: {}",
                            issue.message
                        );
                    }
                    petri_application::IssueLevel::Info => {
                        tracing::info!(
                            net_id = %net_id,
                            code = %issue.code,
                            "Bridge validation: {}",
                            issue.message
                        );
                    }
                }
            }
        }

        response
    }

    /// GET /api/nets/{net_id}/analyze
    pub async fn net_analyze<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::analyze_topology_handler(State(instance.as_app_state())).await)
    }

    /// GET /api/nets/{net_id}/run-mode
    pub async fn net_get_run_mode<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::get_run_mode(State(instance.as_app_state())).await)
    }

    /// PUT /api/nets/{net_id}/run-mode
    ///
    /// When transitioning to Running, validates all bridge connections in strict
    /// mode. Returns 422 with an AnalysisReport if any bridge errors are found.
    pub async fn net_set_run_mode<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
        body: Json<SetRunModeRequest>,
    ) -> Result<impl IntoResponse, (StatusCode, Json<petri_application::AnalysisReport>)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id)
            .await
            .map_err(|(status, message)| {
                let code = if status == StatusCode::CONFLICT {
                    "NET_TOMBSTONED"
                } else {
                    "NET_NOT_FOUND"
                };
                let report = petri_application::AnalysisReport {
                    is_valid: false,
                    issues: vec![petri_application::ValidationIssue {
                        node_id: String::new(),
                        node_type: "system".to_string(),
                        level: petri_application::IssueLevel::Error,
                        code: code.to_string(),
                        message: format!("Net '{}': {}", net_id, message),
                        remote_net_id: None,
                    }],
                    summary: petri_application::AnalysisSummary {
                        error_count: 1,
                        warning_count: 0,
                        info_count: 0,
                    },
                };
                (status, Json(report))
            })?;

        // Gate on bridge validation when transitioning to Running
        if body.mode == RunMode::Running {
            if let Some(topology) = instance.service.get_topology() {
                let report = petri_application::validate_bridges(
                    &net_id,
                    &topology,
                    registry.as_ref(),
                    petri_application::BridgeValidationMode::Strict,
                );
                if !report.is_valid {
                    tracing::error!(
                        net_id = %net_id,
                        errors = report.summary.error_count,
                        "Bridge validation failed — refusing to enter Running mode"
                    );
                    return Err((StatusCode::UNPROCESSABLE_ENTITY, Json(report)));
                }
            }
        }

        Ok(super::set_run_mode(State(instance.as_app_state()), body).await)
    }

    /// PATCH /api/nets/{net_id}/topology/transition/{transition_id}
    pub async fn net_update_transition_script<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path((net_id, transition_id)): Path<(String, String)>,
        body: Json<UpdateTransitionRequest>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::update_transition_script(
            State(instance.as_app_state()),
            Path(transition_id),
            body,
        )
        .await)
    }

    /// GET /api/nets/{net_id}/services
    pub async fn net_get_services<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
    ) -> Result<impl IntoResponse, (StatusCode, &'static str)>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let instance = get_instance(&registry, &net_id).await?;
        Ok(super::get_services(State(instance.as_app_state())).await)
    }

    /// POST /api/nets/{net_id}/command/hibernate — force-hibernate a net.
    pub async fn net_hibernate<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
    ) -> impl IntoResponse
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        match registry.hibernate(&net_id) {
            Ok(()) => (
                StatusCode::OK,
                Json(serde_json::json!({ "success": true })),
            ),
            Err(e) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "success": false, "error": e })),
            ),
        }
    }

    /// POST /api/nets/{net_id}/command/wake — wake a hibernated net (replay events from NATS).
    pub async fn net_wake<E, T, S>(
        State(registry): State<Arc<NetRegistry<E, T, S>>>,
        Path(net_id): Path<String>,
    ) -> impl IntoResponse
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let _instance = registry.get_or_create(&net_id);
        (
            StatusCode::OK,
            Json(serde_json::json!({ "success": true })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    };
    use http_body_util::BodyExt;
    use parking_lot::RwLock;
    use petri_application::PetriNetService;
    use petri_test_harness::prelude::*;
    use rstest::rstest;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tokio::sync::Notify;
    use tower::ServiceExt;

    use crate::dto::RunMode;
    use crate::router::AppState;
    use petri_application::AdapterScheduler;

    // =========================================================================
    // Test Helpers
    // =========================================================================

    fn test_app_state() -> AppState<MockEventRepository, MockTopologyRepository, MockStateProjection>
    {
        let event_repo = Arc::new(MockEventRepository::new());
        let topology_repo = Arc::new(MockTopologyRepository::new());
        let state_projection = Arc::new(MockStateProjection::new());

        let service = Arc::new(PetriNetService::new(
            event_repo,
            topology_repo,
            state_projection,
        ));

        let (event_tx, _) = tokio::sync::broadcast::channel(256);

        AppState {
            service,
            adapter_scheduler: Arc::new(AdapterScheduler::new()),
            run_mode: Arc::new(RwLock::new(RunMode::default())),
            eval_notify: Arc::new(Notify::new()),
            event_tx: Arc::new(event_tx),
            dispatch_options: Arc::new(RwLock::new(
                petri_domain::DispatchOptions::default(),
            )),
        }
    }

    fn test_router(
        app_state: AppState<MockEventRepository, MockTopologyRepository, MockStateProjection>,
    ) -> Router {
        use axum::routing::{get, patch, post};

        Router::new()
            .route("/api/topology", get(get_topology::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .route("/api/topology/transition/:transition_id", patch(update_transition_script::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .route("/api/events", get(get_events::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .route("/api/state", get(get_state::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .route("/api/command/fire/:transition_id", post(fire_transition::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .route("/api/command/create-token", post(create_token::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .route("/api/command/reset", post(reset::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .route("/api/command/evaluate", post(evaluate::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .route("/api/scenario", post(load_scenario::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .route("/api/analyze", get(analyze_topology_handler::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .route("/api/run-mode", get(get_run_mode::<MockEventRepository, MockTopologyRepository, MockStateProjection>).put(set_run_mode::<MockEventRepository, MockTopologyRepository, MockStateProjection>))
            .with_state(app_state)
    }

    async fn get_json(router: Router, uri: &str) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    async fn post_json(router: Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    async fn patch_json(router: Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("PATCH")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    async fn put_json(router: Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, json)
    }

    /// Load a simple scenario for testing.
    ///
    /// Sub-phase 2.5e-γ.mekhan-S2: wraps the bare-ScenarioDefinition shape
    /// in the `LoadScenarioRequest` envelope so `POST /api/scenario`
    /// deserialises (the handler now takes `Json<LoadScenarioRequest>`,
    /// not `Json<ScenarioDefinition>`). Without the envelope keys the
    /// scenario load 422s on missing `scenario` field.
    fn simple_scenario_json() -> Value {
        json!({
            "scenario": {
                "name": "Test Scenario",
                "places": [
                    {
                        "id": "place_a",
                        "name": "Place A",
                        "place_type": "state",
                        "initial_tokens": [null]
                    },
                    {
                        "id": "place_b",
                        "name": "Place B",
                        "place_type": "state",
                        "initial_tokens": []
                    }
                ],
                "transitions": [
                    {
                        "id": "trans_1",
                        "name": "Transition 1",
                        "input_ports": [{"name": "input", "cardinality": "single"}],
                        "output_ports": [{"name": "output", "cardinality": "single"}],
                        "inputs": [{"place": "place_a", "port": "input", "weight": 1}],
                        "outputs": [{"place": "place_b", "port": "output", "weight": 1}],
                        "logic": {"type": "rhai", "source": "#{output: input}"}
                    }
                ],
                "groups": [],
                "mock_adapters": []
            }
        })
    }

    /// Scenario with a guard condition. Envelope-wrapped per
    /// sub-phase 2.5e-γ.mekhan-S2 (see `simple_scenario_json` rationale).
    fn guarded_scenario_json() -> Value {
        json!({
            "scenario": {
                "name": "Guarded Scenario",
                "places": [
                    {
                        "id": "input",
                        "name": "Input",
                        "place_type": "state",
                        "initial_tokens": [{"value": 50}]
                    },
                    {
                        "id": "output",
                        "name": "Output",
                        "place_type": "state",
                        "initial_tokens": []
                    }
                ],
                "transitions": [
                    {
                        "id": "check",
                        "name": "Check Value",
                        "input_ports": [{"name": "data", "cardinality": "single"}],
                        "output_ports": [{"name": "result", "cardinality": "single"}],
                        "inputs": [{"place": "input", "port": "data", "weight": 1}],
                        "outputs": [{"place": "output", "port": "result", "weight": 1}],
                        "logic": {"type": "rhai", "source": "#{result: data}"},
                        "guard": {"type": "rhai", "source": "data.value >= 100"}
                    }
                ],
                "groups": [],
                "mock_adapters": []
            }
        })
    }

    // =========================================================================
    // GET /api/topology Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_get_topology_empty() {
        let app_state = test_app_state();
        let router = test_router(app_state);

        let (status, json) = get_json(router, "/api/topology").await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["topology"].is_null());
    }

    #[rstest]
    #[tokio::test]
    async fn test_get_topology_with_scenario() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (status, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;
        assert_eq!(status, StatusCode::OK);

        let router = test_router(app_state);
        let (status, json) = get_json(router, "/api/topology").await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["topology"].is_object());
        assert!(json["topology"]["places"].is_array());
        assert!(json["topology"]["transitions"].is_array());
    }

    // =========================================================================
    // GET /api/events Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_get_events_empty() {
        let app_state = test_app_state();
        let router = test_router(app_state);

        let (status, json) = get_json(router, "/api/events").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["events"], json!([]));
        assert_eq!(json["chain_valid"], true);
    }

    #[rstest]
    #[tokio::test]
    async fn test_get_events_with_data() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state);
        let (status, json) = get_json(router, "/api/events").await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["events"].is_array());
        let events = json["events"].as_array().unwrap();
        assert!(
            !events.is_empty(),
            "Should have events after loading scenario"
        );
    }

    #[rstest]
    #[tokio::test]
    async fn test_get_events_filtered() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state);
        let (status, json) = get_json(router, "/api/events?from_sequence=1000").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["events"], json!([]));
    }

    // =========================================================================
    // GET /api/state Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_get_state_empty() {
        let app_state = test_app_state();
        let router = test_router(app_state);

        let (status, json) = get_json(router, "/api/state").await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["marking"].is_object());
        assert!(json["enabled_transitions"].is_array());
        assert_eq!(json["enabled_transitions"], json!([]));
    }

    #[rstest]
    #[tokio::test]
    async fn test_get_state_with_tokens() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state);
        let (status, json) = get_json(router, "/api/state").await;

        assert_eq!(status, StatusCode::OK);
        let marking = &json["marking"];
        assert!(marking.is_object());
    }

    #[rstest]
    #[tokio::test]
    async fn test_get_state_transition_statuses() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", guarded_scenario_json()).await;

        let router = test_router(app_state);
        let (status, json) = get_json(router, "/api/state").await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["transition_statuses"].is_object());
    }

    // =========================================================================
    // POST /api/command/fire Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_fire_valid_transition() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (load_status, load_response) =
            post_json(router, "/api/scenario", simple_scenario_json()).await;
        assert_eq!(
            load_status,
            StatusCode::OK,
            "Scenario load failed: {:?}",
            load_response
        );

        let transition_id = "trans_1";

        let router = test_router(app_state);
        let (status, json) = post_json(
            router,
            &format!("/api/command/fire/{}", transition_id),
            json!({}),
        )
        .await;

        if status != StatusCode::OK {
            eprintln!("Fire failed: {:?}", json);
        }
        assert_eq!(status, StatusCode::OK, "Fire failed: {:?}", json);
        assert_eq!(json["success"], true);
        assert!(json["event"].is_object());
    }

    #[rstest]
    #[tokio::test]
    async fn test_fire_nonexistent_transition() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state);
        let fake_uuid = "00000000-0000-0000-0000-000000000000";
        let (status, json) = post_json(
            router,
            &format!("/api/command/fire/{}", fake_uuid),
            json!({}),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["success"], false);
        assert!(json["error"].is_string());
    }

    #[rstest]
    #[tokio::test]
    async fn test_fire_disabled_transition() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;
        let transition_id = "trans_1";

        let router = test_router(app_state.clone());
        let (status, _) = post_json(
            router,
            &format!("/api/command/fire/{}", transition_id),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let router = test_router(app_state);
        let (status, json) = post_json(
            router,
            &format!("/api/command/fire/{}", transition_id),
            json!({}),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["success"], false);
    }

    #[rstest]
    #[tokio::test]
    async fn test_fire_guard_failed() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", guarded_scenario_json()).await;
        let transition_id = "check";

        let router = test_router(app_state);
        let (status, json) = post_json(
            router,
            &format!("/api/command/fire/{}", transition_id),
            json!({}),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["success"], false);
    }

    // =========================================================================
    // POST /api/command/create-token Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_create_token_success() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;
        let place_id = "place_b";

        let router = test_router(app_state);
        let (status, json) = post_json(
            router,
            "/api/command/create-token",
            json!({
                "place_id": place_id,
                "color": {"type": "Unit"}
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "Create token failed: {:?}", json);
        assert_eq!(json["success"], true);
        assert!(json["event"].is_object());
    }

    #[rstest]
    #[tokio::test]
    async fn test_create_token_invalid_place() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state);
        let fake_uuid = "00000000-0000-0000-0000-000000000000";
        let (status, json) = post_json(
            router,
            "/api/command/create-token",
            json!({
                "place_id": fake_uuid,
                "color": {"type": "Unit"}
            }),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["success"], false);
    }

    // =========================================================================
    // POST /api/command/reset Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_reset_clears_events() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state.clone());
        let (status, json) = post_json(router, "/api/command/reset", json!({})).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["success"], true);

        let router = test_router(app_state);
        let (_, events_json) = get_json(router, "/api/events").await;
        let events = events_json["events"].as_array().unwrap();
        assert!(
            events.len() <= 2,
            "Should have at most 2 events after reset (NetInitialized), got {}",
            events.len()
        );
    }

    #[rstest]
    #[tokio::test]
    async fn test_reset_clears_state() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;
        let place_id = "place_b";

        let router = test_router(app_state.clone());
        let (_, _) = post_json(
            router,
            "/api/command/create-token",
            json!({"place_id": place_id, "color": null}),
        )
        .await;

        let router = test_router(app_state.clone());
        let (status, _) = post_json(router, "/api/command/reset", json!({})).await;
        assert_eq!(status, StatusCode::OK);

        let router = test_router(app_state);
        let (_, state_json) = get_json(router, "/api/state").await;
        assert!(state_json["marking"].is_object());
    }

    // =========================================================================
    // POST /api/command/evaluate Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_evaluate_fires_transitions() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state);
        let (status, json) =
            post_json(router, "/api/command/evaluate", json!({"max_steps": 100})).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["success"], true);
        assert!(json["steps_executed"].as_u64().unwrap() >= 1);
        assert_eq!(json["final_state"], "quiescent");
    }

    #[rstest]
    #[tokio::test]
    async fn test_evaluate_respects_max_steps() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state);
        let (status, json) =
            post_json(router, "/api/command/evaluate", json!({"max_steps": 1})).await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["steps_executed"].as_u64().unwrap() <= 1);
    }

    #[rstest]
    #[tokio::test]
    async fn test_evaluate_returns_fired_transitions() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state);
        let (status, json) =
            post_json(router, "/api/command/evaluate", json!({"max_steps": 100})).await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["transitions_fired"].is_array());
    }

    #[rstest]
    #[tokio::test]
    async fn test_evaluate_no_topology() {
        let app_state = test_app_state();
        let router = test_router(app_state);

        let (status, json) =
            post_json(router, "/api/command/evaluate", json!({"max_steps": 100})).await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["success"], false);
    }

    // =========================================================================
    // POST /api/scenario Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_load_scenario_success() {
        let app_state = test_app_state();
        let router = test_router(app_state);

        let (status, json) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["success"], true);
        assert_eq!(json["places_count"], 2);
        assert_eq!(json["transitions_count"], 1);
    }

    #[rstest]
    #[tokio::test]
    async fn test_load_scenario_with_tokens() {
        let app_state = test_app_state();
        let router = test_router(app_state);

        let (status, json) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["tokens_count"], 1);
    }

    #[rstest]
    #[tokio::test]
    async fn test_load_scenario_invalid() {
        let app_state = test_app_state();
        let router = test_router(app_state);

        let (status, _) = post_json(router, "/api/scenario", json!({"name": "bad"})).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    // =========================================================================
    // PATCH /api/topology/transition Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_update_transition_script() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;
        let transition_id = "trans_1";

        let router = test_router(app_state);
        let (status, json) = patch_json(
            router,
            &format!("/api/topology/transition/{}", transition_id),
            json!({
                "script": "#{output: input}",
                "guard": null
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "Update failed: {:?}", json);
        assert_eq!(json["success"], true);
    }

    #[rstest]
    #[tokio::test]
    async fn test_update_transition_guard() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;
        let transition_id = "trans_1";

        let router = test_router(app_state);
        let (status, json) = patch_json(
            router,
            &format!("/api/topology/transition/{}", transition_id),
            json!({
                "script": "#{output: input}",
                "guard": "true"
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "Update failed: {:?}", json);
        assert_eq!(json["success"], true);
    }

    #[rstest]
    #[tokio::test]
    async fn test_update_nonexistent_transition() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state);
        let fake_uuid = "00000000-0000-0000-0000-000000000000";
        let (status, json) = patch_json(
            router,
            &format!("/api/topology/transition/{}", fake_uuid),
            json!({
                "script": "#{output: input}",
                "guard": null
            }),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["success"], false);
    }

    // =========================================================================
    // GET /api/analyze Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_analyze_valid_topology() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (_, _) = post_json(router, "/api/scenario", simple_scenario_json()).await;

        let router = test_router(app_state);
        let (status, json) = get_json(router, "/api/analyze").await;

        assert_eq!(status, StatusCode::OK);
        assert!(json["is_valid"].is_boolean());
        assert!(json["issues"].is_array());
        assert!(json["summary"].is_object());
    }

    #[rstest]
    #[tokio::test]
    async fn test_analyze_no_topology() {
        let app_state = test_app_state();
        let router = test_router(app_state);

        let (status, json) = get_json(router, "/api/analyze").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["is_valid"], false);
        assert_eq!(json["issues"][0]["code"], "NO_TOPOLOGY");
    }

    // =========================================================================
    // GET/PUT /api/run-mode Tests
    // =========================================================================

    #[rstest]
    #[tokio::test]
    async fn test_get_run_mode() {
        let app_state = test_app_state();
        let router = test_router(app_state);

        let (status, json) = get_json(router, "/api/run-mode").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["success"], true);
        assert_eq!(json["current_mode"], "stopped");
    }

    #[rstest]
    #[tokio::test]
    async fn test_set_run_mode() {
        let app_state = test_app_state();
        let router = test_router(app_state.clone());

        let (status, json) = put_json(router, "/api/run-mode", json!({"mode": "running"})).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["success"], true);
        assert_eq!(json["previous_mode"], "stopped");
        assert_eq!(json["current_mode"], "running");

        let router = test_router(app_state);
        let (_, json) = get_json(router, "/api/run-mode").await;
        assert_eq!(json["current_mode"], "running");
    }
}
