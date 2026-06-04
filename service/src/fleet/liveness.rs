//! The unified [`FleetLiveness`] registry (docs/24 S1 + S2).
//!
//! Generalises the worker-pool's `BackendCoverage` into a single advisory
//! registry that tracks BOTH kinds of presence-emitting capacity:
//!
//! - **Workers** — anonymous competing-consumer executor daemons. They publish
//!   to `worker.*.presence` with `{ worker_id, backends }`; this module owns
//!   their subscriber + TTL sweep (the machinery absorbed from
//!   `worker_coverage`). `caps` = advertised `ExecutorJob` backend wire-names.
//! - **Runners** — enrolled instruments. [`crate::runners_presence`] owns their
//!   subscriber/sweep + the control-binding (pool-net inject/expire); it MIRRORS
//!   the runner's advisory facet (its self-reported `backends`) into here via
//!   [`FleetLiveness::upsert_runner`] / [`FleetLiveness::drop_runner`]. This
//!   module's sweep deliberately does NOT touch runner entries — their lifecycle
//!   is driven by that controller so the two TTL views can't disagree.
//!
//! ## Wire contract (workers)
//!
//! Workers are unauthenticated and ephemeral, so the `worker_id` and the
//! advertised `backends: Vec<String>` (snake-case wire names, e.g. `"python"`,
//! `"loki"`) both come from the PAYLOAD. This is safe because liveness is
//! advisory only: a lying worker at worst suppresses a warning that would have
//! been logged (or, conversely, suppresses one that should have fired). Runner
//! `backends` likewise come from the runner's presence PAYLOAD — its typed caps
//! (the authoritative `t_grant` dimension) are NEVER read from here.
//!
//! ## TTL sweep
//!
//! An in-memory map keys each capacity by `(kind, id)` to its advertised `caps`
//! and last-seen [`Instant`]. A background loop sweeps WORKER entries older than
//! the TTL (`MEKHAN__WORKERS__PRESENCE_TTL_SECS`, default 30s; sweep every 5s).
//! A missed worker heartbeat is harmless — the next ping re-registers, and a
//! true departure is reaped within ~one sweep.
//!
//! ## Eligibility (docs/23 §4)
//!
//! [`FleetLiveness::serves_backend`] is the single `satisfies`-shaped membership
//! check that collapses the two old split paths (workers' `is_covered` +
//! runners' `pool_covers`): it answers "is ANY live capacity — worker OR runner
//! — advertising this backend?" That is the trivial static-partition predicate
//! (`backend == <wire>`) evaluated against the live registry, used purely for
//! the best-effort publish-time WARN.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::nats::MekhanNats;

/// Default liveness TTL: a worker missing this long is dropped from the
/// registry. Overridable via `MEKHAN__WORKERS__PRESENCE_TTL_SECS`.
const DEFAULT_PRESENCE_TTL_SECS: u64 = 30;

/// How often the sweep loop wakes to look for TTL misses. Kept well below the
/// TTL so a worker drops within ~one sweep interval past expiry.
const SWEEP_INTERVAL_SECS: u64 = 5;

/// Read the configured liveness TTL (seconds), defaulting to
/// [`DEFAULT_PRESENCE_TTL_SECS`]. A parse failure or non-positive value falls
/// back to the default with a WARN so a typo can't silently disable sweeping.
/// (Same env knob + semantics as the worker_coverage tracker this absorbs.)
fn presence_ttl() -> Duration {
    match std::env::var("MEKHAN__WORKERS__PRESENCE_TTL_SECS") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(n) if n > 0 => Duration::from_secs(n),
            _ => {
                tracing::warn!(
                    raw = %raw,
                    "MEKHAN__WORKERS__PRESENCE_TTL_SECS is not a positive integer; \
                     using default {DEFAULT_PRESENCE_TTL_SECS}s"
                );
                Duration::from_secs(DEFAULT_PRESENCE_TTL_SECS)
            }
        },
        Err(_) => Duration::from_secs(DEFAULT_PRESENCE_TTL_SECS),
    }
}

/// Which kind of capacity a [`LivenessEntry`] tracks. Identity-disambiguates the
/// map key (a worker id and a runner id can't collide) and lets a fleet view
/// label each row. The two kinds differ ONLY in who owns their lifecycle — both
/// feed the SAME advisory eligibility query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapacityKind {
    /// Anonymous competing-consumer executor worker (this module's subscriber +
    /// TTL sweep own its lifecycle).
    Worker,
    /// Enrolled instrument runner (lifecycle owned by
    /// [`crate::runners_presence`]; only its advisory facet is mirrored here).
    Runner,
}

impl CapacityKind {
    /// Stable lowercase tag for logs / a future fleet read endpoint.
    pub fn as_str(&self) -> &'static str {
        match self {
            CapacityKind::Worker => "worker",
            CapacityKind::Runner => "runner",
        }
    }
}

/// One tracked capacity's advisory liveness state.
struct LivenessEntry {
    /// Which kind of capacity this is. Owns lifecycle ownership (see module docs)
    /// and labels the fleet snapshot.
    kind: CapacityKind,
    /// The capabilities this capacity advertises. Today this is the set of
    /// `ExecutorJob` backend wire names it can serve (`["python", "loki", …]`) —
    /// for a worker, its compiled-in backends; for a runner, its self-reported
    /// presence `backends`. Advisory wire-truth only.
    caps: Vec<String>,
    /// Most recent presence heartbeat instant; drives the (worker-only) TTL sweep.
    last_seen: Instant,
}

/// In-memory registry: `(kind, id)` → tracked state. Guarded by a single
/// `Mutex` shared between the worker subscriber/sweep tasks and the runner
/// controller's mirror writes. Critical sections are tiny (a HashMap probe + a
/// small clone), so a plain `Mutex` is correct and contention-free in practice.
type LivenessMap = Arc<Mutex<HashMap<(CapacityKind, String), LivenessEntry>>>;

/// Wire shape of a `worker.*.presence` payload. `worker_id` + `backends` are
/// advisory (see module docs); unknown fields are ignored so the worker can
/// extend the heartbeat without breaking mekhan.
#[derive(Debug, Deserialize)]
struct WorkerPresencePayload {
    worker_id: String,
    #[serde(default)]
    backends: Vec<String>,
}

/// One row of a fleet-liveness snapshot, surfaced to read endpoints. The worker
/// coverage read filters to [`CapacityKind::Worker`]; a future unified fleet
/// view can render both kinds.
pub struct FleetSnapshotEntry {
    /// Which kind of capacity (worker / runner).
    pub kind: CapacityKind,
    /// The capacity's id (worker daemon name, or runner UUID as a string).
    pub id: String,
    /// Advertised backend wire-names (`["python", …]`).
    pub caps: Vec<String>,
    /// Milliseconds since this capacity's last presence heartbeat.
    pub last_seen_ms_ago: u64,
}

/// Public newtype around the [`LivenessMap`] so the `pub` [`crate::AppState`]
/// can hold a handle WITHOUT leaking the private [`LivenessEntry`]/[`LivenessMap`]
/// types (which would trip the `private_interfaces` lint under `-D warnings`).
///
/// The publish-time warning reads through [`Self::serves_backend`]; the worker
/// tasks share the SAME inner map via [`Self::map`]; the runner controller
/// mirrors its advisory facet via [`Self::upsert_runner`] / [`Self::drop_runner`].
#[derive(Clone, Default)]
pub struct FleetLiveness(LivenessMap);

impl FleetLiveness {
    /// Construct a fresh, empty fleet-liveness handle. The worker tasks, the
    /// runner mirror, and the publish reads all share this one map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow the inner shared map for the worker tasks (subscriber + sweep).
    fn map(&self) -> &LivenessMap {
        &self.0
    }

    /// Union of every live capacity's advertised backend wire-names, across BOTH
    /// kinds. The (worker-only) sweep keeps worker entries fresh; runner entries
    /// are kept fresh by the runner controller's own present/expire edges.
    pub async fn covered_backends(&self) -> HashSet<String> {
        let map = self.0.lock().await;
        map.values()
            .flat_map(|e| e.caps.iter().cloned())
            .collect()
    }

    /// The single `satisfies`-shaped eligibility query (docs/23 §4): whether ANY
    /// live capacity — worker OR runner — advertises the given backend wire-name.
    /// This collapses the old split `BackendCoverage::is_covered` (workers) +
    /// `RunnerPresence::pool_covers` (runners) into ONE membership check.
    /// Advisory only — a `false` never blocks a publish, it just logs a
    /// queue-risk warning.
    pub async fn serves_backend(&self, wire: &str) -> bool {
        let map = self.0.lock().await;
        map.values().any(|e| e.caps.iter().any(|b| b == wire))
    }

    /// Snapshot the live registry for a read endpoint. Returns one
    /// [`FleetSnapshotEntry`] per tracked capacity, with each `last_seen`
    /// rendered as a relative age (an `Instant` has no serializable form).
    pub async fn snapshot(&self) -> Vec<FleetSnapshotEntry> {
        let now = Instant::now();
        let map = self.0.lock().await;
        map.iter()
            .map(|((kind, id), e)| FleetSnapshotEntry {
                kind: *kind,
                id: id.clone(),
                caps: e.caps.clone(),
                last_seen_ms_ago: now.duration_since(e.last_seen).as_millis() as u64,
            })
            .collect()
    }

    /// Mirror a runner's advisory facet into the registry (called from
    /// [`crate::runners_presence`] on every present heartbeat). `id` is the
    /// runner UUID as a string; `backends` is its self-reported presence set.
    /// Upserts under the `Runner` key, refreshing `last_seen`. This is telemetry
    /// only — it has NO bearing on the runner's pool-net control binding.
    pub async fn upsert_runner(&self, id: String, backends: Vec<String>) {
        let mut map = self.0.lock().await;
        map.insert(
            (CapacityKind::Runner, id),
            LivenessEntry {
                kind: CapacityKind::Runner,
                caps: backends,
                last_seen: Instant::now(),
            },
        );
    }

    /// Drop a runner's advisory mirror (called from [`crate::runners_presence`]
    /// when its OWN sweep marks the runner absent). The runner controller owns
    /// runner lifecycle, so this module's TTL sweep never reaps `Runner` entries
    /// — they leave only through this call. A no-op if the runner was never
    /// mirrored.
    pub async fn drop_runner(&self, id: &str) {
        let mut map = self.0.lock().await;
        map.remove(&(CapacityKind::Runner, id.to_string()));
    }
}

/// Handle one `worker.*.presence` message: parse the advisory payload and upsert
/// the worker's advertised backends + last_seen under the `Worker` key.
async fn handle_presence(liveness: &LivenessMap, payload: &[u8]) {
    let parsed: WorkerPresencePayload = match serde_json::from_slice(payload) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("ignoring malformed worker presence payload: {e}");
            return;
        }
    };
    let mut map = liveness.lock().await;
    map.insert(
        (CapacityKind::Worker, parsed.worker_id),
        LivenessEntry {
            kind: CapacityKind::Worker,
            caps: parsed.backends,
            last_seen: Instant::now(),
        },
    );
}

/// Start the worker subscriber: a core-NATS subscription to `worker.*.presence`.
///
/// Presence pings are ephemeral liveness (not a durable command stream), so this
/// uses a plain core subscription rather than a JetStream durable — a missed
/// ping is harmless (the next one re-registers; the sweep handles a true
/// absence).
async fn start_worker_subscriber(nats: MekhanNats, liveness: LivenessMap) {
    let mut sub = match nats.client().subscribe("worker.*.presence").await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to subscribe to worker.*.presence: {e}");
            return;
        }
    };
    tracing::info!("fleet liveness: worker subscriber started on worker.*.presence");

    while let Some(msg) = sub.next().await {
        handle_presence(&liveness, &msg.payload).await;
    }

    tracing::warn!("fleet liveness: worker subscriber stream ended");
}

/// Start the worker sweep loop: every [`SWEEP_INTERVAL_SECS`] drop WORKER entries
/// whose `last_seen` is older than the TTL, so the registry reflects only live
/// workers. Runner entries are skipped — the runner controller owns their
/// lifecycle (see module docs).
async fn start_worker_sweep(liveness: LivenessMap) {
    let ttl = presence_ttl();
    let mut tick = tokio::time::interval(Duration::from_secs(SWEEP_INTERVAL_SECS));
    tracing::info!(
        ttl_secs = ttl.as_secs(),
        sweep_secs = SWEEP_INTERVAL_SECS,
        "fleet liveness: worker sweep started"
    );

    loop {
        tick.tick().await;
        let now = Instant::now();
        let mut map = liveness.lock().await;
        map.retain(|(kind, id), entry| {
            // The sweep is worker-only — runner entries are reaped by the runner
            // controller's drop_runner, never here.
            if *kind != CapacityKind::Worker {
                return true;
            }
            let alive = now.duration_since(entry.last_seen) <= ttl;
            if !alive {
                tracing::debug!(worker_id = %id, "fleet liveness: worker TTL miss; dropping");
            }
            alive
        });
    }
}

/// Spawn the worker liveness tasks (subscriber + sweep) sharing the registry's
/// map. Called from `main.rs`. Wire-only (no PgPool/PetriClient): worker
/// liveness is derived entirely from the NATS presence stream. `liveness` is the
/// SHARED handle also stored in [`crate::AppState`] so the publish-time warning —
/// and the runner controller's mirror writes — observe the very map these tasks
/// mutate. The RUNNER facet is fed separately by
/// [`crate::runners_presence::spawn_presence_controller`].
pub fn spawn_worker_liveness(liveness: FleetLiveness, nats: MekhanNats) {
    tokio::spawn(start_worker_subscriber(nats, liveness.map().clone()));
    tokio::spawn(start_worker_sweep(liveness.map().clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn covered_after_presence_then_dropped_on_sweep() {
        let liveness = FleetLiveness::new();
        let payload = br#"{"worker_id":"w1","backends":["python","loki"]}"#;
        handle_presence(liveness.map(), payload).await;

        assert!(liveness.serves_backend("python").await);
        assert!(liveness.serves_backend("loki").await);
        assert!(!liveness.serves_backend("prometheus").await);
        assert_eq!(
            liveness.covered_backends().await,
            HashSet::from(["python".to_string(), "loki".to_string()])
        );

        // Simulate a TTL miss by aging the entry past any positive TTL, then
        // retaining only fresh entries (the sweep body, worker-only).
        {
            let mut map = liveness.map().lock().await;
            let e = map.get_mut(&(CapacityKind::Worker, "w1".to_string())).unwrap();
            e.last_seen = Instant::now() - Duration::from_secs(3600);
        }
        {
            let now = Instant::now();
            let ttl = Duration::from_secs(30);
            let mut map = liveness.map().lock().await;
            map.retain(|(kind, _), entry| {
                *kind != CapacityKind::Worker || now.duration_since(entry.last_seen) <= ttl
            });
        }
        assert!(!liveness.serves_backend("python").await);
        assert!(liveness.covered_backends().await.is_empty());
    }

    #[tokio::test]
    async fn malformed_payload_is_ignored() {
        let liveness = FleetLiveness::new();
        handle_presence(liveness.map(), b"not json").await;
        assert!(liveness.covered_backends().await.is_empty());
    }

    #[tokio::test]
    async fn latest_presence_replaces_advertised_backends() {
        let liveness = FleetLiveness::new();
        handle_presence(liveness.map(), br#"{"worker_id":"w1","backends":["python"]}"#).await;
        handle_presence(liveness.map(), br#"{"worker_id":"w1","backends":["loki"]}"#).await;
        assert!(!liveness.serves_backend("python").await);
        assert!(liveness.serves_backend("loki").await);
    }

    #[tokio::test]
    async fn worker_and_runner_both_satisfy_eligibility() {
        // The unified query: a backend served by EITHER kind counts as covered,
        // and the worker-only sweep never touches a runner mirror.
        let liveness = FleetLiveness::new();
        handle_presence(liveness.map(), br#"{"worker_id":"w1","backends":["python"]}"#).await;
        liveness
            .upsert_runner("r1".to_string(), vec!["xrd".to_string()])
            .await;

        assert!(liveness.serves_backend("python").await); // worker
        assert!(liveness.serves_backend("xrd").await); // runner
        assert_eq!(
            liveness.covered_backends().await,
            HashSet::from(["python".to_string(), "xrd".to_string()])
        );

        // Age the worker out and run the (worker-only) sweep body: the worker
        // drops, the runner mirror survives.
        {
            let mut map = liveness.map().lock().await;
            map.get_mut(&(CapacityKind::Worker, "w1".to_string()))
                .unwrap()
                .last_seen = Instant::now() - Duration::from_secs(3600);
            let now = Instant::now();
            let ttl = Duration::from_secs(30);
            map.retain(|(kind, _), entry| {
                *kind != CapacityKind::Worker || now.duration_since(entry.last_seen) <= ttl
            });
        }
        assert!(!liveness.serves_backend("python").await, "worker swept");
        assert!(liveness.serves_backend("xrd").await, "runner mirror survives sweep");

        // The runner leaves only via drop_runner (controller-owned lifecycle).
        liveness.drop_runner("r1").await;
        assert!(!liveness.serves_backend("xrd").await);
        assert!(liveness.snapshot().await.is_empty());
    }
}
