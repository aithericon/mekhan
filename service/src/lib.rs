#![allow(dead_code)]

pub mod catalogue;
pub mod compiler;

pub mod config;
pub mod db;
pub mod handlers;
pub mod hpi;
pub mod lifecycle;
pub mod models;
pub mod nats;
pub mod petri;
pub mod query;
pub mod s3;
pub mod yjs;

use std::sync::Arc;

use axum::{
    extract::DefaultBodyLimit,
    routing::{delete, get, post, put},
    Router,
};
use sqlx::PgPool;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::config::AppConfig;
use crate::hpi::HpiClient;
use crate::nats::MekhanNats;
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
    pub hpi: HpiClient,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Template endpoints
        .route("/api/templates", get(handlers::templates::list_templates))
        .route(
            "/api/templates",
            post(handlers::templates::create_template),
        )
        .route(
            "/api/templates/{id}",
            get(handlers::templates::get_template),
        )
        .route(
            "/api/templates/{id}",
            put(handlers::templates::update_template),
        )
        .route(
            "/api/templates/{id}",
            delete(handlers::templates::delete_template),
        )
        .route(
            "/api/templates/{id}/publish",
            post(handlers::templates::publish_template),
        )
        .route(
            "/api/templates/{id}/new-version",
            post(handlers::templates::new_version),
        )
        .route(
            "/api/templates/{id}/versions",
            get(handlers::templates::list_versions),
        )
        .route(
            "/api/templates/{id}/air",
            get(handlers::templates::get_air),
        )
        .route(
            "/api/templates/{id}/compile",
            post(handlers::templates::compile_preview),
        )
        // Stateless compile endpoint
        .route("/api/compile", post(handlers::templates::compile_graph))
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
        // Task endpoints (proxied to HPI)
        .route("/api/tasks", get(handlers::tasks::list_tasks))
        .route(
            "/api/tasks/stream",
            get(handlers::task_stream::task_stream),
        )
        .route("/api/tasks/{task_id}", get(handlers::tasks::get_task))
        .route(
            "/api/tasks/{task_id}/complete",
            post(handlers::tasks::complete_task),
        )
        .route(
            "/api/tasks/{task_id}/cancel",
            post(handlers::tasks::cancel_task),
        )
        // Process endpoints (proxied to HPI)
        .route("/api/processes", get(handlers::processes::list_processes))
        .route(
            "/api/processes/{process_id}",
            get(handlers::processes::get_process),
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
        )
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
