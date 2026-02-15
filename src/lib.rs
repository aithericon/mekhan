#![allow(dead_code)]

pub mod compiler;
pub mod config;
pub mod db;
pub mod handlers;
pub mod lifecycle;
pub mod models;
pub mod nats;
pub mod petri;

use axum::{
    routing::{delete, get, post, put},
    Router,
};
use sqlx::PgPool;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::config::AppConfig;
use crate::nats::MekhanNats;
use crate::petri::client::PetriClient;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub petri: PetriClient,
    pub nats: MekhanNats,
    pub config: AppConfig,
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
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
