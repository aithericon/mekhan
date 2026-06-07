//! Model-pool P4 — the replica autoscaler control loop (docs/29 §6').
//!
//! A mekhan background loop (cloning the presence-sweep spawn shape,
//! `runners_presence::start_presence_sweep`) that reconciles ONE `model_replicas`
//! row per `model_policy` resource. Each tick, per policy:
//!
//! 1. Load the `model_policy` config + resolve its datacenter alias → resource uuid.
//! 2. OBSERVE the live replica count from the FLEET ROSTER (live runners
//!    advertising the model_id — the SAME [`crate::handlers::model_pool::serving_runner_counts`]
//!    the loaded-set picker uses; NOT the staging effect result, which only proves
//!    "registered", not "serving").
//! 3. Compute the desired COUNT ([`crate::models::model_replicas::compute_target`])
//!    — manual reads the row's `desired_count` (seeded from the policy), the L2
//!    reactive modes read demand. Gate on a durable cooldown
//!    ([`crate::models::model_replicas::in_cooldown`]) anchored on `last_actuated_at`.
//! 4. If desired ≠ observed and outside cooldown, ACTUATE via
//!    [`actuate::actuate_replica`] (a generated one-shot net firing `stage_template`
//!    for a Nomad `service` job at the desired Count, residency-pinned).
//! 5. Upsert the row (the durable reconciliation target + Control-Plane read).
//!
//! **L1 (manual)** constructs the loop with `demand = None`: only `manual`-mode
//! policies actuate; `scale_to_zero`/`keep_warm` are HARD-BLOCKED on the Router
//! `/metrics` (L2 — see [`demand`]). Fail-soft per-policy: one bad policy never
//! kills the loop. Inference NEVER touches the engine net or the presence net —
//! the loop only provisions replicas via the staging plane.

pub mod actuate;
pub mod demand;
pub mod node_actuate;
pub mod observe;
pub mod placement;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use aithericon_resources::types::NodePoolPolicy;

use crate::fleet::FleetLiveness;
use crate::models::model_replicas::in_cooldown;
use crate::models::node_replicas::{compute_node_target, NodeReplicaRow};
use crate::nats::MekhanNats;
use crate::petri::client::PetriClient;
use crate::runners_presence::RunnerPresence;

use self::demand::DemandSource;
use self::observe::pool_serving_capacity;

/// Reconcile cadence. Short enough that a manual scale POST takes effect quickly;
/// long enough that a stuck datacenter doesn't hammer the engine.
const RECONCILE_INTERVAL_SECS: u64 = 15;

/// Spawn the autoscaler control loop. Called from `main.rs` after the presence +
/// worker-liveness controllers. `demand` is `None` for L1 (manual mode only);
/// L2 passes a [`DemandSource`] scraping the Router `/metrics`.
pub fn spawn_autoscaler(
    db: PgPool,
    petri: PetriClient,
    nats: MekhanNats,
    runner_presence: RunnerPresence,
    fleet: FleetLiveness,
    demand: Option<Arc<dyn DemandSource>>,
) {
    tokio::spawn(run_autoscaler(
        db,
        petri,
        nats,
        runner_presence,
        fleet,
        demand,
    ));
}

async fn run_autoscaler(
    db: PgPool,
    petri: PetriClient,
    nats: MekhanNats,
    runner_presence: RunnerPresence,
    fleet: FleetLiveness,
    demand: Option<Arc<dyn DemandSource>>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(RECONCILE_INTERVAL_SECS));
    tracing::info!(
        interval_secs = RECONCILE_INTERVAL_SECS,
        mode = if demand.is_some() {
            "reactive(L2)"
        } else {
            "manual(L1)"
        },
        "model autoscaler started"
    );

    loop {
        tick.tick().await;
        if let Err(e) =
            reconcile_once(&db, &petri, &nats, &runner_presence, &fleet, demand.as_deref()).await
        {
            tracing::warn!("autoscaler reconcile tick failed: {e}");
        }
    }
}

/// One reconcile pass. Loop 1 (node-fleet capacity, docs/31 Phase 2) runs FIRST so
/// the node-provision demand it raises is observable by Loop 2; then Loop 2 (model
/// placement, docs/31 Phase 3) walks the cheapest-first cascade — adapter-load →
/// sleep/wake → raise-node-demand → dedicated-job-fallback — per `model_policy`
/// with demand. Both passes fail-soft as a whole (one pass's error never kills the
/// other); only a setup `sqlx::Error` kills the tick.
async fn reconcile_once(
    db: &PgPool,
    petri: &PetriClient,
    nats: &MekhanNats,
    runner_presence: &RunnerPresence,
    fleet: &FleetLiveness,
    demand: Option<&dyn DemandSource>,
) -> Result<(), sqlx::Error> {
    // LOOP 1 — node-fleet capacity scaler. Fail-soft as a whole: a node-pool setup
    // error must NOT kill the placement pass below.
    if let Err(e) = reconcile_node_pools(db, petri, runner_presence, fleet, demand).await {
        tracing::warn!("node-pool reconcile pass failed (placement pass continues): {e}");
    }

    // LOOP 2 — model placement (the keystone). DEMOTES the per-model Nomad job to
    // the `dedicated=true` fallback; the default is now packing onto the node fleet
    // via the load/unload publisher.
    if let Err(e) = placement::reconcile_placement(db, petri, nats, runner_presence, demand).await {
        tracing::warn!("placement reconcile pass failed: {e}");
    }

    Ok(())
}

// ── Loop 1: node-fleet capacity scaler (docs/31 Phase 2) ─────────────────────

/// One reconcile pass over every `node_pool` capacity resource in every workspace
/// (docs/31 Loop 1). For each pool:
///
/// 1. Resolve the datacenter alias → resource uuid.
/// 2. OBSERVE the live C-weighted capacity ([`pool_serving_capacity`], DERIVED-B):
///    `(observed_nodes, observed_slots)` over present pool-tagged runners.
/// 3. Compute the DESIRED node count: the aggregate model demand routed to this
///    pool, converted to C-units (`ceil(Σ demand / max_num_seqs)`) and clamped
///    `[min_nodes, max_nodes]` ([`compute_node_target`]). Gate on the durable
///    cooldown anchored on `last_actuated_at`.
/// 4. If desired ≠ observed_nodes and outside cooldown, ACTUATE via
///    [`node_actuate::actuate_node_pool`] (a generic engine fleet, NO `model_id`).
/// 5. Upsert the `node_replicas` row (durable target + Control-Plane read).
///
/// Per-pool failures fail-soft (recorded on the row + carry on). The aggregate
/// model demand is summed from `model_policy` rows whose `node_pool` alias matches
/// the pool's `resources.path`; on L1 (`demand = None`) the per-model demand is
/// `None` and the pool scales purely off `min_nodes`.
async fn reconcile_node_pools(
    db: &PgPool,
    petri: &PetriClient,
    runner_presence: &RunnerPresence,
    fleet: &FleetLiveness,
    demand: Option<&dyn DemandSource>,
) -> Result<(), sqlx::Error> {
    // (pool_resource_id, workspace_id, alias_path, public_config) for every pool.
    let pools: Vec<(Uuid, Uuid, String, Value)> = sqlx::query_as(
        "SELECT r.id, r.workspace_id, r.path, rv.public_config \
         FROM resources r \
         JOIN resource_versions rv ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.resource_type = 'node_pool' AND r.deleted_at IS NULL",
    )
    .fetch_all(db)
    .await?;

    // Cache the per-workspace pool→aggregate-demand map so N pools in one workspace
    // scan the model_policy set + scrape demand once.
    let mut demand_by_ws: HashMap<Uuid, HashMap<String, f64>> = HashMap::new();

    for (pool_id, workspace_id, alias_path, public_config) in pools {
        let pool: NodePoolPolicy = match serde_json::from_value(public_config) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(%pool_id, "skipping unparseable node_pool: {e}");
                continue;
            }
        };

        // Aggregate model demand (C-units) routed to each pool in this workspace,
        // computed once per tick.
        let demand_map = match demand_by_ws.entry(workspace_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let m = aggregate_pool_demand(db, workspace_id, demand).await;
                e.insert(m)
            }
        };
        let pool_demand = demand_map.get(&alias_path).copied();

        if let Err(e) = reconcile_node_pool(
            db,
            petri,
            runner_presence,
            fleet,
            workspace_id,
            pool_id,
            &alias_path,
            &pool,
            pool_demand,
        )
        .await
        {
            tracing::warn!(%pool_id, alias = %alias_path, "node-pool reconcile failed: {e}");
            let _ = mark_node_failed(db, pool_id, workspace_id, &pool, &e).await;
        }
    }
    Ok(())
}

/// A `model_states` row's folded-in autoscale-policy columns. The autoscale policy
/// stopped being a resource; it now lives on the model SET. This is the per-row
/// projection the autoscaler loops read, converted to the in-memory
/// [`ModelAutoscalePolicy`] DTO via [`ModelStatePolicyRow::into_policy`]. The
/// `WHERE autoscale_mode IS NOT NULL AND node_pool IS NOT NULL` filter guarantees
/// both are `Some`.
#[derive(sqlx::FromRow)]
pub(crate) struct ModelStatePolicyRow {
    pub model_id: String,
    pub base: Option<String>,
    pub autoscale_mode: Option<String>,
    pub desired_replicas: Option<i32>,
    pub scale_up_threshold: Option<f64>,
    pub scale_down_threshold: Option<f64>,
    pub cooldown_secs: Option<i64>,
    pub node_pool: Option<String>,
    pub residency_zone: Option<String>,
    pub dedicated: bool,
}

impl ModelStatePolicyRow {
    /// Build the in-memory [`ModelAutoscalePolicy`] DTO from the row. `mode` /
    /// `node_pool` are guaranteed `Some` by the load filter; defensively default
    /// them rather than panic.
    pub(crate) fn into_policy(self) -> aithericon_resources::types::ModelAutoscalePolicy {
        aithericon_resources::types::ModelAutoscalePolicy {
            model_id: self.model_id,
            residency_zone: self.residency_zone.unwrap_or_default(),
            mode: self.autoscale_mode.unwrap_or_default(),
            desired_replicas: self.desired_replicas.map(|v| v as u32),
            scale_up_threshold: self.scale_up_threshold,
            scale_down_threshold: self.scale_down_threshold,
            cooldown_secs: self.cooldown_secs.map(|v| v as u64),
            node_pool: self.node_pool.unwrap_or_default(),
            base: self.base,
            dedicated: Some(self.dedicated),
        }
    }
}

/// Sum the per-model demand of every model with a folded-in autoscale policy in
/// `workspace_id`, bucketed by
/// the pool alias each policy draws from (`node_pool`). On L1 (`demand = None`) the
/// per-model demand is `None` → contributes 0, so the bucket holds `Σ 0 = 0.0` and
/// the pool floors at `min_nodes`. A policy whose pool alias is empty is skipped.
///
/// The demand is the RAW per-model in-flight signal (the same one the model-policy
/// pass uses); Loop 1 converts the SUM to a node count later via
/// `ceil(Σ demand / max_num_seqs)`.
async fn aggregate_pool_demand(
    db: &PgPool,
    workspace_id: Uuid,
    demand: Option<&dyn DemandSource>,
) -> HashMap<String, f64> {
    // The autoscale policy is folded onto `model_states`: a model has a policy iff
    // `autoscale_mode IS NOT NULL AND node_pool IS NOT NULL`.
    let policies: Vec<ModelStatePolicyRow> = match sqlx::query_as(
        "SELECT model_id, base, autoscale_mode, desired_replicas, scale_up_threshold, \
                scale_down_threshold, cooldown_secs, node_pool, residency_zone, dedicated \
         FROM model_states \
         WHERE workspace_id = $1 AND autoscale_mode IS NOT NULL AND node_pool IS NOT NULL",
    )
    .bind(workspace_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(%workspace_id, "aggregate_pool_demand: model_states policy load failed: {e}");
            return HashMap::new();
        }
    };

    let mut buckets: HashMap<String, f64> = HashMap::new();
    for r in policies {
        let policy = r.into_policy();
        if policy.node_pool.trim().is_empty() {
            continue;
        }
        let d = match demand {
            Some(src) => src.demand_for(&policy.model_id).await.unwrap_or(0.0),
            None => 0.0,
        };
        *buckets.entry(policy.node_pool.clone()).or_insert(0.0) += d;
    }
    buckets
}

/// Reconcile ONE `node_pool` to its desired node Count.
#[allow(clippy::too_many_arguments)]
async fn reconcile_node_pool(
    db: &PgPool,
    petri: &PetriClient,
    runner_presence: &RunnerPresence,
    fleet: &FleetLiveness,
    workspace_id: Uuid,
    pool_id: Uuid,
    alias_path: &str,
    pool: &NodePoolPolicy,
    pool_demand: Option<f64>,
) -> Result<(), String> {
    // Resolve the datacenter ALIAS (pool config) → resource uuid.
    let dc_uuid: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM resources \
         WHERE workspace_id = $1 AND resource_type = 'datacenter' AND path = $2 AND deleted_at IS NULL",
    )
    .bind(workspace_id)
    .bind(&pool.datacenter_resource_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("resolve datacenter alias: {e}"))?;
    let dc_uuid = dc_uuid
        .map(|(id,)| id)
        .ok_or_else(|| format!("datacenter alias '{}' not found", pool.datacenter_resource_id))?;

    // Existing row (the durable reconciliation state).
    let existing: Option<NodeReplicaRow> =
        sqlx::query_as("SELECT * FROM node_replicas WHERE pool_resource_id = $1")
            .bind(pool_id)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("load node_replicas row: {e}"))?;

    let now = Utc::now();

    // OBSERVED: the C-weighted capacity from FleetLiveness (DERIVED-B). The scaler
    // tracks node Count against `observed_nodes`; `observed_slots` is recorded as
    // the live capacity for the Control-Plane read.
    let obs = pool_serving_capacity(fleet, runner_presence, alias_path).await;

    // DESIRED node count: aggregate demand (C-units) / per-node C, ceil'd + clamped.
    // `max_num_seqs == 0` would divide-by-zero — treat as "no C declared" → the
    // pool can only floor at `min_nodes` (demand contributes nothing).
    let demand_nodes = match pool_demand {
        Some(d) if pool.max_num_seqs > 0 => Some(d / pool.max_num_seqs as f64),
        // No demand signal (or no declared C): fall back to the `min_nodes` floor
        // by passing demand 0.0 (clamp lifts it to min_nodes).
        _ => Some(0.0),
    };
    let target = compute_node_target(pool, demand_nodes, None);

    let prev_actuated = existing.as_ref().and_then(|r| r.last_actuated_at);
    let prev_desired = existing.as_ref().map(|r| r.desired_nodes.max(0) as u32);
    let cooled = in_cooldown(prev_actuated, pool.cooldown_secs, now);

    struct NodeDecision {
        desired: u32,
        status: &'static str,
        actuate: bool,
        last_actuated_at: Option<DateTime<Utc>>,
    }
    use crate::models::node_replicas::status as nstatus;

    let decision = match target {
        None => {
            if existing.is_none() {
                return Ok(());
            }
            NodeDecision {
                desired: prev_desired.unwrap_or(0),
                status: existing
                    .as_ref()
                    .map(|r| leak_node_status(&r.status))
                    .unwrap_or(nstatus::STOPPED),
                actuate: false,
                last_actuated_at: prev_actuated,
            }
        }
        Some(t) if t == obs.nodes => {
            // Steady: no actuation, refresh observed + settle status.
            NodeDecision {
                desired: t,
                status: if t == 0 {
                    nstatus::STOPPED
                } else {
                    nstatus::ACTIVE
                },
                actuate: false,
                last_actuated_at: prev_actuated,
            }
        }
        Some(t) if cooled => {
            // Want to change but inside the cooldown window: defer; refresh observed.
            NodeDecision {
                desired: prev_desired.unwrap_or(t),
                status: existing
                    .as_ref()
                    .map(|r| leak_node_status(&r.status))
                    .unwrap_or(nstatus::PROVISIONING),
                actuate: false,
                last_actuated_at: prev_actuated,
            }
        }
        Some(t) => {
            let (st, actuate) = if t == 0 {
                if obs.nodes == 0 {
                    (nstatus::STOPPED, false)
                } else {
                    (nstatus::DRAINING, true)
                }
            } else if obs.nodes >= 1 {
                (nstatus::SCALING, true)
            } else {
                (nstatus::PROVISIONING, true)
            };
            NodeDecision {
                desired: t,
                status: st,
                actuate,
                last_actuated_at: if actuate { Some(now) } else { prev_actuated },
            }
        }
    };

    let row = upsert_node_row(
        db,
        workspace_id,
        pool_id,
        pool,
        dc_uuid,
        decision.desired,
        obs.nodes,
        obs.slots,
        decision.status,
        decision.last_actuated_at,
    )
    .await
    .map_err(|e| format!("upsert node_replicas row: {e}"))?;

    if decision.actuate {
        // Generation idiom (verbatim from the model pass): a fresh net id per
        // actuation re-seeds + re-fires `t_stage` (the `e16db353` fix).
        let generation = decision.last_actuated_at.unwrap_or(now).timestamp_millis();
        let prev_generation = prev_actuated.map(|t| t.timestamp_millis());
        match node_actuate::actuate_node_pool(
            db,
            petri,
            workspace_id,
            row.id,
            generation,
            prev_generation,
            pool,
            dc_uuid,
            decision.desired,
        )
        .await
        {
            Ok(slug) => {
                sqlx::query(
                    "UPDATE node_replicas SET node_slug = $2, last_error = NULL, updated_at = NOW() \
                     WHERE id = $1",
                )
                .bind(row.id)
                .bind(&slug)
                .execute(db)
                .await
                .map_err(|e| format!("record node slug: {e}"))?;
            }
            Err(e) => {
                sqlx::query(
                    "UPDATE node_replicas SET status = 'failed', last_error = $2, updated_at = NOW() \
                     WHERE id = $1",
                )
                .bind(row.id)
                .bind(e.to_string())
                .execute(db)
                .await
                .map_err(|e| format!("record node actuation failure: {e}"))?;
                tracing::warn!(%pool_id, "node-pool actuation failed: {e}");
            }
        }
    }

    Ok(())
}

/// Map a stored node-pool status string back onto the `'static` set (defensive).
fn leak_node_status(s: &str) -> &'static str {
    use crate::models::node_replicas::status as nstatus;
    match s {
        nstatus::PROVISIONING => nstatus::PROVISIONING,
        nstatus::ACTIVE => nstatus::ACTIVE,
        nstatus::SCALING => nstatus::SCALING,
        nstatus::DRAINING => nstatus::DRAINING,
        nstatus::STOPPED => nstatus::STOPPED,
        nstatus::FAILED => nstatus::FAILED,
        _ => nstatus::PROVISIONING,
    }
}

/// Upsert a `node_replicas` row to the intended state. Writes `desired_nodes` +
/// the FleetLiveness-driven `observed_nodes`/`observed_slots` every tick (DERIVED-B
/// — the projector never touches observed).
#[allow(clippy::too_many_arguments)]
async fn upsert_node_row(
    db: &PgPool,
    workspace_id: Uuid,
    pool_id: Uuid,
    pool: &NodePoolPolicy,
    dc_uuid: Uuid,
    desired: u32,
    observed_nodes: u32,
    observed_slots: u32,
    status: &str,
    last_actuated_at: Option<DateTime<Utc>>,
) -> Result<NodeReplicaRow, sqlx::Error> {
    let residency = (!pool.residency_zone.trim().is_empty()).then(|| pool.residency_zone.clone());
    sqlx::query_as::<_, NodeReplicaRow>(
        "INSERT INTO node_replicas \
            (workspace_id, pool_resource_id, datacenter_resource_id, \
             desired_nodes, observed_nodes, observed_slots, status, residency_zone, last_actuated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (pool_resource_id) DO UPDATE SET \
            datacenter_resource_id = EXCLUDED.datacenter_resource_id, \
            desired_nodes = EXCLUDED.desired_nodes, \
            observed_nodes = EXCLUDED.observed_nodes, \
            observed_slots = EXCLUDED.observed_slots, \
            status = EXCLUDED.status, \
            residency_zone = EXCLUDED.residency_zone, \
            last_actuated_at = EXCLUDED.last_actuated_at, \
            updated_at = NOW() \
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(pool_id)
    .bind(dc_uuid)
    .bind(desired as i32)
    .bind(observed_nodes as i32)
    .bind(observed_slots as i32)
    .bind(status)
    .bind(residency)
    .bind(last_actuated_at)
    .fetch_one(db)
    .await
}

/// Best-effort failure record when a node_pool can't be reconciled at all (e.g. the
/// datacenter alias is gone). Best-effort — does NOT create the row, and
/// does NOT touch `observed_nodes`/`observed_slots`.
async fn mark_node_failed(
    db: &PgPool,
    pool_id: Uuid,
    workspace_id: Uuid,
    pool: &NodePoolPolicy,
    error: &str,
) -> Result<(), sqlx::Error> {
    let _residency =
        (!pool.residency_zone.trim().is_empty()).then(|| pool.residency_zone.clone());
    sqlx::query(
        "UPDATE node_replicas \
         SET status = 'failed', last_error = $3, updated_at = NOW() \
         WHERE pool_resource_id = $1 AND workspace_id = $2",
    )
    .bind(pool_id)
    .bind(workspace_id)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

