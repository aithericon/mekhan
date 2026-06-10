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
use crate::AppState;

/// Caller-implicit workspace — mirrors `roster::caller_workspace`.
fn caller_workspace(user: &AuthUser) -> Uuid {
    user.workspace_id.unwrap_or_else(Uuid::nil)
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
    let stream = match state.nats.jetstream().get_stream("PETRI_GLOBAL").await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("admin-nets: PETRI_GLOBAL not available: {e}");
            return None;
        }
    };
    let mut info = match stream.info_with_subjects("petri.events.>").await {
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
    require_role(&state.db, &user, caller_workspace(&user), Role::Admin)
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
    require_role(&state.db, &user, caller_workspace(&user), Role::Admin)
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
    require_role(&state.db, &user, caller_workspace(&user), Role::Admin)
        .await
        .map_err(map_to_api_error)?;
    validate_net_id(&net_id)?;

    // Active-net guard. The engine is authoritative for liveness; only its
    // metadata can say "safe to purge". Engine-unreachable → refuse (fail
    // closed): purging blind could destroy a running net's event log.
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
    if let Some(meta) = metas.iter().find(|m| m.net_id == net_id) {
        let status = match &meta.status {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string().trim_matches('"').to_string(),
        };
        if status == "running" || status == "created" {
            return Err(ApiError::conflict(format!(
                "net {net_id} is {status} — terminate it before purging its events"
            )));
        }
    }

    let stream = state
        .nats
        .jetstream()
        .get_stream("PETRI_GLOBAL")
        .await
        .map_err(|e| ApiError::internal(format!("PETRI_GLOBAL: {e}")))?;

    let mut purged: u64 = 0;
    for subject in [
        format!("petri.events.{net_id}.>"),
        format!("petri.signal.{net_id}.>"),
    ] {
        let resp = stream
            .purge()
            .filter(&subject)
            .await
            .map_err(|e| ApiError::internal(format!("purge {subject}: {e}")))?;
        purged += resp.purged;
    }

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
