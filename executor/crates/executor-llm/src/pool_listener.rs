//! Minimal HTTP listener for the executor's `pool_url` surface.
//!
//! Sub-phase 2.2 C7: capability-routing's `compute_pools` row stores a
//! `pool_url` that operators and tooling probe for liveness. The legacy
//! `cloud-layer-pool-ollama` bin exposed `POST /v1/inference/run` here;
//! the executor uses NATS for per-job dispatch, so the pool_url surface
//! is reduced to a single endpoint:
//!
//! - `GET /v1/healthz` — returns `{ "status": "ok", "service":
//!   "aithericon-executor" }` when the executor is alive. Operators use
//!   this to confirm a pool row's url is reachable.
//!
//! Inference still flows over NATS via the executor's existing apalis-nats
//! worker. The `/v1/inference/run` HTTP surface is intentionally NOT
//! re-introduced; a future slice can add an HTTP-bridge if needed.

use std::net::SocketAddr;

use axum::routing::get;
use axum::Json;
use axum::Router;
use tokio_util::sync::CancellationToken;

/// Spawn a minimal axum listener on `bind_addr` serving `/v1/healthz`.
/// Returns the actual bound address (useful when `bind_addr` requested
/// port 0). Shutdown via the cancellation token.
pub async fn spawn_pool_listener(
    bind_addr: SocketAddr,
    shutdown: CancellationToken,
) -> anyhow::Result<SocketAddr> {
    let router = Router::new().route(
        "/v1/healthz",
        get(|| async {
            Json(serde_json::json!({
                "status": "ok",
                "service": "aithericon-executor",
            }))
        }),
    );

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    let actual_addr = listener.local_addr()?;

    tokio::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            shutdown.cancelled().await;
        });
        if let Err(e) = server.await {
            tracing::error!(error = %e, "pool_listener axum::serve exited with error");
        }
    });

    Ok(actual_addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Listener binds, responds to /v1/healthz with 200 + the expected
    /// JSON shape, and shuts down cleanly on cancel.
    #[tokio::test]
    async fn pool_listener_healthz_round_trip() {
        let cancel = CancellationToken::new();
        let bind: SocketAddr = "127.0.0.1:0".parse().expect("addr parse");
        let actual = spawn_pool_listener(bind, cancel.clone())
            .await
            .expect("listener spawns");
        // Give the spawned task a moment to start accepting.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let url = format!("http://{actual}/v1/healthz");
        let body: serde_json::Value = reqwest::get(&url)
            .await
            .expect("request succeeds")
            .json()
            .await
            .expect("json parse");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "aithericon-executor");

        cancel.cancel();
    }
}
