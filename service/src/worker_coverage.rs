//! Worker backend-coverage tracker (worker-pool feature).
//!
//! The worker pool is a set of competing-consumer executor workers, each
//! advertising which `ExecutorJob` backends it can serve. mekhan tracks this as
//! advisory, TTL-swept presence so that at PUBLISH time it can WARN (never
//! hard-fail) when an `AutomatedStep`'s backend is covered by zero live workers
//! — turning the old silent "job rots, stuck at submitted" into a visible
//! diagnostic.
//!
//! This is the COVERAGE-SET path: backends are partitioned by namespace
//! (`executor.<wire>`), not matched through the engine `satisfies` guard or the
//! typed-capability registry (those drive the separate presence-pool /
//! exclusive-instrument path). Coverage is purely advisory wire state.
//!
//! ## Wire contract
//!
//! Workers publish to `worker.*.presence`. Unlike runners — whose identity is
//! authenticated and parsed from the SUBJECT — workers are unauthenticated and
//! ephemeral, so the `worker_id` and the advertised `backends: Vec<String>`
//! (snake-case wire names, e.g. `"python"`, `"loki"`) both come from the
//! PAYLOAD. This is safe because coverage is advisory only: a lying worker at
//! worst suppresses a warning that would have been logged.
//!
//! ## TTL sweep
//!
//! An in-memory map keys each `worker_id` to its advertised backends + last-seen
//! [`Instant`]. A background loop sweeps entries older than the TTL
//! (`MEKHAN__WORKERS__PRESENCE_TTL_SECS`, default 30s; sweep every 5s),
//! mirroring [`crate::runners_presence`]. A missed heartbeat is harmless — the
//! next ping re-registers, and a true departure is reaped within ~one sweep.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::nats::MekhanNats;

/// Default coverage TTL: a worker missing this long is dropped from the coverage
/// set. Overridable via `MEKHAN__WORKERS__PRESENCE_TTL_SECS`.
const DEFAULT_PRESENCE_TTL_SECS: u64 = 30;

/// How often the sweep loop wakes to look for TTL misses. Kept well below the
/// TTL so coverage drops within ~one sweep interval past expiry.
const SWEEP_INTERVAL_SECS: u64 = 5;

/// Read the configured coverage TTL (seconds), defaulting to
/// [`DEFAULT_PRESENCE_TTL_SECS`]. A parse failure or non-positive value falls
/// back to the default with a WARN so a typo can't silently disable sweeping.
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

/// One tracked worker's coverage state.
struct WorkerEntry {
    /// The `ExecutorJob` backend wire names this worker advertises serving.
    backends: Vec<String>,
    /// Most recent presence heartbeat instant; drives the TTL sweep.
    last_seen: Instant,
}

/// In-memory coverage map: `worker_id` → its tracked state. Guarded by a single
/// `Mutex` shared between the subscriber task and the sweep task. Critical
/// sections are tiny (a HashMap probe + a small clone), so a plain `Mutex` is
/// correct and contention-free in practice.
type CoverageMap = Arc<Mutex<HashMap<String, WorkerEntry>>>;

/// Wire shape of a `worker.*.presence` payload. `worker_id` + `backends` are
/// advisory (see module docs); unknown fields are ignored so the worker can
/// extend the heartbeat without breaking mekhan.
#[derive(Debug, Deserialize)]
struct WorkerPresencePayload {
    worker_id: String,
    #[serde(default)]
    backends: Vec<String>,
}

/// Public newtype around the [`CoverageMap`] so the `pub` [`crate::AppState`]
/// can hold a handle WITHOUT leaking the private [`WorkerEntry`]/[`CoverageMap`]
/// types (which would trip the `private_interfaces` lint under `-D warnings`).
///
/// The publish-time warning reads through [`Self::is_covered`]; the coverage
/// tasks share the SAME inner map via [`Self::map`].
#[derive(Clone, Default)]
pub struct BackendCoverage(CoverageMap);

impl BackendCoverage {
    /// Construct a fresh, empty coverage handle. The coverage tasks + the
    /// publish reads share this one map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow the inner shared map for the coverage tasks (subscriber + sweep).
    fn map(&self) -> &CoverageMap {
        &self.0
    }

    /// Union of every live worker's advertised backend wire names. TTL-expired
    /// workers are not necessarily reaped yet, but [`is_covered`] and the publish
    /// warning only run against this snapshot, which the sweep keeps fresh.
    pub async fn covered_backends(&self) -> HashSet<String> {
        let map = self.0.lock().await;
        map.values()
            .flat_map(|e| e.backends.iter().cloned())
            .collect()
    }

    /// Whether ANY live worker advertises the given backend wire name.
    pub async fn is_covered(&self, wire: &str) -> bool {
        let map = self.0.lock().await;
        map.values().any(|e| e.backends.iter().any(|b| b == wire))
    }

    /// Snapshot per-worker coverage for a future read endpoint (no route in v1).
    /// Returns `(worker_id, backends, last_seen_ms_ago)` triples.
    pub async fn snapshot(&self) -> Vec<(String, Vec<String>, u64)> {
        let now = Instant::now();
        let map = self.0.lock().await;
        map.iter()
            .map(|(id, e)| {
                (
                    id.clone(),
                    e.backends.clone(),
                    now.duration_since(e.last_seen).as_millis() as u64,
                )
            })
            .collect()
    }
}

/// Handle one `worker.*.presence` message: parse the advisory payload and upsert
/// the worker's advertised backends + last_seen.
async fn handle_presence(coverage: &CoverageMap, payload: &[u8]) {
    let parsed: WorkerPresencePayload = match serde_json::from_slice(payload) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("ignoring malformed worker presence payload: {e}");
            return;
        }
    };
    let mut map = coverage.lock().await;
    map.insert(
        parsed.worker_id,
        WorkerEntry {
            backends: parsed.backends,
            last_seen: Instant::now(),
        },
    );
}

/// Start the coverage subscriber: a core-NATS subscription to `worker.*.presence`.
///
/// Coverage pings are ephemeral liveness (not a durable command stream), so this
/// uses a plain core subscription rather than a JetStream durable — a missed
/// ping is harmless (the next one re-registers; the sweep handles a true
/// absence).
async fn start_coverage_subscriber(nats: MekhanNats, coverage: CoverageMap) {
    let mut sub = match nats.client().subscribe("worker.*.presence").await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to subscribe to worker.*.presence: {e}");
            return;
        }
    };
    tracing::info!("worker coverage subscriber started on worker.*.presence");

    while let Some(msg) = sub.next().await {
        handle_presence(&coverage, &msg.payload).await;
    }

    tracing::warn!("worker coverage subscriber stream ended");
}

/// Start the coverage sweep loop: every [`SWEEP_INTERVAL_SECS`] drop entries
/// whose `last_seen` is older than the TTL, so the coverage set reflects only
/// live workers.
async fn start_coverage_sweep(coverage: CoverageMap) {
    let ttl = presence_ttl();
    let mut tick = tokio::time::interval(Duration::from_secs(SWEEP_INTERVAL_SECS));
    tracing::info!(
        ttl_secs = ttl.as_secs(),
        sweep_secs = SWEEP_INTERVAL_SECS,
        "worker coverage sweep started"
    );

    loop {
        tick.tick().await;
        let now = Instant::now();
        let mut map = coverage.lock().await;
        map.retain(|worker_id, entry| {
            let alive = now.duration_since(entry.last_seen) <= ttl;
            if !alive {
                tracing::debug!(%worker_id, "worker coverage TTL miss; dropping from coverage set");
            }
            alive
        });
    }
}

/// Spawn BOTH coverage tasks (subscriber + sweep) sharing one coverage map.
/// Called from `main.rs`. Wire-only (no PgPool/PetriClient needed): coverage is
/// derived entirely from the NATS presence stream. `coverage` is the SHARED
/// handle also stored in [`crate::AppState`] so the publish-time warning
/// observes the very map the tasks mutate.
pub fn spawn_worker_coverage(coverage: BackendCoverage, nats: MekhanNats) {
    tokio::spawn(start_coverage_subscriber(nats, coverage.map().clone()));
    tokio::spawn(start_coverage_sweep(coverage.map().clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn covered_after_presence_then_dropped_on_sweep() {
        let coverage = BackendCoverage::new();
        let payload = br#"{"worker_id":"w1","backends":["python","loki"]}"#;
        handle_presence(coverage.map(), payload).await;

        assert!(coverage.is_covered("python").await);
        assert!(coverage.is_covered("loki").await);
        assert!(!coverage.is_covered("prometheus").await);
        assert_eq!(
            coverage.covered_backends().await,
            HashSet::from(["python".to_string(), "loki".to_string()])
        );

        // Simulate a TTL miss by aging the entry past any positive TTL, then
        // retaining only fresh entries (the sweep body).
        {
            let mut map = coverage.map().lock().await;
            let e = map.get_mut("w1").unwrap();
            e.last_seen = Instant::now() - Duration::from_secs(3600);
        }
        {
            let now = Instant::now();
            let ttl = Duration::from_secs(30);
            let mut map = coverage.map().lock().await;
            map.retain(|_, entry| now.duration_since(entry.last_seen) <= ttl);
        }
        assert!(!coverage.is_covered("python").await);
        assert!(coverage.covered_backends().await.is_empty());
    }

    #[tokio::test]
    async fn malformed_payload_is_ignored() {
        let coverage = BackendCoverage::new();
        handle_presence(coverage.map(), b"not json").await;
        assert!(coverage.covered_backends().await.is_empty());
    }

    #[tokio::test]
    async fn latest_presence_replaces_advertised_backends() {
        let coverage = BackendCoverage::new();
        handle_presence(coverage.map(), br#"{"worker_id":"w1","backends":["python"]}"#).await;
        handle_presence(coverage.map(), br#"{"worker_id":"w1","backends":["loki"]}"#).await;
        assert!(!coverage.is_covered("python").await);
        assert!(coverage.is_covered("loki").await);
    }
}
