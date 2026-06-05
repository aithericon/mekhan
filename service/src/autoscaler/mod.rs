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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use aithericon_resources::types::ModelAutoscalePolicy;

use crate::handlers::model_pool::serving_runner_counts;
use crate::models::model_replicas::{compute_target, in_cooldown, status, ModelReplicaRow};
use crate::petri::client::PetriClient;
use crate::runners_presence::RunnerPresence;

use self::demand::DemandSource;

/// Reconcile cadence. Short enough that a manual scale POST takes effect quickly;
/// long enough that a stuck datacenter doesn't hammer the engine.
const RECONCILE_INTERVAL_SECS: u64 = 15;

/// Spawn the autoscaler control loop. Called from `main.rs` after the presence +
/// worker-liveness controllers. `demand` is `None` for L1 (manual mode only);
/// L2 passes a [`DemandSource`] scraping the Router `/metrics`.
pub fn spawn_autoscaler(
    db: PgPool,
    petri: PetriClient,
    runner_presence: RunnerPresence,
    demand: Option<Arc<dyn DemandSource>>,
) {
    tokio::spawn(run_autoscaler(db, petri, runner_presence, demand));
}

async fn run_autoscaler(
    db: PgPool,
    petri: PetriClient,
    runner_presence: RunnerPresence,
    demand: Option<Arc<dyn DemandSource>>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(RECONCILE_INTERVAL_SECS));
    tracing::info!(
        interval_secs = RECONCILE_INTERVAL_SECS,
        mode = if demand.is_some() { "reactive(L2)" } else { "manual(L1)" },
        "model autoscaler started"
    );

    loop {
        tick.tick().await;
        if let Err(e) = reconcile_once(&db, &petri, &runner_presence, demand.as_deref()).await {
            tracing::warn!("autoscaler reconcile tick failed: {e}");
        }
    }
}

/// One reconcile pass over every `model_policy` resource in every workspace.
async fn reconcile_once(
    db: &PgPool,
    petri: &PetriClient,
    runner_presence: &RunnerPresence,
    demand: Option<&dyn DemandSource>,
) -> Result<(), sqlx::Error> {
    // (policy_resource_id, workspace_id, public_config) for every live model_policy.
    let policies: Vec<(Uuid, Uuid, Value)> = sqlx::query_as(
        "SELECT r.id, r.workspace_id, rv.public_config \
         FROM resources r \
         JOIN resource_versions rv ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.resource_type = 'model_policy' AND r.deleted_at IS NULL",
    )
    .fetch_all(db)
    .await?;

    // Cache the per-workspace observed-count map so N policies in one workspace
    // scan the runner catalogs once.
    let mut counts_by_ws: HashMap<Uuid, HashMap<String, u32>> = HashMap::new();

    for (policy_id, workspace_id, public_config) in policies {
        let policy: ModelAutoscalePolicy = match serde_json::from_value(public_config) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(%policy_id, "skipping unparseable model_policy: {e}");
                continue;
            }
        };

        // Compute the per-workspace observed-count map once per tick (the value
        // is async, so this is an explicit Entry match rather than `or_insert_with`).
        let counts = match counts_by_ws.entry(workspace_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let m = serving_runner_counts(db, runner_presence, workspace_id).await;
                e.insert(m)
            }
        };
        let observed = counts.get(&policy.model_id).copied().unwrap_or(0);

        if let Err(e) = reconcile_policy(
            db,
            petri,
            workspace_id,
            policy_id,
            &policy,
            observed,
            demand,
        )
        .await
        {
            // Fail-soft: record on the row + carry on.
            tracing::warn!(%policy_id, model_id = %policy.model_id, "reconcile failed: {e}");
            let _ = mark_failed(db, policy_id, workspace_id, &policy, observed, &e).await;
        }
    }
    Ok(())
}

async fn reconcile_policy(
    db: &PgPool,
    petri: &PetriClient,
    workspace_id: Uuid,
    policy_id: Uuid,
    policy: &ModelAutoscalePolicy,
    observed: u32,
    demand: Option<&dyn DemandSource>,
) -> Result<(), String> {
    // Resolve the datacenter ALIAS (policy config) → resource uuid.
    let dc_uuid: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM resources \
         WHERE workspace_id = $1 AND resource_type = 'datacenter' AND path = $2 AND deleted_at IS NULL",
    )
    .bind(workspace_id)
    .bind(&policy.datacenter_resource_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("resolve datacenter alias: {e}"))?;
    let dc_uuid = dc_uuid
        .map(|(id,)| id)
        .ok_or_else(|| format!("datacenter alias '{}' not found", policy.datacenter_resource_id))?;

    // Existing row (the durable reconciliation state).
    let existing: Option<ModelReplicaRow> =
        sqlx::query_as("SELECT * FROM model_replicas WHERE policy_resource_id = $1")
            .bind(policy_id)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("load model_replicas row: {e}"))?;

    let now = Utc::now();
    // Demand: L1 → None. L2 calls the source for the reactive modes.
    let demand_val = match demand {
        Some(src) => src.demand_for(&policy.model_id).await,
        None => None,
    };
    // Manual override = the row's desired_count (seeded from the policy on first
    // run; the scale endpoint writes it). Only consulted in manual mode.
    let manual_override = existing.as_ref().map(|r| r.desired_count.max(0) as u32);
    let target = compute_target(policy, demand_val, manual_override);

    let prev_actuated = existing.as_ref().and_then(|r| r.last_actuated_at);
    let prev_desired = existing.as_ref().map(|r| r.desired_count.max(0) as u32);
    let cooled = in_cooldown(prev_actuated, policy.cooldown_secs, now);

    // Decide the intended row state + whether to actuate this tick.
    struct Decision {
        desired: u32,
        status: &'static str,
        actuate: bool,
        last_actuated_at: Option<DateTime<Utc>>,
    }

    let decision = match target {
        // No decision (unknown mode, or a reactive mode with no demand on L1).
        None => {
            if existing.is_none() {
                // Nothing to persist for an undecidable policy with no row yet.
                return Ok(());
            }
            Decision {
                desired: prev_desired.unwrap_or(0),
                status: existing.as_ref().map(|r| leak_status(&r.status)).unwrap_or(status::STOPPED),
                actuate: false,
                last_actuated_at: prev_actuated,
            }
        }
        Some(t) if t == observed => {
            // Steady: no actuation, refresh observed + settle status.
            Decision {
                desired: t,
                status: if t == 0 { status::STOPPED } else { status::ACTIVE },
                actuate: false,
                last_actuated_at: prev_actuated,
            }
        }
        Some(t) if cooled => {
            // Want to change but inside the cooldown window: defer; just refresh
            // observed, keep prior desired/status/last_actuated.
            Decision {
                desired: prev_desired.unwrap_or(t),
                status: existing.as_ref().map(|r| leak_status(&r.status)).unwrap_or(status::PROVISIONING),
                actuate: false,
                last_actuated_at: prev_actuated,
            }
        }
        Some(t) => {
            // Actuate. target==0 with nothing running is already stopped (no net).
            let (st, actuate) = if t == 0 {
                if observed == 0 {
                    (status::STOPPED, false)
                } else {
                    (status::DRAINING, true)
                }
            } else if observed >= 1 {
                (status::SCALING, true)
            } else {
                (status::PROVISIONING, true)
            };
            Decision {
                desired: t,
                status: st,
                actuate,
                last_actuated_at: if actuate { Some(now) } else { prev_actuated },
            }
        }
    };

    // Upsert the row to the intended state (RETURNING id for the net key).
    let row = upsert_row(
        db,
        workspace_id,
        policy_id,
        policy,
        dc_uuid,
        decision.desired,
        observed,
        decision.status,
        decision.last_actuated_at,
    )
    .await
    .map_err(|e| format!("upsert model_replicas row: {e}"))?;

    if decision.actuate {
        // An actuation is an EVENT — discriminate the net id by this actuation's
        // monotonic stamp so the engine re-seeds + re-fires the one-shot net
        // (a stable-per-row id never re-fired → the P4-L1 scale/teardown no-op).
        // `decision.last_actuated_at` is `Some(now)` whenever `actuate` is true.
        let generation = decision.last_actuated_at.unwrap_or(now).timestamp_millis();
        let prev_generation = prev_actuated.map(|t| t.timestamp_millis());
        match actuate::actuate_replica(
            db,
            petri,
            workspace_id,
            row.id,
            generation,
            prev_generation,
            policy,
            dc_uuid,
            decision.desired,
        )
        .await
        {
            Ok(slug) => {
                sqlx::query(
                    "UPDATE model_replicas SET replica_slug = $2, last_error = NULL, updated_at = NOW() \
                     WHERE id = $1",
                )
                .bind(row.id)
                .bind(&slug)
                .execute(db)
                .await
                .map_err(|e| format!("record replica slug: {e}"))?;
            }
            Err(e) => {
                // Deploy / fail-closed refusal: record on the row, don't strand.
                sqlx::query(
                    "UPDATE model_replicas SET status = 'failed', last_error = $2, updated_at = NOW() \
                     WHERE id = $1",
                )
                .bind(row.id)
                .bind(e.to_string())
                .execute(db)
                .await
                .map_err(|e| format!("record actuation failure: {e}"))?;
                tracing::warn!(%policy_id, "actuation failed: {e}");
            }
        }
    }

    Ok(())
}

/// Map a stored status string back onto the `'static` set (defensive — an
/// unexpected stored value settles to `provisioning`).
fn leak_status(s: &str) -> &'static str {
    match s {
        status::PROVISIONING => status::PROVISIONING,
        status::ACTIVE => status::ACTIVE,
        status::SCALING => status::SCALING,
        status::DRAINING => status::DRAINING,
        status::STOPPED => status::STOPPED,
        status::FAILED => status::FAILED,
        _ => status::PROVISIONING,
    }
}

#[allow(clippy::too_many_arguments)]
async fn upsert_row(
    db: &PgPool,
    workspace_id: Uuid,
    policy_id: Uuid,
    policy: &ModelAutoscalePolicy,
    dc_uuid: Uuid,
    desired: u32,
    observed: u32,
    status: &str,
    last_actuated_at: Option<DateTime<Utc>>,
) -> Result<ModelReplicaRow, sqlx::Error> {
    let residency = (!policy.residency_zone.trim().is_empty()).then(|| policy.residency_zone.clone());
    sqlx::query_as::<_, ModelReplicaRow>(
        "INSERT INTO model_replicas \
            (workspace_id, policy_resource_id, model_id, datacenter_resource_id, \
             desired_count, observed_count, status, residency_zone, last_actuated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (policy_resource_id) DO UPDATE SET \
            model_id = EXCLUDED.model_id, \
            datacenter_resource_id = EXCLUDED.datacenter_resource_id, \
            desired_count = EXCLUDED.desired_count, \
            observed_count = EXCLUDED.observed_count, \
            status = EXCLUDED.status, \
            residency_zone = EXCLUDED.residency_zone, \
            last_actuated_at = EXCLUDED.last_actuated_at, \
            updated_at = NOW() \
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(policy_id)
    .bind(&policy.model_id)
    .bind(dc_uuid)
    .bind(desired as i32)
    .bind(observed as i32)
    .bind(status)
    .bind(residency)
    .bind(last_actuated_at)
    .fetch_one(db)
    .await
}

/// Best-effort failure record when a policy can't be reconciled at all (e.g. the
/// datacenter alias is gone). Keeps `desired`/`last_actuated_at` at their prior
/// values (a fresh policy with no row stays unpersisted).
async fn mark_failed(
    db: &PgPool,
    policy_id: Uuid,
    workspace_id: Uuid,
    policy: &ModelAutoscalePolicy,
    observed: u32,
    error: &str,
) -> Result<(), sqlx::Error> {
    let residency = (!policy.residency_zone.trim().is_empty()).then(|| policy.residency_zone.clone());
    sqlx::query(
        "UPDATE model_replicas \
         SET status = 'failed', observed_count = $3, last_error = $4, updated_at = NOW() \
         WHERE policy_resource_id = $1 AND workspace_id = $2",
    )
    .bind(policy_id)
    .bind(workspace_id)
    .bind(observed as i32)
    .bind(error)
    .execute(db)
    .await?;
    let _ = residency; // residency recorded on the success path's upsert.
    Ok(())
}
