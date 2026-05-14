#![allow(dead_code)]

pub mod catalogue;
pub mod causality;
pub mod compiler;

pub mod config;
pub mod db;
pub mod handlers;
pub mod lifecycle;
pub mod models;
pub mod nats;
pub mod openapi;
pub mod petri;
pub mod process;
pub mod query;
pub mod s3;
pub mod yjs;

use std::sync::Arc;

use std::path::PathBuf;

use axum::{
    extract::DefaultBodyLimit,
    routing::{delete, get, post},
    Router,
};
use sqlx::PgPool;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_swagger_ui::SwaggerUi;

use crate::catalogue::repository::CatalogueRepository;
use crate::causality::live::LiveBroadcasts;
use crate::config::AppConfig;
use crate::nats::MekhanNats;
use crate::openapi::ApiDoc;
use crate::petri::client::PetriClient;
use crate::s3::ArtifactStore;
use crate::yjs::manager::YjsManager;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub petri: PetriClient,
    pub nats: MekhanNats,
    pub config: AppConfig,
    pub yjs: Arc<YjsManager>,
    pub s3: Arc<ArtifactStore>,
    pub artifact_s3: Option<Arc<ArtifactStore>>,
    pub catalogue_repo: Arc<dyn CatalogueRepository>,
    pub live: Arc<LiveBroadcasts>,
}

/// Build the `OpenApiRouter` containing every `#[utoipa::path]`-annotated
/// handler. Single source of truth for both [`build_router`] (runtime mount +
/// swagger-ui) and [`openapi_spec`] (CLI dump for frontend codegen).
fn build_openapi_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::with_openapi(ApiDoc::openapi())
        .routes(routes!(
            handlers::templates::list_templates,
            handlers::templates::create_template
        ))
        .routes(routes!(
            handlers::templates::get_template,
            handlers::templates::update_template,
            handlers::templates::delete_template
        ))
        .routes(routes!(handlers::templates::publish_template))
        .routes(routes!(handlers::templates::new_version))
        .routes(routes!(handlers::templates::list_versions))
        .routes(routes!(handlers::templates::get_air))
        .routes(routes!(handlers::templates::compile_preview))
        .routes(routes!(handlers::templates::compile_graph))
}

pub fn build_router(state: AppState) -> Router {
    let frontend_dir = state.config.frontend_dir.clone();

    // Routes that are #[utoipa::path]-annotated go through OpenApiRouter so
    // they appear in the generated spec. Remaining (legacy) routes are wired
    // on the plain axum Router below and merged together.
    let (templates_router, api_spec) = build_openapi_router().split_for_parts();

    let legacy = Router::new()
        // Health
        .route("/health", get(handlers::health::liveness))
        // Instance endpoints
        .route(
            "/api/instances",
            get(handlers::instances::list_instances),
        )
        .route(
            "/api/instances",
            post(handlers::instances::create_instance),
        )
        .route(
            "/api/instances/{id}",
            get(handlers::instances::get_instance),
        )
        .route(
            "/api/instances/{id}/state",
            get(handlers::instances::get_instance_state),
        )
        .route(
            "/api/instances/{id}/events",
            get(handlers::instances::get_instance_events),
        )
        .route(
            "/api/instances/{id}",
            delete(handlers::instances::cancel_instance),
        )
        // Process endpoints (native)
        .route("/api/processes", get(process::handlers::list_processes))
        .route("/api/processes/stats", get(process::handlers::process_stats))
        .route(
            "/api/processes/{process_id}",
            get(process::handlers::get_process).put(process::handlers::update_process),
        )
        .route(
            "/api/processes/{process_id}/metrics",
            get(process::handlers::get_process_metrics),
        )
        .route(
            "/api/processes/{process_id}/metrics/summary",
            get(process::handlers::get_process_metrics_summary),
        )
        .route(
            "/api/processes/{process_id}/logs",
            get(process::handlers::get_process_logs),
        )
        .route(
            "/api/processes/{process_id}/tasks",
            get(process::handlers::get_process_tasks),
        )
        .route(
            "/api/processes/{process_id}/metrics/series",
            get(handlers::process_live::metrics_series),
        )
        .route(
            "/api/processes/{process_id}/metrics/stream",
            get(handlers::process_live::metrics_stream),
        )
        .route(
            "/api/processes/{process_id}/logs/tail",
            get(handlers::process_live::logs_tail),
        )
        .route(
            "/api/processes/{process_id}/logs/stream",
            get(handlers::process_live::logs_stream),
        )
        .route(
            "/api/processes/{process_id}/artifacts/list",
            get(handlers::process_live::artifacts_list),
        )
        .route(
            "/api/processes/{process_id}/artifacts/stream",
            get(handlers::process_live::artifacts_stream),
        )
        .route(
            "/api/processes/{process_id}/artifacts",
            get(process::handlers::get_process_artifacts),
        )
        // Provenance endpoints (causality)
        .route(
            "/api/provenance/{net_id}/{token_id}",
            get(causality::routes::token_provenance),
        )
        .route(
            "/api/provenance/link/{signal_key}",
            get(causality::routes::cross_link),
        )
        .route(
            "/api/provenance/from-artifact/{execution_id}/{artifact_id}",
            get(causality::routes::provenance_from_artifact),
        )
        .route(
            "/api/provenance/{net_id}/{event_seq}/detail",
            get(causality::routes::event_detail),
        )
        // Task endpoints (native)
        .route("/api/tasks", get(process::handlers::list_tasks))
        .route(
            "/api/tasks/stream",
            get(handlers::task_stream::task_stream),
        )
        .route("/api/tasks/{id}", get(process::handlers::get_task))
        .route(
            "/api/tasks/{id}/complete",
            post(process::handlers::complete_task),
        )
        .route(
            "/api/tasks/{id}/cancel",
            post(process::handlers::cancel_task),
        )
        // File upload/download endpoints (50 MB limit)
        .route(
            "/api/files/upload/{id}/{node_id}",
            post(handlers::files::upload_file)
                .layer(DefaultBodyLimit::max(50 * 1024 * 1024)),
        )
        .route("/api/files/{*key}", get(handlers::files::get_file))
        // Catalogue endpoints
        .route("/api/catalogue", get(catalogue::handlers::list_entries))
        .route("/api/catalogue/stats", get(catalogue::handlers::stats))
        .route(
            "/api/catalogue/stats/by-net",
            get(catalogue::handlers::stats_by_net),
        )
        .route(
            "/api/catalogue/lineage/{process_id}",
            get(catalogue::handlers::lineage),
        )
        .route(
            "/api/catalogue/distinct/{column}",
            get(catalogue::handlers::distinct_values),
        )
        .route(
            "/api/catalogue/distinct-jsonb/{column}/{key}",
            get(catalogue::handlers::distinct_jsonb_values),
        )
        .route(
            "/api/catalogue/download/{*path}",
            get(catalogue::handlers::download_artifact),
        )
        .route(
            "/api/catalogue/{execution_id}/{id}",
            get(catalogue::handlers::get_entry),
        )
        // Yjs WebSocket endpoint
        .route(
            "/api/yjs/{template_id}",
            get(handlers::yjs_sync::ws_handler),
        );

    // Merge stateful sub-routers first, then ground out the state generic with
    // `.with_state(state)` so we can attach the stateless SwaggerUI router and
    // SPA fallback.
    let api: Router = templates_router.merge(legacy).with_state(state);

    let swagger = SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", api_spec);

    let app = if let Some(dir) = frontend_dir {
        let path = PathBuf::from(dir);
        let index = path.join("index.html");
        let spa = ServeDir::new(&path).fallback(ServeFile::new(&index));
        api.merge(swagger).fallback_service(spa)
    } else {
        api.merge(swagger)
    };

    app.layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}

/// Build the OpenAPI document without booting any state — used by the CLI's
/// `mekhan openapi` subcommand to dump the spec for codegen pipelines.
pub fn openapi_spec() -> utoipa::openapi::OpenApi {
    let (_, api) = build_openapi_router().split_for_parts();
    api
}
