use std::sync::Arc;

use axum::{
    routing::{get, patch, post},
    Router,
};
use parking_lot::RwLock;
use tokio::sync::{broadcast, Notify};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use petri_application::{
    AdapterScheduler, EventRepository, PetriNetService, StateProjection, TopologyRepository,
};

use crate::dto::RunMode;
use petri_domain::{
    Arc as PetriArc, ArcDirection, ArcId, DomainEvent, Marking, PersistedEvent, PetriNet, Place,
    PlaceId, PlaceKind, Port, PortCardinality, Token, TokenColor, TokenId, Transition,
    TransitionId,
};

use crate::dto::{
    AnalysisReport, AnalysisSummary, CommandResponse, CreateTokenRequest, ErrorResponse,
    EvaluateFinalState, EvaluateRequest, EvaluateResponse, EventsResponse, FiredTransition,
    IssueLevel, LoadScenarioRequest, LoadScenarioResponse, RunModeResponse, ScenarioArc,
    ScenarioDefinition, ScenarioPlace, ScenarioPort, ScenarioToken, ScenarioTransition,
    SetRunModeRequest, StateResponse, TopologyResponse, UpdateTransitionRequest, ValidationIssue,
};
use crate::handlers::net_scoped;
use crate::handlers::{
    analyze_topology_handler, create_token, evaluate, event_stream, fire_transition, get_events,
    get_run_mode, get_services, get_state, get_topology, load_scenario, reset, set_run_mode,
    update_transition_script,
};
use crate::net_registry::NetRegistry;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Digital Lab - Colored Petri Net API",
        version = "0.1.0",
        description = "Event-sourced Petri Net engine for the Aithericon Lab"
    ),
    paths(
        crate::handlers::get_topology,
        crate::handlers::get_events,
        crate::handlers::get_state,
        crate::handlers::fire_transition,
        crate::handlers::create_token,
        crate::handlers::reset,
        crate::handlers::load_scenario,
        crate::handlers::update_transition_script,
        crate::handlers::analyze_topology_handler,
        crate::handlers::evaluate,
        crate::handlers::get_run_mode,
        crate::handlers::set_run_mode,
    ),
    components(schemas(
        // Domain types
        PetriNet,
        Place,
        PlaceId,
        PlaceKind,
        Transition,
        TransitionId,
        Port,
        PortCardinality,
        PetriArc,
        ArcId,
        ArcDirection,
        Token,
        TokenId,
        TokenColor,
        Marking,
        DomainEvent,
        PersistedEvent,
        // DTO types
        TopologyResponse,
        EventsResponse,
        StateResponse,
        CreateTokenRequest,
        CommandResponse,
        ErrorResponse,
        // Scenario loading DTOs
        ScenarioDefinition,
        ScenarioPlace,
        ScenarioTransition,
        ScenarioPort,
        ScenarioArc,
        ScenarioToken,
        LoadScenarioRequest,
        LoadScenarioResponse,
        UpdateTransitionRequest,
        // Analysis DTOs
        AnalysisReport,
        AnalysisSummary,
        ValidationIssue,
        IssueLevel,
        // Run Mode and Evaluation DTOs
        RunMode,
        EvaluateRequest,
        EvaluateResponse,
        EvaluateFinalState,
        FiredTransition,
        SetRunModeRequest,
        RunModeResponse,
    )),
    tags(
        (name = "Topology", description = "Petri Net structure endpoints"),
        (name = "Events", description = "Event log endpoints"),
        (name = "State", description = "Current state endpoints"),
        (name = "Commands", description = "Command endpoints for mutations"),
        (name = "Scenario", description = "Dynamic scenario loading"),
        (name = "Analysis", description = "Static analysis and validation"),
        (name = "RunMode", description = "Engine run mode control"),
    )
)]
pub struct ApiDoc;

/// Signal type for the SSE broadcast channel.
#[derive(Clone, Debug)]
pub enum SseSignal {
    /// A new persisted event.
    Event(Box<PersistedEvent>),
    /// The net was reset — clients should clear local state.
    Reset,
}

/// Combined app state for core API handlers (no NATS dependencies).
pub struct AppState<E, T, S>
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    pub service: Arc<PetriNetService<E, T, S>>,
    pub adapter_scheduler: Arc<AdapterScheduler>,
    /// Current run mode (stopped/running)
    pub run_mode: Arc<RwLock<RunMode>>,
    /// Notify channel to wake evaluation loop when tokens are created
    pub eval_notify: Arc<Notify>,
    /// Broadcast sender for SSE: each mutation broadcasts to all connected SSE clients.
    pub event_tx: Arc<broadcast::Sender<SseSignal>>,
    /// Sub-phase 2.5e-γ.mekhan per-run dispatch options. Mutated by
    /// `load_scenario`; consulted by the firing path at evaluate/fire time.
    /// Per-NetInstance scope — the Arc is cloned from
    /// `NetInstance::dispatch_options` via `as_app_state`, so distinct nets
    /// carry independent dispatch options.
    pub dispatch_options: Arc<RwLock<petri_domain::DispatchOptions>>,
}

// Manual Clone implementation - Arc and RwLock are Clone regardless of inner types
impl<E, T, S> Clone for AppState<E, T, S>
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    fn clone(&self) -> Self {
        Self {
            service: self.service.clone(),
            adapter_scheduler: self.adapter_scheduler.clone(),
            run_mode: self.run_mode.clone(),
            eval_notify: self.eval_notify.clone(),
            event_tx: self.event_tx.clone(),
            dispatch_options: self.dispatch_options.clone(),
        }
    }
}

/// Create the core API routes.
///
/// Returns a Router with all core API endpoints but **without** CORS,
/// SwaggerUI, or NATS-specific routes. The caller is responsible for
/// nesting under `/api`, adding CORS layers, merging NATS routes, etc.
pub fn create_router<E, T, S>(state: AppState<E, T, S>) -> Router
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    Router::new()
        .route("/topology", get(get_topology::<E, T, S>))
        .route(
            "/topology/transition/:transition_id",
            patch(update_transition_script::<E, T, S>),
        )
        .route("/events", get(get_events::<E, T, S>))
        .route("/events/stream", get(event_stream::<E, T, S>))
        .route("/state", get(get_state::<E, T, S>))
        .route(
            "/command/fire/:transition_id",
            post(fire_transition::<E, T, S>),
        )
        .route("/command/create-token", post(create_token::<E, T, S>))
        .route("/command/reset", post(reset::<E, T, S>))
        .route("/command/evaluate", post(evaluate::<E, T, S>))
        .route("/scenario", post(load_scenario::<E, T, S>))
        .route("/analyze", get(analyze_topology_handler::<E, T, S>))
        .route(
            "/run-mode",
            get(get_run_mode::<E, T, S>).put(set_run_mode::<E, T, S>),
        )
        .route("/services", get(get_services::<E, T, S>))
        .with_state(state)
}

/// Create net-scoped API routes that operate on named net instances via the registry.
///
/// All routes are mounted under `/api/nets/{net_id}/*`.
fn create_net_routes<E, T, S>() -> Router<Arc<NetRegistry<E, T, S>>>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    Router::new()
        .route("/", get(net_scoped::list_nets::<E, T, S>))
        // NOTE: DELETE /:net_id is wired in main.rs with full NATS cleanup support
        .route(
            "/:net_id/topology",
            get(net_scoped::net_get_topology::<E, T, S>),
        )
        .route(
            "/:net_id/topology/transition/:transition_id",
            patch(net_scoped::net_update_transition_script::<E, T, S>),
        )
        .route(
            "/:net_id/events",
            get(net_scoped::net_get_events::<E, T, S>),
        )
        .route(
            "/:net_id/events/stream",
            get(net_scoped::net_event_stream::<E, T, S>),
        )
        .route("/:net_id/state", get(net_scoped::net_get_state::<E, T, S>))
        .route(
            "/:net_id/command/fire/:transition_id",
            post(net_scoped::net_fire_transition::<E, T, S>),
        )
        .route(
            "/:net_id/command/create-token",
            post(net_scoped::net_create_token::<E, T, S>),
        )
        .route(
            "/:net_id/command/reset",
            post(net_scoped::net_reset::<E, T, S>),
        )
        .route(
            "/:net_id/command/evaluate",
            post(net_scoped::net_evaluate::<E, T, S>),
        )
        .route(
            "/:net_id/scenario",
            post(net_scoped::net_load_scenario::<E, T, S>),
        )
        .route("/:net_id/analyze", get(net_scoped::net_analyze::<E, T, S>))
        .route(
            "/:net_id/run-mode",
            get(net_scoped::net_get_run_mode::<E, T, S>)
                .put(net_scoped::net_set_run_mode::<E, T, S>),
        )
        .route(
            "/:net_id/services",
            get(net_scoped::net_get_services::<E, T, S>),
        )
        .route(
            "/:net_id/command/hibernate",
            post(net_scoped::net_hibernate::<E, T, S>),
        )
        .route(
            "/:net_id/command/wake",
            post(net_scoped::net_wake::<E, T, S>),
        )
}

/// Create a full application router with CORS, SwaggerUI, and net-scoped routes.
///
/// All API routes are mounted under `/api/nets/{net_id}/*`.
/// If an `ArtifactStoreState` is provided, adds `PUT /api/artifacts/*path`
/// for uploading files referenced by `storage_path` job inputs.
pub fn create_router_with_registry<E, T, S>(registry: Arc<NetRegistry<E, T, S>>) -> Router
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    use tower_http::cors::{Any, CorsLayer};

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let bridge_check_route = Router::new()
        .route(
            "/api/bridges/check",
            get(super::handlers::check_all_bridges::<E, T, S>),
        )
        .with_state(registry.clone());

    let net_routes = create_net_routes::<E, T, S>().with_state(registry);

    #[allow(unused_mut)]
    let mut app = Router::new()
        .nest("/api/nets", net_routes)
        .merge(bridge_check_route)
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()));

    // Add artifact upload route if configured (requires `artifact-store` feature + env vars)
    #[cfg(feature = "artifact-store")]
    {
        if let Some(store) = crate::artifact_store::ArtifactStoreState::from_env() {
            tracing::info!("Artifact store enabled");
            let artifact_route = Router::new()
                .route(
                    "/api/artifacts/*path",
                    axum::routing::put(crate::artifact_store::upload_artifact),
                )
                .with_state(store);
            app = app.merge(artifact_route);
        }
    }

    app.layer(cors)
}
