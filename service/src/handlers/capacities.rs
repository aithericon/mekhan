//! Server-side capacity aggregator (docs/23 + docs/24): the ONE read that
//! powers the whole Control-Plane "capacities" surface.
//!
//! `GET /api/v1/capacities` lists every `capacity` AND `datacenter` resource in
//! the caller's workspace, classified by the SINGLE dispatch authority
//! ([`crate::models::capacity::CapacityAxes::backend`]) — never a re-derived
//! kind switch — and enriched with live utilization read from the same sources
//! the per-backend pages already use:
//!
//! - **Tokens** (seeded concurrency limit) — `seeded` is the `Fixed(n)` count
//!   from the resource's `public_config`; `in_use` + `holders` come from the
//!   `allocations` projection's `concurrency_limit_grant` rows on the pool net
//!   `pool-<resource_id>` (held ⇔ `released_at IS NULL`).
//! - **Presence** (instrument / runner group) — `total` is the count of runners
//!   whose `runner_group` aliases this capacity's `path`; `online` + `backends`
//!   are read from the in-memory presence snapshot on [`AppState`].
//! - **Queue** (worker pool) — now PER-GROUP: the enrolled workers whose
//!   `worker_group` aliases this capacity's `path`, intersected with the
//!   fleet-liveness worker facet for online count + advertised backends. NOT a
//!   fleet-global view.
//! - **Scheduler** (datacenter) — the live cluster summary for that datacenter,
//!   assembled by the shared [`crate::handlers::clusters::assemble_cluster_summaries`]
//!   so the cluster page and this aggregator cannot drift.
//! - **Deferred** (the `consume` quota path, no net yet) — [`CapacityLive::None`].
//!
//! Read-only and fail-soft: if a live source (engine, presence map) is
//! unreachable it degrades to zeros rather than failing the whole list — it must
//! never block a grant.

use std::collections::HashMap;

use axum::{extract::State, Json};
use serde::Serialize;
use serde_json::{Map, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::compiler::well_known;
use crate::handlers::clusters::assemble_cluster_summaries;
use crate::models::capacity::{axes_for_resource, CapacityAxes, CapacityBackend};
use crate::models::error::ApiError;
use crate::AppState;

/// One holder of a live token grant, best-effort decoded from an `allocations`
/// row. `instance_id` is the owning workflow instance (NULL for pool-management
/// grants); `since` is the RFC3339 `acquired_at`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GrantHolder {
    /// Owning workflow instance UUID (string), when the grant resolved one.
    pub instance_id: Option<String>,
    /// RFC3339 acquisition timestamp, when recorded.
    pub since: Option<String>,
}

/// Live utilization for one capacity, tagged by its backend. The shape mirrors
/// the backend so the UI renders the right gauge without a second lookup.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapacityLive {
    /// Seeded token pool (concurrency limit): `seeded` fixed units, `in_use`
    /// currently held, with the per-holder ledger.
    Tokens {
        seeded: u32,
        in_use: u32,
        holders: Vec<GrantHolder>,
    },
    /// Presence-driven pool (instrument / runner group): `online` present
    /// runners out of `total` enrolled in this group, plus the union of their
    /// advertised backends.
    Presence {
        online: u32,
        total: u32,
        backends: Vec<String>,
    },
    /// Per-group worker queue: `online` present workers of `enrolled` total in
    /// this group, plus the union of their advertised backends.
    Queue {
        online: u32,
        enrolled: u32,
        backends: Vec<String>,
    },
    /// Lease/scheduler (datacenter): live cluster state from the registry.
    Scheduler {
        flavor: String,
        watcher_state: String,
        active_leases: i64,
        success_rate: Option<f64>,
        draining: bool,
    },
    /// Deferred (`consume` quota) or any capacity with no live source.
    None,
}

/// One capacity resource, classified + live. The unified Control-Plane row.
#[derive(Debug, Serialize, ToSchema)]
pub struct CapacitySummary {
    /// Resource UUID.
    pub id: Uuid,
    /// Snake_case `path` (the alias steps + runners/workers bind to).
    pub path: String,
    /// Human display name.
    pub display_name: String,
    /// The dispatch target, from the SINGLE authority `CapacityAxes::backend()`.
    pub backend: CapacityBackend,
    /// The resolved trait-space axes (`None` for a `capacity` whose
    /// `public_config` doesn't parse — it still lists, with `backend` defaulted).
    pub axes: Option<CapacityAxes>,
    /// Live utilization, tagged by backend.
    pub live: CapacityLive,
}

/// `GET /api/v1/capacities` — every `capacity` + `datacenter` resource in the
/// workspace, classified by backend with live utilization. One read powers the
/// whole Control-Plane capacity surface.
#[utoipa::path(
    get,
    path = "/api/v1/capacities",
    responses(
        (status = 200, description = "Workspace capacities, classified + live", body = Vec<CapacitySummary>),
    ),
    tag = "capacities",
)]
pub async fn list_capacities(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<CapacitySummary>>, ApiError> {
    let workspace_id = user.workspace_id.unwrap_or_else(Uuid::nil);

    // The pool resources: `capacity` (parses its axes) + `datacenter` (locked
    // lease axes). Join the latest version's public_config in one round-trip.
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, Value)>(
        "SELECT r.id, r.path, r.display_name, r.resource_type, \
                COALESCE(rv.public_config, '{}'::jsonb) AS public_config \
         FROM resources r \
         JOIN resource_versions rv \
           ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.workspace_id = $1 \
           AND r.resource_type IN ('capacity', 'datacenter') \
           AND r.deleted_at IS NULL \
         ORDER BY r.path",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("capacity resource lookup: {e}")))?;

    // Live cluster state for the scheduler (datacenter) backend, keyed by
    // resource_id string. Assembled by the SHARED cluster assembler so this
    // aggregator and the cluster page never drift; fail-soft to an empty map.
    let clusters: HashMap<String, crate::handlers::clusters::ClusterSummary> =
        assemble_cluster_summaries(&state, workspace_id)
            .await
            .map(|cs| cs.into_iter().map(|c| (c.resource_id.clone(), c)).collect())
            .unwrap_or_default();

    // Presence snapshot (online runners + their backends), filtered later per
    // group. Fail-soft: the snapshot read is in-memory and infallible.
    let presence = state.runner_presence.snapshot().await;

    let mut out = Vec::with_capacity(rows.len());
    for (id, path, display_name, resource_type, public_config) in rows {
        let public_map: Map<String, Value> = public_config
            .as_object()
            .cloned()
            .unwrap_or_default();

        let axes = axes_for_resource(&resource_type, &public_map);
        // A `capacity` whose config doesn't parse still lists; default its
        // backend to Deferred (no live source) rather than dropping the row.
        let backend = axes
            .map(|a| a.backend())
            .unwrap_or(CapacityBackend::Deferred);

        let live = match backend {
            CapacityBackend::Tokens => {
                tokens_live(&state, id, axes).await
            }
            CapacityBackend::Presence => presence_live(&state, workspace_id, &path, &presence).await,
            CapacityBackend::Queue => queue_live(&state, workspace_id, &path).await,
            CapacityBackend::Scheduler => {
                scheduler_live(clusters.get(&id.to_string()))
            }
            CapacityBackend::Deferred => CapacityLive::None,
        };

        out.push(CapacitySummary {
            id,
            path,
            display_name,
            backend,
            axes,
            live,
        });
    }

    Ok(Json(out))
}

/// Live token-pool utilization: `seeded` from the resource's `Fixed(n)` axis,
/// `in_use` + `holders` from the `allocations` projection's
/// `concurrency_limit_grant` rows on `pool-<resource_id>` that are still held
/// (`released_at IS NULL`). Fail-soft: a query error yields an empty ledger.
async fn tokens_live(state: &AppState, id: Uuid, axes: Option<CapacityAxes>) -> CapacityLive {
    use crate::models::capacity::CapacityAmount;

    let seeded = match axes.map(|a| a.capacity_amount) {
        Some(CapacityAmount::Fixed(n)) => n,
        _ => 0,
    };

    let net_id = well_known::pool_net_id(id);
    let held: Vec<(Option<Uuid>, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT instance_id, acquired_at FROM allocations \
         WHERE net_id = $1 AND kind = 'concurrency_limit_grant' \
           AND released_at IS NULL \
         ORDER BY acquired_at DESC NULLS LAST",
    )
    .bind(&net_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let holders: Vec<GrantHolder> = held
        .into_iter()
        .map(|(instance_id, acquired_at)| GrantHolder {
            instance_id: instance_id.map(|i| i.to_string()),
            since: acquired_at.map(|t| t.to_rfc3339()),
        })
        .collect();

    CapacityLive::Tokens {
        seeded,
        in_use: holders.len() as u32,
        holders,
    }
}

/// Live presence-pool utilization: `total` is the count of runners whose
/// `runner_group` aliases this capacity's `path`; `online` + `backends` come
/// from the in-memory presence snapshot (filtered to those runners). Fail-soft:
/// a DB error on the total yields 0.
async fn presence_live(
    state: &AppState,
    workspace_id: Uuid,
    path: &str,
    presence: &[crate::models::runner::RunnerPresenceSnapshot],
) -> CapacityLive {
    // Enrolled runners in this group (the `runner_group` column aliases the
    // capacity's `path`). This is the SAME alias the presence controller +
    // enroll gate key on, so the "total" here matches the live admission set.
    let group_runners: Vec<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM runners \
         WHERE workspace_id = $1 AND runner_group = $2 AND revoked_at IS NULL",
    )
    .bind(workspace_id)
    .bind(path)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let group_set: std::collections::HashSet<Uuid> = group_runners.iter().copied().collect();
    let total = group_set.len() as u32;

    let mut online = 0u32;
    let mut backends: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for s in presence.iter().filter(|s| group_set.contains(&s.runner_id)) {
        if s.present {
            online += 1;
            for b in &s.backends {
                backends.insert(b.clone());
            }
        }
    }

    CapacityLive::Presence {
        online,
        total,
        backends: backends.into_iter().collect(),
    }
}

/// Live per-group worker-queue state: `enrolled` is the count of workers whose
/// `worker_group` aliases this capacity's `path`; `online` + `backends` come from
/// the fleet-liveness worker snapshot (filtered to those workers). Mirrors
/// [`presence_live`]. Fail-soft: a DB error on the enrolled set yields 0.
async fn queue_live(state: &AppState, workspace_id: Uuid, path: &str) -> CapacityLive {
    // Enrolled workers in this group (the `worker_group` column aliases the
    // capacity's `path`). Same alias the worker enroll/dispatch path keys on.
    let ids: Vec<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM workers \
         WHERE workspace_id = $1 AND worker_group = $2 AND revoked_at IS NULL",
    )
    .bind(workspace_id)
    .bind(path)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let enrolled = ids.len() as u32;
    let enrolled_set: std::collections::HashSet<String> =
        ids.iter().map(|id| id.to_string()).collect();

    let mut online = 0u32;
    let mut backends: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for entry in state
        .fleet
        .snapshot()
        .await
        .into_iter()
        .filter(|e| matches!(e.kind, crate::fleet::CapacityKind::Worker))
        .filter(|e| enrolled_set.contains(&e.id))
    {
        online += 1;
        for b in entry.caps {
            backends.insert(b);
        }
    }

    CapacityLive::Queue {
        online,
        enrolled,
        backends: backends.into_iter().collect(),
    }
}

/// Scheduler (datacenter) live state from the shared cluster summary. `None` →
/// idle defaults (no live client resident).
fn scheduler_live(cluster: Option<&crate::handlers::clusters::ClusterSummary>) -> CapacityLive {
    match cluster {
        Some(c) => CapacityLive::Scheduler {
            flavor: c.flavor.clone(),
            watcher_state: c.watcher_state.clone(),
            active_leases: c.active_lease_count,
            // The cluster summary doesn't carry a windowed success rate; the
            // metrics endpoint owns that. Left None here (the aggregator is a
            // liveness rollup, not the accounting view).
            success_rate: None,
            draining: c.draining,
        },
        None => CapacityLive::Scheduler {
            flavor: String::new(),
            watcher_state: "idle".to_string(),
            active_leases: 0,
            success_rate: None,
            draining: false,
        },
    }
}
