//! Worker-pool coverage endpoint.
//!
//! The worker pool is a set of anonymous, competing-consumer executor workers
//! (NOT enrolled runners — see [`crate::handlers::runners`] for the
//! presence-pool / instrument path). Each worker advertises which
//! `ExecutorJob` backends it serves via `worker.<id>.presence`;
//! [`crate::worker_coverage`] tracks that as advisory, TTL-swept presence.
//!
//! This read surfaces that map so an operator can see the live pool: which
//! workers are connected and, crucially, which backends are covered by ZERO
//! live workers (a step on such a backend will queue at `submitted` until a
//! worker connects). The per-backend list enumerates EVERY `ExecutorJob`
//! backend — a `worker_count` of 0 is the actionable signal.
//!
//! Read-only, behind the auth gate like the other management reads. The pool is
//! shared infrastructure with no workspace, so — unlike the workspace-scoped
//! runner reads — coverage is global (not filtered per tenant).

use axum::{extract::State, Json};
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::AuthUser;
use crate::models::error::ApiError;
use crate::AppState;

/// One live worker's advertised coverage.
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkerCoverageEntry {
    /// Self-reported worker id (the executor daemon's name).
    pub worker_id: String,
    /// `ExecutorJob` backend wire names this worker serves (e.g. `python`).
    pub backends: Vec<String>,
    /// Milliseconds since this worker's last presence heartbeat.
    pub last_seen_ms_ago: u64,
}

/// Per-backend coverage across every `ExecutorJob` backend. A `worker_count` of
/// 0 means NO live worker serves this backend — steps on it will queue.
#[derive(Debug, Serialize, ToSchema)]
pub struct BackendCoverageEntry {
    /// Snake-case backend wire name (`python`, `loki`, …).
    pub backend: String,
    /// Human label for the backend (editor display name).
    pub display_name: String,
    /// Number of live workers advertising this backend.
    pub worker_count: u32,
}

/// Worker-pool coverage snapshot: live workers + per-backend coverage.
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkerCoverageResponse {
    /// Live workers (TTL-swept), each with its advertised backends + freshness.
    pub workers: Vec<WorkerCoverageEntry>,
    /// Coverage for EVERY `ExecutorJob` backend; `worker_count == 0` is uncovered.
    pub backends: Vec<BackendCoverageEntry>,
}

/// `GET /api/v1/workers/coverage` — live worker-pool coverage.
///
/// Reads the in-memory presence map populated from `worker.*.presence`. Global
/// (the pool has no workspace); behind the auth gate like the other reads.
#[utoipa::path(
    get,
    path = "/api/v1/workers/coverage",
    responses(
        (status = 200, description = "Live worker-pool coverage snapshot", body = WorkerCoverageResponse),
    ),
    tag = "workers",
)]
pub async fn worker_coverage(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<WorkerCoverageResponse>, ApiError> {
    let snapshot = state.worker_coverage.snapshot().await;

    let workers: Vec<WorkerCoverageEntry> = snapshot
        .iter()
        .map(|(worker_id, backends, last_seen_ms_ago)| WorkerCoverageEntry {
            worker_id: worker_id.clone(),
            backends: backends.clone(),
            last_seen_ms_ago: *last_seen_ms_ago,
        })
        .collect();

    // Enumerate EVERY ExecutorJob backend (not just covered ones) so the UI can
    // surface uncovered backends (worker_count == 0) — the actionable signal.
    let backends: Vec<BackendCoverageEntry> = aithericon_backends::BACKENDS
        .iter()
        .filter(|m| matches!(m.dispatch_mode, aithericon_backends::DispatchMode::ExecutorJob))
        .map(|m| {
            let worker_count = snapshot
                .iter()
                .filter(|(_, bs, _)| bs.iter().any(|b| b == m.wire_name))
                .count() as u32;
            BackendCoverageEntry {
                backend: m.wire_name.to_string(),
                display_name: m.display_name.to_string(),
                worker_count,
            }
        })
        .collect();

    Ok(Json(WorkerCoverageResponse { workers, backends }))
}
