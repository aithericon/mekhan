//! The model PLACEMENT controller — the only autoscaler.
//!
//! This controller is the embryo of the future SERVICE RECONCILER (docs/35
//! §9): it reconciles desired replicas × placement constraints into held
//! "serve model X on runner Y" assignments. It is allocation-plane — it
//! decides placement — and never traffic-plane: it never sees an inference
//! request. Extract the generic loop when the second consumer (the crawler
//! fleet, docs/35 §10) exists, not before.
//!
//! Each tick, for every model with an autoscale policy folded onto its
//! `model_states` row, the controller decides WHICH already-registered runners
//! serve the model and publishes NATS load/unload to reach that state. There is no
//! node provisioning: placement targets enrolled runners (the live engine
//! inventory, [`crate::handlers::model_pool::serving_runner_catalogs`]), never
//! Nomad allocations.
//!
//! Per model:
//!   - `manual` mode places by `desired_replicas` (the runner-count target),
//!     ignoring demand.
//!   - `scale_to_zero` / `keep_warm` are demand-gated: with demand they spread to
//!     `desired_replicas` runners, at zero demand they idle-evict (if opted in)
//!     and are otherwise left alone (HARD-BLOCKED on the router `/metrics` in L1).
//!
//! The cheapest-first leg per runner:
//!   (a) ADAPTER LOAD — a LoRA whose base is resident on an in-zone runner with
//!       headroom → publish `Load{Lora}`. **ms, idempotent.**
//!   (b) BASE LOAD/WAKE — a base resident (wake, idempotent / 404-tolerant) or
//!       pulled-to-disk (load) on an in-zone runner → publish `Load{Base}`.
//!   Neither satisfiable in zone → terminal `NoEligibleRunner` (status note,
//!       nothing published — enrol a runner or pull the model).
//!
//! ## Residency fail-closed
//!
//! Each runner advertises its zone in its interface catalog
//! (`RunnerInterfaceCatalog.residency_zone`). A zoned model places ONLY on a
//! runner whose zone is strictly equal (an unknown-zone runner is EXCLUDED for a
//! zoned model — fail-closed); a zoneless model places on any runner. The zone
//! filter is applied while building the candidate set, so a wrong-zone runner is
//! never even a candidate.
//!
//! ## Sleep detection (a known gap)
//!
//! The runner catalog has no "asleep" flag, so the base leg cannot distinguish a
//! resident-but-slept base from a resident-and-awake one. We therefore publish a
//! WAKE (`Load{Base}`) whenever the wanted base is resident in-zone — `wake_up` is
//! idempotent / 404-tolerant on the agent, so re-issuing each tick is safe
//! (placement is desired-state).

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use aithericon_resources::types::ModelAutoscalePolicy;

use crate::handlers::model_pool::{serving_runner_catalogs, serving_runner_counts};
use crate::models::model_replicas::{in_cooldown, status};
use crate::models::runner::{ModelInterfaceKind, RunnerInterfaceCatalog};
use crate::nats::MekhanNats;
use crate::presence::RunnerPresence;
use crate::runner_commands::{publish_model_command, LoadTarget, ModelCommand};

use super::demand::DemandSource;

/// Default WARM WINDOW (seconds) for a reactive (`scale_to_zero` / `keep_warm`)
/// model whose policy sets no explicit `cooldown_secs`. After a wake the model is
/// held resident for at least this long before idle-eviction may sleep it again,
/// so it survives the cold-load + client cold-start-retry window instead of
/// flapping on the one-shot starved-demand edge. An explicit `cooldown_secs`
/// overrides it. 120s comfortably covers a small-model cold load plus a few
/// exponential-backoff client retries.
const DEFAULT_REACTIVE_WARM_SECS: u64 = 120;

/// One base engine resident on one in-zone runner, with its computed headroom +
/// the adapters already loaded on it. Derived from the runner catalog ∩ the
/// in-zone filter, headroom layered from the router in-flight gauge.
#[derive(Debug, Clone)]
pub struct EngineSlot {
    pub runner_id: Uuid,
    pub base: String,
    /// Free slots = `C − Σ(base + adapters in-flight)`. `None` = unknown budget
    /// (no `max_num_seqs` advertised, or the router poll is unconfigured) → the
    /// cascade treats unknown headroom as AVAILABLE (fail-soft).
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

/// The in-zone candidate surface for one model: the resident base engines (with
/// headroom + loaded adapters) and the pulled-to-disk model ids per in-zone
/// runner. The pure [`plan_placements`] reads exactly this.
#[derive(Debug, Clone, Default)]
pub struct ZoneInventory {
    pub slots: Vec<EngineSlot>,
    /// `runner_id → [model id pulled to disk]` for the in-zone runners (loadable
    /// without a download even when not currently resident).
    pub pulled: HashMap<Uuid, Vec<String>>,
}

/// One publish the placement plan resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementAction {
    /// Load this LoRA adapter onto the base engine on `runner_id`.
    LoadAdapter {
        runner_id: Uuid,
        adapter_id: String,
        base: String,
        source_uri: Option<String>,
    },
    /// Load/wake the base engine on `runner_id` (idempotent — `Load{Base}` both
    /// wakes a slept base and loads a pulled one).
    LoadBase { runner_id: Uuid, base: String },
}

/// The placement decision for one model: the set of loads to publish to reach the
/// desired runner spread, or a terminal "nothing in zone can serve it".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementPlan {
    /// Publish each action (may be empty when already spread to the target count).
    Place { actions: Vec<PlacementAction> },
    /// No in-zone runner serves / can host the base — enrol a runner or pull the
    /// model. Nothing published; the row is marked with a note.
    NoEligibleRunner,
}

/// The PURE spread-to-N placement decision. Loads the model onto up to
/// `desired_n` distinct in-zone runners, cheapest-first:
///
/// - **LoRA** (`policy.base.is_some()`): a runner is a candidate if the base is
///   resident with headroom; already-serving runners count toward the target and
///   the shortfall is filled by the least-loaded candidates. No in-zone base ⇒
///   `NoEligibleRunner`.
/// - **Base**: a runner is a candidate if the base is resident (wake, idempotent)
///   or pulled-to-disk (load). Resident runners are preferred (least-loaded
///   first), then pulled runners. None of either ⇒ `NoEligibleRunner`.
pub fn plan_placements(
    policy: &ModelAutoscalePolicy,
    slots: &[EngineSlot],
    pulled: &HashMap<Uuid, Vec<String>>,
    desired_n: u32,
) -> PlacementPlan {
    let n = desired_n.max(1) as usize;
    let wanted_base = policy
        .base
        .clone()
        .unwrap_or_else(|| policy.model_id.clone());

    if policy.base.is_some() {
        // ── (a) LoRA adapter ──────────────────────────────────────────────────
        let adapter = &policy.model_id;
        let serving: HashSet<Uuid> = slots
            .iter()
            .filter(|s| s.base == wanted_base && s.adapters.iter().any(|a| a == adapter))
            .map(|s| s.runner_id)
            .collect();

        // Candidate base engines: right base, has headroom, adapter not yet loaded,
        // runner not already serving. Least-loaded (most headroom) first.
        let mut loadable: Vec<&EngineSlot> = slots
            .iter()
            .filter(|s| {
                s.base == wanted_base
                    && s.has_headroom()
                    && !s.adapters.iter().any(|a| a == adapter)
                    && !serving.contains(&s.runner_id)
            })
            .collect();
        loadable.sort_by_key(|s| std::cmp::Reverse(s.headroom.unwrap_or(u32::MAX)));

        let need = n.saturating_sub(serving.len());
        let mut seen = serving.clone();
        let mut actions = Vec::new();
        for s in loadable {
            if actions.len() >= need {
                break;
            }
            if seen.insert(s.runner_id) {
                actions.push(PlacementAction::LoadAdapter {
                    runner_id: s.runner_id,
                    adapter_id: adapter.clone(),
                    base: wanted_base.clone(),
                    source_uri: None,
                });
            }
        }

        if serving.is_empty() && actions.is_empty() {
            return PlacementPlan::NoEligibleRunner;
        }
        PlacementPlan::Place { actions }
    } else {
        // ── (b) Base model ────────────────────────────────────────────────────
        // Resident runners (wake, idempotent — covers slept), least-loaded first.
        let mut resident: Vec<&EngineSlot> =
            slots.iter().filter(|s| s.base == wanted_base).collect();
        resident.sort_by_key(|s| std::cmp::Reverse(s.headroom.unwrap_or(u32::MAX)));
        let resident_ids: HashSet<Uuid> = resident.iter().map(|s| s.runner_id).collect();

        // Loadable runners: have the base pulled to disk, not currently resident.
        let mut loadable_ids: Vec<Uuid> = pulled
            .iter()
            .filter(|(rid, ids)| {
                !resident_ids.contains(rid) && ids.iter().any(|m| m == &wanted_base)
            })
            .map(|(rid, _)| *rid)
            .collect();
        loadable_ids.sort(); // deterministic ordering

        let mut seen = HashSet::new();
        let mut actions = Vec::new();
        for s in &resident {
            if actions.len() >= n {
                break;
            }
            if seen.insert(s.runner_id) {
                actions.push(PlacementAction::LoadBase {
                    runner_id: s.runner_id,
                    base: wanted_base.clone(),
                });
            }
        }
        for rid in loadable_ids {
            if actions.len() >= n {
                break;
            }
            if seen.insert(rid) {
                actions.push(PlacementAction::LoadBase {
                    runner_id: rid,
                    base: wanted_base.clone(),
                });
            }
        }

        if actions.is_empty() {
            return PlacementPlan::NoEligibleRunner;
        }
        PlacementPlan::Place { actions }
    }
}

/// The PURE idle-eviction decision (vLLM sleep). Returns `Some((runner_id, base))`
/// to SLEEP a resident base model — exactly when ALL hold:
///
/// - the policy OPTED IN (`idle_evict == Some(true)`),
/// - demand has dropped to zero (`demand_zero`),
/// - the actuation cooldown is NOT active (`!in_cooldown` — the flap guard),
/// - the wanted base IS resident on an in-zone slot (nothing to sleep otherwise).
///
/// LoRA policies never idle-evict here (the base engine owns the GPU residency), so
/// a policy with a `base` back-pointer returns `None`.
pub fn plan_idle_eviction(
    policy: &ModelAutoscalePolicy,
    in_zone_slots: &[EngineSlot],
    demand_zero: bool,
    in_cooldown: bool,
) -> Option<(Uuid, String)> {
    if policy.idle_evict != Some(true) || !demand_zero || in_cooldown {
        return None;
    }
    if policy.base.is_some() {
        return None;
    }
    let wanted_base = policy.model_id.clone();
    in_zone_slots
        .iter()
        .find(|s| s.base == wanted_base)
        .map(|s| (s.runner_id, wanted_base))
}

/// The load-timing state transition for one model on one reconcile tick. PURE:
/// maps (was the wanted base resident BEFORE this tick's action, is it resident
/// NOW, is a measurement already in flight) → the single `model_replicas` write.
///
/// - **StartCold**: a cold `LoadBase` was published (base NOT resident → loaded
///   from pulled) and no measurement is in flight → stamp `load_started_at = now()`.
///   A warm wake of an already-resident base does NOT start one.
/// - **Finish**: the base IS now observed resident AND a measurement was in flight
///   → compute `last_load_duration_ms`, set `load_finished_at = now()`, CLEAR
///   `load_started_at` so the next cold load re-measures.
/// - **None**: nothing to write (warm wake, still-loading, or never measuring).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadTimingUpdate {
    /// Stamp `load_started_at = now()` (only when it was NULL).
    StartCold,
    /// Stamp `load_finished_at = now()`, compute duration, clear `load_started_at`.
    Finish,
    /// No write.
    None,
}

/// The PURE load-timing decision for one model this tick.
///
/// * `cold_load_published` — this tick we published a `LoadBase` for a base that
///   was NOT resident on its target runner (a cold load from `pulled`, not a warm
///   wake). Drives the START stamp.
/// * `now_resident` — the wanted base IS observed resident on at least one in-zone
///   runner right now (the runner-catalog signal already read for the zone
///   inventory). Drives the FINISH stamp.
/// * `load_in_flight` — `load_started_at IS NOT NULL` on the row (a measurement is
///   already running).
///
/// FINISH wins over START in the (degenerate) case both are true on the same tick:
/// if the base is already resident we are not actually cold-loading, so a stale
/// in-flight measurement should be closed out rather than left dangling.
pub fn load_timing_transition(
    cold_load_published: bool,
    now_resident: bool,
    load_in_flight: bool,
) -> LoadTimingUpdate {
    if load_in_flight && now_resident {
        // The base finished loading (or was already there) → close the measurement.
        LoadTimingUpdate::Finish
    } else if cold_load_published && !load_in_flight {
        // A fresh cold load began and nothing is being measured yet → start.
        LoadTimingUpdate::StartCold
    } else {
        LoadTimingUpdate::None
    }
}

/// One reconcile pass over every `model_states` policy. Per-policy failures
/// fail-soft (recorded on the row + carry on); a setup `sqlx::Error` (the
/// policy-list fetch) kills the tick.
pub async fn reconcile_placement(
    db: &PgPool,
    nats: &MekhanNats,
    runner_presence: &RunnerPresence,
    demand: Option<&dyn DemandSource>,
) -> Result<(), sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct PolicyScanRow {
        workspace_id: Uuid,
        #[sqlx(flatten)]
        policy: super::ModelStatePolicyRow,
    }
    let policies: Vec<PolicyScanRow> = sqlx::query_as(
        "SELECT workspace_id, model_id, base, autoscale_mode, desired_replicas, \
                scale_up_threshold, scale_down_threshold, cooldown_secs, \
                residency_zone, idle_evict \
         FROM model_states \
         WHERE autoscale_mode IS NOT NULL",
    )
    .fetch_all(db)
    .await?;

    // Per-workspace caches (one scan per workspace per tick).
    let mut catalogs_by_ws: HashMap<Uuid, Vec<(Uuid, RunnerInterfaceCatalog)>> = HashMap::new();
    let mut counts_by_ws: HashMap<Uuid, HashMap<String, u32>> = HashMap::new();

    for scan in policies {
        let workspace_id = scan.workspace_id;
        let policy: ModelAutoscalePolicy = scan.policy.into_policy();
        let model_id = policy.model_id.clone();

        let model_demand = match demand {
            Some(src) => src.demand_for(&policy.model_id).await.unwrap_or(0.0),
            None => 0.0,
        };
        let demand_zero = model_demand <= 0.0;
        let reactive = policy.mode != "manual";

        let catalogs = match catalogs_by_ws.entry(workspace_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(serving_runner_catalogs(db, runner_presence, workspace_id).await)
            }
        };
        let inv = build_zone_inventory(catalogs, &policy, demand).await;

        // Reactive modes at zero demand: idle-evict (if opted in), else leave alone.
        if reactive && demand_zero {
            if policy.idle_evict == Some(true) {
                if let Err(e) =
                    apply_idle_eviction(db, nats, workspace_id, &model_id, &policy, &inv.slots)
                        .await
                {
                    tracing::warn!(%workspace_id, %model_id, "idle-eviction failed: {e}");
                    mark_placement_failed(db, workspace_id, &model_id, &policy, &e).await;
                }
            }
            continue;
        }

        // Place onto up to `desired_replicas` in-zone runners (default 1).
        let desired_n = policy.desired_replicas.unwrap_or(1).max(1);
        let plan = plan_placements(&policy, &inv.slots, &inv.pulled, desired_n);

        let counts = match counts_by_ws.entry(workspace_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(serving_runner_counts(db, runner_presence, workspace_id).await)
            }
        };
        let observed = counts.get(&model_id).copied().unwrap_or(0);

        // Resident signal for load-timing: which runners already host the wanted
        // base RIGHT NOW (the same in-zone slot set the planner read — no second
        // catalog scan). `now_resident` drives the FINISH stamp; the per-runner set
        // lets `apply_plan` classify each `LoadBase` as a cold load vs a warm wake.
        let wanted_base = policy.base.clone().unwrap_or_else(|| model_id.clone());
        let resident_runners: HashSet<Uuid> = inv
            .slots
            .iter()
            .filter(|s| s.base == wanted_base)
            .map(|s| s.runner_id)
            .collect();
        let now_resident = !resident_runners.is_empty();

        match apply_plan(
            db,
            nats,
            workspace_id,
            &model_id,
            &policy,
            plan,
            desired_n,
            observed,
            &resident_runners,
        )
        .await
        {
            Ok(cold_load_published) => {
                // Single load-timing UPDATE per model per tick (best-effort).
                apply_load_timing(
                    db,
                    workspace_id,
                    &model_id,
                    cold_load_published,
                    now_resident,
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(%workspace_id, %model_id, "placement apply failed: {e}");
                mark_placement_failed(db, workspace_id, &model_id, &policy, &e).await;
            }
        }
    }
    Ok(())
}

/// Build the in-zone candidate inventory for one model from the workspace's
/// present-runner catalogs: every runner whose advertised zone satisfies the
/// model's residency requirement, with its resident base engines (headroom layered
/// from the router gauge) and its pulled-to-disk set. A zoned model takes ONLY
/// runners with a strictly-equal KNOWN zone (fail-closed); a zoneless model takes
/// all present runners.
async fn build_zone_inventory(
    catalogs: &[(Uuid, RunnerInterfaceCatalog)],
    policy: &ModelAutoscalePolicy,
    demand: Option<&dyn DemandSource>,
) -> ZoneInventory {
    let wanted_zone = policy.residency_zone.trim();
    let mut inv = ZoneInventory::default();

    for (runner_id, catalog) in catalogs {
        // Zone gate: a zoned model needs the runner to advertise a strictly-equal
        // zone (unknown zone ⇒ excluded, fail-closed); a zoneless model takes any.
        if !wanted_zone.is_empty() {
            match catalog.residency_zone.as_deref() {
                Some(z) if z.trim() == wanted_zone => {}
                _ => continue,
            }
        }

        inv.pulled.insert(*runner_id, catalog.pulled.clone());

        // Base engines on this runner (with their advertised C).
        let mut bases: HashMap<String, Option<u32>> = HashMap::new();
        for e in &catalog.models {
            if e.kind == ModelInterfaceKind::Base {
                bases.insert(e.model_id.clone(), e.max_num_seqs);
            }
        }
        // Adapters grouped by base back-pointer.
        let mut adapters_by_base: HashMap<String, Vec<String>> = HashMap::new();
        for e in &catalog.models {
            if e.kind == ModelInterfaceKind::Lora {
                if let Some(base) = &e.base {
                    adapters_by_base
                        .entry(base.clone())
                        .or_default()
                        .push(e.model_id.clone());
                    bases.entry(base.clone()).or_insert(None);
                }
            }
        }

        for (base, c) in bases {
            let adapters = adapters_by_base.get(&base).cloned().unwrap_or_default();
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
            inv.slots.push(EngineSlot {
                runner_id: *runner_id,
                base,
                headroom,
                adapters,
            });
        }
    }
    inv
}

/// Carry out the placement plan: publish each load + record the row `active` with
/// the live observed count, or mark `provisioning` with a note when nothing in
/// zone can serve the model.
///
/// Returns `true` when at least one published `LoadBase` was a COLD load — its
/// target runner did NOT already host the base (`!resident_runners.contains`), so
/// it was loaded from `pulled` rather than woken. The caller uses this to stamp the
/// cold-start measurement. Adapter loads + warm base wakes do not count as cold.
#[allow(clippy::too_many_arguments)]
async fn apply_plan(
    db: &PgPool,
    nats: &MekhanNats,
    workspace_id: Uuid,
    model_id: &str,
    policy: &ModelAutoscalePolicy,
    plan: PlacementPlan,
    desired_n: u32,
    observed: u32,
    resident_runners: &HashSet<Uuid>,
) -> Result<bool, String> {
    match plan {
        PlacementPlan::NoEligibleRunner => {
            let msg = format!(
                "no registered in-zone runner serves '{}' — enrol a runner or load/pull the model",
                policy.base.as_deref().unwrap_or(model_id)
            );
            upsert_status(
                db,
                workspace_id,
                model_id,
                policy,
                None,
                Some(observed),
                status::PROVISIONING,
                None,
                Some(&msg),
            )
            .await;
            Ok(false)
        }
        PlacementPlan::Place { actions } => {
            // A `LoadBase` onto a runner that does NOT already host the base is a
            // COLD load (loaded from `pulled`); onto a resident runner it is a warm
            // wake. Adapter loads never count.
            let mut cold_load_published = false;
            for action in actions {
                let (runner_id, cmd) = match action {
                    PlacementAction::LoadAdapter {
                        runner_id,
                        adapter_id,
                        base,
                        source_uri,
                    } => (
                        runner_id,
                        ModelCommand::Load {
                            target: LoadTarget::Lora {
                                adapter_id,
                                base,
                                source_uri,
                            },
                        },
                    ),
                    PlacementAction::LoadBase { runner_id, base } => {
                        if !resident_runners.contains(&runner_id) {
                            cold_load_published = true;
                        }
                        (
                            runner_id,
                            ModelCommand::Load {
                                target: LoadTarget::Base { model_id: base },
                            },
                        )
                    }
                };
                publish_model_command(nats, runner_id, &cmd)
                    .await
                    .map_err(|e| format!("publish load: {e}"))?;
            }
            // Stamp `last_actuated_at` on the placement: a wake (or a steady-
            // demand re-assertion) starts/refreshes the WARM-WINDOW clock that
            // `apply_idle_eviction` reads. Without this, a `scale_to_zero` model
            // woken by a one-shot starved-demand edge is re-evicted on the very
            // next zero-demand tick (the edge is gone) — it flaps and never stays
            // resident long enough to drain the client's cold-start retries.
            upsert_status(
                db,
                workspace_id,
                model_id,
                policy,
                Some(desired_n),
                Some(observed),
                status::ACTIVE,
                Some(Utc::now()),
                None,
            )
            .await;
            Ok(cold_load_published)
        }
    }
}

/// Carry out the idle-eviction decision (vLLM sleep) for one zero-demand, opted-in
/// model. Loads the durable cooldown anchor, runs the pure [`plan_idle_eviction`],
/// and on `Some` publishes an `Unload{Base}` (→ vLLM `/sleep`) + marks the row
/// `sleeping` with a fresh `last_actuated_at` (the flap guard). On `None` it is a
/// no-op (still hot, in cooldown, not opted in, or the base isn't resident).
async fn apply_idle_eviction(
    db: &PgPool,
    nats: &MekhanNats,
    workspace_id: Uuid,
    model_id: &str,
    policy: &ModelAutoscalePolicy,
    slots: &[EngineSlot],
) -> Result<(), String> {
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
    // Reactive models with NO explicit cooldown still get a default WARM WINDOW:
    // a model just woken from sleep must stay resident long enough to finish a
    // cold load and absorb the client's cold-start retries before we let it idle
    // back to zero. `cooldown_secs = NULL` previously meant `in_cooldown` always
    // returned false → the model was re-evicted the instant demand read 0 (the
    // one-shot starved edge), so it could never serve. An operator-set
    // `cooldown_secs` still wins.
    let warm_secs = policy.cooldown_secs.or(Some(DEFAULT_REACTIVE_WARM_SECS));
    let cooled = in_cooldown(last_actuated, warm_secs, now);

    let Some((runner_id, base)) = plan_idle_eviction(policy, slots, true, cooled) else {
        return Ok(());
    };

    let cmd = ModelCommand::Unload {
        target: LoadTarget::Base { model_id: base },
    };
    publish_model_command(nats, runner_id, &cmd)
        .await
        .map_err(|e| format!("publish idle-eviction sleep: {e}"))?;

    upsert_status(
        db,
        workspace_id,
        model_id,
        policy,
        None,
        None,
        status::SLEEPING,
        Some(now),
        None,
    )
    .await;
    Ok(())
}

/// Upsert the model's `model_replicas` row to the given placement status. Best-
/// effort (the row is a Control-Plane read + the idle-evict cooldown anchor; a
/// failure to write it never blocks the publish). `desired`/`observed` are written
/// only when `Some`; `last_actuated` is written only when `Some` (an active
/// placement is not an actuation, so it leaves the cooldown anchor untouched).
#[allow(clippy::too_many_arguments)]
async fn upsert_status(
    db: &PgPool,
    workspace_id: Uuid,
    model_id: &str,
    policy: &ModelAutoscalePolicy,
    desired: Option<u32>,
    observed: Option<u32>,
    status: &str,
    last_actuated: Option<chrono::DateTime<Utc>>,
    last_error: Option<&str>,
) {
    let residency =
        (!policy.residency_zone.trim().is_empty()).then(|| policy.residency_zone.clone());
    // COALESCE keeps a column unchanged on conflict when the bound value is NULL,
    // so the single upsert serves active / sleeping / no-eligible-runner without
    // clobbering counts or the cooldown anchor it shouldn't touch.
    let _ = sqlx::query(
        "INSERT INTO model_replicas \
            (workspace_id, model_id, desired_count, observed_count, status, \
             residency_zone, last_actuated_at, last_error) \
         VALUES ($1, $2, COALESCE($3, 0), COALESCE($4, 0), $5, $6, $7, $8) \
         ON CONFLICT (workspace_id, model_id) DO UPDATE SET \
            desired_count = COALESCE($3, model_replicas.desired_count), \
            observed_count = COALESCE($4, model_replicas.observed_count), \
            status = EXCLUDED.status, \
            residency_zone = EXCLUDED.residency_zone, \
            last_actuated_at = COALESCE($7, model_replicas.last_actuated_at), \
            last_error = $8, \
            updated_at = NOW()",
    )
    .bind(workspace_id)
    .bind(model_id)
    .bind(desired.map(|v| v as i32))
    .bind(observed.map(|v| v as i32))
    .bind(status)
    .bind(residency)
    .bind(last_actuated)
    .bind(last_error)
    .execute(db)
    .await;
}

/// Apply the load-timing transition for one model this tick (best-effort, a single
/// `UPDATE`). Reads the row's `load_started_at` to know whether a measurement is in
/// flight, runs the PURE [`load_timing_transition`], and writes at most one column
/// set using the DB clock (`now()`) — never Rust wall-clock — so START/FINISH
/// timestamps share the Postgres clock and the duration is skew-free.
///
/// The row is created by `upsert_status` before this runs, so the `UPDATE` always
/// targets an existing row (a placed/active model). A missing row (no placement
/// this tick) is a no-op `UPDATE` — harmless.
async fn apply_load_timing(
    db: &PgPool,
    workspace_id: Uuid,
    model_id: &str,
    cold_load_published: bool,
    now_resident: bool,
) {
    // In-flight = `load_started_at IS NOT NULL` on the row.
    let load_in_flight: bool = sqlx::query_scalar::<_, Option<chrono::DateTime<Utc>>>(
        "SELECT load_started_at FROM model_replicas WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(model_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten()
    .is_some();

    match load_timing_transition(cold_load_published, now_resident, load_in_flight) {
        LoadTimingUpdate::None => {}
        LoadTimingUpdate::StartCold => {
            // Stamp the start ONLY when currently NULL — never reset an in-flight
            // measurement (the `IS NULL` guard makes this idempotent across ticks).
            let _ = sqlx::query(
                "UPDATE model_replicas \
                    SET load_started_at = now(), updated_at = now() \
                 WHERE workspace_id = $1 AND model_id = $2 AND load_started_at IS NULL",
            )
            .bind(workspace_id)
            .bind(model_id)
            .execute(db)
            .await;
        }
        LoadTimingUpdate::Finish => {
            // Close the measurement: duration = now() - load_started_at (ms), stamp
            // finished, CLEAR load_started_at so the next cold load re-measures. The
            // `IS NOT NULL` guard keeps it a no-op if a concurrent tick already
            // closed it.
            let _ = sqlx::query(
                "UPDATE model_replicas \
                    SET last_load_duration_ms = \
                            (EXTRACT(EPOCH FROM (now() - load_started_at)) * 1000)::BIGINT, \
                        load_finished_at = now(), \
                        load_started_at = NULL, \
                        updated_at = now() \
                 WHERE workspace_id = $1 AND model_id = $2 AND load_started_at IS NOT NULL",
            )
            .bind(workspace_id)
            .bind(model_id)
            .execute(db)
            .await;
        }
    }
}

/// Record a placement failure on the model's row (best-effort).
async fn mark_placement_failed(
    db: &PgPool,
    workspace_id: Uuid,
    model_id: &str,
    policy: &ModelAutoscalePolicy,
    error: &str,
) {
    upsert_status(
        db,
        workspace_id,
        model_id,
        policy,
        None,
        None,
        status::FAILED,
        None,
        Some(error),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lora_policy(zone: &str, base: &str) -> ModelAutoscalePolicy {
        ModelAutoscalePolicy {
            model_id: "my-lora".into(),
            residency_zone: zone.into(),
            mode: "keep_warm".into(),
            desired_replicas: None,
            scale_up_threshold: None,
            scale_down_threshold: None,
            cooldown_secs: None,
            base: Some(base.into()),
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
            base: None,
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

    fn no_pulled() -> HashMap<Uuid, Vec<String>> {
        HashMap::new()
    }

    #[test]
    fn adapter_loads_when_base_resident_with_headroom() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(4), &[])];
        let plan = plan_placements(&lora_policy("eu", "llama"), &slots, &no_pulled(), 1);
        assert_eq!(
            plan,
            PlacementPlan::Place {
                actions: vec![PlacementAction::LoadAdapter {
                    runner_id: r,
                    adapter_id: "my-lora".into(),
                    base: "llama".into(),
                    source_uri: None,
                }]
            }
        );
    }

    #[test]
    fn adapter_already_loaded_is_a_noop_place() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(4), &["my-lora"])];
        // Already serving on the only runner, target 1 → no further actions.
        let plan = plan_placements(&lora_policy("eu", "llama"), &slots, &no_pulled(), 1);
        assert_eq!(plan, PlacementPlan::Place { actions: vec![] });
    }

    #[test]
    fn adapter_no_base_in_zone_is_no_eligible_runner() {
        // No base resident anywhere → nowhere to host the adapter.
        let plan = plan_placements(&lora_policy("eu", "llama"), &[], &no_pulled(), 1);
        assert_eq!(plan, PlacementPlan::NoEligibleRunner);
    }

    #[test]
    fn adapter_no_headroom_is_no_eligible_runner() {
        let r = Uuid::new_v4();
        // Base resident but zero headroom, adapter not loaded → cannot place.
        let slots = vec![slot(r, "llama", Some(0), &[])];
        let plan = plan_placements(&lora_policy("eu", "llama"), &slots, &no_pulled(), 1);
        assert_eq!(plan, PlacementPlan::NoEligibleRunner);
    }

    #[test]
    fn base_resident_wakes() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(0), &[])];
        let plan = plan_placements(&base_policy("eu"), &slots, &no_pulled(), 1);
        assert_eq!(
            plan,
            PlacementPlan::Place {
                actions: vec![PlacementAction::LoadBase {
                    runner_id: r,
                    base: "llama".into()
                }]
            }
        );
    }

    #[test]
    fn base_loads_from_pulled_when_not_resident() {
        let r = Uuid::new_v4();
        let mut pulled = HashMap::new();
        pulled.insert(r, vec!["llama".to_string()]);
        // Nothing resident, but the runner has it pulled → Load{Base}.
        let plan = plan_placements(&base_policy("eu"), &[], &pulled, 1);
        assert_eq!(
            plan,
            PlacementPlan::Place {
                actions: vec![PlacementAction::LoadBase {
                    runner_id: r,
                    base: "llama".into()
                }]
            }
        );
    }

    #[test]
    fn base_no_resident_no_pulled_is_no_eligible_runner() {
        let plan = plan_placements(&base_policy("eu"), &[], &no_pulled(), 1);
        assert_eq!(plan, PlacementPlan::NoEligibleRunner);
    }

    #[test]
    fn base_spreads_to_n_runners_resident_first_then_pulled() {
        let r1 = Uuid::new_v4();
        let r2 = Uuid::new_v4();
        let r3 = Uuid::new_v4();
        // r1 resident, r2+r3 have it pulled; target 2 → r1 (resident) + one pulled.
        let slots = vec![slot(r1, "llama", Some(8), &[])];
        let mut pulled = HashMap::new();
        pulled.insert(r2, vec!["llama".to_string()]);
        pulled.insert(r3, vec!["llama".to_string()]);
        let mut p = base_policy("eu");
        p.desired_replicas = Some(2);
        let plan = plan_placements(&p, &slots, &pulled, 2);
        let PlacementPlan::Place { actions } = plan else {
            panic!("expected Place, got {plan:?}");
        };
        assert_eq!(actions.len(), 2);
        // r1 (resident) is first; the second is one of the pulled runners.
        assert_eq!(
            actions[0],
            PlacementAction::LoadBase {
                runner_id: r1,
                base: "llama".into()
            }
        );
        assert!(matches!(
            actions[1],
            PlacementAction::LoadBase { runner_id, .. } if runner_id == r2 || runner_id == r3
        ));
    }

    #[test]
    fn adapter_spreads_to_n_filling_shortfall() {
        let r1 = Uuid::new_v4();
        let r2 = Uuid::new_v4();
        // r1 already serves the adapter, r2 has the base with headroom; target 2 →
        // one new load onto r2 (r1 counts toward the target).
        let slots = vec![
            slot(r1, "llama", Some(4), &["my-lora"]),
            slot(r2, "llama", Some(4), &[]),
        ];
        let mut p = lora_policy("eu", "llama");
        p.desired_replicas = Some(2);
        let plan = plan_placements(&p, &slots, &no_pulled(), 2);
        assert_eq!(
            plan,
            PlacementPlan::Place {
                actions: vec![PlacementAction::LoadAdapter {
                    runner_id: r2,
                    adapter_id: "my-lora".into(),
                    base: "llama".into(),
                    source_uri: None,
                }]
            }
        );
    }

    #[test]
    fn unknown_headroom_is_treated_as_available() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", None, &[])];
        let plan = plan_placements(&lora_policy("eu", "llama"), &slots, &no_pulled(), 1);
        assert!(matches!(plan, PlacementPlan::Place { .. }));
    }

    // ── idle-eviction (vLLM sleep) decision ───────────────────────────────────

    fn evictable_base_policy(zone: &str, idle_evict: bool) -> ModelAutoscalePolicy {
        ModelAutoscalePolicy {
            idle_evict: Some(idle_evict),
            ..base_policy(zone)
        }
    }

    #[test]
    fn idle_evict_sleeps_resident_base_at_zero_demand() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(8), &[])];
        let out = plan_idle_eviction(&evictable_base_policy("eu", true), &slots, true, false);
        assert_eq!(out, Some((r, "llama".to_string())));
    }

    #[test]
    fn idle_evict_noop_when_not_opted_in() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(8), &[])];
        assert_eq!(
            plan_idle_eviction(&evictable_base_policy("eu", false), &slots, true, false),
            None
        );
        assert_eq!(
            plan_idle_eviction(&base_policy("eu"), &slots, true, false),
            None
        );
    }

    #[test]
    fn idle_evict_noop_within_cooldown() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(8), &[])];
        assert_eq!(
            plan_idle_eviction(&evictable_base_policy("eu", true), &slots, true, true),
            None
        );
    }

    #[test]
    fn idle_evict_noop_when_base_not_resident() {
        assert_eq!(
            plan_idle_eviction(&evictable_base_policy("eu", true), &[], true, false),
            None
        );
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "other-model", Some(8), &[])];
        assert_eq!(
            plan_idle_eviction(&evictable_base_policy("eu", true), &slots, true, false),
            None
        );
    }

    #[test]
    fn idle_evict_noop_when_demand_nonzero() {
        let r = Uuid::new_v4();
        let slots = vec![slot(r, "llama", Some(8), &[])];
        assert_eq!(
            plan_idle_eviction(&evictable_base_policy("eu", true), &slots, false, false),
            None
        );
    }

    // ── load-timing transition (cold-vs-warm gate) ────────────────────────────

    #[test]
    fn cold_load_starts_measurement() {
        // A cold LoadBase published (base not resident), nothing in flight → START.
        // cold=true, now_resident=false (the base is loading, not yet resident).
        assert_eq!(
            load_timing_transition(true, false, false),
            LoadTimingUpdate::StartCold
        );
    }

    #[test]
    fn warm_wake_does_not_start_measurement() {
        // A warm wake = a LoadBase onto an already-resident runner → apply_plan
        // reports cold_load_published=false, and the base is resident now. With no
        // measurement in flight that is a no-op (no start, nothing to finish).
        assert_eq!(
            load_timing_transition(false, true, false),
            LoadTimingUpdate::None
        );
    }

    #[test]
    fn observing_residency_finishes_in_flight_measurement() {
        // A measurement is in flight and the base is now observed resident → FINISH
        // (compute duration, clear load_started_at). The classic cold-load close-out.
        assert_eq!(
            load_timing_transition(false, true, true),
            LoadTimingUpdate::Finish
        );
    }

    #[test]
    fn in_flight_but_not_yet_resident_is_a_noop() {
        // Still loading: measurement in flight, base not resident yet, no fresh cold
        // load this tick → no write, keep measuring.
        assert_eq!(
            load_timing_transition(false, false, true),
            LoadTimingUpdate::None
        );
    }

    #[test]
    fn cold_load_while_already_measuring_does_not_restart() {
        // A second cold load published while a measurement is already in flight and
        // the base isn't resident yet → no write (the StartCold guard is
        // `!load_in_flight`); the original measurement keeps running.
        assert_eq!(
            load_timing_transition(true, false, true),
            LoadTimingUpdate::None
        );
    }

    #[test]
    fn finish_wins_over_start_when_both_hold() {
        // Degenerate same-tick: a stale in-flight measurement AND the base is already
        // resident (so a published "cold" load is really a wake) → FINISH closes out
        // the dangling measurement rather than leaving it open.
        assert_eq!(
            load_timing_transition(true, true, true),
            LoadTimingUpdate::Finish
        );
    }

    #[test]
    fn nothing_happening_is_a_noop() {
        assert_eq!(
            load_timing_transition(false, false, false),
            LoadTimingUpdate::None
        );
    }
}
