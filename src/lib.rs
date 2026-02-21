#![allow(dead_code)]

pub mod compiler;
pub mod config;
pub mod db;
pub mod handlers;
pub mod lifecycle;
pub mod models;
pub mod nats;
pub mod petri;
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
        // File upload/download endpoints (50 MB limit)
        .route(
            "/api/files/upload/{id}/{node_id}",
            post(handlers::files::upload_file)
                .layer(DefaultBodyLimit::max(50 * 1024 * 1024)),
        )
        .route("/api/files/{*key}", get(handlers::files::get_file))
        // Yjs WebSocket endpoint
        .route(
            "/api/yjs/{template_id}",
            get(handlers::yjs_sync::ws_handler),
        )
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
