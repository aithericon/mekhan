//! Loop 2 — the model PLACEMENT controller (docs/31 Phase 3, the keystone).
//!
//! A second pass in the SAME `run_autoscaler` tick AFTER the node-fleet scaler
//! (Loop 1). Where Loop 1 owns COUNT (how many generic engine nodes exist), Loop
//! 2 owns BINDING (which model lands on which engine). For each `model_policy`
//! with demand the controller walks a CHEAPEST-FIRST mechanism cascade against the
//! Phase-0 engine-inventory read model
//! ([`crate::handlers::model_pool::serving_runner_inventory`]), short-circuiting at
//! the first satisfiable mechanism (OQ-5):
//!
//!   (a) ADAPTER LOAD — a LoRA whose base is resident on a live IN-ZONE node with
//!       headroom → publish `Load{Lora}` on `runner.{id}.load`. **ms, no process.**
//!       Reacts every tick, idempotent, NOT cooldown-gated.
//!   (b) SLEEP/WAKE — a live node whose resident base IS the wanted base → publish
//!       `Load{Base}` (wake). **seconds**, gated strictly on base-identity match.
//!   (c) RAISE NODE DEMAND — no in-zone base with headroom → leave the row
//!       `pending`; Loop 1 already sees this model's demand via
//!       `aggregate_pool_demand` and provisions a node next tick. **minutes**,
//!       cooldown-gated (must not flap the status).
//!   (d) FALLBACK DEDICATED JOB — only when `policy.dedicated == true` → call the
//!       existing [`crate::autoscaler::actuate::actuate_replica`] (the doc-29
//!       per-model Nomad job, DEMOTED from default to last resort). Cooldown-gated.
//!
//! ## Residency fail-closed BEFORE any publish (OQ-4, DERIVED-A)
//!
//! Single-zone-per-pool, strict equality. The referenced `node_pool`'s
//! `residency_zone` is the single zone source; a non-empty `model_policy`
//! residency requirement that is NOT strictly equal to the pool's zone → the row
//! is marked `failed` with a `last_error` and NO command is published. Reuses the
//! `actuate.rs:187` fail-closed shape and the router `routing.rs:88` zone-equality
//! filter (equal-or-reject, never relax) so the two enforcement points cannot
//! drift.
//!
//! ## Sleep detection (a known gap)
//!
//! The runner catalog has no "asleep" flag, so mechanism (b) cannot distinguish a
//! resident-but-slept base from a resident-and-awake one. We therefore publish a
//! WAKE (`Load{Base}`) whenever the wanted base is resident in-zone for a
//! base-policy model — `wake_up` is idempotent / 404-tolerant on the agent, so
//! re-issuing each tick is safe (placement is desired-state). Targeting a SPECIFIC
//! base for sleep/wake on a multi-base node is a deferred vLLM-contract gap.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use aithericon_resources::types::{ModelAutoscalePolicy, NodePoolPolicy};

use crate::handlers::model_pool::serving_runner_inventory;
use crate::models::model_replicas::{in_cooldown, status, ModelReplicaRow};
use crate::models::runner::{ModelEntry, ModelInterfaceKind};
use crate::nats::MekhanNats;
use crate::petri::client::PetriClient;
use crate::runner_commands::{publish_model_command, LoadTarget, ModelCommand};
use crate::runners_presence::RunnerPresence;

use super::actuate;
use super::demand::DemandSource;

/// One base engine live on one in-zone node, with its computed headroom + the
/// adapters already loaded on it. The cascade input — derived from the Phase-0
/// inventory ∩ the in-zone runner set, headroom layered from the router gauge.
#[derive(Debug, Clone)]
pub struct EngineSlot {
    pub runner_id: Uuid,
    pub base: String,
    /// Free slots = `C − Σ(base + adapters in-flight)`. `None` = unknown budget
    /// (no `max_num_seqs` advertised, or the router poll is unconfigured) → the
    /// cascade treats unknown headroom as AVAILABLE (fail-soft, like the rest of
    /// the model-pool reads).
    pub headroom: Option<u32>,
    /// adapter model ids already resident on this base engine.
    pub adapters: Vec<String>,
}

impl EngineSlot {
    /// Whether this slot can accept a new load: unknown headroom (fail-soft) or
    /// strictly positive free slots.
    fn has_headroom(&self) -> bool {
        self.headroom.map(|h| h > 0).unwrap_or(true)
    }
}

/// The cheapest satisfiable placement mechanism for one model. The pure cascade
/// ([`plan_placement`]) returns this; the IO layer publishes / actuates / records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementOutcome {
    /// (a) Load this LoRA adapter onto the base engine on `runner_id`.
    AdapterLoad {
        runner_id: Uuid,
        adapter_id: String,
        base: String,
        source_uri: Option<String>,
    },
    /// (b) Wake/ensure the base engine on `runner_id` is awake.
    Wake { runner_id: Uuid, base: String },
    /// Already satisfied — the wanted base/adapter is resident in-zone. No publish.
    AlreadyPlaced,
    /// (c) No in-zone headroom — leave `pending`; Loop 1 provisions next tick.
    RaiseNodeDemand,
    /// (d) No headroom AND `dedicated == true` — fall back to a dedicated job.
    DedicatedFallback,
    /// OQ-4 fail-closed: the model's residency requirement ≠ the pool's zone.
    ResidencyMismatch { wanted: String, pool_zone: String },
}

/// The PURE cheapest-first cascade (OQ-5). No IO — `in_zone_slots` is the Phase-0
/// inventory already filtered to nodes in pools whose zone satisfies the model's
/// residency requirement (the residency equality check runs in [`reconcile_placement`]
/// before this is called, and is re-asserted here for the mismatch outcome).
///
/// `wanted_base` is `policy.base` for a LoRA, else `policy.model_id` (a base model
/// IS its own base). `is_lora` selects the adapter-load vs wake leg.
pub fn plan_placement(
    policy: &ModelAutoscalePolicy,
    pool: &NodePoolPolicy,
    in_zone_slots: &[EngineSlot],
) -> PlacementOutcome {
    // Residency equality (OQ-4) — re-asserted as the first cascade gate. A zoneless
    // model places on any pool; a zoned model only on a strictly-equal pool zone.
    let wanted_zone = policy.residency_zone.trim();
    if !wanted_zone.is_empty() && wanted_zone != pool.residency_zone.trim() {
        return PlacementOutcome::ResidencyMismatch {
            wanted: wanted_zone.to_string(),
            pool_zone: pool.residency_zone.trim().to_string(),
        };
    }

    let is_lora = policy.base.is_some();
    let wanted_base = policy
        .base
        .clone()
        .unwrap_or_else(|| policy.model_id.clone());

    if is_lora {
        // (a) ADAPTER LOAD. Already-loaded anywhere in-zone ⇒ satisfied.
        let already = in_zone_slots
            .iter()
            .any(|s| s.base == wanted_base && s.adapters.iter().any(|a| a == &policy.model_id));
        if already {
            return PlacementOutcome::AlreadyPlaced;
        }
        // Cheapest base engine for this base WITH headroom.
        if let Some(slot) = in_zone_slots
            .iter()
            .filter(|s| s.base == wanted_base && s.has_headroom())
            .max_by_key(|s| s.headroom.unwrap_or(u32::MAX))
        {
            return PlacementOutcome::AdapterLoad {
                runner_id: slot.runner_id,
                adapter_id: policy.model_id.clone(),
                base: wanted_base,
                source_uri: None,
            };
        }
    } else {
        // (b) SLEEP/WAKE — the base is resident in-zone (idempotent wake).
        if let Some(slot) = in_zone_slots.iter().find(|s| s.base == wanted_base) {
            return PlacementOutcome::Wake {
                runner_id: slot.runner_id,
                base: wanted_base,
            };
        }
    }

    // (c)/(d) — no in-zone base with headroom. Dedicated opt-out → fallback; else
    // raise node demand (Loop 1 provisions next tick).
    if policy.dedicated == Some(true) {
        PlacementOutcome::DedicatedFallback
    } else {
        PlacementOutcome::RaiseNodeDemand
    }
}

/// The PURE idle-eviction decision (GAP B). Returns `Some((runner_id, base))` to
/// SLEEP a resident base model — exactly when ALL hold:
///
/// - the policy OPTED IN (`idle_evict == Some(true)`),
/// - demand has dropped to zero (`demand_zero`),
/// - the actuation cooldown is NOT active (`!in_cooldown` — the flap guard),
/// - the wanted base IS resident on an in-zone slot (nothing to sleep otherwise).
///
/// Mirrors the demand>0 [`plan_placement`] wake leg in reverse: a base IS its own
/// base, so the runner carrying the wanted base is the sleep target. LoRA policies
/// never idle-evict here (the base engine owns the GPU residency — sleeping a base
/// out from under loaded adapters is a deferred vLLM-contract gap), so a policy
/// with a `base` back-pointer returns `None`.
pub fn plan_idle_eviction(
    policy: &ModelAutoscalePolicy,
    in_zone_slots: &[EngineSlot],
    demand_zero: bool,
    in_cooldown: bool,
) -> Option<(Uuid, String)> {
    if policy.idle_evict != Some(true) || !demand_zero || in_cooldown {
        return None;
    }
    // Only a base model (no LoRA back-pointer) is eligible to sleep.
    if policy.base.is_some() {
        return None;
    }
    let wanted_base = policy.model_id.clone();
    in_zone_slots
        .iter()
        .find(|s| s.base == wanted_base)
        .map(|s| (s.runner_id, wanted_base))
}

/// One reconcile pass over every `model_policy` with demand (docs/31 Loop 2). Runs
/// AFTER the node pass. Per-policy failures fail-soft (recorded on the row + carry
/// on); a setup `sqlx::Error` (the policy-list fetch) kills the tick.
///
/// `pool_zones` maps each `node_pool` alias → its `(NodePoolPolicy, datacenter
/// uuid)` (resolved once by the node pass / here), used for the residency-equality
/// check and the dedicated-fallback engine spec.
pub async fn reconcile_placement(
    db: &PgPool,
    petri: &PetriClient,
    nats: &MekhanNats,
    runner_presence: &RunnerPresence,
    demand: Option<&dyn DemandSource>,
) -> Result<(), sqlx::Error> {
    // The autoscale policy is folded onto `model_states` (no longer a resource).
    // Scan every model in every workspace that has a policy set
    // (`autoscale_mode IS NOT NULL AND node_pool IS NOT NULL`); the per-model KEY is
    // now (workspace_id, model_id).
    #[derive(sqlx::FromRow)]
    struct PolicyScanRow {
        workspace_id: Uuid,
        #[sqlx(flatten)]
        policy: super::ModelStatePolicyRow,
    }
    let policies: Vec<PolicyScanRow> = sqlx::query_as(
        "SELECT workspace_id, model_id, base, autoscale_mode, desired_replicas, \
                scale_up_threshold, scale_down_threshold, cooldown_secs, node_pool, \
                residency_zone, dedicated, idle_evict \
         FROM model_states \
         WHERE autoscale_mode IS NOT NULL AND node_pool IS NOT NULL",
    )
    .fetch_all(db)
    .await?;

    // Per-workspace inventory + pool caches (one scan per workspace per tick).
    let mut inventory_by_ws: HashMap<Uuid, HashMap<Uuid, Vec<ModelEntry>>> = HashMap::new();
    let mut membership_by_ws: HashMap<Uuid, HashMap<Uuid, String>> = HashMap::new();
    let mut pools_by_ws: HashMap<Uuid, HashMap<String, NodePoolPolicy>> = HashMap::new();

    for scan in policies {
        let workspace_id = scan.workspace_id;
        let policy: ModelAutoscalePolicy = scan.policy.into_policy();
        let model_id = policy.model_id.clone();

        // Demand gate: only models with demand > 0 are PLACED (L1 demand=None ⇒
        // demand 0 — nothing to place; Loop 1 floors capacity at min_nodes). A
        // zero-demand model still gets the idle-EVICTION pass below WHEN it opted
        // into `idle_evict`; without the opt-in, skip it entirely.
        let model_demand = match demand {
            Some(src) => src.demand_for(&policy.model_id).await.unwrap_or(0.0),
            None => 0.0,
        };
        let demand_zero = model_demand <= 0.0;
        if demand_zero && policy.idle_evict != Some(true) {
            continue;
        }

        // Resolve the referenced node_pool (config) — cached per workspace.
        let pools = match pools_by_ws.entry(workspace_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(load_pools(db, workspace_id).await)
            }
        };
        let Some(pool) = pools.get(&policy.node_pool).cloned() else {
            let msg = format!("node_pool alias '{}' not found", policy.node_pool);
            tracing::warn!(%workspace_id, %model_id, "placement: {msg}");
            mark_placement_failed(db, workspace_id, &model_id, &msg).await;
            continue;
        };

        // Inventory + pool membership for this workspace (cached).
        let inventory = match inventory_by_ws.entry(workspace_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(
                serving_runner_inventory(db, runner_presence, workspace_id).await,
            ),
        };
        let membership = match membership_by_ws.entry(workspace_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(runner_presence.pool_membership().await)
            }
        };

        // Build the in-zone engine slots: every node whose pool zone satisfies this
        // model's residency requirement, with per-base headroom. Headroom is read
        // from the router in-flight gauge (fail-soft: unknown → available).
        let slots = build_in_zone_slots(inventory, membership, pools, &policy, demand).await;

        if demand_zero {
            // GAP B — idle eviction (vLLM sleep). Demand has dropped to zero and the
            // policy opted in: sleep the resident base, gated on the durable
            // cooldown anchored on `model_replicas.last_actuated_at` (the flap
            // guard — the SAME anchor the placement / node passes use).
            if let Err(e) = apply_idle_eviction(db, nats, workspace_id, &model_id, &policy, &slots)
                .await
            {
                tracing::warn!(%workspace_id, %model_id, "idle-eviction failed: {e}");
                mark_placement_failed(db, workspace_id, &model_id, &e).await;
            }
            continue;
        }

        let outcome = plan_placement(&policy, &pool, &slots);

        if let Err(e) = apply_outcome(
            db,
            petri,
            nats,
            workspace_id,
            &model_id,
            &policy,
            &pool,
            outcome,
        )
        .await
        {
            tracing::warn!(%workspace_id, %model_id, "placement apply failed: {e}");
            mark_placement_failed(db, workspace_id, &model_id, &e).await;
        }
    }
    Ok(())
}

/// Carry out the GAP B idle-eviction decision (vLLM sleep) for one zero-demand,
/// opted-in model. Loads the `model_replicas.last_actuated_at` cooldown anchor,
/// computes `in_cooldown`, runs the pure [`plan_idle_eviction`], and on `Some`
/// publishes an `Unload{Base}` (→ vLLM `/sleep`) + stamps the row `sleeping` with a
/// fresh `last_actuated_at` (the flap guard). On `None` it is a no-op (still hot,
/// in cooldown, not opted in, or the base isn't resident). Returns `Err` only on a
/// hard publish failure the caller records.
async fn apply_idle_eviction(
    db: &PgPool,
    nats: &MekhanNats,
    workspace_id: Uuid,
    model_id: &str,
    policy: &ModelAutoscalePolicy,
    slots: &[EngineSlot],
) -> Result<(), String> {
    use chrono::Utc;

    // The durable reconciliation row carries the cooldown anchor (survives a
    // restart). No row yet ⇒ never actuated ⇒ never in cooldown.
    let last_actuated: Option<chrono::DateTime<Utc>> = sqlx::query_scalar(
        "SELECT last_actuated_at FROM model_replicas WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(model_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("load model_replicas last_actuated_at: {e}"))?
    .flatten();

    let now = Utc::now();
    let cooled = in_cooldown(last_actuated, policy.cooldown_secs, now);

    let Some((runner_id, base)) = plan_idle_eviction(policy, slots, true, cooled) else {
        // Not eligible this tick (still hot, in cooldown, not opted in, or the base
        // isn't resident in-zone) — no-op.
        return Ok(());
    };

    let cmd = ModelCommand::Unload {
        target: LoadTarget::Base { model_id: base },
    };
    publish_model_command(nats, runner_id, &cmd)
        .await
        .map_err(|e| format!("publish idle-eviction sleep: {e}"))?;

    // Mark `sleeping` + stamp `last_actuated_at` (the flap guard — gates a re-sleep
    // / immediate-wake oscillation through the cooldown window).
    let _ = sqlx::query(
        "UPDATE model_replicas \
         SET status = $3, last_actuated_at = $4, last_error = NULL, updated_at = NOW() \
         WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(model_id)
    .bind(status::SLEEPING)
    .bind(now)
    .execute(db)
    .await;

    Ok(())
}

/// Load every `node_pool` in the workspace as `alias → NodePoolPolicy`.
async fn load_pools(db: &PgPool, workspace_id: Uuid) -> HashMap<String, NodePoolPolicy> {
    let rows: Vec<(String, serde_json::Value)> = match sqlx::query_as(
        "SELECT r.path, rv.public_config \
         FROM resources r \
         JOIN resource_versions rv ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.resource_type = 'node_pool' AND r.workspace_id = $1 AND r.deleted_at IS NULL",
    )
    .bind(workspace_id)
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(%workspace_id, "placement: node_pool load failed: {e}");
            return HashMap::new();
        }
    };
    rows.into_iter()
        .filter_map(|(alias, cfg)| serde_json::from_value(cfg).ok().map(|p| (alias, p)))
        .collect()
}

/// Resolve a datacenter alias (`resources.path`) → resource uuid in a workspace.
async fn resolve_datacenter_uuid(
    db: &PgPool,
    workspace_id: Uuid,
    alias: &str,
) -> Result<Uuid, String> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM resources \
         WHERE workspace_id = $1 AND resource_type = 'datacenter' AND path = $2 AND deleted_at IS NULL",
    )
    .bind(workspace_id)
    .bind(alias)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("resolve datacenter alias: {e}"))?;
    row.map(|(id,)| id)
        .ok_or_else(|| format!("datacenter alias '{alias}' not found"))
}

/// Collapse the raw `runner_id → [ModelEntry]` inventory for the in-zone nodes
/// into [`EngineSlot`]s (base engines + their adapters + headroom). A node is
/// "in zone" when the pool it is tagged to (via `membership`) has a `residency_zone`
/// that satisfies the model's requirement (zoneless model ⇒ any pool).
async fn build_in_zone_slots(
    inventory: &HashMap<Uuid, Vec<ModelEntry>>,
    membership: &HashMap<Uuid, String>,
    pools: &HashMap<String, NodePoolPolicy>,
    policy: &ModelAutoscalePolicy,
    demand: Option<&dyn DemandSource>,
) -> Vec<EngineSlot> {
    let wanted_zone = policy.residency_zone.trim();
    let mut slots: Vec<EngineSlot> = Vec::new();

    for (runner_id, entries) in inventory {
        // Resolve the node's pool zone via its pool-membership alias. A node with
        // no pool tag, or whose pool is unknown, is conservatively EXCLUDED for a
        // zoned model (fail-closed) but INCLUDED for a zoneless one.
        let node_zone = membership
            .get(runner_id)
            .and_then(|alias| pools.get(alias))
            .map(|p| p.residency_zone.trim().to_string());

        if !wanted_zone.is_empty() {
            // Zoned model: strict equality with a KNOWN node zone.
            match node_zone.as_deref() {
                Some(z) if z == wanted_zone => {}
                _ => continue,
            }
        }

        // Base engines on this node (with their advertised C).
        let mut bases: HashMap<String, Option<u32>> = HashMap::new();
        for e in entries {
            if e.kind == ModelInterfaceKind::Base {
                bases.insert(e.model_id.clone(), e.max_num_seqs);
            }
        }
        // Adapters grouped by base back-pointer.
        let mut adapters_by_base: HashMap<String, Vec<String>> = HashMap::new();
        for e in entries {
            if e.kind == ModelInterfaceKind::Lora {
                if let Some(base) = &e.base {
                    adapters_by_base
                        .entry(base.clone())
                        .or_default()
                        .push(e.model_id.clone());
                    // Ensure a slot exists even if the base entry wasn't advertised.
                    bases.entry(base.clone()).or_insert(None);
                }
            }
        }

        for (base, c) in bases {
            let adapters = adapters_by_base.get(&base).cloned().unwrap_or_default();
            // Headroom = C − Σ(base + adapters in-flight). Fail-soft: unknown C ⇒
            // None (available); router unconfigured ⇒ full budget (= C).
            let headroom = match c {
                None => None,
                Some(cap) => {
                    let mut used = 0.0_f64;
                    if let Some(src) = demand {
                        if let Some(v) = src.inflight_for(&base).await {
                            used += v;
                        }
                        for a in &adapters {
                            if let Some(v) = src.inflight_for(a).await {
                                used += v;
                            }
                        }
                    }
                    Some(cap.saturating_sub(used.max(0.0).round() as u32))
                }
            };
            slots.push(EngineSlot {
                runner_id: *runner_id,
                base,
                headroom,
                adapters,
            });
        }
    }
    slots
}

/// Carry out the cascade decision: publish (a/b), record `pending` (c), actuate the
/// dedicated fallback (d), or fail-closed on a residency mismatch. Returns `Err`
/// only on a hard failure the caller should record on the row.
#[allow(clippy::too_many_arguments)]
async fn apply_outcome(
    db: &PgPool,
    petri: &PetriClient,
    nats: &MekhanNats,
    workspace_id: Uuid,
    model_id: &str,
    policy: &ModelAutoscalePolicy,
    pool: &NodePoolPolicy,
    outcome: PlacementOutcome,
) -> Result<(), String> {
    match outcome {
        PlacementOutcome::ResidencyMismatch { wanted, pool_zone } => {
            let msg = format!(
                "GDPR fail-closed: model residency_zone '{wanted}' ≠ node_pool zone '{pool_zone}' \
                 — refusing to place (single-zone-per-pool, strict equality)"
            );
            mark_placement_failed(db, workspace_id, model_id, &msg).await;
            Ok(())
        }
        PlacementOutcome::AdapterLoad {
            runner_id,
            adapter_id,
            base,
            source_uri,
        } => {
            let cmd = ModelCommand::Load {
                target: LoadTarget::Lora {
                    adapter_id,
                    base,
                    source_uri,
                },
            };
            publish_model_command(nats, runner_id, &cmd)
                .await
                .map_err(|e| format!("publish adapter-load: {e}"))?;
            set_placement_status(db, workspace_id, model_id, status::ACTIVE, None).await;
            Ok(())
        }
        PlacementOutcome::Wake { runner_id, base } => {
            let cmd = ModelCommand::Load {
                target: LoadTarget::Base { model_id: base },
            };
            publish_model_command(nats, runner_id, &cmd)
                .await
                .map_err(|e| format!("publish wake: {e}"))?;
            set_placement_status(db, workspace_id, model_id, status::ACTIVE, None).await;
            Ok(())
        }
        PlacementOutcome::AlreadyPlaced => {
            set_placement_status(db, workspace_id, model_id, status::ACTIVE, None).await;
            Ok(())
        }
        PlacementOutcome::RaiseNodeDemand => {
            // Loop 1 already sees this model's demand via aggregate_pool_demand and
            // will provision a node next tick; just mark pending + retry. Cooldown
            // gates the status flap (no actuation happens here, so this is purely a
            // status hygiene — re-marking pending each tick is harmless).
            set_placement_status(db, workspace_id, model_id, status::PROVISIONING, None).await;
            Ok(())
        }
        PlacementOutcome::DedicatedFallback => {
            dedicated_fallback(db, petri, workspace_id, model_id, policy, pool).await
        }
    }
}

/// (d) The demoted dedicated-job fallback. Cooldown-gated like the old default
/// model pass: resolve the pool's datacenter, gen-key the actuation, drive
/// [`actuate::actuate_replica`] sourcing the engine spec from the pool's
/// `engine_spec` stamped with this model's id.
async fn dedicated_fallback(
    db: &PgPool,
    petri: &PetriClient,
    workspace_id: Uuid,
    model_id: &str,
    policy: &ModelAutoscalePolicy,
    pool: &NodePoolPolicy,
) -> Result<(), String> {
    use chrono::Utc;

    // Resolve the pool's datacenter (the model policy no longer carries one).
    let dc_uuid = resolve_datacenter_uuid(db, workspace_id, &pool.datacenter_resource_id).await?;

    // The durable reconciliation row (per (workspace, model)). Cooldown-gate the
    // dedicated leg.
    let existing: Option<ModelReplicaRow> = sqlx::query_as(
        "SELECT * FROM model_replicas WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(model_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("load model_replicas row: {e}"))?;

    let now = Utc::now();
    let prev_actuated = existing.as_ref().and_then(|r| r.last_actuated_at);
    if in_cooldown(prev_actuated, policy.cooldown_secs, now) {
        // Inside the cooldown window: don't re-actuate, keep the row as-is.
        return Ok(());
    }

    // Desired COUNT for the dedicated job = the demand-slot ceiling (default 1 — a
    // dedicated fallback exists precisely because packing failed, so serve ≥1).
    let target = policy.desired_replicas.unwrap_or(1).max(1);

    // Ensure a row exists + stamp last_actuated_at (the generation source).
    let row = upsert_dedicated_row(
        db,
        workspace_id,
        policy,
        dc_uuid,
        target,
        status::PROVISIONING,
        now,
    )
    .await
    .map_err(|e| format!("upsert model_replicas row: {e}"))?;

    let generation = now.timestamp_millis();
    let prev_generation = prev_actuated.map(|t| t.timestamp_millis());

    match actuate::actuate_replica(
        db,
        petri,
        workspace_id,
        row.id,
        generation,
        prev_generation,
        policy,
        pool,
        dc_uuid,
        target,
    )
    .await
    {
        Ok(slug) => {
            let _ = sqlx::query(
                "UPDATE model_replicas SET replica_slug = $2, last_error = NULL, updated_at = NOW() \
                 WHERE id = $1",
            )
            .bind(row.id)
            .bind(&slug)
            .execute(db)
            .await;
            Ok(())
        }
        Err(e) => Err(format!("dedicated actuation failed: {e}")),
    }
}

/// Upsert a `model_replicas` row for the dedicated fallback, stamping
/// `last_actuated_at` (the generation source) + `desired_count`.
async fn upsert_dedicated_row(
    db: &PgPool,
    workspace_id: Uuid,
    policy: &ModelAutoscalePolicy,
    dc_uuid: Uuid,
    desired: u32,
    status: &str,
    last_actuated_at: chrono::DateTime<chrono::Utc>,
) -> Result<ModelReplicaRow, sqlx::Error> {
    let residency =
        (!policy.residency_zone.trim().is_empty()).then(|| policy.residency_zone.clone());
    sqlx::query_as::<_, ModelReplicaRow>(
        "INSERT INTO model_replicas \
            (workspace_id, model_id, datacenter_resource_id, \
             desired_count, observed_count, status, residency_zone, last_actuated_at) \
         VALUES ($1, $2, $3, $4, 0, $5, $6, $7) \
         ON CONFLICT (workspace_id, model_id) DO UPDATE SET \
            datacenter_resource_id = EXCLUDED.datacenter_resource_id, \
            desired_count = EXCLUDED.desired_count, \
            status = EXCLUDED.status, \
            residency_zone = EXCLUDED.residency_zone, \
            last_actuated_at = EXCLUDED.last_actuated_at, \
            updated_at = NOW() \
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(&policy.model_id)
    .bind(dc_uuid)
    .bind(desired as i32)
    .bind(status)
    .bind(residency)
    .bind(last_actuated_at)
    .fetch_one(db)
    .await
}

/// Set the placement status on the policy's `model_replicas` row WITHOUT touching
/// observed/desired/last_actuated (a publish is not an actuation). Best-effort: a
/// row may not exist yet for a packed (non-dedicated) model — in that case the
/// status is informational only and the upsert is skipped (no-op on 0 rows).
async fn set_placement_status(
    db: &PgPool,
    workspace_id: Uuid,
    model_id: &str,
    status: &str,
    last_error: Option<&str>,
) {
    let _ = sqlx::query(
        "UPDATE model_replicas SET status = $3, last_error = $4, updated_at = NOW() \
         WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(model_id)
    .bind(status)
    .bind(last_error)
    .execute(db)
    .await;
}

/// Record a placement failure on the model's row (best-effort, like `mark_failed`).
async fn mark_placement_failed(db: &PgPool, workspace_id: Uuid, model_id: &str, error: &str) {
    set_placement_status(db, workspace_id, model_id, status::FAILED, Some(error)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pool(zone: &str) -> NodePoolPolicy {
        NodePoolPolicy {
            datacenter_resource_id: "dev-nomad".into(),
            residency_zone: zone.into(),
            gpu_class: "a100".into(),
            max_num_seqs: 8,
            engine_spec: json!({}),
            min_nodes: 0,
            max_nodes: 4,
            cooldown_secs: None,
        }
    }

    fn lora_policy(zone: &str, base: &str, dedicated: bool) -> ModelAutoscalePolicy {
        ModelAutoscalePolicy {
            model_id: "my-lora".into(),
            residency_zone: zone.into(),
            mode: "keep_warm".into(),
            desired_replicas: None,
            scale_up_threshold: None,
            scale_down_threshold: None,
            cooldown_secs: None,
            node_pool: "p".into(),
            base: Some(base.into()),
            dedicated: Some(dedicated),
            idle_evict: None,
        }
    }

    fn base_policy(zone: &str) -> ModelAutoscalePolicy {
        ModelAutoscalePolicy {
            model_id: "llama".into(),
            residency_zone: zone.into(),
            mode: "keep_warm".into(),
            desired_replicas: None,
            scale_up_threshold: None,
            scale_down_threshold: None,
            cooldown_secs: None,
            node_pool: "p".into(),
            base: None,
            dedicated: None,
            idle_evict: None,
        }
    }

    fn slot(runner: Uuid, base: &str, headroom: Option<u32>, adapters: &[&str]) -> EngineSlot {
        EngineSlot {
            runner_id: runner,
            base: base.into(),
            headroom,
            adapters: adapters.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn adapter_load_when_base_resident_with_headroom() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(4), &[])];
        let out = plan_placement(&lora_policy("eu", "llama", false), &pool("eu"), &slots);
        assert_eq!(
            out,
            PlacementOutcome::AdapterLoad {
                runner_id: r,
                adapter_id: "my-lora".into(),
                base: "llama".into(),
                source_uri: None,
            }
        );
    }

    #[test]
    fn adapter_already_loaded_is_already_placed() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(4), &["my-lora"])];
        let out = plan_placement(&lora_policy("eu", "llama", false), &pool("eu"), &slots);
        assert_eq!(out, PlacementOutcome::AlreadyPlaced);
    }

    #[test]
    fn base_resident_wakes() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(0), &[])];
        let out = plan_placement(&base_policy("eu"), &pool("eu"), &slots);
        assert_eq!(
            out,
            PlacementOutcome::Wake {
                runner_id: r,
                base: "llama".into()
            }
        );
    }

    #[test]
    fn no_headroom_raises_node_demand_when_not_dedicated() {
        let r = Uuid::new_v4();
        // Base resident but zero headroom for a LoRA → can't adapter-load.
        let slots = vec![slot(r, "llama", Some(0), &[])];
        let out = plan_placement(&lora_policy("eu", "llama", false), &pool("eu"), &slots);
        assert_eq!(out, PlacementOutcome::RaiseNodeDemand);
    }

    #[test]
    fn no_headroom_falls_back_to_dedicated_when_opted_in() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(0), &[])];
        let out = plan_placement(&lora_policy("eu", "llama", true), &pool("eu"), &slots);
        assert_eq!(out, PlacementOutcome::DedicatedFallback);
    }

    #[test]
    fn no_in_zone_node_raises_node_demand() {
        // Empty in-zone slots ⇒ nothing resident ⇒ raise node demand.
        let out = plan_placement(&base_policy("eu"), &pool("eu"), &[]);
        assert_eq!(out, PlacementOutcome::RaiseNodeDemand);
    }

    #[test]
    fn residency_mismatch_fails_closed_before_publish() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(8), &[])];
        // Model wants eu, pool is us → fail-closed, no placement.
        let out = plan_placement(&lora_policy("eu", "llama", false), &pool("us"), &slots);
        assert_eq!(
            out,
            PlacementOutcome::ResidencyMismatch {
                wanted: "eu".into(),
                pool_zone: "us".into()
            }
        );
    }

    #[test]
    fn zoneless_model_places_on_any_pool() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(4), &[])];
        // No residency requirement → eu pool is fine.
        let out = plan_placement(&lora_policy("", "llama", false), &pool("eu"), &slots);
        assert!(matches!(out, PlacementOutcome::AdapterLoad { .. }));
    }

    #[test]
    fn unknown_headroom_is_treated_as_available() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", None, &[])];
        let out = plan_placement(&lora_policy("eu", "llama", false), &pool("eu"), &slots);
        assert!(matches!(out, PlacementOutcome::AdapterLoad { .. }));
    }

    // ── GAP B: idle-eviction (vLLM sleep) decision ────────────────────────────

    /// A base policy with `idle_evict` toggled on (the `base_policy` helper serves
    /// `"llama"` as a base model).
    fn evictable_base_policy(zone: &str, idle_evict: bool) -> ModelAutoscalePolicy {
        ModelAutoscalePolicy {
            idle_evict: Some(idle_evict),
            ..base_policy(zone)
        }
    }

    #[test]
    fn idle_evict_sleeps_resident_base_at_zero_demand() {
        let r = Uuid::new_v4();
        // The base `"llama"` is resident in-zone, opted in, demand zero, not cooled.
        let slots = vec![slot(r, "llama", Some(8), &[])];
        let out = plan_idle_eviction(&evictable_base_policy("eu", true), &slots, true, false);
        assert_eq!(out, Some((r, "llama".to_string())));
    }

    #[test]
    fn idle_evict_noop_when_not_opted_in() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(8), &[])];
        // Opt-out (the default) → never sleep, even with zero demand + resident base.
        assert_eq!(
            plan_idle_eviction(&evictable_base_policy("eu", false), &slots, true, false),
            None
        );
        // `None` (unset) is also opt-out.
        assert_eq!(
            plan_idle_eviction(&base_policy("eu"), &slots, true, false),
            None
        );
    }

    #[test]
    fn idle_evict_noop_within_cooldown() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(8), &[])];
        // Opted in + zero demand + resident, but inside the cooldown window → defer.
        assert_eq!(
            plan_idle_eviction(&evictable_base_policy("eu", true), &slots, true, true),
            None
        );
    }

    #[test]
    fn idle_evict_noop_when_base_not_resident() {
        // Opted in + zero demand + not cooled, but nothing resident in-zone →
        // nothing to sleep.
        assert_eq!(
            plan_idle_eviction(&evictable_base_policy("eu", true), &[], true, false),
            None
        );
        // A different base resident (not the wanted one) is also a no-op.
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "other-model", Some(8), &[])];
        assert_eq!(
            plan_idle_eviction(&evictable_base_policy("eu", true), &slots, true, false),
            None
        );
    }

    #[test]
    fn idle_evict_noop_when_demand_nonzero() {
        // Even opted in + resident + not cooled, a non-zero demand never sleeps
        // (the reconcile loop only calls this on the zero-demand branch, but the
        // pure fn defends the gate too).
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(8), &[])];
        assert_eq!(
            plan_idle_eviction(&evictable_base_policy("eu", true), &slots, false, false),
            None
        );
    }
}
