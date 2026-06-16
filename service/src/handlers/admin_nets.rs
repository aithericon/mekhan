//! Admin engine-net overview + kill-switch / cleanup (2026-06-10 incident
//! follow-up).
//!
//! Three workspace-admin-gated endpoints over the engine's net population:
//!
//!   - `GET    /api/v1/admin/nets` — every net the engine knows about (hot AND
//!     hibernated, from `KV_NET_METADATA` via the engine's
//!     `/api/nets/metadata`), enriched with the PETRI_GLOBAL per-net event
//!     count (the runaway-net tell: a six-digit count on a running net is a
//!     fire) and the owning `workflow_instances` row when there is one.
//!   - `DELETE /api/v1/admin/nets/{net_id}` — the KILL SWITCH. Proxies the
//!     engine's `DELETE /api/nets/{id}`, which is a proper terminate:
//!     rehydrates a hibernated-but-active net, drains lease finalizers,
//!     emits `NetCancelled`, cancels tasks. Admin override — deliberately not
//!     scoped to instance-workspace membership like the `/petri/*` proxy,
//!     because runaway INFRA nets (pool nets, actuation nets) have no
//!     instance row to be a member of.
//!   - `POST   /api/v1/admin/nets/{net_id}/purge-events` — the CLEANUP.
//!     Purges the net's `petri.events.{id}.>` + `petri.signal.{id}.>`
//!     subjects out of PETRI_GLOBAL. Refused for active (running/created)
//!     nets — their event log is their authoritative state (hydration replays
//!     it); a terminal net's history is only projection fodder, and the
//!     projections have already folded it.
//!
//! The list intentionally serves a flat, engine-wide view (not
//! workspace-filtered): it is an OPERATOR surface for spotting runaways and
//! orphans, mounted admin-only, mirroring the `/nets` Engine Nets browser it
//! powers.

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{map_to_api_error, require_role, AuthUser, Role};
use crate::models::error::{ApiError, ErrorResponse};
use crate::nats::subjects::{net_events_filter, net_signals_filter, Subjects};
use crate::AppState;

/// Caller-implicit workspace — mirrors `roster::caller_workspace`. 403 when
/// the caller has no active workspace (no silent nil-tenant fallback).
fn caller_workspace(user: &AuthUser) -> Result<Uuid, ApiError> {
    user.require_workspace()
}

/// A net id is interpolated into NATS subject filters (`petri.events.{id}.>`)
/// on the purge path. Reject anything that could widen the filter — a `.`
/// adds a token boundary, `*`/`>` are wildcards — so a crafted id can never
/// purge more than its own subjects. Engine ids are `[A-Za-z0-9_-]` in
/// practice (`mekhan-<uuid>`, `pool-<id>`, `staging-<id>`).
fn validate_net_id(net_id: &str) -> Result<(), ApiError> {
    if net_id.is_empty() || net_id.len() > 256 {
        return Err(ApiError::bad_request("invalid net id"));
    }
    if !net_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiError::bad_request(
            "net id may only contain [A-Za-z0-9_-]",
        ));
    }
    Ok(())
}

/// Is this a mekhan-managed WORKFLOW INSTANCE net (`mekhan-<instance_id>`), as
/// opposed to an INFRASTRUCTURE net (`pool-*`, `staging-*`, `materialize-*`,
/// `model-replica-*`, …)? Every workflow instance deploys as `mekhan-{id}`
/// (see `format!("mekhan-{instance_id}")` throughout the control plane), so the
/// prefix is the reliable classifier. Bulk-kill targets only these by default
/// — killing infra nets out from under the platform (a pool net = a runner's
/// admission gate; a staging net = an in-flight publish) is the catastrophic
/// case the default guards against.
fn is_workflow_instance_net(net_id: &str) -> bool {
    net_id.starts_with("mekhan-")
}

/// Normalize an engine `status` field (a bare `"running"` string, tolerant of
/// an older enum-object shape) to a plain status string.
fn status_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string().trim_matches('"').to_string(),
    }
}

/// A net the engine still treats as live — its event log is authoritative
/// state, so it must never be purged (and bulk-purge skips it).
fn is_active_status(status: &str) -> bool {
    status == "running" || status == "created"
}

/// Fetch + deserialize the engine's `/api/nets/metadata` population. Shared by
/// the overview, the single + bulk purge guards, and the terminal sweep.
async fn fetch_engine_metas(state: &AppState) -> Result<Vec<EngineNetMeta>, ApiError> {
    let raw = state
        .petri
        .list_nets_metadata()
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("engine metadata: {e}")))?;
    serde_json::from_value(raw).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("engine metadata shape: {e}"),
        )
    })
}

/// Purge a single net's `petri.events.{id}.>` + `petri.signal.{id}.>` subjects
/// from an open PETRI_GLOBAL handle; returns the message count removed. Caller
/// owns the active-net guard (an active net's log must not be purged).
async fn purge_net_subjects(
    stream: &async_nats::jetstream::stream::Stream,
    net_id: &str,
) -> Result<u64, String> {
    let mut purged: u64 = 0;
    for subject in [net_events_filter(net_id), net_signals_filter(net_id)] {
        let resp = stream
            .purge()
            .filter(&subject)
            .await
            .map_err(|e| format!("purge {subject}: {e}"))?;
        purged += resp.purged;
    }
    Ok(purged)
}

/// One row of the admin net overview.
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminNetRow {
    /// Engine net id (`mekhan-<instance>`, `pool-<resource>`, …).
    pub net_id: String,
    /// Engine lifecycle status: `created | running | completed | cancelled | failed`.
    pub status: String,
    /// Hot (in the engine registry) vs hibernated.
    pub in_memory: bool,
    /// Human label from the net metadata, when the deployer set one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Engine-side template reference from the net metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// Principal recorded at deploy time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Messages currently on `petri.events.{net_id}.>` in PETRI_GLOBAL.
    /// `None` when the stream scan failed (fail-soft — the list still renders).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_count: Option<u64>,
    /// Owning workflow instance, when this net is a mekhan-managed instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<Uuid>,
}

/// Wire shape of one engine `/api/nets/metadata` entry (untyped on the
/// engine side; deserialized defensively here).
#[derive(Debug, Deserialize)]
struct EngineNetMeta {
    net_id: String,
    status: serde_json::Value,
    #[serde(default)]
    in_memory: bool,
    #[serde(default)]
    template_id: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

/// Result of a purge: how many messages were removed from PETRI_GLOBAL.
#[derive(Debug, Serialize, ToSchema)]
pub struct PurgeEventsResponse {
    /// Total messages purged across `petri.events.{id}.>` and
    /// `petri.signal.{id}.>`.
    pub purged_messages: u64,
}

/// Per-net message counts on `petri.events.>`, aggregated from the stream's
/// subjects map. Fail-soft: any error yields `None` and the overview renders
/// without counts rather than 500ing the whole page.
async fn event_counts_by_net(state: &AppState) -> Option<HashMap<String, u64>> {
    let stream = match state
        .nats
        .jetstream()
        .get_stream(Subjects::STREAM_GLOBAL)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("admin-nets: PETRI_GLOBAL not available: {e}");
            return None;
        }
    };
    let mut info = match stream.info_with_subjects(Subjects::EVENTS_ALL).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("admin-nets: subjects scan failed: {e}");
            return None;
        }
    };
    let mut counts: HashMap<String, u64> = HashMap::new();
    loop {
        match info.try_next().await {
            Ok(Some((subject, count))) => {
                // petri.events.{net_id}.<rest>
                if let Some(net_id) = subject.split('.').nth(2) {
                    *counts.entry(net_id.to_string()).or_default() += count as u64;
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!("admin-nets: subjects page failed: {e}");
                return None;
            }
        }
    }
    Some(counts)
}

/// `GET /api/v1/admin/nets` — engine-wide net overview (admin).
#[utoipa::path(
    get,
    path = "/api/v1/admin/nets",
    responses(
        (status = 200, description = "Every engine net with status, event count and instance join", body = Vec<AdminNetRow>),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 502, description = "Engine unreachable", body = ErrorResponse),
    ),
    tag = "admin-nets",
)]
pub async fn list_admin_nets(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<AdminNetRow>>, ApiError> {
    require_role(&state.db, &user, caller_workspace(&user)?, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    let raw = state
        .petri
        .list_nets_metadata()
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("engine metadata: {e}")))?;
    let metas: Vec<EngineNetMeta> = serde_json::from_value(raw).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("engine metadata shape: {e}"),
        )
    })?;

    let counts = event_counts_by_net(&state).await;

    // Join owning instances in one query. `net_id` is unique per instance.
    let net_ids: Vec<String> = metas.iter().map(|m| m.net_id.clone()).collect();
    let instance_by_net: HashMap<String, Uuid> = sqlx::query_as::<_, (String, Uuid)>(
        "SELECT net_id, id FROM workflow_instances WHERE net_id = ANY($1)",
    )
    .bind(&net_ids)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("instance join: {e}")))?
    .into_iter()
    .collect();

    let mut rows: Vec<AdminNetRow> = metas
        .into_iter()
        .map(|m| {
            // NetStatus serializes as a bare string ("running") — but be
            // tolerant of an object/enum-variant shape from older engines.
            let status = match &m.status {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string().trim_matches('"').to_string(),
            };
            AdminNetRow {
                event_count: counts.as_ref().map(|c| *c.get(&m.net_id).unwrap_or(&0)),
                instance_id: instance_by_net.get(&m.net_id).copied(),
                net_id: m.net_id,
                status,
                in_memory: m.in_memory,
                label: m.label,
                template_id: m.template_id,
                created_by: m.created_by,
            }
        })
        .collect();

    // Highest event count first — the runaway floats to the top.
    rows.sort_by_key(|r| std::cmp::Reverse(r.event_count));
    Ok(Json(rows))
}

/// `DELETE /api/v1/admin/nets/{net_id}` — kill switch (admin).
///
/// Proxies the engine's terminate-with-cleanup: rehydrates a hibernated
/// active net, drains lease finalizers, emits `NetCancelled`, cancels tasks.
/// Idempotent — an already-terminal or unknown net is a no-op success.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/nets/{net_id}",
    params(("net_id" = String, Path, description = "Engine net id")),
    responses(
        (status = 204, description = "Net terminated (or already gone)"),
        (status = 400, description = "Invalid net id", body = ErrorResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 502, description = "Engine unreachable", body = ErrorResponse),
    ),
    tag = "admin-nets",
)]
pub async fn kill_admin_net(
    State(state): State<AppState>,
    user: AuthUser,
    Path(net_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_role(&state.db, &user, caller_workspace(&user)?, Role::Admin)
        .await
        .map_err(map_to_api_error)?;
    validate_net_id(&net_id)?;

    state
        .petri
        .delete_net(&net_id)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("engine terminate: {e}")))?;
    tracing::info!(net_id = %net_id, by = %user.subject, "admin kill-switch: net terminated");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/admin/nets/{net_id}/purge-events` — cleanup (admin).
///
/// Purges the net's event + signal subjects from PETRI_GLOBAL. Refused (409)
/// while the engine still reports the net active — an active net's event log
/// is its authoritative state and purging it would corrupt rehydration.
/// A net UNKNOWN to the engine is purgeable: that is exactly the orphan-
/// garbage case this endpoint exists for.
#[utoipa::path(
    post,
    path = "/api/v1/admin/nets/{net_id}/purge-events",
    params(("net_id" = String, Path, description = "Engine net id")),
    responses(
        (status = 200, description = "Events purged", body = PurgeEventsResponse),
        (status = 400, description = "Invalid net id", body = ErrorResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 409, description = "Net is still active — terminate it first", body = ErrorResponse),
        (status = 502, description = "Engine unreachable", body = ErrorResponse),
    ),
    tag = "admin-nets",
)]
pub async fn purge_admin_net_events(
    State(state): State<AppState>,
    user: AuthUser,
    Path(net_id): Path<String>,
) -> Result<Json<PurgeEventsResponse>, ApiError> {
    require_role(&state.db, &user, caller_workspace(&user)?, Role::Admin)
        .await
        .map_err(map_to_api_error)?;
    validate_net_id(&net_id)?;

    // Active-net guard. The engine is authoritative for liveness; only its
    // metadata can say "safe to purge". Engine-unreachable → refuse (fail
    // closed): purging blind could destroy a running net's event log.
    let metas = fetch_engine_metas(&state).await?;
    if let Some(meta) = metas.iter().find(|m| m.net_id == net_id) {
        let status = status_str(&meta.status);
        if is_active_status(&status) {
            return Err(ApiError::conflict(format!(
                "net {net_id} is {status} — terminate it before purging its events"
            )));
        }
    }

    let stream = state
        .nats
        .jetstream()
        .get_stream(Subjects::STREAM_GLOBAL)
        .await
        .map_err(|e| ApiError::internal(format!("PETRI_GLOBAL: {e}")))?;

    let purged = purge_net_subjects(&stream, &net_id)
        .await
        .map_err(ApiError::internal)?;

    tracing::info!(
        net_id = %net_id,
        purged,
        by = %user.subject,
        "admin cleanup: net events purged from PETRI_GLOBAL"
    );
    Ok(Json(PurgeEventsResponse {
        purged_messages: purged,
    }))
}

// ── Bulk operations ──────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/admin/nets/bulk-kill`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkKillRequest {
    /// Explicit net ids to terminate. The client computes the set (typically
    /// every active net in the current view) so the operation is auditable and
    /// the server never guesses intent.
    pub net_ids: Vec<String>,
    /// Opt-in to also kill INFRASTRUCTURE nets (`pool-*`, `staging-*`, …). When
    /// false (default), any non-`mekhan-` id in `net_ids` is SKIPPED, not
    /// killed — the safe default that stops a "kill all" from cancelling the
    /// pool/staging/materialize nets that keep the platform running.
    #[serde(default)]
    pub include_infrastructure: bool,
}

/// One net that a bulk op could not act on, with the reason.
#[derive(Debug, Serialize, ToSchema)]
pub struct BulkFailure {
    pub net_id: String,
    pub error: String,
}

/// Result of a bulk kill.
#[derive(Debug, Serialize, ToSchema)]
pub struct BulkKillResponse {
    /// Nets successfully terminated (engine accepted the delete).
    pub killed: Vec<String>,
    /// Infrastructure nets skipped because `include_infrastructure` was false.
    pub skipped_infrastructure: Vec<String>,
    /// Nets the engine failed to terminate (per-net errors don't abort the run).
    pub failed: Vec<BulkFailure>,
}

/// Result of a terminal-net purge sweep.
#[derive(Debug, Serialize, ToSchema)]
pub struct PurgeTerminalResponse {
    /// How many terminal nets had their subjects purged.
    pub nets_purged: u64,
    /// Total messages removed from PETRI_GLOBAL across all purged nets.
    pub total_messages: u64,
    /// Nets the sweep failed to purge (per-net errors don't abort the run).
    pub failed: Vec<BulkFailure>,
}

/// `POST /api/v1/admin/nets/bulk-kill` — kill many nets at once (admin).
///
/// Terminates each net in `net_ids` via the engine's terminate-with-cleanup,
/// EXCEPT infrastructure (`non-mekhan-`) nets unless `include_infrastructure`
/// is set — those are reported under `skipped_infrastructure`. Per-net failures
/// are collected, not fatal, so one unreachable net doesn't abort the batch.
#[utoipa::path(
    post,
    path = "/api/v1/admin/nets/bulk-kill",
    request_body = BulkKillRequest,
    responses(
        (status = 200, description = "Per-net kill outcome", body = BulkKillResponse),
        (status = 400, description = "Invalid net id in the batch", body = ErrorResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
    ),
    tag = "admin-nets",
)]
pub async fn bulk_kill_nets(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<BulkKillRequest>,
) -> Result<Json<BulkKillResponse>, ApiError> {
    require_role(&state.db, &user, caller_workspace(&user)?, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    // Validate every id up front so a malformed batch fails atomically (before
    // we kill anything) rather than half-applying.
    for net_id in &req.net_ids {
        validate_net_id(net_id)?;
    }

    let mut killed = Vec::new();
    let mut skipped_infrastructure = Vec::new();
    let mut failed = Vec::new();

    for net_id in req.net_ids {
        if !is_workflow_instance_net(&net_id) && !req.include_infrastructure {
            skipped_infrastructure.push(net_id);
            continue;
        }
        match state.petri.delete_net(&net_id).await {
            Ok(()) => killed.push(net_id),
            Err(e) => failed.push(BulkFailure {
                net_id,
                error: e.to_string(),
            }),
        }
    }

    tracing::info!(
        killed = killed.len(),
        skipped = skipped_infrastructure.len(),
        failed = failed.len(),
        include_infrastructure = req.include_infrastructure,
        by = %user.subject,
        "admin bulk kill-switch"
    );
    Ok(Json(BulkKillResponse {
        killed,
        skipped_infrastructure,
        failed,
    }))
}

/// `POST /api/v1/admin/nets/purge-terminal` — sweep every terminal net's
/// events from PETRI_GLOBAL (admin).
///
/// Server-driven: snapshots the engine metadata, purges the event+signal
/// subjects of every net whose status is NOT active (running/created), and
/// reports the totals. Active nets are never touched — their log is
/// authoritative state. The single-net `purge-events` endpoint remains for
/// orphan event-streams whose net metadata is already gone (not enumerated
/// here, since they have no metadata row).
#[utoipa::path(
    post,
    path = "/api/v1/admin/nets/purge-terminal",
    responses(
        (status = 200, description = "Terminal nets swept", body = PurgeTerminalResponse),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 502, description = "Engine unreachable", body = ErrorResponse),
    ),
    tag = "admin-nets",
)]
pub async fn purge_terminal_nets(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<PurgeTerminalResponse>, ApiError> {
    require_role(&state.db, &user, caller_workspace(&user)?, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    let metas = fetch_engine_metas(&state).await?;
    let stream = state
        .nats
        .jetstream()
        .get_stream(Subjects::STREAM_GLOBAL)
        .await
        .map_err(|e| ApiError::internal(format!("PETRI_GLOBAL: {e}")))?;

    let mut nets_purged: u64 = 0;
    let mut total_messages: u64 = 0;
    let mut failed = Vec::new();

    for meta in &metas {
        if is_active_status(&status_str(&meta.status)) {
            continue;
        }
        // Defensive: a metadata id should already be subject-safe, but the
        // purge path interpolates it into a NATS filter — never skip the check.
        if validate_net_id(&meta.net_id).is_err() {
            continue;
        }
        match purge_net_subjects(&stream, &meta.net_id).await {
            Ok(n) => {
                nets_purged += 1;
                total_messages += n;
            }
            Err(e) => failed.push(BulkFailure {
                net_id: meta.net_id.clone(),
                error: e,
            }),
        }
    }

    tracing::info!(
        nets_purged,
        total_messages,
        failed = failed.len(),
        by = %user.subject,
        "admin cleanup: terminal-net sweep"
    );
    Ok(Json(PurgeTerminalResponse {
        nets_purged,
        total_messages,
        failed,
    }))
}
