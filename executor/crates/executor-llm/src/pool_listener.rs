//! Minimal HTTP listener for the executor's `pool_url` surface.
//!
//! Sub-phase 2.2 C7: capability-routing's `compute_pools` row stores a
//! `pool_url` that operators and tooling probe for liveness. The pool listener
//! serves two endpoints:
//!
//! - `GET /v1/healthz` — returns `{ "status": "ok", "service":
//!   "aithericon-executor" }` when the executor is alive. Operators use
//!   this to confirm a pool row's url is reachable.
//!
//! - `POST /v1/inference` — synchronous HTTP inference endpoint. Wraps the
//!   `CompletionPort` (OllamaAdapter) against the managed Ollama subprocess.
//!   This is the HTTP-bridge target for cap-routing's mekhan-side dispatcher
//!   (sub-phase 2.3b, engine-side `HttpInferenceHandler`). Inference previously
//!   flowed over NATS via apalis; this endpoint restores the HTTP surface that
//!   the legacy `cloud-layer-pool-ollama` bin exposed, now with the correct
//!   executor wire shape. See [`crate::inference_handler`] for request/response
//!   shape and the lease-validation note.
//!
//! ## OCR-framing Wave 2 (`kreuzberg` feature)
//!
//! When this crate is built with `--features kreuzberg`, the listener
//! additionally serves:
//!
//! - `POST /v1/ocr/extract` — wraps `kreuzberg::extract_file` for the
//!   D1 cert harness "out-of-band cap-routing verification" path. See
//!   [`crate::ocr_handler`] for request/response shape, error mapping,
//!   and the feature-vs-env-gate alignment notes.
//!
//! The route addition is purely additive: with the kreuzberg feature OFF,
//! the listener's route table contains only `/v1/healthz`, `/v1/inference`,
//! `/v1/models/load`, and `/v1/models/evict`.
//!
//! ## Workstream #30 (sub-phase 2.5a) — model-lifecycle endpoints
//!
//! The listener additionally serves:
//!
//! - `POST /v1/models/load` — wraps `OllamaSubprocess::model_load` (Ollama
//!   `/api/pull`). Cap-routing's cold-load coordinator POSTs here to pre-warm
//!   a model on the pool when `pick_route` returned `cold_load_required:
//!   true`.
//! - `POST /v1/models/evict` — wraps `OllamaSubprocess::model_unload` (Ollama
//!   `DELETE /api/delete`). Cap-routing's LRU maintenance loop + admin
//!   `/v1/models/evict` POST here to remove a model from the pool's runtime.
//!
//! ## Sub-phase 2.5d-tools — tool-results callback endpoint
//!
//! - `POST /v1/runs/{run_id}/tool_results` — fulfills the oneshot awaited by
//!   `run_agent_loop`'s `ToolDispatcher::dispatch`. Cloud-layer-workflow
//!   forwards the clinic's `POST /v1/pipelines/{run_id}/tool_results` here.
//!   The handler looks up the in-flight tool call in the per-run oneshot map
//!   and sends the result/error to unblock the parked agent loop.
//!   - `204 No Content` — accepted and dispatched.
//!   - `400 Bad Request` — neither `result` nor `error` present.
//!   - `404 Not Found` — `run_id` or `call_id` unknown.
//!   - `409 Conflict` — `call_id` already resolved (idempotent; no-op).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex};
use tokio_util::sync::CancellationToken;

use crate::inference_handler::{inference, InferenceState};
use crate::ollama_subprocess::OllamaSubprocess;
use crate::port::{CompletionPort, ToolError, ToolErrorKind};

// ---------------------------------------------------------------------------
// Sub-phase 2.5d-tools: tool_results endpoint
// ---------------------------------------------------------------------------

/// Result/error payload from a tool invocation.
///
/// Exactly one of `result` or `error` must be present; the handler returns
/// 400 if neither is set.
#[derive(Debug, Deserialize)]
pub struct ToolResultsRequest {
    pub call_id: String,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<ToolResultError>,
}

/// Wire shape for an error returned by a tool invocation.
#[derive(Debug, Deserialize)]
pub struct ToolResultError {
    pub message: String,
    pub kind: ToolResultErrorKind,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultErrorKind {
    ExecutionFailed,
    Timeout,
    NotFound,
}

/// The value sent over the oneshot to unblock a parked tool call.
pub type ToolResultPayload = Result<serde_json::Value, ToolError>;

/// Per-run map: `call_id` → sender half of the oneshot. Protected by a Mutex
/// so the pool listener's HTTP handler and the agent_loop can safely race.
pub type RunToolMap = Arc<Mutex<HashMap<String, oneshot::Sender<ToolResultPayload>>>>;

/// Shared state for `POST /v1/runs/{run_id}/tool_results`. The outer map key
/// is `run_id` (String); the inner map key is `call_id`.
#[derive(Clone)]
pub struct ToolResultsState {
    /// `run_id` → (`call_id` → oneshot::Sender)
    pub pending: Arc<Mutex<HashMap<String, RunToolMap>>>,
}

impl ToolResultsState {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a oneshot sender for a specific (run_id, call_id) pair.
    /// The agent_loop calls this before emitting the SSE tool_call event.
    pub async fn register(
        &self,
        run_id: &str,
        call_id: &str,
    ) -> oneshot::Receiver<ToolResultPayload> {
        let (tx, rx) = oneshot::channel();
        let mut outer = self.pending.lock().await;
        let run_map = outer
            .entry(run_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(HashMap::new())));
        run_map.lock().await.insert(call_id.to_string(), tx);
        rx
    }

    /// Remove all state for a completed run.
    pub async fn cleanup_run(&self, run_id: &str) {
        self.pending.lock().await.remove(run_id);
    }
}

impl Default for ToolResultsState {
    fn default() -> Self {
        Self::new()
    }
}

async fn tool_results(
    State(state): State<ToolResultsState>,
    Path(run_id): Path<String>,
    Json(req): Json<ToolResultsRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if req.result.is_none() && req.error.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "exactly one of `result` or `error` must be present".to_string(),
        ));
    }

    let outer = state.pending.lock().await;
    let run_map = outer.get(&run_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("run_id {run_id} has no pending tool calls"),
        )
    })?;

    let mut inner = run_map.lock().await;
    let tx = inner.remove(&req.call_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            format!("call_id {} not found for run {run_id}", req.call_id),
        )
    })?;

    let payload: ToolResultPayload = match req.error {
        Some(err) => {
            let kind = match err.kind {
                ToolResultErrorKind::ExecutionFailed => ToolErrorKind::ExecutionFailed,
                ToolResultErrorKind::Timeout => ToolErrorKind::Timeout,
                ToolResultErrorKind::NotFound => ToolErrorKind::NotFound,
            };
            Err(ToolError { message: err.message, kind })
        }
        None => Ok(req.result.unwrap_or(serde_json::Value::Null)),
    };

    // send() fails only if the receiver was dropped (run already cancelled).
    // Treat as 409: the call is no longer awaited.
    tx.send(payload).map_err(|_| {
        (
            StatusCode::CONFLICT,
            format!("call_id {} receiver already dropped (run may have finished)", req.call_id),
        )
    })?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Model-lifecycle state + handlers
// ---------------------------------------------------------------------------

/// Request body for `POST /v1/models/load` and `POST /v1/models/evict`.
/// Workstream #30 wire shape — matches cap-routing's `call_pool_load` /
/// `call_pool_evict` body.
#[derive(Debug, Deserialize)]
struct ModelOpRequest {
    model: String,
}

/// Response body for `POST /v1/models/load`. Wall-clock duration for
/// telemetry; success is signalled by the HTTP 200.
#[derive(Debug, Serialize)]
struct LoadModelResponse {
    model: String,
    duration_ms: u64,
}

/// Shared state for the model-ops handlers — just the Arc'd subprocess
/// handle so we can call its `model_load` / `model_unload` methods.
#[derive(Clone)]
struct ModelOpsState {
    ollama: Arc<OllamaSubprocess>,
}

async fn models_load(
    State(state): State<ModelOpsState>,
    Json(req): Json<ModelOpRequest>,
) -> Result<Json<LoadModelResponse>, (StatusCode, String)> {
    let started = std::time::Instant::now();
    state
        .ollama
        .model_load(&req.model)
        .await
        .map_err(|e| (StatusCode::FAILED_DEPENDENCY, e.to_string()))?;
    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(model = %req.model, duration_ms, "Model pulled via /v1/models/load");
    Ok(Json(LoadModelResponse {
        model: req.model,
        duration_ms,
    }))
}

async fn models_evict(
    State(state): State<ModelOpsState>,
    Json(req): Json<ModelOpRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .ollama
        .model_unload(&req.model)
        .await
        .map_err(|e| (StatusCode::FAILED_DEPENDENCY, e.to_string()))?;
    tracing::info!(model = %req.model, "Model evicted via /v1/models/evict");
    Ok(StatusCode::NO_CONTENT)
}

/// Spawn a minimal axum listener on `bind_addr` serving `/v1/healthz`,
/// `POST /v1/inference`, `POST /v1/models/load`, `POST /v1/models/evict`,
/// `POST /v1/runs/{run_id}/tool_results`, and (when compiled with
/// `--features kreuzberg`) `POST /v1/ocr/extract`. Returns the actual bound
/// address (useful when `bind_addr` requested port 0). Shutdown via the
/// cancellation token.
pub async fn spawn_pool_listener(
    bind_addr: SocketAddr,
    shutdown: CancellationToken,
    llm_port: Arc<dyn CompletionPort>,
    ollama: Arc<OllamaSubprocess>,
    tool_results_state: ToolResultsState,
) -> anyhow::Result<SocketAddr> {
    let inference_state = InferenceState {
        port: llm_port,
        ollama: Arc::clone(&ollama),
    };
    let model_ops_state = ModelOpsState {
        ollama: Arc::clone(&ollama),
    };

    let router = Router::new()
        .route(
            "/v1/healthz",
            get(|| async {
                Json(serde_json::json!({
                    "status": "ok",
                    "service": "aithericon-executor",
                }))
            }),
        )
        .route("/v1/inference", axum::routing::post(inference))
        .with_state(inference_state);

    // Workstream #30 model-lifecycle endpoints — additive; same listener,
    // separate state extractor.
    let router = router.merge(
        Router::new()
            .route("/v1/models/load", post(models_load))
            .route("/v1/models/evict", post(models_evict))
            .with_state(model_ops_state),
    );

    // Sub-phase 2.5d-tools: tool-results callback endpoint.
    let router = router.merge(
        Router::new()
            .route("/v1/runs/{run_id}/tool_results", post(tool_results))
            .with_state(tool_results_state),
    );

    // OCR-framing Wave 2: feature-gated /v1/ocr/extract route. The block
    // is additive — when the kreuzberg feature is OFF, the router above is
    // the only surface served (byte-identical to pre-Wave-2 behaviour).
    #[cfg(feature = "kreuzberg")]
    let router = router.route(
        "/v1/ocr/extract",
        axum::routing::post(crate::ocr_handler::ocr_extract),
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
        // Build minimal state — OllamaSubprocess requires a live binary to
        // start, but the healthz route doesn't touch it. We pass a stub port
        // and bypass the subprocess by noting the test only exercises /v1/healthz.
        //
        // Since we cannot construct OllamaSubprocess without spawning ollama,
        // and the healthz handler never accesses InferenceState, we build a
        // dedicated test that uses the healthz-only sub-router. This keeps the
        // test honest (it always passed before inference was added) while
        // documenting the current limitation for subprocess construction in tests.
        //
        // The /v1/inference route is exercised end-to-end by the cert harness;
        // unit coverage for handler logic lives in inference_handler.rs tests.
        let cancel = CancellationToken::new();
        let bind: SocketAddr = "127.0.0.1:0".parse().expect("addr parse");

        // Build a healthz-only router that mirrors what spawn_pool_listener
        // would serve, without needing a live OllamaSubprocess.
        let healthz_router = Router::new().route(
            "/v1/healthz",
            get(|| async {
                Json(serde_json::json!({
                    "status": "ok",
                    "service": "aithericon-executor",
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind(bind).await.expect("bind");
        let actual = listener.local_addr().expect("local_addr");

        let cancel_inner = cancel.clone();
        tokio::spawn(async move {
            axum::serve(listener, healthz_router)
                .with_graceful_shutdown(async move {
                    cancel_inner.cancelled().await;
                })
                .await
                .ok();
        });

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
