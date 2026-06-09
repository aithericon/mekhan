//! Human presence controller (docs/33 §4/§7 — humans as a capacity).
//!
//! The human analogue of [`crate::runners_presence`]. A human `capacity`
//! resource (`presence · offer · …`) is a capacity-LESS pool
//! ([`crate::petri::presence_pool_net`]) whose admission is driven not by a
//! runner daemon heartbeat but by a roster MEMBER's availability. A person has
//! no data-plane daemon, so this controller is the generalization point: ONE
//! parameterised presence loop instead of three code paths, with two SOURCES
//! that the runner controller collapses into one:
//!
//! 1. **INTENT** — core-subscribe `human.*.availability`. A member flips their
//!    durable availability on a specific human capacity. Subject is
//!    `human.{member}.availability`; the member is parsed from the SUBJECT, the
//!    `capacity_id` + `workspace_id` from the payload. On `available=true` we
//!    load the TRUSTED roster row (caps / concurrency / availability are
//!    admin-assigned, never the wire claim), cache its facets, set
//!    `intent_available=true` and `last_seen=now` (so a `session` entry is
//!    immediately live on toggle), then reconcile. On `available=false` we clear
//!    the intent and reconcile (which expires the member).
//!
//! 2. **LIVENESS** — core-subscribe `human.*.presence`. A heartbeat published by
//!    the task-SSE handler renews a `session`/`external` member's presence.
//!    Subject is `human.{member}.presence`; the member is the SUBJECT (the
//!    payload is empty/ignored). We bump `last_seen` for ALL of that member's
//!    entries (a member may be enrolled in several pools) and reconcile each.
//!
//! **ADMISSION** is the pure [`should_admit`] predicate: intent on AND, for a
//! `session`/`external` liveness source, a fresh-enough `last_seen`. A `None`
//! (durable) source has `ttl=∞`: it is admitted on intent alone and only an
//! `available=false` toggle expires it — the TTL sweep never touches it.
//!
//! **RECONCILE** drives the pool net exactly like the runner controller: on the
//! absent→present edge it injects `C` `presence_acquire` units, on the
//! present→absent edge it injects `C` bare `presence_expired` signals. The
//! injected unit reuses the runner plumbing VERBATIM — the `runner_id` field
//! carries the member id so the engine pool net's generic `t_grant`/`t_claim`/
//! `t_reap_*` correlate without a human-specific net (the whole point of the
//! generalization, docs/33 §4).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::Mutex;
use utoipa::ToSchema;
use uuid::Uuid;

use futures::StreamExt;

use crate::compiler::well_known;
use crate::models::roster::{AvailabilityConfig, LivenessSource};
use crate::nats::MekhanNats;
use crate::petri::client::PetriClient;

/// Default presence TTL when a roster row carries none: a `session`/`external`
/// member missing this long is reaped. Overridable via
/// `MEKHAN__HUMAN__PRESENCE_TTL_SECS`. The PER-ENTRY ttl comes from the roster
/// row's [`AvailabilityConfig::ttl_secs`]; this default only seeds an entry that
/// has no configured window (and the sweep reads the per-entry value, not this).
const DEFAULT_PRESENCE_TTL_SECS: u64 = 45;

/// How often the sweep loop wakes to look for TTL misses. Kept well below the
/// TTL so the reap latency is bounded by ~one sweep interval past expiry.
const SWEEP_INTERVAL_SECS: u64 = 5;

/// Read the configured default presence TTL (seconds), defaulting to
/// [`DEFAULT_PRESENCE_TTL_SECS`]. A parse failure or non-positive value falls
/// back to the default with a WARN so a typo can't silently disable reaping.
/// Used only when a roster row omits an explicit `ttl_secs`.
fn default_presence_ttl() -> Duration {
    match std::env::var("MEKHAN__HUMAN__PRESENCE_TTL_SECS") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(n) if n > 0 => Duration::from_secs(n),
            _ => {
                tracing::warn!(
                    raw = %raw,
                    "MEKHAN__HUMAN__PRESENCE_TTL_SECS is not a positive integer; \
                     using default {DEFAULT_PRESENCE_TTL_SECS}s"
                );
                Duration::from_secs(DEFAULT_PRESENCE_TTL_SECS)
            }
        },
        Err(_) => Duration::from_secs(DEFAULT_PRESENCE_TTL_SECS),
    }
}

/// One tracked roster member's presence state in one human capacity.
pub(crate) struct HumanPresenceEntry {
    /// Most recent liveness renewal instant (a `presence` heartbeat, or the
    /// `availability=true` toggle which seeds it).
    last_seen: Instant,
    /// Whether the member's durable availability intent is currently ON for this
    /// capacity. Set by the `availability` source; admission requires it.
    intent_available: bool,
    /// Whether mekhan currently considers the member ADMITTED to the pool (a
    /// `presence_acquire` has been injected and no expire since). Drives the
    /// absent→present acquire edge + the present→absent expire edge.
    present: bool,
    /// Per-person `C` — the number of pool slots applied (the count of
    /// `presence_acquire` units injected) and the number of expire signals to
    /// inject on reap. From the TRUSTED roster row.
    concurrency: u32,
    /// Pool net id (`pool-<capacity_id>`) this member's presence is admitted to.
    /// Resolved DIRECTLY from `capacity_id` ([`well_known::pool_net_id`]) — humans
    /// reference the capacity resource id directly, with no group-alias hop.
    pool_net_id: String,
    /// Admin-assigned capability blob from the trusted roster row — the engine
    /// `t_claim` matcher's authority. Injected into the pool unit verbatim, NEVER
    /// taken from the wire.
    caps: serde_json::Value,
    /// What renews this member's presence (cached from the roster row's
    /// [`AvailabilityConfig`]). Selects the admission/TTL behaviour in
    /// [`should_admit`] + the sweep.
    liveness_source: LivenessSource,
    /// Per-entry expiry window (from the roster row's
    /// [`AvailabilityConfig::ttl_secs`]). The sweep uses THIS, not a global, so
    /// each member's configured availability governs its own reap.
    ttl: Duration,
    /// The member's workspace (cached from the wire payload + verified against the
    /// trusted roster row's `workspace_id`).
    workspace_id: Uuid,
    /// The enrolled member's `workspace_members.user_id` — the reap key carried as
    /// the generic `runner_id` field on the injected unit.
    member_user_id: Uuid,
}

/// Composite map key: `(capacity_id, member_user_id)`. A single member can be
/// enrolled in several human capacities, so the member id alone is not unique.
type HumanKey = (Uuid, Uuid);

/// In-memory presence map: `(capacity_id, member)` → its tracked state. Guarded
/// by a single `Mutex` shared between the subscriber task and the sweep task. The
/// critical sections are tiny (a HashMap probe + a clone of small values), so a
/// plain `Mutex` is correct and contention-free in practice.
type HumanPresenceMap = Arc<Mutex<HashMap<HumanKey, HumanPresenceEntry>>>;

/// Public newtype wrapper around the [`HumanPresenceMap`] so the `pub`
/// [`crate::AppState`] can hold a handle to the live map WITHOUT leaking the
/// `pub(crate)` [`HumanPresenceEntry`]/[`HumanPresenceMap`] types (which would
/// trip the `private_interfaces` lint that CI's `-D warnings` rejects). Mirrors
/// [`crate::runners_presence::RunnerPresence`].
///
/// The presence-controller tasks share the inner map via [`Self::map`]; a read
/// API reads through [`Self::snapshot`].
#[derive(Clone)]
pub struct HumanPresence(HumanPresenceMap);

impl HumanPresence {
    /// Construct a fresh, empty presence handle. The controller tasks + any read
    /// API share this one map.
    pub fn new() -> Self {
        Self(new_human_presence_map())
    }

    /// Borrow the inner shared map for the controller tasks (subscriber + sweep).
    pub(crate) fn map(&self) -> &HumanPresenceMap {
        &self.0
    }

    /// Snapshot the live presence map for the read API. Locks the mutex, then for
    /// each tracked member emits a [`HumanPresenceSnapshot`] with the elapsed time
    /// since its last renewal computed against [`Instant::now`] (an `Instant` has
    /// no serializable form, so we surface a relative age instead). `async`
    /// because the inner map is a `tokio::sync::Mutex` shared with the async
    /// controller tasks.
    pub async fn snapshot(&self) -> Vec<HumanPresenceSnapshot> {
        let now = Instant::now();
        let map = self.0.lock().await;
        map.iter()
            .map(|((capacity_id, member_user_id), entry)| HumanPresenceSnapshot {
                capacity_id: *capacity_id,
                member_user_id: *member_user_id,
                present: entry.present,
                last_seen_ms_ago: now.duration_since(entry.last_seen).as_millis() as u64,
            })
            .collect()
    }
}

impl Default for HumanPresence {
    fn default() -> Self {
        Self::new()
    }
}

/// Read-API row: one tracked roster member's live presence in a human capacity.
#[derive(serde::Serialize, ToSchema)]
pub struct HumanPresenceSnapshot {
    /// The human-capacity `resources.id` (its pool net is `pool-<capacity_id>`).
    pub capacity_id: Uuid,
    /// The enrolled member's `workspace_members.user_id`.
    pub member_user_id: Uuid,
    /// Whether mekhan currently considers the member admitted to the pool.
    pub present: bool,
    /// Milliseconds since the member's last liveness renewal.
    pub last_seen_ms_ago: u64,
}

/// Whether a tracked member should be ADMITTED to its pool right now (docs/33
/// §7.1). A FREE pure function (not a method) so the predicate is unit-testable
/// without NATS/DB.
///
/// Intent must be on; then the liveness source decides freshness:
/// - [`LivenessSource::None`] — durable: `ttl=∞`, admitted on intent alone (only
///   an `available=false` toggle ever expires it).
/// - [`LivenessSource::Session`] / [`LivenessSource::External`] — admitted only
///   while `now - last_seen <= ttl`.
fn should_admit(e: &HumanPresenceEntry, now: Instant, ttl: Duration) -> bool {
    e.intent_available
        && match e.liveness_source {
            LivenessSource::None => true,
            LivenessSource::Session | LivenessSource::External => {
                now.duration_since(e.last_seen) <= ttl
            }
        }
}

/// Whether the TTL sweep should REAP this entry now. A FREE pure function (like
/// [`should_admit`]) so the sweep's gate is unit-testable without NATS.
///
/// Reaping requires all three: the member is currently ADMITTED (`present` — a
/// not-yet-present entry has nothing to reap), the source is TTL-governed (a
/// [`LivenessSource::None`]/durable entry is NEVER swept — only an
/// `available=false` toggle expires it), and the per-entry window has elapsed
/// since the last renewal (`now - last_seen > ttl`). The `> ttl` here is the
/// exact complement of the `<= ttl` admit gate in [`should_admit`], so a
/// `session` member at the boundary is consistently classified by both.
fn should_sweep(e: &HumanPresenceEntry, now: Instant) -> bool {
    e.present
        && e.liveness_source != LivenessSource::None
        && now.duration_since(e.last_seen) > e.ttl
}

/// Inject ONE slot's `presence_acquire` unit into the pool net's
/// `presence_acquire` bridge_in place via
/// `petri.bridge.<pool_net_id>.presence_acquire`.
///
/// Reuses the runner plumbing VERBATIM (docs/33 §4): the `runner_id` field
/// carries the MEMBER id so the engine pool net's generic `t_grant`/`t_reap_*`
/// correlate on it without a human-specific net. `unit_id` is per-slot AND
/// per-episode (`"{member}#{slot}@{epoch}"`) so each is an independently grantable
/// lease; the shared `runner_id` is the reap key (the `presence_expired` signals
/// reap all of them by `runner_id`, NOT by `unit_id`, so a fresh per-episode
/// `unit_id` is safe). `assignee` is an ADDITIVE field the P3 grant relays to the
/// human inbox. `executor_namespace` is `human/<member>`. Wire shape is the
/// engine's [`CrossNetTokenTransfer`] envelope.
///
/// `epoch` is a per-admission stamp (wall-clock millis captured ONCE at the
/// absent→present edge in [`reconcile`], shared across the C slots). It is folded
/// into the `unit_id` — and thereby the `dedup_id` — so every availability EPISODE
/// re-admits fresh. WITHOUT it, a member toggling available→unavailable→available
/// within JetStream's ~2-minute dedup window would have the re-acquire silently
/// SUPPRESSED (the stable `presence-acquire:{member}#{slot}` dedup_id collides with
/// the prior episode's still-cached publish) — a real human UX bug, since
/// off→on is common (mirrors the model-pool generation-keyed fix). The per-slot
/// suffix keeps the C slots distinct within one episode (keying on the member
/// alone would collapse all C-1 extra slots to one).
async fn inject_acquire(
    nats: &MekhanNats,
    pool_net_id: &str,
    member: Uuid,
    slot: u32,
    epoch: i64,
    caps: &serde_json::Value,
) {
    let subject = format!(
        "petri.bridge.{pool_net_id}.{}",
        well_known::POOL_PRESENCE_ACQUIRE_INBOX
    );
    let unit_id = format!("{member}#{slot}@{epoch}");
    let envelope = json!({
        "source_net_id": "mekhan-human-presence-controller",
        "source_place_name": "presence",
        "token_color": {
            "unit_id": unit_id,
            // CRITICAL: the field is `runner_id` (= the member id) so the engine
            // pool net's generic reap/grant correlation matches — we reuse the
            // runner plumbing verbatim. `assignee` is additive; P3's grant relays it.
            "runner_id": member.to_string(),
            "executor_namespace": format!("human/{member}"),
            "assignee": member.to_string(),
            "caps": caps,
        },
        "signal_key": format!("human-presence-acquire-{unit_id}"),
        "timestamp": Utc::now().to_rfc3339(),
        "dedup_id": format!("presence-acquire:{unit_id}"),
    });
    publish_jetstream(nats, &subject, &envelope, "human presence acquire").await;
}

/// Inject a BARE `presence_expired { runner_id }` signal into the pool net's
/// signal place via `petri.signal.<pool_net_id>.presence_expired`.
///
/// Same shape as [`crate::runners_presence`]'s expire: `runner_id` carries the
/// MEMBER id (the reap key), and one signal is injected per applied slot — each
/// is consumed once and reaps exactly one of the member's `C` slots
/// (reap-ALL-by-reap-key). Wire shape is the engine's `ExternalSignal` envelope;
/// NO reply routing (signals are injected routing-less).
async fn inject_expire(nats: &MekhanNats, pool_net_id: &str, member: Uuid) {
    let subject = format!(
        "petri.signal.{pool_net_id}.{}",
        well_known::POOL_PRESENCE_EXPIRED_SIGNAL
    );
    let envelope = json!({
        "source": "human-presence",
        "signal_key": format!("human-presence-expire-{member}-{}", Utc::now().timestamp_millis()),
        "payload": { "runner_id": member.to_string() },
        "timestamp": Utc::now().to_rfc3339(),
    });
    publish_jetstream(nats, &subject, &envelope, "human presence expire").await;
}

/// Publish a JSON envelope to a JetStream subject and await the ack, logging at
/// WARN on any failure (a missed injection is non-fatal — the next heartbeat
/// re-acquires, and the sweep re-expires).
async fn publish_jetstream(
    nats: &MekhanNats,
    subject: &str,
    envelope: &serde_json::Value,
    what: &str,
) {
    let bytes = match serde_json::to_vec(envelope) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(subject, "failed to serialize {what} envelope: {e}");
            return;
        }
    };
    match nats
        .jetstream()
        .publish(subject.to_string(), bytes.into())
        .await
    {
        Ok(ack) => {
            if let Err(e) = ack.await {
                tracing::warn!(subject, "{what} publish ack failed: {e}");
            }
        }
        Err(e) => tracing::warn!(subject, "{what} publish failed: {e}"),
    }
}

/// Count how many pool units the engine net currently holds for `member` — the
/// `runner_id`-matching tokens in BOTH the FREE (`pool`) and HELD (`in_use`)
/// places. This is the leak-free authority for the acquire top-up: the engine
/// net is the source of truth for admitted slots, NOT mekhan's in-memory map
/// (which is wiped on a mekhan restart while the engine retains its units).
///
/// Returns `None` on any engine error or unexpected marking shape — callers
/// treat `None` as "assume already at capacity" (inject NOTHING) so a transient
/// engine blip can never DOUBLE-admit. The marking shape is
/// `marking.tokens.{pool,in_use}[].color.value.runner_id`.
async fn count_member_units(petri: &PetriClient, pool_net_id: &str, member: Uuid) -> Option<u32> {
    let state = petri.try_get_state(pool_net_id).await?;
    let marking = serde_json::to_value(&state.marking).ok()?;
    Some(count_units_in_marking(&marking, &member.to_string()))
}

/// Pure token-counter over an engine marking JSON: the number of `pool` + `in_use`
/// tokens whose `color.value.runner_id` equals `member`. Free function so the
/// shape-parsing is unit-testable without an engine.
fn count_units_in_marking(marking: &serde_json::Value, member: &str) -> u32 {
    let tokens = &marking["tokens"];
    let mut n = 0u32;
    for place in ["pool", "in_use"] {
        if let Some(arr) = tokens[place].as_array() {
            for tok in arr {
                if tok["color"]["value"]["runner_id"].as_str() == Some(member) {
                    n += 1;
                }
            }
        }
    }
    n
}

/// Reconcile ONE member's admission against the pool net. Computes
/// [`should_admit`] and drives the edge: on the absent→present edge TOP UP the
/// member's pool slots to `C` (inject only `C - existing`, where `existing` is
/// the engine's CURRENT unit count — so a re-admit after a mekhan restart that
/// left the engine's units intact does NOT double-admit), and flip
/// `present=true`; on the present→absent edge inject `C` expire signals and flip
/// `present=false`. A steady state (no edge) injects nothing.
///
/// The entry's facets (pool_net_id, concurrency, caps, member) are snapshotted
/// under the lock and the injection (incl. the engine count query) runs OUTSIDE
/// it so a slow JetStream publish / engine round-trip never holds the map.
async fn reconcile(
    petri: &PetriClient,
    nats: &MekhanNats,
    presence: &HumanPresenceMap,
    key: HumanKey,
) {
    let now = Instant::now();
    // Decide the edge under the lock, snapshot what we need to inject, flip the
    // `present` flag in the same critical section so a concurrent renewal can't
    // race the edge.
    enum Edge {
        Acquire {
            pool_net_id: String,
            member: Uuid,
            concurrency: u32,
            /// Per-admission stamp folded into each slot's `unit_id`/`dedup_id` so
            /// a re-admit within JetStream's dedup window is NOT suppressed.
            epoch: i64,
            caps: serde_json::Value,
        },
        Expire {
            pool_net_id: String,
            member: Uuid,
            concurrency: u32,
        },
        None,
    }
    let edge = {
        let mut map = presence.lock().await;
        let Some(entry) = map.get_mut(&key) else {
            return;
        };
        let should = should_admit(entry, now, entry.ttl);
        if should && !entry.present {
            entry.present = true;
            Edge::Acquire {
                pool_net_id: entry.pool_net_id.clone(),
                member: entry.member_user_id,
                concurrency: entry.concurrency,
                // Stamp this admission EPISODE so the C slots' unit_ids (and thus
                // dedup_ids) differ from any prior episode for this member — a
                // toggle off→on inside JetStream's dedup window re-admits fresh.
                epoch: Utc::now().timestamp_millis(),
                caps: entry.caps.clone(),
            }
        } else if !should && entry.present {
            entry.present = false;
            Edge::Expire {
                pool_net_id: entry.pool_net_id.clone(),
                member: entry.member_user_id,
                concurrency: entry.concurrency,
            }
        } else {
            Edge::None
        }
    };

    match edge {
        Edge::Acquire {
            pool_net_id,
            member,
            concurrency,
            epoch,
            caps,
        } => {
            // Top up to C against the engine's CURRENT count (the leak-free
            // authority). `None` (engine error) is treated as "already at C" so a
            // blip never double-admits; the next edge reconciles. A member whose
            // engine slots survived a mekhan restart counts as `existing == C` →
            // inject 0 (we just re-track them in-memory), the case that previously
            // double-admitted.
            let existing = count_member_units(petri, &pool_net_id, member)
                .await
                .unwrap_or(concurrency);
            let need = concurrency.saturating_sub(existing);
            for slot in 0..need {
                inject_acquire(nats, &pool_net_id, member, slot, epoch, &caps).await;
            }
            tracing::info!(
                %member, pool_net_id, concurrency, existing, need,
                "human presence acquired (topped member's pool slots up to C)"
            );
        }
        Edge::Expire {
            pool_net_id,
            member,
            concurrency,
        } => {
            for _ in 0..concurrency {
                inject_expire(nats, &pool_net_id, member).await;
            }
            tracing::info!(
                %member, pool_net_id, concurrency,
                "human presence expired (member reaped from pool)"
            );
        }
        Edge::None => {}
    }
}

/// The `availability` (INTENT) payload: a member's durable toggle on a specific
/// human capacity. `capacity_id` + `workspace_id` come from the payload; the
/// member is authoritative from the SUBJECT.
#[derive(Deserialize)]
struct AvailabilityPayload {
    available: bool,
    capacity_id: Uuid,
    workspace_id: Uuid,
}

/// Load the TRUSTED roster row's facets (caps, concurrency, availability config)
/// for `(workspace, capacity, member)`. `None` if the member is not enrolled (or
/// is revoked). These are admin-assigned — NEVER the wire claim.
async fn load_roster_row(
    db: &PgPool,
    workspace_id: Uuid,
    capacity_id: Uuid,
    member_user_id: Uuid,
) -> Option<(serde_json::Value, i32, AvailabilityConfig)> {
    let row: Option<(serde_json::Value, i32, serde_json::Value)> =
        sqlx::query_as::<_, (serde_json::Value, i32, serde_json::Value)>(
            "SELECT caps, concurrency, availability FROM roster_members \
             WHERE workspace_id = $1 AND capacity_id = $2 AND member_user_id = $3 \
               AND revoked_at IS NULL",
        )
        .bind(workspace_id)
        .bind(capacity_id)
        .bind(member_user_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    row.map(|(caps, concurrency, availability)| {
        let cfg: AvailabilityConfig = serde_json::from_value(availability).unwrap_or_default();
        (caps, concurrency, cfg)
    })
}

/// Handle one `human.*.availability` (INTENT) message. On `available=true` load
/// the trusted roster row, cache its facets, set intent on + `last_seen=now`
/// (so a `session` entry is immediately live on toggle), and reconcile. On
/// `available=false` clear the intent and reconcile (which expires it).
async fn handle_availability(
    db: &PgPool,
    petri: &PetriClient,
    nats: &MekhanNats,
    presence: &HumanPresenceMap,
    member: Uuid,
    payload: &[u8],
) {
    let Ok(p) = serde_json::from_slice::<AvailabilityPayload>(payload) else {
        tracing::debug!(%member, "malformed availability payload; ignoring");
        return;
    };
    let key: HumanKey = (p.capacity_id, member);

    if p.available {
        // Load the TRUSTED row — caps / concurrency / availability are
        // admin-assigned, never the wire claim. A member toggling availability on
        // a capacity they aren't enrolled in is silently ignored.
        let Some((caps, concurrency, cfg)) =
            load_roster_row(db, p.workspace_id, p.capacity_id, member).await
        else {
            tracing::debug!(
                %member, capacity_id = %p.capacity_id,
                "availability=true from a member not on the roster; ignoring"
            );
            return;
        };
        let ttl = if cfg.ttl_secs > 0 {
            Duration::from_secs(cfg.ttl_secs)
        } else {
            default_presence_ttl()
        };
        let concurrency = concurrency.max(0) as u32;
        let pool_net_id = well_known::pool_net_id(p.capacity_id);
        {
            let mut map = presence.lock().await;
            let entry = map.entry(key).or_insert_with(|| HumanPresenceEntry {
                last_seen: Instant::now(),
                intent_available: false,
                present: false,
                concurrency,
                pool_net_id: pool_net_id.clone(),
                caps: caps.clone(),
                liveness_source: cfg.liveness_source,
                ttl,
                workspace_id: p.workspace_id,
                member_user_id: member,
            });
            // Refresh the trusted facets + arm intent. `last_seen=now` makes a
            // `session` entry immediately admissible on toggle (its heartbeat
            // hasn't arrived yet, but the toggle itself is a renewal).
            entry.last_seen = Instant::now();
            entry.intent_available = true;
            entry.concurrency = concurrency;
            entry.pool_net_id = pool_net_id;
            entry.caps = caps;
            entry.liveness_source = cfg.liveness_source;
            entry.ttl = ttl;
            entry.workspace_id = p.workspace_id;
        }
    } else {
        // Clear intent → reconcile will expire the member if it was present.
        let mut map = presence.lock().await;
        if let Some(entry) = map.get_mut(&key) {
            entry.intent_available = false;
        } else {
            // Not tracked → nothing to expire.
            return;
        }
    }

    reconcile(petri, nats, presence, key).await;
}

/// One durably-`available` enrollment of a member — the trusted facets needed to
/// (re)admit them, read from the `roster_members` source of truth.
struct AvailableEnrollment {
    capacity_id: Uuid,
    workspace_id: Uuid,
    caps: serde_json::Value,
    concurrency: u32,
    ttl: Duration,
    cfg: AvailabilityConfig,
}

/// Load a member's currently-`available` (durable intent ON), non-revoked
/// enrollments. The durable `available` column is the source of truth the
/// heartbeat self-heal re-admits from after the in-memory map is lost (a mekhan
/// restart) or never had the entry (reconnect on a non-inbox page).
async fn load_available_enrollments_for_member(
    db: &PgPool,
    member: Uuid,
) -> Vec<AvailableEnrollment> {
    let rows: Vec<(Uuid, Uuid, serde_json::Value, i32, serde_json::Value)> =
        sqlx::query_as(
            "SELECT workspace_id, capacity_id, caps, concurrency, availability \
             FROM roster_members \
             WHERE member_user_id = $1 AND available = TRUE AND revoked_at IS NULL",
        )
        .bind(member)
        .fetch_all(db)
        .await
        .unwrap_or_default();

    rows.into_iter()
        .map(|(workspace_id, capacity_id, caps, concurrency, availability)| {
            let cfg: AvailabilityConfig = serde_json::from_value(availability).unwrap_or_default();
            let ttl = if cfg.ttl_secs > 0 {
                Duration::from_secs(cfg.ttl_secs)
            } else {
                default_presence_ttl()
            };
            AvailableEnrollment {
                capacity_id,
                workspace_id,
                caps,
                concurrency: concurrency.max(0) as u32,
                ttl,
                cfg,
            }
        })
        .collect()
}

/// Handle one `human.*.presence` (LIVENESS) heartbeat. The payload is
/// empty/ignored — the member is the SUBJECT.
///
/// Two jobs: (1) bump `last_seen` for ALL of that member's tracked entries (a
/// member may be enrolled in several pools) so a `session` entry stays live, and
/// (2) SELF-HEAL — ensure every durably-`available` enrollment is tracked with
/// intent ON, seeding it from the `roster_members` source of truth if the
/// in-memory entry is missing. This is what re-onlines a member after a mekhan
/// restart (map wiped, durable `available` still TRUE) or a reconnect on a
/// non-inbox page — WITHOUT a re-toggle. Reconcile then tops their pool slots up
/// to `C` against the engine's CURRENT count, so a self-heal NEVER double-admits.
async fn handle_heartbeat(
    db: &PgPool,
    petri: &PetriClient,
    nats: &MekhanNats,
    presence: &HumanPresenceMap,
    member: Uuid,
) {
    let now = Instant::now();
    // Durable source of truth for this member's available enrollments (read
    // before taking the lock — `await` must not be held across the std Mutex).
    let available = load_available_enrollments_for_member(db, member).await;

    let keys: Vec<HumanKey> = {
        let mut map = presence.lock().await;
        // (1) bump every tracked entry of this member (a live session renewal).
        for (key, entry) in map.iter_mut() {
            if key.1 == member {
                entry.last_seen = now;
            }
        }
        // (2) self-heal: ensure each durably-available enrollment is tracked with
        //     intent ON + fresh facets. `or_insert` seeds a missing entry (the
        //     post-restart / reconnect case); reconcile then admits via top-up.
        for a in &available {
            let key: HumanKey = (a.capacity_id, member);
            let pool_net_id = well_known::pool_net_id(a.capacity_id);
            let entry = map.entry(key).or_insert_with(|| HumanPresenceEntry {
                last_seen: now,
                intent_available: false,
                present: false,
                concurrency: a.concurrency,
                pool_net_id: pool_net_id.clone(),
                caps: a.caps.clone(),
                liveness_source: a.cfg.liveness_source,
                ttl: a.ttl,
                workspace_id: a.workspace_id,
                member_user_id: member,
            });
            entry.last_seen = now;
            entry.intent_available = true;
            entry.concurrency = a.concurrency;
            entry.caps = a.caps.clone();
            entry.liveness_source = a.cfg.liveness_source;
            entry.ttl = a.ttl;
            entry.pool_net_id = pool_net_id;
            entry.workspace_id = a.workspace_id;
        }
        map.keys().filter(|k| k.1 == member).copied().collect()
    };
    for key in keys {
        reconcile(petri, nats, presence, key).await;
    }
}

/// Parse the member UUID out of a `human.{member}.{suffix}` subject. Returns
/// `None` on any structural mismatch (wrong arity, wrong prefix/suffix, or a
/// non-UUID token).
fn member_from_subject(subject: &str, expect_suffix: &str) -> Option<Uuid> {
    let parts: Vec<&str> = subject.split('.').collect();
    // human.{member}.{suffix}
    if parts.len() != 3 || parts[0] != "human" || parts[2] != expect_suffix {
        return None;
    }
    Uuid::parse_str(parts[1]).ok()
}

/// Start the human presence subscriber: ONE task that `tokio::select!`s over BOTH
/// the `human.*.availability` (INTENT) and `human.*.presence` (LIVENESS) core-NATS
/// subscriptions. Both are ephemeral liveness (not a durable command stream), so a
/// plain core subscription is right — a missed message is harmless (the next one
/// re-renews; the sweep handles a true absence).
pub(crate) async fn start_human_presence_subscriber(
    nats: MekhanNats,
    db: PgPool,
    petri: PetriClient,
    presence: HumanPresenceMap,
) {
    let mut availability = match nats.client().subscribe("human.*.availability").await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to subscribe to human.*.availability: {e}");
            return;
        }
    };
    let mut heartbeat = match nats.client().subscribe("human.*.presence").await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to subscribe to human.*.presence: {e}");
            return;
        }
    };
    tracing::info!("human presence subscriber started on human.*.availability + human.*.presence");

    loop {
        tokio::select! {
            msg = availability.next() => {
                let Some(msg) = msg else {
                    tracing::warn!("human availability subscriber stream ended");
                    break;
                };
                let Some(member) = member_from_subject(msg.subject.as_str(), "availability") else {
                    tracing::debug!(subject = %msg.subject, "ignoring non-availability subject");
                    continue;
                };
                handle_availability(&db, &petri, &nats, &presence, member, &msg.payload).await;
            }
            msg = heartbeat.next() => {
                let Some(msg) = msg else {
                    tracing::warn!("human presence subscriber stream ended");
                    break;
                };
                let Some(member) = member_from_subject(msg.subject.as_str(), "presence") else {
                    tracing::debug!(subject = %msg.subject, "ignoring non-presence subject");
                    continue;
                };
                handle_heartbeat(&db, &petri, &nats, &presence, member).await;
            }
        }
    }
}

/// Start the human presence sweep loop: every [`SWEEP_INTERVAL_SECS`] scan the
/// presence map for `present` entries whose liveness source is NOT
/// [`LivenessSource::None`] and whose per-entry `ttl` has elapsed since
/// `last_seen`, inject `C` bare expire signals for each, and flip them to absent.
///
/// `None` (durable) entries never TTL-expire — only an `available=false` toggle
/// (via [`handle_availability`] → [`reconcile`]) expires them.
pub(crate) async fn start_human_presence_sweep(nats: MekhanNats, presence: HumanPresenceMap) {
    let mut tick = tokio::time::interval(Duration::from_secs(SWEEP_INTERVAL_SECS));
    tracing::info!(
        sweep_secs = SWEEP_INTERVAL_SECS,
        "human presence sweep started"
    );

    loop {
        tick.tick().await;
        let now = Instant::now();

        // Collect the expired set under the lock, flipping them to absent in the
        // same critical section so a concurrent heartbeat racing past here either
        // re-bumps last_seen (no expiry) or is cleanly re-acquired afterwards. The
        // per-entry `ttl` governs (not a global), and a `None` (durable) source is
        // never swept — it only expires on an availability toggle.
        let expired: Vec<(Uuid, String, u32)> = {
            let mut map = presence.lock().await;
            let mut out = Vec::new();
            for (key, entry) in map.iter_mut() {
                if should_sweep(entry, now) {
                    entry.present = false;
                    out.push((key.1, entry.pool_net_id.clone(), entry.concurrency));
                }
            }
            out
        };

        for (member, pool_net_id, concurrency) in expired {
            tracing::info!(
                %member, pool_net_id, concurrency,
                "human presence TTL miss; reaping member's slots"
            );
            for _ in 0..concurrency {
                inject_expire(&nats, &pool_net_id, member).await;
            }
        }
    }
}

/// Construct a fresh, empty human presence map. The subscriber + sweep tasks
/// share it.
pub(crate) fn new_human_presence_map() -> HumanPresenceMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Spawn BOTH human presence tasks (the dual-subscription subscriber + the sweep)
/// sharing one presence map. Called from the Wire phase. The `presence` handle is
/// the SHARED one also stored in [`crate::AppState`] so a read API observes the
/// very map the tasks mutate. Admission edges drive the pool net over NATS
/// (bridge + signal); the `petri` client is used READ-ONLY on the acquire edge to
/// count the member's current engine units so a re-admit tops up to `C` instead
/// of double-injecting (the leak-free reconcile).
pub fn spawn_human_presence_controller(
    presence: HumanPresence,
    nats: MekhanNats,
    db: PgPool,
    petri: PetriClient,
) {
    tokio::spawn(start_human_presence_subscriber(
        nats.clone(),
        db,
        petri,
        presence.map().clone(),
    ));
    tokio::spawn(start_human_presence_sweep(nats, presence.map().clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(intent: bool, source: LivenessSource, last_seen: Instant) -> HumanPresenceEntry {
        HumanPresenceEntry {
            last_seen,
            intent_available: intent,
            present: false,
            concurrency: 1,
            pool_net_id: "pool-x".to_string(),
            caps: serde_json::json!({}),
            liveness_source: source,
            ttl: Duration::from_secs(45),
            workspace_id: Uuid::new_v4(),
            member_user_id: Uuid::new_v4(),
        }
    }

    /// An ADMITTED entry (`present = true`) with the given source + last renewal,
    /// for exercising the TTL sweep gate.
    fn present_entry(source: LivenessSource, last_seen: Instant) -> HumanPresenceEntry {
        let mut e = entry(true, source, last_seen);
        e.present = true;
        e
    }

    #[test]
    fn counts_member_units_in_marking() {
        let me = "3bb26085-29f3-5fbf-8a8c-a2e485a1f55b";
        let other = "00000000-0000-0000-0000-000000000aaa";
        // Real engine marking shape: marking.tokens.{place}[].color.value.runner_id.
        let marking = serde_json::json!({
            "tokens": {
                "pool": [
                    { "id": "u1", "color": { "type": "Data", "value": { "runner_id": me, "unit_id": "x#0@1" } } },
                    { "id": "u2", "color": { "type": "Data", "value": { "runner_id": me, "unit_id": "x#1@1" } } },
                    { "id": "u3", "color": { "type": "Data", "value": { "runner_id": other, "unit_id": "y#0@1" } } }
                ],
                "in_use": [
                    { "id": "h1", "color": { "type": "Data", "value": { "runner_id": me, "unit_id": "x#2@1" } } }
                ],
                "presence_acquire": [],
                "done": [
                    // a reaped token for `me` must NOT count (not a live pool/in_use slot)
                    { "id": "d1", "color": { "type": "Data", "value": { "runner_id": me, "outcome": "reaped_free" } } }
                ]
            }
        });
        // 2 free (pool) + 1 held (in_use) = 3 live units for `me`; `done` excluded.
        assert_eq!(count_units_in_marking(&marking, me), 3);
        // The other member has exactly its 1 free unit.
        assert_eq!(count_units_in_marking(&marking, other), 1);
        // An unknown member has none.
        assert_eq!(count_units_in_marking(&marking, "deadbeef"), 0);
        // A degenerate/empty marking is 0, never a panic.
        assert_eq!(count_units_in_marking(&serde_json::json!({}), me), 0);
    }

    #[test]
    fn parses_well_formed_subjects() {
        let id = Uuid::new_v4();
        assert_eq!(
            member_from_subject(&format!("human.{id}.availability"), "availability"),
            Some(id)
        );
        assert_eq!(
            member_from_subject(&format!("human.{id}.presence"), "presence"),
            Some(id)
        );
    }

    #[test]
    fn rejects_malformed_subjects() {
        let id = Uuid::new_v4();
        // Wrong suffix.
        assert!(member_from_subject(&format!("human.{id}.presence"), "availability").is_none());
        // Wrong prefix.
        assert!(member_from_subject(&format!("runner.{id}.presence"), "presence").is_none());
        // Not a UUID.
        assert!(member_from_subject("human.not-a-uuid.presence", "presence").is_none());
        // Too few / too many tokens.
        assert!(member_from_subject("human.presence", "presence").is_none());
        assert!(member_from_subject(&format!("human.{id}.presence.extra"), "presence").is_none());
    }

    #[test]
    fn should_admit_truth_table() {
        let now = Instant::now();
        let ttl = Duration::from_secs(45);

        // Intent OFF → never admitted, regardless of source.
        assert!(!should_admit(&entry(false, LivenessSource::None, now), now, ttl));
        assert!(!should_admit(
            &entry(false, LivenessSource::Session, now),
            now,
            ttl
        ));

        // Intent ON + None (durable) → admitted, even with a stale last_seen.
        let stale = now - Duration::from_secs(3600);
        assert!(should_admit(&entry(true, LivenessSource::None, now), now, ttl));
        assert!(should_admit(
            &entry(true, LivenessSource::None, stale),
            now,
            ttl
        ));

        // Intent ON + Session, fresh → admitted.
        assert!(should_admit(
            &entry(true, LivenessSource::Session, now),
            now,
            ttl
        ));

        // Intent ON + Session, stale (last_seen older than ttl) → NOT admitted.
        let just_over = now - Duration::from_secs(46);
        assert!(!should_admit(
            &entry(true, LivenessSource::Session, just_over),
            now,
            ttl
        ));

        // External behaves like Session for the TTL gate.
        assert!(should_admit(
            &entry(true, LivenessSource::External, now),
            now,
            ttl
        ));
        assert!(!should_admit(
            &entry(true, LivenessSource::External, just_over),
            now,
            ttl
        ));
    }

    #[test]
    fn should_sweep_truth_table() {
        let now = Instant::now();
        let fresh = now; // last_seen == now → 0 elapsed
        let just_over = now - Duration::from_secs(46); // > 45s ttl
        let just_under = now - Duration::from_secs(44); // <= 45s ttl

        // A not-yet-admitted entry has nothing to reap, even if stale.
        let mut absent = present_entry(LivenessSource::Session, just_over);
        absent.present = false;
        assert!(!should_sweep(&absent, now));

        // Session/External + present + stale → swept.
        assert!(should_sweep(
            &present_entry(LivenessSource::Session, just_over),
            now
        ));
        assert!(should_sweep(
            &present_entry(LivenessSource::External, just_over),
            now
        ));

        // Session + present but FRESH (within ttl) → not swept. Boundary: the
        // `> ttl` sweep gate is the exact complement of `should_admit`'s `<= ttl`.
        assert!(!should_sweep(
            &present_entry(LivenessSource::Session, fresh),
            now
        ));
        assert!(!should_sweep(
            &present_entry(LivenessSource::Session, just_under),
            now
        ));

        // None (durable) is NEVER swept, however stale — only a toggle expires it.
        assert!(!should_sweep(
            &present_entry(LivenessSource::None, just_over),
            now
        ));
        assert!(!should_sweep(
            &present_entry(LivenessSource::None, now - Duration::from_secs(86_400)),
            now
        ));
    }
}
