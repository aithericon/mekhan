//! Runner presence adapter (Phase 3 — presence-lease pool capacity).
//!
//! A `runner_group` resource is a capacity-LESS pool ([`crate::petri::presence_pool_net`]):
//! its capacity is not a seeded count but is driven by runner **presence**. This
//! module is mekhan's adapter that turns the runner data-plane heartbeat into
//! pool-net admission/reap (the shared substrate lives in [`super::core`]):
//!
//! 1. **SUBSCRIBE** to `runner.*.presence`. Each message is a liveness ping from
//!    a runner's data plane (Phase 2 JWT already grants `runner.{id}.presence`).
//!    The `runner_id` is parsed from the SUBJECT, never the payload. The payload
//!    carries an advisory `concurrency: C` (default 1; P3) — the number of
//!    simultaneous leases the runner can serve. On the ABSENT→PRESENT edge we
//!    inject **C** `presence_acquire` tokens
//!    `{ unit_id: "{runner_id}#{slot}", runner_id, executor_namespace, caps }`
//!    (slot 0..C) into the runner's pool net via the cross-net bridge subject
//!    `petri.bridge.pool-<rid>.presence_acquire` — one per slot, each with a
//!    per-slot dedup id so a redelivery suppresses exactly that slot.
//!    `executor_namespace` + `caps` come from the TRUSTED `runners` DB row,
//!    NEVER from the wire payload. GROW-EAGER: on a later heartbeat whose wire C
//!    exceeds the applied C, the new slots are injected immediately; SHRINK is
//!    LAZY (the surplus drains on release / full expire).
//!
//! 2. **SWEEP** a background loop tracks the last-renewal instant + applied C per
//!    runner_id in memory. On a TTL miss the runner is marked absent and
//!    **applied-C** BARE `presence_expired { runner_id }` SIGNALS are injected via
//!    `petri.signal.pool-<rid>.presence_expired` — one per slot, since each
//!    signal is consumed once and reaps exactly one of the runner's C slots
//!    (reap-ALL-by-runner_id). The net's `t_reap_free` / `t_reap_held`
//!    discriminate free-vs-held by input place, so mekhan keeps NO holder
//!    tracking.
//!
//! ## Idempotency + false-positive avoidance
//!
//! The in-memory `PresenceMap` keys each known runner to a `PresenceEntry`
//! holding its last-seen [`Instant`], its resolved pool net id, and a `present`
//! flag. Acquire fires ONLY on the absent→present edge (`present == false`),
//! then flips `present = true`; subsequent heartbeats only bump `last_seen`. A
//! sweep that finds `now - last_seen > ttl` on a `present` entry injects ONE
//! expire signal and flips `present = false` — so a runner is reaped at most
//! once per presence episode, and the next heartbeat cleanly re-acquires.

use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use futures::StreamExt;

use super::core::{self, ExpiredSlots, PoolInjection, PoolSignal};
use crate::compiler::well_known;
use crate::fleet::FleetLiveness;
use crate::models::runner::{HostInfo, RunnerRow};
use crate::nats::MekhanNats;
use crate::petri::client::PetriClient;

/// Default presence TTL: a runner missing this long is reaped. The runner's
/// data plane is expected to renew well inside this window (Phase 2 sets a
/// heartbeat interval comfortably shorter). Overridable via
/// `MEKHAN__RUNNERS__PRESENCE_TTL_SECS`.
const DEFAULT_PRESENCE_TTL_SECS: u64 = 30;

/// How often the sweep loop wakes to look for TTL misses. Kept well below the
/// TTL so the reap latency is bounded by ~one sweep interval past expiry.
const SWEEP_INTERVAL_SECS: u64 = 5;

/// Read the configured presence TTL (seconds), defaulting to
/// [`DEFAULT_PRESENCE_TTL_SECS`]. A parse failure or non-positive value falls
/// back to the default with a WARN so a typo can't silently disable reaping.
fn presence_ttl() -> Duration {
    core::env_ttl_secs(
        "MEKHAN__RUNNERS__PRESENCE_TTL_SECS",
        DEFAULT_PRESENCE_TTL_SECS,
    )
}

/// One tracked runner's presence state.
pub(crate) struct PresenceEntry {
    /// Most recent presence heartbeat instant.
    last_seen: Instant,
    /// The number of pool slots mekhan has APPLIED for this runner — the count
    /// of `presence_acquire` tokens injected (and not yet fully expired). Drives
    /// the grow-eager / shrink-lazy delta (P3): a heartbeat whose wire C exceeds
    /// this eagerly injects the new slots and bumps it; a smaller wire C just
    /// lowers the stored target (the surplus drains on release / full expire).
    /// The sweep injects exactly this many expire signals on a TTL miss so every
    /// applied slot is reaped (reap-all-by-runner_id). `0` for a liveness-only
    /// runner with no pool to admit into.
    concurrency: u32,
    /// Pool net id (`pool-<resource_id>`) the runner's presence is admitted to.
    /// Resolved once on the acquire edge and cached so the sweep can inject the
    /// expire signal without another DB round-trip.
    pool_net_id: String,
    /// The runner's `group` alias (the `resources.path` of its `runner_group`),
    /// cached from the trusted DB row on the acquire edge. This is the SAME
    /// alias string a step's `CapacityBinding.alias` carries, so the
    /// publish-time backend-coverage warning can match a presence-pool step to
    /// the live runners in its target pool ([`RunnerPresence::pool_covers`])
    /// without resolving net ids. `None` for a liveness-only runner (no pool).
    pool_alias: Option<String>,
    /// The runner's self-reported `backends` — the executor backend wire-names
    /// its daemon registered (`["python", ...]`), from the presence PAYLOAD (the
    /// set-membership dimension, docs/23 §4; advisory wire-truth — see
    /// `executor`'s `presence` module). Used for fleet visibility + the
    /// publish-time coverage warning, NEVER to gate the engine `t_grant` guard
    /// (caps remain authoritative there).
    backends: Vec<String>,
    /// The runner's self-reported host / hardware fingerprint (hostname,
    /// accelerator, IP) from the presence PAYLOAD — refreshed on every heartbeat
    /// alongside `backends`. Advisory wire-truth, surfaced for fleet visibility
    /// only (never gates placement). `None` until a heartbeat carries a `host`
    /// block (older runner / probe failure).
    host: Option<HostInfo>,
    /// Whether mekhan currently considers the runner PRESENT (a `presence_acquire`
    /// has been injected and no expire has been injected since). Drives the
    /// absent→present acquire edge + the present→absent expire edge.
    present: bool,
}

/// In-memory presence map: `runner_id` → its tracked state (the shared
/// [`core::EntryMap`], keyed by the runner's STABLE UUID — unlike the human
/// adapter's composite `(capacity, member)` key).
type PresenceMap = core::EntryMap<Uuid, PresenceEntry>;

/// Public newtype wrapper around the [`PresenceMap`] so the `pub` [`crate::AppState`]
/// can hold a handle to the live presence map WITHOUT leaking the `pub(crate)`
/// [`PresenceEntry`]/[`PresenceMap`] types (which would trip the
/// `private_interfaces` lint that CI's `-D warnings` rejects).
///
/// The read API (`GET /api/v1/runners/presence`) reads through [`Self::snapshot`];
/// the presence-controller tasks share the SAME inner map via [`Self::map`].
#[derive(Clone)]
pub struct RunnerPresence(PresenceMap);

impl RunnerPresence {
    /// Construct a fresh, empty presence handle. The controller tasks + the read
    /// API share this one map.
    pub fn new() -> Self {
        Self(core::new_entry_map())
    }

    /// Borrow the inner shared map for the controller tasks (subscriber + sweep).
    pub(crate) fn map(&self) -> &PresenceMap {
        &self.0
    }

    /// Snapshot the live presence map for the read API (the shared
    /// [`core::snapshot_entries`] walk): each tracked runner becomes a
    /// [`RunnerPresenceSnapshot`] with the elapsed time since its last heartbeat.
    pub async fn snapshot(&self) -> Vec<crate::models::runner::RunnerPresenceSnapshot> {
        core::snapshot_entries(&self.0, |runner_id, entry, now| {
            crate::models::runner::RunnerPresenceSnapshot {
                runner_id: *runner_id,
                present: entry.present,
                last_seen_ms_ago: now.duration_since(entry.last_seen).as_millis() as u64,
                backends: entry.backends.clone(),
                host: entry.host.clone(),
            }
        })
        .await
    }

    /// Whether ANY currently-present runner in the pool aliased `pool_alias`
    /// advertises the backend wire-name `wire`.
    ///
    /// The runner-side, POOL-SCOPED coverage primitive: it answers "is this
    /// presence-pool step's backend covered by a live runner in its TARGET
    /// pool?", matching by the `pool` alias (the `resources.path` shared by
    /// `runner.group` and `CapacityBinding.alias`), so no net-id resolution is
    /// needed. Advisory only — a `false` here never blocks anything.
    ///
    /// As of docs/24 S2 the publish-time backend-coverage warning no longer calls
    /// this — it uses the fleet-wide [`crate::fleet::FleetLiveness::serves_backend`]
    /// union (worker OR runner, pool-agnostic). This pool-scoped variant is kept
    /// for callers that need per-pool coverage and is exercised by its unit test.
    pub async fn pool_covers(&self, pool_alias: &str, wire: &str) -> bool {
        let map = self.0.lock().await;
        map.values().any(|e| {
            e.present
                && e.pool_alias.as_deref() == Some(pool_alias)
                && e.backends.iter().any(|b| b == wire)
        })
    }

    /// Map every currently-PRESENT runner's UUID → its pool alias (`resources.path`
    /// of its `runner_group`). Runners with no pool (`pool_alias == None`) and
    /// absent runners are omitted. The presence snapshot carries each runner's
    /// concurrency `C` but NOT its pool tag, so the two registries are joined on the
    /// runner UUID; mirrored on every heartbeat, a momentary drift just
    /// under/over-counts one node for one tick.
    pub async fn pool_membership(&self) -> std::collections::HashMap<Uuid, String> {
        let map = self.0.lock().await;
        map.iter()
            .filter_map(|(id, e)| {
                if !e.present {
                    return None;
                }
                e.pool_alias.as_ref().map(|alias| (*id, alias.clone()))
            })
            .collect()
    }

    /// Test-only: seed a runner's pool membership directly so `pool_membership`
    /// can be exercised without the full acquire/heartbeat machinery.
    #[cfg(test)]
    pub async fn test_set_membership(&self, runner_id: Uuid, pool_alias: &str, present: bool) {
        let mut map = self.0.lock().await;
        map.insert(
            runner_id,
            PresenceEntry {
                last_seen: Instant::now(),
                concurrency: 0,
                pool_net_id: String::new(),
                pool_alias: Some(pool_alias.to_string()),
                backends: Vec::new(),
                host: None,
                present,
            },
        );
    }

    /// Mark a runner PRESENT (or absent) in the in-memory presence map directly,
    /// bypassing the `runner.*.presence` heartbeat → acquire/sweep machinery.
    ///
    /// The model-pool placement reconciler only ever consumes presence through
    /// `serving_runner_catalogs` / `serving_runner_counts`, which gate the
    /// `runner_interfaces` catalog scan on `snapshot()` entries with
    /// `present == true`. The `PresenceEntry`/`PresenceMap` types are `pub(crate)`,
    /// so an out-of-crate INTEGRATION test (under `service/tests/`) cannot
    /// construct a present entry the way the in-crate `#[cfg(test)]`
    /// [`Self::test_set_membership`] does. This is the public seam those tests use
    /// to make the placement loop SEE a seeded model-serving runner without a live
    /// executor or a real heartbeat: it inserts a liveness-only entry (empty pool,
    /// no caps) whose only load-bearing field for placement is `present`.
    ///
    /// Test-support ONLY — production presence is driven by the heartbeat
    /// controller. Calling this in prod would inject a phantom present runner the
    /// sweep would reap on the next TTL miss (no heartbeat renews it), so it is a
    /// no-op risk rather than a correctness hazard, but it must never be wired into
    /// a request path.
    pub async fn inject_present_for_test(&self, runner_id: Uuid, present: bool) {
        self.inject_present_in_group_for_test(runner_id, None, present)
            .await;
    }

    /// Like [`Self::inject_present_for_test`] but also stamps the runner's `group`
    /// alias (the `pool_alias`), so `pool_membership()` — and therefore the
    /// model-pool reads that gate on `model_serving` group membership — see the
    /// seeded runner as a pool member. Pass `Some("model_serving")` to make a
    /// seeded runner a model-pool replica, or `None` for a group-less present
    /// runner (e.g. to assert it is EXCLUDED from the pool). Test-support only.
    pub async fn inject_present_in_group_for_test(
        &self,
        runner_id: Uuid,
        group: Option<&str>,
        present: bool,
    ) {
        let mut map = self.0.lock().await;
        map.insert(
            runner_id,
            PresenceEntry {
                last_seen: Instant::now(),
                concurrency: 0,
                pool_net_id: String::new(),
                pool_alias: group.map(|g| g.to_string()),
                backends: Vec::new(),
                host: None,
                present,
            },
        );
    }
}

impl Default for RunnerPresence {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve a runner's `group` alias to its backing presence-pool net id.
///
/// `runner.group` is an alias string (the `resources.path` column). It maps to a
/// presence-backed `capacity` resource in the runner's workspace: a `resources`
/// row with `resource_type = 'capacity'`, `path = <alias>`, and `liveness =
/// 'presence'` + `acceptance = 'auto'` (the `instrument` preset) in its latest
/// version's `public_config`. The `acceptance` filter excludes `consent`
/// (human-roster) pools — a heartbeating runner must never be admitted into a
/// consent pool (humans enroll via `roster_members` and resolve their pool net
/// by `capacity_id` directly in [`super::humans`]); mirrors
/// `model_serving_group::resolve_model_serving_group_uuid`.
/// The net id is then [`well_known::pool_net_id`] over that resource's id. Returns
/// `None` (with a skip log at the call site) when the runner has no group alias,
/// or the alias resolves to no presence-backed capacity in its workspace. This is
/// the SAME lookup `handlers::runners::runner_group_exists` gates enrollment on,
/// so the enroll gate and the runtime admission agree on what "the group exists"
/// means.
async fn resolve_pool_net_id(db: &PgPool, runner: &RunnerRow) -> Option<String> {
    let alias = runner.group.as_deref()?;
    let resource_id: Option<(Uuid,)> = sqlx::query_as::<_, (Uuid,)>(
        "SELECT r.id FROM resources r \
         JOIN resource_versions rv \
           ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.workspace_id = $1 AND r.path = $2 \
           AND r.resource_type = 'capacity' AND r.deleted_at IS NULL \
           AND rv.public_config ->> 'liveness' = 'presence' \
           AND rv.public_config ->> 'acceptance' = 'auto'",
    )
    .bind(runner.workspace_id)
    .bind(alias)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    resource_id.map(|(rid,)| well_known::pool_net_id(rid))
}

/// Look up a non-revoked runner row by id. Returns `None` if missing or revoked.
async fn load_live_runner(db: &PgPool, runner_id: Uuid) -> Option<RunnerRow> {
    let row: Option<RunnerRow> = sqlx::query_as::<_, RunnerRow>(
        "SELECT id, workspace_id, name, runner_group, token_hash, nats_public_key, capabilities, \
                status, last_seen_at, enrolled_by, enrolled_at, revoked_at \
         FROM runners WHERE id = $1",
    )
    .bind(runner_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let row = row?;
    if row.revoked_at.is_some() {
        return None;
    }
    Some(row)
}

/// Build the caller parts of ONE slot's `presence_acquire` injection (pure, so
/// the envelope byte-shape is pinned in [`super::core`]'s tests).
///
/// With C-unit concurrency (P3) the controller injects once per slot
/// (`slot = 0..C`): each token mints one distinct pool unit with
/// `unit_id = "{runner_id}#{slot}"` (the granular per-slot identity that becomes
/// an independently grantable lease) and the shared `runner_id` (the reap key —
/// `t_reap_*` correlate on it). `token_color` carries the `{ unit_id, runner_id,
/// executor_namespace, caps }` unit; `source_*` are informational, tagged so
/// causality/tracing attributes the unit to the presence controller. `dedup_id`
/// keys on the runner AND the slot (`presence-acquire:{runner_id}#{slot}`) so a
/// redelivered acquire for a given slot is suppressed at the engine while the
/// runner stays present — CRITICAL: keying on the runner alone would make the
/// engine suppress all C-1 extra slots as duplicates and only ONE unit would
/// ever mint.
pub(crate) fn acquire_injection(
    runner_id: Uuid,
    slot: u32,
    epoch: i64,
    executor_namespace: &str,
    caps: &serde_json::Value,
) -> PoolInjection<'static> {
    // `epoch` (a per-admission wall-clock stamp) is folded into the unit_id — and
    // thus the `dedup_id` — so a re-acquire after a reap (or a toggle off→on
    // inside the engine's bridge dedup window) is NOT suppressed as a duplicate.
    // The OLD scheme used a lifetime-stable `{runner_id}#{slot}`, so once a unit
    // was granted-and-reaped every subsequent re-acquire carried the same
    // dedup_id and the engine silently dropped it → the runner's pool sat empty
    // forever after its first reap. Mirrors the human adapter's `{member}#{slot}@{epoch}`.
    let unit_id = format!("{runner_id}#{slot}@{epoch}");
    PoolInjection {
        source_net_id: "mekhan-presence-controller",
        source_place_name: "presence",
        token_color: json!({
            "unit_id": unit_id,
            "runner_id": runner_id.to_string(),
            "executor_namespace": executor_namespace,
            "caps": caps,
        }),
        signal_key: format!("presence-acquire-{unit_id}"),
        dedup_id: format!("presence-acquire:{unit_id}"),
    }
}

/// Count how many pool units the engine net currently holds for `runner_id` — the
/// `runner_id`-matching tokens in BOTH the FREE (`pool`) and HELD (`in_use`)
/// places. This is the leak-free authority for the acquire top-up: the engine net
/// is the source of truth for admitted slots, NOT mekhan's in-memory map (which is
/// wiped on a mekhan restart while the engine retains its units). Returns `None` on
/// any engine error or unexpected marking shape — callers treat `None` as "assume
/// already at capacity" (inject NOTHING) so a transient engine blip can never
/// DOUBLE-admit. Mirrors the human adapter's `count_member_units`.
async fn count_runner_units(petri: &PetriClient, pool_net_id: &str, runner_id: Uuid) -> Option<u32> {
    let state = petri.try_get_state(pool_net_id).await?;
    let marking = serde_json::to_value(&state.marking).ok()?;
    Some(count_units_in_marking(&marking, &runner_id.to_string()))
}

/// Pure token-counter over an engine marking JSON: the number of `pool` + `in_use`
/// tokens whose `color.value.runner_id` equals `runner_id`. Free function so the
/// shape-parsing is unit-testable without an engine.
fn count_units_in_marking(marking: &serde_json::Value, runner_id: &str) -> u32 {
    let tokens = &marking["tokens"];
    let mut n = 0u32;
    for place in ["pool", "in_use"] {
        if let Some(arr) = tokens[place].as_array() {
            for tok in arr {
                if tok["color"]["value"]["runner_id"].as_str() == Some(runner_id) {
                    n += 1;
                }
            }
        }
    }
    n
}

/// Build the caller parts of a BARE `presence_expired { runner_id }` signal
/// (pure, so the envelope byte-shape is pinned in [`super::core`]'s tests).
/// `now_ms` is the emission stamp folded into the signal key. Injected via the
/// shared [`core::inject_expires`].
pub(crate) fn expire_signal(runner_id: Uuid, now_ms: i64) -> PoolSignal<'static> {
    PoolSignal {
        source: "presence",
        signal_key: format!("presence-expire-{runner_id}-{now_ms}"),
        payload: json!({ "runner_id": runner_id.to_string() }),
    }
}

/// Handle one `runner.*.presence` message: resolve the runner + its pool, and
/// on the absent→present edge inject the acquire token. Touches `runners.last_seen_at`
/// as a cheap UI liveness signal (best-effort; capacity is driven by the
/// in-memory map, not this column).
async fn handle_presence(
    db: &PgPool,
    nats: &MekhanNats,
    petri: &PetriClient,
    presence: &PresenceMap,
    fleet: &FleetLiveness,
    runner_id: Uuid,
    backends: Vec<String>,
    concurrency: u32,
    host: Option<HostInfo>,
) {
    // Mirror the runner's advisory facet (its self-reported backends +
    // concurrency) into the shared fleet-liveness registry on EVERY heartbeat —
    // telemetry only (docs/24 S1). This feeds the unified publish-time
    // `serves_backend` eligibility query alongside the worker pool (and surfaces
    // C as advisory telemetry for the model-pool router); it has NO bearing on
    // the pool-net control binding injected below (caps stay authoritative
    // there). The runner controller owns this entry's lifecycle, so it is removed
    // only by `drop_runner` on expiry — the fleet module's worker sweep never
    // touches it.
    fleet
        .upsert_runner(runner_id.to_string(), backends.clone(), concurrency)
        .await;

    // Fast path: already present → bump last_seen + reconcile the C delta under
    // the lock. We still re-touch last_seen_at periodically below, but avoid a DB
    // lookup on every heartbeat of an already-admitted runner.
    //
    // GROW-EAGER / SHRINK-LAZY: if the wire C exceeds the applied C, we EAGERLY
    // inject the new slots (`applied..wire`) so the extra capacity is available
    // immediately, then bump the stored applied C. If the wire C is smaller, we
    // only lower the stored target (SHRINK is lazy — a held surplus slot must
    // finish its lease; it drains on release or at the next full expire). We need
    // the pool_net_id + namespace/caps to inject grow slots, so collect what we
    // need under the lock, then inject outside it.
    {
        let mut map = presence.lock().await;
        if let Some(entry) = map.get_mut(&runner_id) {
            entry.last_seen = Instant::now();
            // Refresh the advertised backend set on every heartbeat — cheap, and
            // it keeps coverage current if a daemon's feature set changes without
            // a full re-acquire (caps still come from the DB, untouched here).
            entry.backends = backends.clone();
            // Refresh the host fingerprint too (cheap, keeps fleet visibility
            // current if a runner moves host / changes GPU between heartbeats).
            entry.host = host.clone();
            if entry.present {
                // Compute the grow delta. SHRINK is lazy (just lower the target);
                // GROW eagerly injects the new slots below. A pool-less
                // (liveness-only) entry never injects.
                let new_slots =
                    core::grow_slots(entry.pool_net_id.is_empty(), entry.concurrency, concurrency);
                let grow = new_slots.map(|s| (entry.pool_net_id.clone(), s));
                // Always record the new target C (grow OR shrink).
                entry.concurrency = concurrency;
                drop(map);

                if let Some((pool_net_id, new_slots)) = grow {
                    // Re-resolve the trusted namespace + caps from the DB row to
                    // mint the new slots (never from the wire payload).
                    if let Some(runner) = load_live_runner(db, runner_id).await {
                        let executor_namespace = format!("runner-jobs/{runner_id}");
                        let caps = runner.capabilities.clone();
                        // Fresh epoch for the grow batch (the grown slot indices are
                        // already new, but stamping the episode keeps the dedup id
                        // unique if the same index is re-grown after a later shrink).
                        let epoch = Utc::now().timestamp_millis();
                        for slot in new_slots {
                            core::inject_acquire(
                                nats,
                                &pool_net_id,
                                acquire_injection(runner_id, slot, epoch, &executor_namespace, &caps),
                                "presence acquire",
                            )
                            .await;
                        }
                        tracing::info!(
                            %runner_id, pool_net_id, concurrency,
                            "presence concurrency grew; minted new slots"
                        );
                    }
                }

                // Already admitted — best-effort last_seen_at touch.
                touch_last_seen(db, runner_id).await;
                return;
            }
            // Known but currently absent (was reaped) — fall through to re-acquire.
        }
    }

    // Absent→present edge (first-ever presence OR re-acquire after expiry).
    // Resolve from the TRUSTED DB row — caps + namespace NEVER come from the wire.
    let Some(runner) = load_live_runner(db, runner_id).await else {
        tracing::debug!(%runner_id, "presence from unknown/revoked runner; ignoring");
        return;
    };

    let Some(pool_net_id) = resolve_pool_net_id(db, &runner).await else {
        tracing::debug!(
            %runner_id,
            group = ?runner.group,
            "runner present but no presence-backed `capacity` resource in its workspace; tracking liveness only"
        );
        // No pool to admit into, but the runner IS heartbeating — record it as
        // present with an empty pool_net_id so the fleet "online" view (the read
        // API + the sweep) sees it. The empty pool id means the sweep reaps it on
        // TTL WITHOUT injecting a (bogus) pool expire. A later resource-create +
        // re-acquire (the absent→present edge) upgrades it to a real admission.
        {
            let mut map = presence.lock().await;
            map.insert(
                runner_id,
                PresenceEntry {
                    last_seen: Instant::now(),
                    pool_net_id: String::new(),
                    pool_alias: runner.group.clone(),
                    backends: backends.clone(),
                    host: host.clone(),
                    // No pool to admit into → no slots applied. The sweep injects
                    // 0 expires for a liveness-only entry (its empty pool_net_id
                    // already short-circuits the engine reap).
                    concurrency: 0,
                    present: true,
                },
            );
        }
        touch_last_seen(db, runner_id).await;
        return;
    };

    // `{shared-stream}/{partition}` — the granted job routes to the SHARED
    // `runner-jobs` apalis stream, PARTITIONED to this runner id. The engine
    // producer (`executor` client `split_namespace`) splits on `/`: it ensures
    // the shared stream `runner-jobs_{prio}` and publishes to
    // `runner-jobs.{prio}.{runner_id}.{exec}`, which the runner daemon's
    // `PartitionedPool` consumer filter (`runner-jobs.{prio}.{runner_id}.>`)
    // drains exclusively. This keeps ONE stream-set for an unbounded fleet
    // instead of a stream per runner. The `/` is a pure stamping delimiter — it
    // never reaches a NATS subject/stream name. Must byte-match the runner
    // daemon's `RUNNER_JOBS_NAMESPACE` + partition (`runner_id`). The presence
    // *subject* (`runner.{id}.presence`) stays dotted — it's a NATS subject.
    let executor_namespace = format!("runner-jobs/{runner_id}");
    let caps = runner.capabilities.clone();

    // Per-admission epoch stamped into every slot's unit_id/dedup_id so this
    // admission EPISODE is distinct from any prior one for this runner — a toggle
    // off→on (or a re-acquire after a reap) inside the engine's bridge dedup
    // window re-admits fresh instead of being silently suppressed.
    let epoch = Utc::now().timestamp_millis();

    // Top up to C against the engine's CURRENT unit count (the leak-free
    // authority), not mekhan's in-memory map. `None` (engine error) is treated as
    // "already at C" so a blip never double-admits; the next edge reconciles. A
    // runner whose engine slots survived a mekhan restart counts as
    // `existing == C` → inject 0 (we just re-track it in-memory), the case a
    // pure-epoch scheme would otherwise double-admit.
    let existing = count_runner_units(petri, &pool_net_id, runner_id)
        .await
        .unwrap_or(concurrency);
    let need = core::grow_slots(false, existing, concurrency)
        .map(|r| r.len() as u32)
        .unwrap_or(0);

    // Mint `need` distinct slots (slot 0..need), one bridge token each. `unit_id`
    // is per-slot AND per-epoch so each is an independently grantable lease that
    // the engine won't dedup against a prior episode; they share `runner_id` so
    // the reap-all-by-runner_id signals match any of them.
    for slot in 0..need {
        core::inject_acquire(
            nats,
            &pool_net_id,
            acquire_injection(runner_id, slot, epoch, &executor_namespace, &caps),
            "presence acquire",
        )
        .await;
    }

    // Commit the present edge AFTER injecting. A crash between inject + map update
    // re-injects on the next edge with a FRESH epoch; the engine-count top-up
    // above (not the dedup id) is what prevents a double-admit, so losing the
    // in-memory epoch on a restart is safe. Record the applied C so the sweep
    // knows how many expire signals to inject and the grow path knows the slot count.
    {
        let mut map = presence.lock().await;
        map.insert(
            runner_id,
            PresenceEntry {
                last_seen: Instant::now(),
                pool_net_id: pool_net_id.clone(),
                pool_alias: runner.group.clone(),
                backends: backends.clone(),
                host: host.clone(),
                concurrency,
                present: true,
            },
        );
    }
    touch_last_seen(db, runner_id).await;

    tracing::info!(%runner_id, pool_net_id, concurrency, "presence acquired (runner admitted to pool with C slots)");
}

/// Best-effort `runners.last_seen_at = now()` bump. A failed update is logged at
/// debug and swallowed — presence capacity is driven by the in-memory map.
async fn touch_last_seen(db: &PgPool, runner_id: Uuid) {
    if let Err(e) = sqlx::query("UPDATE runners SET last_seen_at = NOW() WHERE id = $1")
        .bind(runner_id)
        .execute(db)
        .await
    {
        tracing::debug!(%runner_id, "failed to bump runner last_seen_at: {e}");
    }
}

/// Parse the runner UUID out of a `runner.{runner_id}.presence` subject (the
/// shared [`core::uuid_from_subject`] grammar). Returns `None` on any
/// structural mismatch.
fn parse_runner_subject(subject: &str) -> Option<Uuid> {
    core::uuid_from_subject(subject, "runner", "presence")
}

/// Extract the runner's advertised `backends` + `concurrency` from a presence
/// payload. The `runner_id` is authoritative from the SUBJECT (never the
/// payload); `backends` + `concurrency` are advisory wire-truth (see
/// [`PresenceEntry::backends`] / [`PresenceEntry::concurrency`]).
///
/// A missing/malformed `backends` field yields an empty set — the runner is
/// still tracked as present (liveness is subject-driven), it just advertises no
/// coverage until a well-formed heartbeat arrives. A missing/zero `concurrency`
/// (older runner, or a malformed payload) defaults to **1** so a runner that
/// doesn't report C still gets one slot (the pre-P3 behaviour). The value is
/// CLAMPED to a conservative ceiling ([`MAX_RUNNER_CONCURRENCY`]) so a lying
/// runner reporting C=10000 can't mint 10000 pool units.
//
// TODO(P3 §6 residual): clamp against the group `capacity` resource's
// `public_config` per-runner ceiling once that field is specified, rather than
// the global `MAX_RUNNER_CONCURRENCY` constant.
fn parse_presence(payload: &[u8]) -> (Vec<String>, u32, Option<HostInfo>) {
    #[derive(serde::Deserialize)]
    struct PresencePayload {
        #[serde(default)]
        backends: Vec<String>,
        #[serde(default)]
        concurrency: Option<u32>,
        /// Best-effort host/hardware fingerprint (additive; older runners omit
        /// it). Parsed leniently — a malformed `host` block leaves it `None`
        /// without dropping the whole heartbeat.
        #[serde(default)]
        host: Option<HostInfo>,
    }
    match serde_json::from_slice::<PresencePayload>(payload) {
        Ok(p) => {
            let c = p.concurrency.filter(|&c| c > 0).unwrap_or(1);
            (p.backends, c.min(MAX_RUNNER_CONCURRENCY), p.host)
        }
        Err(_) => (Vec::new(), 1, None),
    }
}

/// Conservative upper bound on a runner's self-reported concurrency. `concurrency`
/// is advisory wire-truth (like `backends`) from the UNTRUSTED presence payload;
/// without a cap a runner reporting an absurd C would mint that many pool units.
/// A real lab instrument serves a handful of simultaneous leases at most, so this
/// ceiling is generous while bounding the blast radius of a lying/buggy runner.
const MAX_RUNNER_CONCURRENCY: u32 = 256;

/// Start the presence subscriber: a core-NATS subscription to `runner.*.presence`
/// (the shared [`core::subscribe`] harness — ephemeral liveness, no JetStream
/// durable).
pub(crate) async fn start_presence_subscriber(
    nats: MekhanNats,
    db: PgPool,
    petri: PetriClient,
    presence: PresenceMap,
    fleet: FleetLiveness,
) {
    let Some(mut sub) = core::subscribe(&nats, "runner.*.presence").await else {
        return;
    };
    tracing::info!("presence subscriber started on runner.*.presence");

    while let Some(msg) = sub.next().await {
        let Some(runner_id) = parse_runner_subject(msg.subject.as_str()) else {
            tracing::debug!(subject = %msg.subject, "ignoring non-presence subject");
            continue;
        };
        let (backends, concurrency, host) = parse_presence(&msg.payload);
        handle_presence(
            &db,
            &nats,
            &petri,
            &presence,
            &fleet,
            runner_id,
            backends,
            concurrency,
            host,
        )
        .await;
    }

    tracing::warn!("presence subscriber stream ended");
}

/// Start the presence sweep loop: every [`SWEEP_INTERVAL_SECS`] scan the
/// presence map for `present` entries whose `last_seen` is older than the TTL,
/// inject a BARE expire signal for each, and flip them to absent.
///
/// The loop mechanics are the shared [`core::sweep_loop`]; this adapter
/// supplies the global-TTL freshness gate, the under-lock flip (snapshotting +
/// zeroing the applied C), and the per-runner reap (fleet-mirror drop +
/// liveness-only short-circuit + expire injection).
pub(crate) async fn start_presence_sweep(
    nats: MekhanNats,
    presence: PresenceMap,
    fleet: FleetLiveness,
) {
    let ttl = presence_ttl();
    tracing::info!(
        ttl_secs = ttl.as_secs(),
        sweep_secs = SWEEP_INTERVAL_SECS,
        "presence sweep started"
    );

    core::sweep_loop(
        presence,
        Duration::from_secs(SWEEP_INTERVAL_SECS),
        move |entry: &PresenceEntry, now| {
            entry.present && now.duration_since(entry.last_seen) > ttl
        },
        |rid: &Uuid, entry: &mut PresenceEntry| {
            entry.present = false;
            // Snapshot applied C BEFORE we zero it so the reap injects exactly as
            // many expire signals as there are live slots.
            let applied_c = entry.concurrency;
            entry.concurrency = 0;
            ExpiredSlots {
                reap_key: *rid,
                pool_net_id: entry.pool_net_id.clone(),
                slots: applied_c,
            }
        },
        |expired: ExpiredSlots| {
            let nats = nats.clone();
            let fleet = fleet.clone();
            async move {
                let runner_id = expired.reap_key;
                let pool_net_id = expired.pool_net_id;
                let applied_c = expired.slots;

                // Telemetry plane: a TTL-missed runner is offline, so drop its
                // advisory mirror from the shared fleet-liveness registry (docs/24
                // S1). This runs for BOTH pool-backed and liveness-only entries — it
                // is pure telemetry and never touches the pool-net control binding.
                fleet.drop_runner(&runner_id.to_string()).await;

                if pool_net_id.is_empty() {
                    // Liveness-only entry (runner not admitted to any pool): nothing to
                    // expire on the engine — flipping `present = false` above is enough
                    // for the fleet view to show it offline.
                    tracing::debug!(%runner_id, "presence TTL miss; runner offline (no pool to expire)");
                    return;
                }
                // Inject one bare `{ runner_id }` expire signal PER applied slot:
                // each signal is consumed once and reaps exactly one of the runner's
                // C slots (reap-ALL-by-runner_id). `applied_c == 0` (shouldn't happen
                // for a pool-backed entry, but be defensive) means nothing to reap.
                tracing::info!(
                    %runner_id, pool_net_id, applied_c,
                    "presence TTL miss; reaping runner's slots"
                );
                core::inject_expires(
                    &nats,
                    &pool_net_id,
                    applied_c,
                    |now_ms| expire_signal(runner_id, now_ms),
                    "presence expire",
                )
                .await;
            }
        },
    )
    .await;
}

/// Spawn BOTH presence tasks (subscriber + sweep) sharing one presence map.
/// Called from `main.rs`. Threads the PgPool + NATS client. `PetriClient` is
/// accepted for symmetry with the other controllers and to keep the spawn site
/// uniform, even though the controller drives the pool net purely over NATS
/// (bridge + signal) and does not need the engine HTTP client today.
/// `presence` is the SHARED handle also stored in [`crate::AppState`] so the read
/// API (`GET /api/v1/runners/presence`) observes the very map the tasks mutate.
/// `fleet` is the shared [`FleetLiveness`] registry: both tasks mirror each
/// runner's advisory backend facet into it (subscriber upserts on heartbeat,
/// sweep drops on TTL miss) so the unified publish-time eligibility check sees
/// runners alongside workers (docs/24 S1) — telemetry only, never the pool-net
/// control binding.
pub fn spawn_presence_controller(
    presence: RunnerPresence,
    nats: MekhanNats,
    db: PgPool,
    petri: PetriClient,
    fleet: FleetLiveness,
) {
    core::spawn_controller(
        start_presence_subscriber(nats.clone(), db, petri, presence.map().clone(), fleet.clone()),
        start_presence_sweep(nats, presence.map().clone(), fleet),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_presence_subject() {
        let id = Uuid::new_v4();
        let subject = format!("runner.{id}.presence");
        assert_eq!(parse_runner_subject(&subject), Some(id));
    }

    #[test]
    fn rejects_malformed_subjects() {
        assert!(parse_runner_subject("runner.presence").is_none());
        assert!(parse_runner_subject("runner.not-a-uuid.presence").is_none());
        assert!(parse_runner_subject("runner.abc.heartbeat").is_none());
        let id = Uuid::new_v4();
        assert!(parse_runner_subject(&format!("runner.{id}.presence.extra")).is_none());
        assert!(parse_runner_subject(&format!("foo.{id}.presence")).is_none());
    }

    #[tokio::test]
    async fn acquire_fires_once_then_idempotent_until_expiry() {
        // White-box the present-edge state machine on the map alone (no NATS/DB):
        // first observation should be the absent→present edge, subsequent ones
        // should be no-ops, and a sweep-style flip re-arms the edge.
        let presence: PresenceMap = core::new_entry_map();
        let rid = Uuid::new_v4();

        // Simulate the acquire commit.
        {
            let mut map = presence.lock().await;
            map.insert(
                rid,
                PresenceEntry {
                    last_seen: Instant::now(),
                    pool_net_id: "pool-x".to_string(),
                    pool_alias: Some("lab_fleet".to_string()),
                    backends: vec!["python".to_string()],
                    host: None,
                    concurrency: 1,
                    present: true,
                },
            );
        }

        // A heartbeat while present is a no-op edge (present stays true).
        {
            let mut map = presence.lock().await;
            let e = map.get_mut(&rid).unwrap();
            assert!(e.present, "still present after heartbeat");
            e.last_seen = Instant::now();
            assert!(e.present);
        }

        // Sweep flips to absent.
        {
            let mut map = presence.lock().await;
            map.get_mut(&rid).unwrap().present = false;
        }

        // Next heartbeat sees absent → re-acquire edge is available.
        {
            let map = presence.lock().await;
            assert!(!map.get(&rid).unwrap().present, "absent → re-acquire armed");
        }
    }

    #[test]
    fn parse_presence_reads_backends_and_concurrency() {
        // Backends + an explicit C.
        let (b, c, _h) =
            parse_presence(br#"{"runner_id":"x","backends":["python","docker"],"concurrency":4}"#);
        assert_eq!(b, vec!["python".to_string(), "docker".to_string()]);
        assert_eq!(c, 4);

        // Missing concurrency → default 1 (pre-P3 behaviour: one slot).
        let (b, c, _h) = parse_presence(br#"{"runner_id":"x","backends":["python"]}"#);
        assert_eq!(b, vec!["python".to_string()]);
        assert_eq!(c, 1);

        // Missing/malformed backends → empty (still tracked present); C defaults.
        let (b, c, _h) = parse_presence(br#"{"runner_id":"x"}"#);
        assert!(b.is_empty());
        assert_eq!(c, 1);
        let (b, c, _h) = parse_presence(b"not json");
        assert!(b.is_empty());
        assert_eq!(c, 1);

        // Zero concurrency is coerced to 1 (a present runner always gets ≥1 slot).
        let (_b, c, _h) = parse_presence(br#"{"concurrency":0}"#);
        assert_eq!(c, 1);

        // A lying runner is clamped to the conservative ceiling.
        let (_b, c, _h) = parse_presence(br#"{"concurrency":100000}"#);
        assert_eq!(c, MAX_RUNNER_CONCURRENCY);
    }

    #[test]
    fn parse_presence_reads_host_fingerprint() {
        // A heartbeat carrying a host block surfaces it; older runners omit it.
        let (_b, _c, host) = parse_presence(
            br#"{"backends":["python"],"host":{"hostname":"gpu-box-3","os":"linux","arch":"x86_64","cpu_cores":32,"mem_gb":256,"accelerator":"cuda","gpu_count":2,"vram_gb":80,"compute_capability":"9.0","ips":["10.0.0.7"]}}"#,
        );
        let host = host.expect("host block present");
        assert_eq!(host.hostname.as_deref(), Some("gpu-box-3"));
        assert_eq!(host.accelerator.as_deref(), Some("cuda"));
        assert_eq!(host.gpu_count, Some(2));
        assert_eq!(host.vram_gb, Some(80));
        assert_eq!(host.ips, vec!["10.0.0.7".to_string()]);

        // No host block → None (legacy runner), heartbeat still parses.
        let (_b, _c, host) = parse_presence(br#"{"backends":["python"]}"#);
        assert!(host.is_none());
    }

    #[test]
    fn c_units_mint_distinct_unit_ids_sharing_one_runner_id() {
        // The controller mints slot 0..C, each with a distinct per-slot+epoch
        // unit_id `"{runner_id}#{slot}@{epoch}"` and the SHARED runner_id (the reap
        // key). Mirror the inject_acquire identity + dedup formatting exactly.
        let rid = Uuid::new_v4();
        let epoch = 1_700_000_000_123i64;
        let c = 4u32;
        let unit_ids: Vec<String> = (0..c).map(|slot| format!("{rid}#{slot}@{epoch}")).collect();
        let dedup_ids: Vec<String> = (0..c)
            .map(|slot| format!("presence-acquire:{rid}#{slot}@{epoch}"))
            .collect();

        // C distinct unit_ids ...
        let distinct: std::collections::HashSet<&String> = unit_ids.iter().collect();
        assert_eq!(distinct.len(), c as usize, "C distinct slot unit_ids");
        // ... and C distinct dedup ids (the highest-risk line: keying dedup on the
        // runner alone would collapse all C-1 extra acquires to one).
        let distinct_dedup: std::collections::HashSet<&String> = dedup_ids.iter().collect();
        assert_eq!(
            distinct_dedup.len(),
            c as usize,
            "per-slot dedup keys distinct"
        );
        // ... all sharing the one runner_id prefix.
        assert!(unit_ids.iter().all(|u| u.starts_with(&format!("{rid}#"))));

        // The real builder agrees with the mirrored formatting (identity + dedup
        // come from `acquire_injection`, the single source of truth).
        let inj = acquire_injection(rid, 0, epoch, "runner-jobs/x", &serde_json::json!({}));
        assert_eq!(inj.dedup_id, dedup_ids[0]);
        assert_eq!(
            inj.token_color["unit_id"].as_str(),
            Some(unit_ids[0].as_str())
        );
    }

    #[test]
    fn re_acquire_after_reap_is_dedup_fresh() {
        // Regression guard for the stuck-after-first-reap bug: two acquisitions of
        // the SAME slot in DIFFERENT epochs must produce DIFFERENT dedup_ids, so the
        // engine's bridge dedup re-admits the runner instead of suppressing the
        // re-acquire (which left the pool permanently empty under the old
        // lifetime-stable `{runner_id}#{slot}` scheme).
        let rid = Uuid::new_v4();
        let caps = serde_json::json!({});
        let first = acquire_injection(rid, 0, 1_700_000_000_000, "runner-jobs/x", &caps);
        let after_reap = acquire_injection(rid, 0, 1_700_000_009_999, "runner-jobs/x", &caps);
        assert_ne!(
            first.dedup_id, after_reap.dedup_id,
            "a re-acquire in a new epoch must NOT collide with the reaped unit's dedup id"
        );
    }

    #[test]
    fn count_units_in_marking_counts_free_and_held_for_runner() {
        let me = "11111111-1111-1111-1111-111111111111";
        let other = "22222222-2222-2222-2222-222222222222";
        let marking = serde_json::json!({
            "tokens": {
                "pool": [
                    {"color": {"value": {"runner_id": me}}},
                    {"color": {"value": {"runner_id": other}}}
                ],
                "in_use": [
                    {"color": {"value": {"runner_id": me}}},
                    {"color": {"value": {"runner_id": me}}}
                ]
            }
        });
        // 1 free + 2 held for `me`.
        assert_eq!(count_units_in_marking(&marking, me), 3);
        assert_eq!(count_units_in_marking(&marking, other), 1);
        assert_eq!(count_units_in_marking(&marking, "deadbeef"), 0);
        // An empty/odd marking shape counts as zero (never panics).
        assert_eq!(count_units_in_marking(&serde_json::json!({}), me), 0);
    }

    #[tokio::test]
    async fn pool_covers_matches_alias_and_backend_only_when_present() {
        let presence = RunnerPresence::new();
        let rid = Uuid::new_v4();
        {
            let mut map = presence.map().lock().await;
            map.insert(
                rid,
                PresenceEntry {
                    last_seen: Instant::now(),
                    pool_net_id: "pool-x".to_string(),
                    pool_alias: Some("lab_fleet".to_string()),
                    backends: vec!["python".to_string()],
                    host: None,
                    concurrency: 1,
                    present: true,
                },
            );
        }

        // Right pool + right backend → covered.
        assert!(presence.pool_covers("lab_fleet", "python").await);
        // Right pool, backend the runner doesn't have → not covered.
        assert!(!presence.pool_covers("lab_fleet", "docker").await);
        // Backend present but a different pool → not covered (pool-scoped).
        assert!(!presence.pool_covers("other_pool", "python").await);

        // Flip absent → no longer covers (a reaped runner can't serve).
        {
            let mut map = presence.map().lock().await;
            map.get_mut(&rid).unwrap().present = false;
        }
        assert!(!presence.pool_covers("lab_fleet", "python").await);
    }
}
