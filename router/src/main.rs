//! `inference-router` — the OpenAI-compatible model-pool data plane.
//!
//! A standalone deployable (umbrella workspace member, peer to mekhan-service)
//! so it stays off mekhan's session-cookie middleware and scales on its own.
//! See `docs/29-model-pool-impl-plan.md` (Router-MVP) and
//! `docs/11-inference-router.md`.

use std::sync::Arc;

use anyhow::Context;
use axum::{
    routing::{get, post},
    Router,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use inference_router::auth::AuthConfig;
use inference_router::cancel::{self, CancellationRegistry};
use inference_router::config::RouterConfig;
use inference_router::metrics::Metrics;
use inference_router::proxy::{self, RouterCtx};
use inference_router::routing::ReplicaTable;
use inference_router::{inventory, openapi};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cfg = RouterConfig::load()?;
    info!(
        bind = %cfg.bind_addr,
        replicas = cfg.replicas.len(),
        auth = %cfg.auth.mode,
        "starting inference-router"
    );

    let table = Arc::new(ReplicaTable::from_config(&cfg.replicas));
    let cancels = CancellationRegistry::new();
    let auth = Arc::new(AuthConfig::from_settings(
        &cfg.auth.mode,
        &cfg.auth.default_tenant,
    ));
    let metrics = Arc::new(Metrics::default());
    let shutdown = CancellationToken::new();

    // NATS is optional: it backs cancel-subscribe + metering-publish. Without
    // it the router still routes + admits; cancel/metering are simply off.
    let nats = match &cfg.nats_url {
        Some(url) => match async_nats::connect(url).await {
            Ok(client) => {
                info!(%url, "connected to NATS (cancel + metering enabled)");
                cancel::spawn_cancel_listener(client.clone(), cancels.clone(), shutdown.clone())
                    .await
                    .context("starting inference cancel listener")?;
                Some(client)
            }
            Err(e) => {
                warn!(%url, error = %e, "NATS connect failed — cancel/metering disabled");
                None
            }
        },
        None => {
            info!("no nats_url configured — cancel/metering disabled");
            None
        }
    };

    inventory::spawn_inventory_refresh(table.clone(), cfg.mekhan_url.clone());

    let ctx = RouterCtx {
        table,
        cancels,
        auth,
        nats,
        metrics,
    };

    let app = Router::new()
        .route("/v1/chat/completions", post(proxy::chat_completions))
        .route("/v1/models", get(proxy::list_models))
        .route("/metrics", get(proxy::metrics_handler))
        .route("/healthz", get(proxy::healthz))
        .route("/openapi.json", get(openapi::openapi_doc))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(ctx);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr)
        .await
        .with_context(|| format!("binding {}", cfg.bind_addr))?;
    info!(addr = %cfg.bind_addr, "inference-router listening");

    let shutdown_signal = {
        let shutdown = shutdown.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("shutdown signal received");
            shutdown.cancel();
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .context("router server error")?;
    Ok(())
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,inference_router=debug")),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}
