#![allow(dead_code)]

mod compiler;
mod config;
mod db;
mod handlers;
mod lifecycle;
mod models;
mod nats;
mod petri;

use axum::{
    routing::{delete, get, post, put},
    Router,
};
use sqlx::PgPool;
use std::net::SocketAddr;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mekhan_service=info,tower_http=info".into()),
        )
        .init();

    let config = AppConfig::load().expect("failed to load configuration");
    tracing::info!(
        "starting mekhan-service on {}:{}",
        config.host,
        config.port
    );

    let db = db::create_pool(&config.database_url).await?;
    tracing::info!("database connected and migrations applied");

    let petri = PetriClient::new(&config.petri_lab_url);

    let mekhan_nats = MekhanNats::connect(&config.nats_url).await?;
    tracing::info!("NATS connected at {}", config.nats_url);

    // Spawn lifecycle event listener (updates DB on NetCompleted/NetCancelled)
    tokio::spawn(lifecycle::start_lifecycle_listener(
        mekhan_nats.clone(),
        db.clone(),
    ));

    // Spawn background cleanup sweep
    tokio::spawn(lifecycle::start_cleanup_sweep(
        config.cleanup.clone(),
        db.clone(),
        mekhan_nats.clone(),
        petri.clone(),
    ));

    let state = AppState {
        db,
        petri,
        nats: mekhan_nats,
        config: config.clone(),
    };

    let app = Router::new()
        // Template endpoints
        .route("/api/templates", get(handlers::templates::list_templates))
        .route("/api/templates", post(handlers::templates::create_template))
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
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    tracing::info!("listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
