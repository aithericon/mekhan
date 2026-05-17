//! Minimal HTTP listener for the executor-surya pool's `pool_url`.
//!
//! Mirrors `aithericon_executor_llm::pool_listener` shape. Serves:
//!
//! - `GET /v1/healthz` — returns `{ "status": "ok", "service":
//!   "aithericon-executor-surya" }`. Cap-routing's control-URL probe
//!   verifies the pool is reachable via this endpoint per the
//!   workstream #74 contract (executor advertises
//!   `control_url = pool_url` because the listener serves /v1/healthz
//!   on pool_url itself).
//! - `POST /v1/ocr/extract` — the OCR primary surface; routes through
//!   [`crate::adapters::surya::SuryaAdapter`] to the managed Python
//!   subprocess. See [`crate::ocr_handler`] for request/response shape.
//!
//! ## Why /v1/ocr/extract is unconditional (not feature-gated)
//!
//! executor-llm gates `/v1/ocr/extract` behind a `kreuzberg` feature
//! flag because it's an additive OCR surface on a primarily-LLM
//! pool; the kreuzberg dep is heavy and the gate keeps non-OCR
//! deployments lean. For executor-surya the OCR endpoint is the
//! ENTIRE reason the pool exists; no gating needed.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::{get, post};
use axum::{Json, Router};
use tokio_util::sync::CancellationToken;

use crate::adapters::surya::SuryaAdapter;

/// Spawn the executor-surya pool_listener on `bind_addr` with the OCR
/// adapter wired in as router state. Returns the actual bound address
/// (useful when `bind_addr` requested port 0 for tests). Shutdown via
/// the cancellation token.
pub async fn spawn_pool_listener(
    bind_addr: SocketAddr,
    adapter: Arc<SuryaAdapter>,
    shutdown: CancellationToken,
) -> anyhow::Result<SocketAddr> {
    let router = Router::new()
        .route(
            "/v1/healthz",
            get(|| async {
                Json(serde_json::json!({
                    "status": "ok",
                    "service": "aithericon-executor-surya",
                }))
            }),
        )
        .route("/v1/ocr/extract", post(crate::ocr_handler::ocr_extract))
        .with_state(adapter);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    let actual_addr = listener.local_addr()?;
    tokio::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            shutdown.cancelled().await;
        });
        if let Err(e) = server.await {
            tracing::error!(error = %e, "executor-surya pool_listener exited with error");
        }
    });
    Ok(actual_addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn pool_listener_healthz_round_trip() {
        let cancel = CancellationToken::new();
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        // Adapter URL doesn't matter for the healthz path — /v1/healthz
        // doesn't touch the adapter at all.
        let adapter = Arc::new(SuryaAdapter::new("http://127.0.0.1:0"));
        let actual = spawn_pool_listener(bind, adapter, cancel.clone())
            .await
            .expect("listener spawns");
        tokio::time::sleep(Duration::from_millis(20)).await;

        let url = format!("http://{actual}/v1/healthz");
        let body: serde_json::Value = reqwest::get(&url)
            .await
            .expect("request succeeds")
            .json()
            .await
            .expect("json parse");
        assert_eq!(body["status"], "ok");
        // Honest disclosure: service name must distinguish Surya pool
        // from executor-llm's `aithericon-executor` so cap-routing's
        // control probe + operator-facing curl can identify the pool
        // flavour from the healthz body alone.
        assert_eq!(body["service"], "aithericon-executor-surya");
        assert_ne!(body["service"], "aithericon-executor");

        cancel.cancel();
    }
}
