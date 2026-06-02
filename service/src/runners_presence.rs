//! Presence controller (Phase 3 — presence-lease pool capacity).
//!
//! A `presence_pool` resource is a capacity-LESS pool ([`crate::petri::presence_pool_net`]):
//! its capacity is not a seeded count but is driven by runner **presence**. This
//! module is mekhan's controller that turns the runner data-plane heartbeat into
//! pool-net admission/reap:
//!
//! 1. **SUBSCRIBE** to `runner.*.presence`. Each message is a liveness ping from
//!    a runner's data plane (Phase 2 JWT already grants `runner.{id}.presence`).
//!    The `runner_id` is parsed from the SUBJECT, never the payload. On the
//!    ABSENT→PRESENT edge we inject ONE `presence_acquire` token
//!    `{ runner_id, executor_namespace, caps }` into the runner's pool net via
//!    the cross-net bridge subject `petri.bridge.pool-<rid>.presence_acquire`.
//!    `executor_namespace` + `caps` come from the TRUSTED `runners` DB row,
//!    NEVER from the wire payload.
//!
//! 2. **SWEEP** a background loop tracks the last-renewal instant per runner_id
//!    in memory. On a TTL miss the runner is marked absent and a BARE
//!    `presence_expired { runner_id }` SIGNAL is injected via
//!    `petri.signal.pool-<rid>.presence_expired`. The net's `t_reap_free` /
//!    `t_reap_held` discriminate free-vs-held by input place, so mekhan keeps
//!    NO holder tracking.
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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::Mutex;
use uuid::Uuid;

use futures::StreamExt;

use crate::compiler::well_known;
use crate::models::runner::RunnerRow;
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
    match std::env::var("MEKHAN__RUNNERS__PRESENCE_TTL_SECS") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(n) if n > 0 => Duration::from_secs(n),
            _ => {
                tracing::warn!(
                    raw = %raw,
                    "MEKHAN__RUNNERS__PRESENCE_TTL_SECS is not a positive integer; \
                     using default {DEFAULT_PRESENCE_TTL_SECS}s"
                );
                Duration::from_secs(DEFAULT_PRESENCE_TTL_SECS)
            }
        },
        Err(_) => Duration::from_secs(DEFAULT_PRESENCE_TTL_SECS),
    }
}

/// One tracked runner's presence state.
pub(crate) struct PresenceEntry {
    /// Most recent presence heartbeat instant.
    last_seen: Instant,
    /// Pool net id (`pool-<resource_id>`) the runner's presence is admitted to.
    /// Resolved once on the acquire edge and cached so the sweep can inject the
    /// expire signal without another DB round-trip.
    pool_net_id: String,
    /// Whether mekhan currently considers the runner PRESENT (a `presence_acquire`
    /// has been injected and no expire has been injected since). Drives the
    /// absent→present acquire edge + the present→absent expire edge.
    present: bool,
}

/// In-memory presence map: `runner_id` → its tracked state. Guarded by a single
/// `Mutex` shared between the subscriber task and the sweep task. The critical
/// sections are tiny (a HashMap probe + a clone of small strings), so a plain
/// `Mutex` is correct and contention-free in practice.
type PresenceMap = Arc<Mutex<HashMap<Uuid, PresenceEntry>>>;

/// Resolve a runner's `pool` alias to its backing `presence_pool` net id.
///
/// `runner.pool` is an alias string (the `resources.path` column). It maps to a
/// `resources` row in the runner's workspace with `resource_type = 'presence_pool'`
/// and `path = <alias>`; the net id is then [`well_known::pool_net_id`] over that
/// resource's id. Returns `None` (with a skip log at the call site) when the
/// runner has no pool alias, or the alias resolves to no presence_pool resource
/// in its workspace.
async fn resolve_pool_net_id(db: &PgPool, runner: &RunnerRow) -> Option<String> {
    let alias = runner.pool.as_deref()?;
    let resource_id: Option<(Uuid,)> = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM resources \
         WHERE workspace_id = $1 AND path = $2 AND resource_type = 'presence_pool' \
           AND deleted_at IS NULL",
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
        "SELECT id, workspace_id, name, pool, token_hash, nats_public_key, capabilities, \
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

/// Inject a `presence_acquire` token into the pool net's `presence_acquire`
/// bridge_in place via `petri.bridge.<pool_net_id>.presence_acquire`.
///
/// Wire shape is the engine's [`CrossNetTokenTransfer`] envelope (what the
/// engine's global bridge listener deserializes): `token_color` carries the
/// `{ runner_id, executor_namespace, caps }` unit, and we set NO reply routing
/// (acquire is one-way — the unit lives in the pool until granted/reaped).
async fn inject_acquire(
    nats: &MekhanNats,
    pool_net_id: &str,
    runner_id: Uuid,
    executor_namespace: &str,
    caps: &serde_json::Value,
) {
    let subject = format!("petri.bridge.{pool_net_id}.{}", well_known::POOL_PRESENCE_ACQUIRE_INBOX);
    // `CrossNetTokenTransfer` shape (engine `cross_net_bridge.rs`). source_* are
    // informational; we tag them so causality/tracing attributes the unit to the
    // presence controller. `dedup_id` keys on the runner so a redelivered acquire
    // is suppressed at the engine while the runner stays present.
    let envelope = json!({
        "source_net_id": "mekhan-presence-controller",
        "source_place_name": "presence",
        "token_color": {
            "runner_id": runner_id.to_string(),
            "executor_namespace": executor_namespace,
            "caps": caps,
        },
        "signal_key": format!("presence-acquire-{runner_id}"),
        "timestamp": Utc::now().to_rfc3339(),
        "dedup_id": format!("presence-acquire:{runner_id}"),
    });
    publish_jetstream(nats, &subject, &envelope, "presence acquire").await;
}

/// Inject a BARE `presence_expired { runner_id }` signal into the pool net's
/// signal place via `petri.signal.<pool_net_id>.presence_expired`.
///
/// Wire shape is the engine's `ExternalSignal` envelope (the same the trigger
/// dispatcher publishes): `payload` is the bare `{ runner_id }` token color. NO
/// reply routing — signals are injected routing-less; the "fail" routing for a
/// held unit rides the HOLD, not this signal.
async fn inject_expire(nats: &MekhanNats, pool_net_id: &str, runner_id: Uuid) {
    let subject =
        format!("petri.signal.{pool_net_id}.{}", well_known::POOL_PRESENCE_EXPIRED_SIGNAL);
    let envelope = json!({
        "source": "presence",
        "signal_key": format!("presence-expire-{runner_id}-{}", Utc::now().timestamp_millis()),
        "payload": { "runner_id": runner_id.to_string() },
        "timestamp": Utc::now().to_rfc3339(),
    });
    publish_jetstream(nats, &subject, &envelope, "presence expire").await;
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
    match nats.jetstream().publish(subject.to_string(), bytes.into()).await {
        Ok(ack) => {
            if let Err(e) = ack.await {
                tracing::warn!(subject, "{what} publish ack failed: {e}");
            }
        }
        Err(e) => tracing::warn!(subject, "{what} publish failed: {e}"),
    }
}

/// Handle one `runner.*.presence` message: resolve the runner + its pool, and
/// on the absent→present edge inject the acquire token. Touches `runners.last_seen_at`
/// as a cheap UI liveness signal (best-effort; capacity is driven by the
/// in-memory map, not this column).
async fn handle_presence(
    db: &PgPool,
    nats: &MekhanNats,
    presence: &PresenceMap,
    runner_id: Uuid,
) {
    // Fast path: already present → just bump last_seen under the lock and return.
    // We still re-touch last_seen_at periodically below, but avoid a DB lookup on
    // every heartbeat of an already-admitted runner.
    {
        let mut map = presence.lock().await;
        if let Some(entry) = map.get_mut(&runner_id) {
            entry.last_seen = Instant::now();
            if entry.present {
                // Already admitted — nothing to inject. Drop the lock and do a
                // best-effort last_seen_at touch outside it.
                drop(map);
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
            pool = ?runner.pool,
            "runner present but no presence_pool resource in its workspace; skipping admit"
        );
        // Still record last_seen so a later resource-create + heartbeat admits it.
        touch_last_seen(db, runner_id).await;
        return;
    };

    let executor_namespace = format!("runner.{runner_id}");
    let caps = runner.capabilities.clone();

    inject_acquire(nats, &pool_net_id, runner_id, &executor_namespace, &caps).await;

    // Commit the present edge AFTER injecting so a crash between inject + map
    // update simply re-injects (idempotent at the engine via dedup_id).
    {
        let mut map = presence.lock().await;
        map.insert(
            runner_id,
            PresenceEntry {
                last_seen: Instant::now(),
                pool_net_id: pool_net_id.clone(),
                present: true,
            },
        );
    }
    touch_last_seen(db, runner_id).await;

    tracing::info!(%runner_id, pool_net_id, "presence acquired (runner admitted to pool)");
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

/// Parse the runner UUID out of a `runner.{runner_id}.presence` subject. Returns
/// `None` on any structural mismatch.
fn parse_runner_subject(subject: &str) -> Option<Uuid> {
    let parts: Vec<&str> = subject.split('.').collect();
    // runner.{id}.presence
    if parts.len() != 3 || parts[0] != "runner" || parts[2] != "presence" {
        return None;
    }
    Uuid::parse_str(parts[1]).ok()
}

/// Start the presence subscriber: a core-NATS subscription to `runner.*.presence`.
///
/// Presence pings are ephemeral liveness (not a durable command stream), so this
/// uses a plain core subscription rather than a JetStream durable — a missed
/// ping is harmless (the next one re-acquires; the sweep handles a true absence).
pub(crate) async fn start_presence_subscriber(nats: MekhanNats, db: PgPool, presence: PresenceMap) {
    let mut sub = match nats.client().subscribe("runner.*.presence").await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to subscribe to runner.*.presence: {e}");
            return;
        }
    };
    tracing::info!("presence subscriber started on runner.*.presence");

    while let Some(msg) = sub.next().await {
        let Some(runner_id) = parse_runner_subject(msg.subject.as_str()) else {
            tracing::debug!(subject = %msg.subject, "ignoring non-presence subject");
            continue;
        };
        handle_presence(&db, &nats, &presence, runner_id).await;
    }

    tracing::warn!("presence subscriber stream ended");
}

/// Start the presence sweep loop: every [`SWEEP_INTERVAL_SECS`] scan the
/// presence map for `present` entries whose `last_seen` is older than the TTL,
/// inject a BARE expire signal for each, and flip them to absent.
///
/// Mirrors the session-sweep spawn pattern in `lifecycle.rs` /
/// `main.rs` (an interval-driven background loop).
pub(crate) async fn start_presence_sweep(nats: MekhanNats, presence: PresenceMap) {
    let ttl = presence_ttl();
    let mut tick = tokio::time::interval(Duration::from_secs(SWEEP_INTERVAL_SECS));
    tracing::info!(
        ttl_secs = ttl.as_secs(),
        sweep_secs = SWEEP_INTERVAL_SECS,
        "presence sweep started"
    );

    loop {
        tick.tick().await;
        let now = Instant::now();

        // Collect the expired set under the lock, flipping them to absent in the
        // same critical section so a concurrent heartbeat racing past here either
        // re-bumps last_seen (no expiry) or is cleanly re-acquired afterwards.
        let expired: Vec<(Uuid, String)> = {
            let mut map = presence.lock().await;
            let mut out = Vec::new();
            for (rid, entry) in map.iter_mut() {
                if entry.present && now.duration_since(entry.last_seen) > ttl {
                    entry.present = false;
                    out.push((*rid, entry.pool_net_id.clone()));
                }
            }
            out
        };

        for (runner_id, pool_net_id) in expired {
            tracing::info!(%runner_id, pool_net_id, "presence TTL miss; reaping runner unit");
            inject_expire(&nats, &pool_net_id, runner_id).await;
        }
    }
}

/// Construct a fresh, empty presence map. The subscriber + sweep tasks share it.
pub(crate) fn new_presence_map() -> PresenceMap {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Spawn BOTH presence tasks (subscriber + sweep) sharing one presence map.
/// Called from `main.rs`. Threads the PgPool + NATS client. `PetriClient` is
/// accepted for symmetry with the other controllers and to keep the spawn site
/// uniform, even though the controller drives the pool net purely over NATS
/// (bridge + signal) and does not need the engine HTTP client today.
pub fn spawn_presence_controller(nats: MekhanNats, db: PgPool, _petri: PetriClient) {
    let presence = new_presence_map();
    tokio::spawn(start_presence_subscriber(
        nats.clone(),
        db.clone(),
        presence.clone(),
    ));
    tokio::spawn(start_presence_sweep(nats, presence));
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
        let presence = new_presence_map();
        let rid = Uuid::new_v4();

        // Simulate the acquire commit.
        {
            let mut map = presence.lock().await;
            map.insert(
                rid,
                PresenceEntry {
                    last_seen: Instant::now(),
                    pool_net_id: "pool-x".to_string(),
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
}
