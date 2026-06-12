//! The shared presence substrate (docs/35 §1) — everything the two liveness
//! adapters ([`super::runners`], [`super::humans`]) have in common, extracted
//! verbatim so the engine-facing wire shapes cannot drift between kinds:
//!
//! - [`publish_jetstream`] — the one JetStream publish-and-ack helper.
//! - [`PoolInjection`]/[`bridge_envelope`] + [`PoolSignal`]/[`signal_envelope`]
//!   — the engine-injection envelope builders. The CALLER builds the
//!   `token_color` value and the dedup-id string (the field sets and dedup
//!   SCHEMES are per-kind policy: runner acquires key on a stable
//!   `{runner_id}#{slot}`, human acquires on an epoch-stamped
//!   `{member}#{slot}@{epoch}`, claims on `presence-claim:{grant_id}`); core
//!   wraps them into the engine's `CrossNetTokenTransfer` / `ExternalSignal`
//!   shapes so the envelope JSON is byte-identical across kinds.
//! - [`inject_claim`] — the consent-pool claim bridge (docs/33 + docs/35 §4),
//!   kind-agnostic by construction (`runner_id` is the generic correlate key).
//! - [`inject_acquire`]/[`inject_expires`] — the acquire/expire delegators
//!   over the well-known pool inbox/signal (the BUILDERS stay per-kind).
//! - [`grow_slots`] — the pure grow-eager / shrink-lazy slot delta.
//! - [`sweep_loop`] — the generic TTL sweep, parameterized over the entry map,
//!   a `should_sweep` predicate, the under-lock expire flip, and the per-item
//!   async reap.
//! - [`env_ttl_secs`] — the env-var TTL reader (var name + default per kind).
//! - [`EntryMap`]/[`new_entry_map`]/[`snapshot_entries`] — the shared
//!   in-memory liveness map, generic over the per-kind KEY (runner UUID vs the
//!   human `(capacity, member)` tuple — the key SCHEMES are per-kind policy)
//!   and entry, plus its read-API snapshot walk.
//! - [`uuid_from_subject`] — the `{prefix}.{uuid}.{suffix}` liveness-subject
//!   grammar (the prefixes/suffixes and which SOURCES exist stay per-kind).
//! - [`subscribe`] — the core-NATS subscribe-or-log harness.
//! - [`spawn_controller`] — the two-task (subscriber + sweep) spawn harness.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::json;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::compiler::well_known;
use crate::nats::MekhanNats;

/// Read a configured presence TTL (seconds) from the env var `var`, defaulting
/// to `default_secs`. A parse failure or non-positive value falls back to the
/// default with a WARN so a typo can't silently disable reaping.
pub(crate) fn env_ttl_secs(var: &str, default_secs: u64) -> Duration {
    match std::env::var(var) {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(n) if n > 0 => Duration::from_secs(n),
            _ => {
                tracing::warn!(
                    raw = %raw,
                    "{var} is not a positive integer; using default {default_secs}s"
                );
                Duration::from_secs(default_secs)
            }
        },
        Err(_) => Duration::from_secs(default_secs),
    }
}

/// The shared in-memory liveness map both adapters track presence in: per-kind
/// KEY (the runner UUID; the human `(capacity_id, member)` tuple) → per-kind
/// entry. Guarded by a single `tokio::sync::Mutex` shared between the
/// subscriber task and the sweep task. The critical sections are tiny (a
/// HashMap probe + a clone of small values), so a plain `Mutex` is correct and
/// contention-free in practice.
pub(crate) type EntryMap<K, E> = Arc<Mutex<HashMap<K, E>>>;

/// Construct a fresh, empty entry map. The subscriber + sweep tasks share it.
pub(crate) fn new_entry_map<K, E>() -> EntryMap<K, E> {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Snapshot the live entry map for a read API: lock the mutex, then map every
/// tracked entry through `row` against ONE shared [`Instant::now`] (an
/// `Instant` has no serializable form, so the adapters surface a relative age
/// computed against it). `async` because the inner map is a
/// `tokio::sync::Mutex` shared with the async controller tasks —
/// `blocking_lock` would panic inside the runtime.
pub(crate) async fn snapshot_entries<K, E, S>(
    map: &EntryMap<K, E>,
    row: impl Fn(&K, &E, Instant) -> S,
) -> Vec<S> {
    let now = Instant::now();
    let map = map.lock().await;
    map.iter().map(|(k, e)| row(k, e, now)).collect()
}

/// Parse the unit UUID out of a `{prefix}.{uuid}.{suffix}` liveness subject.
/// Returns `None` on any structural mismatch (wrong arity, wrong
/// prefix/suffix, or a non-UUID token). The middle token is the AUTHORITATIVE
/// identity for both adapters — never the payload.
pub(crate) fn uuid_from_subject(subject: &str, prefix: &str, suffix: &str) -> Option<Uuid> {
    let parts: Vec<&str> = subject.split('.').collect();
    if parts.len() != 3 || parts[0] != prefix || parts[2] != suffix {
        return None;
    }
    Uuid::parse_str(parts[1]).ok()
}

/// Open one core-NATS subscription, logging at ERROR and returning `None` on
/// failure (the caller's controller task exits — presence then visibly
/// degrades to "nothing is admitted" rather than panicking the process).
/// Liveness pings are ephemeral (not a durable command stream), so a plain
/// core subscription is right — a missed ping is harmless (the next one
/// re-renews; the sweep handles a true absence).
pub(crate) async fn subscribe(nats: &MekhanNats, subject: &str) -> Option<async_nats::Subscriber> {
    match nats.client().subscribe(subject.to_string()).await {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!("failed to subscribe to {subject}: {e}");
            None
        }
    }
}

/// Spawn one adapter's controller pair — the subscriber task and the TTL sweep
/// task — sharing one entry map. The harness is shape-only; each adapter
/// supplies its own futures (and their per-kind deps: the runner controller
/// threads [`crate::fleet::FleetLiveness`], the human one a `PetriClient`).
pub(crate) fn spawn_controller<A, B>(subscriber: A, sweep: B)
where
    A: Future<Output = ()> + Send + 'static,
    B: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(subscriber);
    tokio::spawn(sweep);
}

/// The caller-built parts of one pool-net BRIDGE injection (an acquire or a
/// claim): everything per-kind about the envelope, as data. Core wraps these
/// into the engine's `CrossNetTokenTransfer` shape ([`bridge_envelope`]) and
/// publishes ([`inject_bridge`]).
pub(crate) struct PoolInjection<'a> {
    /// Informational source tag (`mekhan-presence-controller` /
    /// `mekhan-human-presence-controller`) for causality/tracing attribution.
    pub source_net_id: &'a str,
    /// Informational source place tag (`presence` / `presence_claim`).
    pub source_place_name: &'a str,
    /// The unit/claim token color — the per-kind field set (runner:
    /// `{unit_id, runner_id, executor_namespace, caps}`; human adds
    /// `assignee`; claim: `{grant_id, runner_id}`). Built by the caller.
    pub token_color: serde_json::Value,
    /// The injection's signal key (per-kind prefix + identity).
    pub signal_key: String,
    /// The engine-side dedup id. The SCHEME is per-kind policy (stable
    /// per-slot vs epoch-stamped vs per-grant) — core treats it as data.
    pub dedup_id: String,
}

/// Build the engine's `CrossNetTokenTransfer` envelope for a bridge injection.
/// Pure (the timestamp is a parameter) so the exact JSON shape is
/// unit-testable; [`inject_bridge`] stamps `Utc::now()`.
pub(crate) fn bridge_envelope(inj: PoolInjection<'_>, timestamp: &str) -> serde_json::Value {
    json!({
        "source_net_id": inj.source_net_id,
        "source_place_name": inj.source_place_name,
        "token_color": inj.token_color,
        "signal_key": inj.signal_key,
        "timestamp": timestamp,
        "dedup_id": inj.dedup_id,
    })
}

/// Publish one bridge injection to
/// `petri.bridge.<pool_net_id>.<inbox>`. NO reply routing — bridge injections
/// are one-way (the unit lives in the pool until granted/reaped; a claim is
/// consumed by `t_claim`).
pub(crate) async fn inject_bridge(
    nats: &MekhanNats,
    pool_net_id: &str,
    inbox: &str,
    inj: PoolInjection<'_>,
    what: &str,
) {
    let subject = format!("petri.bridge.{pool_net_id}.{inbox}");
    let envelope = bridge_envelope(inj, &Utc::now().to_rfc3339());
    publish_jetstream(nats, &subject, &envelope, what).await;
}

/// Inject ONE slot's `presence_acquire` token into the pool net's
/// `presence_acquire` bridge_in place via
/// `petri.bridge.<pool_net_id>.presence_acquire`. Wire shape is the engine's
/// `CrossNetTokenTransfer` envelope (what the engine's global bridge listener
/// deserializes); NO reply routing (acquire is one-way — the unit lives in the
/// pool until granted/reaped). The BUILDER (`inj`) is per-kind: see the
/// adapters' `acquire_injection` for each identity + dedup scheme.
pub(crate) async fn inject_acquire(
    nats: &MekhanNats,
    pool_net_id: &str,
    inj: PoolInjection<'_>,
    what: &str,
) {
    inject_bridge(
        nats,
        pool_net_id,
        well_known::POOL_PRESENCE_ACQUIRE_INBOX,
        inj,
        what,
    )
    .await;
}

/// The caller-built parts of one pool-net SIGNAL injection (an expire). Core
/// wraps these into the engine's `ExternalSignal` shape ([`signal_envelope`])
/// and publishes ([`inject_signal`]).
pub(crate) struct PoolSignal<'a> {
    /// Informational source tag (`presence` / `human-presence`).
    pub source: &'a str,
    /// The signal key (per-kind prefix + identity + a per-emission stamp).
    pub signal_key: String,
    /// The bare token color (`{ runner_id }` — the generic reap key).
    pub payload: serde_json::Value,
}

/// Build the engine's `ExternalSignal` envelope for a signal injection. Pure
/// (the timestamp is a parameter) so the exact JSON shape is unit-testable;
/// [`inject_signal`] stamps `Utc::now()`. NO reply routing — signals are
/// injected routing-less; the "fail" routing for a held unit rides the HOLD,
/// not this signal.
pub(crate) fn signal_envelope(sig: PoolSignal<'_>, timestamp: &str) -> serde_json::Value {
    json!({
        "source": sig.source,
        "signal_key": sig.signal_key,
        "payload": sig.payload,
        "timestamp": timestamp,
    })
}

/// Publish one signal injection to `petri.signal.<pool_net_id>.<signal>`.
pub(crate) async fn inject_signal(
    nats: &MekhanNats,
    pool_net_id: &str,
    signal: &str,
    sig: PoolSignal<'_>,
    what: &str,
) {
    let subject = format!("petri.signal.{pool_net_id}.{signal}");
    let envelope = signal_envelope(sig, &Utc::now().to_rfc3339());
    publish_jetstream(nats, &subject, &envelope, what).await;
}

/// Inject `slots` BARE `presence_expired { runner_id }` signals into the pool
/// net's signal place via `petri.signal.<pool_net_id>.presence_expired` — one
/// per applied slot, since each signal is consumed once and reaps exactly one
/// of the unit's slots (reap-ALL-by-reap-key; the net's `t_reap_free` /
/// `t_reap_held` discriminate free-vs-held by input place, so mekhan keeps NO
/// holder tracking). `mk_signal` builds the per-kind signal from the
/// per-emission stamp (wall-clock millis, folded into the signal key); see the
/// adapters' `expire_signal` for each source/key scheme.
pub(crate) async fn inject_expires(
    nats: &MekhanNats,
    pool_net_id: &str,
    slots: u32,
    mk_signal: impl Fn(i64) -> PoolSignal<'static>,
    what: &str,
) {
    for _ in 0..slots {
        inject_signal(
            nats,
            pool_net_id,
            well_known::POOL_PRESENCE_EXPIRED_SIGNAL,
            mk_signal(Utc::now().timestamp_millis()),
            what,
        )
        .await;
    }
}

/// Build the claim injection's caller parts (pure, for the byte-shape test).
/// See [`inject_claim`].
pub(crate) fn claim_injection(grant_id: &str, runner_id: &str) -> PoolInjection<'static> {
    PoolInjection {
        source_net_id: "mekhan-presence-controller",
        source_place_name: "presence_claim",
        token_color: json!({
            "grant_id": grant_id,
            "runner_id": runner_id,
        }),
        signal_key: format!("presence-claim-{grant_id}-{runner_id}"),
        dedup_id: format!("presence-claim:{grant_id}"),
    }
}

/// Inject a UNIT-INITIATED `presence_claim { grant_id, runner_id }` token into
/// the pool net's `presence_claim` bridge_in place via
/// `petri.bridge.<pool_net_id>.presence_claim` (the `Acceptance::Consent`
/// claim path, docs/33 + docs/35 §4).
///
/// A claim binds a parked offer to the claiming MEMBER (docs/34 §3): the offer
/// net's `t_claim` correlates the unit on `runner_id` (= the member id), so the
/// claim binds ANY free slot of that member rather than an exact `unit_id`.
/// `t_claim` is first-claim-wins, so the offer token is consumed by the first
/// claim and any subsequent claims for the same offer are implicitly rescinded.
/// Wire shape mirrors the acquire's `CrossNetTokenTransfer` envelope EXACTLY:
/// `token_color` carries the `{ grant_id, runner_id }` claim, `source_*` tag
/// the claim to the presence controller for causality/tracing, and `dedup_id`
/// keys on the `grant_id` alone (`presence-claim:{grant_id}`) so a redelivered
/// claim for the same offer is suppressed at the engine (a claim is per-offer,
/// not per-unit-per-offer). Kind-agnostic — `runner_id` is the engine's generic
/// correlate key whatever the capacity kind — so it lives in core, not a
/// per-kind adapter.
pub(crate) async fn inject_claim(
    nats: &MekhanNats,
    pool_net_id: &str,
    grant_id: &str,
    runner_id: &str,
) {
    inject_bridge(
        nats,
        pool_net_id,
        well_known::POOL_PRESENCE_CLAIM_INBOX,
        claim_injection(grant_id, runner_id),
        "presence claim",
    )
    .await;
}

/// Publish a JSON envelope to a JetStream subject and await the ack, logging at
/// WARN on any failure (a missed injection is non-fatal — the next heartbeat
/// re-acquires, and the sweep re-expires).
pub(crate) async fn publish_jetstream(
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

/// The grow-eager / shrink-lazy slot delta for an already-present entry: given
/// it is `pool_less` (a liveness-only entry with no pool to admit into), its
/// `applied` slot count, and the new `wire` count, return the half-open range of
/// NEW slot indices to inject — `Some(applied..wire)` only on a true GROW into a
/// real pool, `None` on shrink/no-change/pool-less. Pure so the delta math is
/// unit-testable without NATS/DB. SHRINK is intentionally `None`: a held surplus
/// slot must finish its lease (it drains on release or at the next full expire),
/// so we never proactively reap on shrink — the caller just lowers the target.
///
/// Used by both adapters: the runner heartbeat injects the returned RANGE
/// directly (slot indices continue past the applied count); the human top-up
/// uses the range's LENGTH (its slot indices restart at 0 each admission
/// episode, disambiguated by the epoch stamp).
pub(crate) fn grow_slots(pool_less: bool, applied: u32, wire: u32) -> Option<std::ops::Range<u32>> {
    if pool_less || wire <= applied {
        return None;
    }
    Some(applied..wire)
}

/// One swept entry's reap order: what the per-kind `on_expire` callback needs
/// to inject the expire signals after the lock is released.
pub(crate) struct ExpiredSlots {
    /// The generic reap key (`runner_id` on the wire): the runner UUID or the
    /// member UUID.
    pub reap_key: Uuid,
    /// The entry's pool net id (may be empty for a liveness-only runner).
    pub pool_net_id: String,
    /// How many expire signals to inject (one per applied slot).
    pub slots: u32,
}

/// The generic TTL sweep loop shared by both adapters: every `interval`, scan
/// the entry map for entries `should_sweep` says are expired, flip each via
/// `expire_entry` (under the lock, in the same critical section that collected
/// it — so a concurrent renewal racing past either re-bumps `last_seen` before
/// the check or is cleanly re-acquired afterwards), then run the per-kind
/// `on_expire` reap OUTSIDE the lock.
///
/// - `should_sweep(entry, now)` — the per-kind reap gate (runners: global-TTL
///   freshness; humans: per-entry TTL + the durable-source never-swept rule).
/// - `expire_entry(key, entry)` — flips `present = false` (and any per-kind
///   bookkeeping, e.g. zeroing a runner's applied C) and snapshots the
///   [`ExpiredSlots`] reap order.
/// - `on_expire(expired)` — the async per-item reap (fleet-mirror drop, expire
///   signal injection, per-kind logging).
pub(crate) async fn sweep_loop<K, E, P, X, C, Fut>(
    map: Arc<Mutex<HashMap<K, E>>>,
    interval: Duration,
    should_sweep: P,
    expire_entry: X,
    mut on_expire: C,
) where
    P: Fn(&E, Instant) -> bool,
    X: Fn(&K, &mut E) -> ExpiredSlots,
    C: FnMut(ExpiredSlots) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut tick = tokio::time::interval(interval);
    loop {
        tick.tick().await;
        let now = Instant::now();

        let expired: Vec<ExpiredSlots> = {
            let mut map = map.lock().await;
            let mut out = Vec::new();
            for (key, entry) in map.iter_mut() {
                if should_sweep(entry, now) {
                    out.push(expire_entry(key, entry));
                }
            }
            out
        };

        for item in expired {
            on_expire(item).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::{humans, runners};

    #[test]
    fn grow_eager_shrink_lazy_delta() {
        // GROW into a real pool → inject the NEW slots only.
        assert_eq!(grow_slots(false, 2, 5), Some(2..5));
        assert_eq!(grow_slots(false, 0, 3), Some(0..3));
        // No change / SHRINK → inject nothing (shrink is lazy; the surplus drains
        // naturally).
        assert_eq!(grow_slots(false, 3, 3), None);
        assert_eq!(grow_slots(false, 5, 2), None);
        // A pool-less (liveness-only) entry never injects, even on a "grow".
        assert_eq!(grow_slots(true, 1, 4), None);
    }

    // ---- Envelope byte-shape pins (one per injection kind) ----
    //
    // These pin the EXACT JSON object each adapter emits for a fixed input, so
    // the extraction into core is provably byte-stable and a future edit to
    // either adapter cannot silently drift the engine wire contract. The
    // timestamp is the one dynamic field, pinned here via the pure builders'
    // `timestamp` parameter.

    const T: &str = "2026-01-01T00:00:00+00:00";

    #[test]
    fn runner_acquire_envelope_shape() {
        let rid = uuid::Uuid::parse_str("3bb26085-29f3-5fbf-8a8c-a2e485a1f55b").unwrap();
        let caps = serde_json::json!({ "backend": "python" });
        let inj = runners::acquire_injection(
            rid,
            2,
            "runner-jobs/3bb26085-29f3-5fbf-8a8c-a2e485a1f55b",
            &caps,
        );
        assert_eq!(
            inj.dedup_id,
            "presence-acquire:3bb26085-29f3-5fbf-8a8c-a2e485a1f55b#2"
        );
        assert_eq!(
            bridge_envelope(inj, T),
            serde_json::json!({
                "source_net_id": "mekhan-presence-controller",
                "source_place_name": "presence",
                "token_color": {
                    "unit_id": "3bb26085-29f3-5fbf-8a8c-a2e485a1f55b#2",
                    "runner_id": "3bb26085-29f3-5fbf-8a8c-a2e485a1f55b",
                    "executor_namespace": "runner-jobs/3bb26085-29f3-5fbf-8a8c-a2e485a1f55b",
                    "caps": { "backend": "python" },
                },
                "signal_key": "presence-acquire-3bb26085-29f3-5fbf-8a8c-a2e485a1f55b#2",
                "timestamp": T,
                "dedup_id": "presence-acquire:3bb26085-29f3-5fbf-8a8c-a2e485a1f55b#2",
            })
        );
    }

    #[test]
    fn runner_expire_envelope_shape() {
        let rid = uuid::Uuid::parse_str("3bb26085-29f3-5fbf-8a8c-a2e485a1f55b").unwrap();
        let sig = runners::expire_signal(rid, 1_700_000_000_123);
        assert_eq!(
            signal_envelope(sig, T),
            serde_json::json!({
                "source": "presence",
                "signal_key": "presence-expire-3bb26085-29f3-5fbf-8a8c-a2e485a1f55b-1700000000123",
                "payload": { "runner_id": "3bb26085-29f3-5fbf-8a8c-a2e485a1f55b" },
                "timestamp": T,
            })
        );
    }

    #[test]
    fn human_acquire_envelope_shape() {
        let member = uuid::Uuid::parse_str("9c0e1b2a-0000-4000-8000-000000000001").unwrap();
        let caps = serde_json::json!({ "skill": "review" });
        let inj = humans::acquire_injection(member, 1, 1_700_000_000_123, &caps);
        // Epoch-stamped per-episode dedup scheme (vs the runner's stable one).
        assert_eq!(
            inj.dedup_id,
            "presence-acquire:9c0e1b2a-0000-4000-8000-000000000001#1@1700000000123"
        );
        assert_eq!(
            bridge_envelope(inj, T),
            serde_json::json!({
                "source_net_id": "mekhan-human-presence-controller",
                "source_place_name": "presence",
                "token_color": {
                    "unit_id": "9c0e1b2a-0000-4000-8000-000000000001#1@1700000000123",
                    "runner_id": "9c0e1b2a-0000-4000-8000-000000000001",
                    "executor_namespace": "human/9c0e1b2a-0000-4000-8000-000000000001",
                    "assignee": "9c0e1b2a-0000-4000-8000-000000000001",
                    "caps": { "skill": "review" },
                },
                "signal_key": "human-presence-acquire-9c0e1b2a-0000-4000-8000-000000000001#1@1700000000123",
                "timestamp": T,
                "dedup_id": "presence-acquire:9c0e1b2a-0000-4000-8000-000000000001#1@1700000000123",
            })
        );
    }

    #[test]
    fn human_expire_envelope_shape() {
        let member = uuid::Uuid::parse_str("9c0e1b2a-0000-4000-8000-000000000001").unwrap();
        let sig = humans::expire_signal(member, 1_700_000_000_123);
        assert_eq!(
            signal_envelope(sig, T),
            serde_json::json!({
                "source": "human-presence",
                "signal_key": "human-presence-expire-9c0e1b2a-0000-4000-8000-000000000001-1700000000123",
                "payload": { "runner_id": "9c0e1b2a-0000-4000-8000-000000000001" },
                "timestamp": T,
            })
        );
    }

    #[test]
    fn claim_envelope_shape() {
        let inj = claim_injection("grant-42", "9c0e1b2a-0000-4000-8000-000000000001");
        assert_eq!(inj.dedup_id, "presence-claim:grant-42");
        assert_eq!(
            bridge_envelope(inj, T),
            serde_json::json!({
                "source_net_id": "mekhan-presence-controller",
                "source_place_name": "presence_claim",
                "token_color": {
                    "grant_id": "grant-42",
                    "runner_id": "9c0e1b2a-0000-4000-8000-000000000001",
                },
                "signal_key": "presence-claim-grant-42-9c0e1b2a-0000-4000-8000-000000000001",
                "timestamp": T,
                "dedup_id": "presence-claim:grant-42",
            })
        );
    }
}
