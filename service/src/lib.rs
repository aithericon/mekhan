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
    routing::get,
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
        // Health
        .routes(routes!(handlers::health::liveness))
        // Templates
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
        // Instances
        .routes(routes!(
            handlers::instances::list_instances,
            handlers::instances::create_instance
        ))
        .routes(routes!(
            handlers::instances::get_instance,
            handlers::instances::cancel_instance
        ))
        .routes(routes!(handlers::instances::get_instance_state))
        .routes(routes!(handlers::instances::get_instance_events))
        // Processes (HPI inspection)
        .routes(routes!(process::handlers::list_processes))
        .routes(routes!(process::handlers::process_stats))
        .routes(routes!(
            process::handlers::get_process,
            process::handlers::update_process
        ))
        .routes(routes!(process::handlers::get_process_metrics))
        .routes(routes!(process::handlers::get_process_metrics_summary))
        .routes(routes!(process::handlers::get_process_logs))
        .routes(routes!(process::handlers::get_process_tasks))
        .routes(routes!(process::handlers::get_process_artifacts))
        // Processes-live (SSE)
        .routes(routes!(handlers::process_live::metrics_series))
        .routes(routes!(handlers::process_live::metrics_stream))
        .routes(routes!(handlers::process_live::logs_tail))
        .routes(routes!(handlers::process_live::logs_stream))
        .routes(routes!(handlers::process_live::artifacts_list))
        .routes(routes!(handlers::process_live::artifacts_stream))
        // Tasks
        .routes(routes!(process::handlers::list_tasks))
        .routes(routes!(handlers::task_stream::task_stream))
        .routes(routes!(process::handlers::get_task))
        .routes(routes!(process::handlers::complete_task))
        .routes(routes!(process::handlers::cancel_task))
        // Catalogue
        .routes(routes!(catalogue::handlers::list_entries))
        .routes(routes!(catalogue::handlers::stats))
        .routes(routes!(catalogue::handlers::stats_by_net))
        .routes(routes!(catalogue::handlers::lineage))
        .routes(routes!(catalogue::handlers::distinct_values))
        .routes(routes!(catalogue::handlers::distinct_jsonb_values))
        .routes(routes!(catalogue::handlers::download_artifact))
        .routes(routes!(catalogue::handlers::get_entry))
        // Provenance
        .routes(routes!(causality::routes::token_provenance))
        .routes(routes!(causality::routes::cross_link))
        .routes(routes!(causality::routes::provenance_from_artifact))
        .routes(routes!(causality::routes::event_detail))
        // Files (upload has a 50 MB body limit applied at the merged-router level
        // since utoipa-axum doesn't expose per-route layers here)
        .routes(routes!(handlers::files::upload_file))
        .routes(routes!(handlers::files::get_file))
}

pub fn build_router(state: AppState) -> Router {
    let frontend_dir = state.config.frontend_dir.clone();

    // Every #[utoipa::path]-annotated handler is registered via OpenApiRouter
    // so the spec stays in sync with the runtime mounts. The Yjs WebSocket is
    // out-of-band (binary protocol, not OpenAPI-modeled).
    let (api_router, api_spec) = build_openapi_router().split_for_parts();

    let legacy = Router::new()
        // Yjs WebSocket endpoint — binary CRDT protocol, intentionally not in the spec.
        .route(
            "/api/yjs/{template_id}",
            get(handlers::yjs_sync::ws_handler),
        );

    // Merge stateful sub-routers first, then ground out the state generic with
    // `.with_state(state)` so we can attach the stateless SwaggerUI router and
    // SPA fallback. The 50 MB body limit covers /api/files/upload (only path
    // that exceeds Axum's default).
    let api: Router = api_router
        .merge(legacy)
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .with_state(state);

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
