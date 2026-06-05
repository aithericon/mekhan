//! Runner presence heartbeat (Phase 3 — presence-lease pool capacity).
//!
//! A registered lab-runner advertises liveness by publishing a payload to
//! `runner.{runner_id}.presence` on a fixed interval. mekhan watches that
//! subject and keeps the runner's presence-pool unit alive (injecting a
//! `presence_acquire` into the pool net on first sight, and a
//! `presence_expired` signal when heartbeats stop). Capabilities / pool /
//! executor-namespace are looked up by mekhan from the runner's DB row — they
//! are **not** trusted from the wire.
//!
//! The payload also carries the runner's `backends` — the executor backend
//! wire-names this daemon actually registered (e.g. `["python", "docker"]`),
//! the SAME set a worker-pool daemon advertises on `worker.{id}.presence`. This
//! is the runner's `backends` dimension (set-membership, docs/23 §4), ORTHOGONAL
//! to its typed `capabilities` (predicate-matched at the pool's `t_grant`).
//! Unlike caps, the backend set is self-reported wire-truth: a runner
//! over-claiming a backend only fails its OWN granted jobs (visible self-harm) —
//! it can't escalate to unauthorized work — so trusting the daemon's self-report
//! is safe and avoids a DB round-trip. mekhan uses it purely for fleet
//! visibility + a best-effort publish-time coverage warning, never to hard-gate
//! placement.
//!
//! The task reuses the daemon's existing NATS connection (which already carries
//! the runner's Phase-2 scoped creds); it never opens a second connection. It
//! is best-effort: a publish error is logged at `warn` and the loop continues —
//! a transient broker hiccup must not crash the daemon. It shuts down when the
//! supplied `CancellationToken` fires (same token the cancel/chunk listeners
//! use), so it stops cleanly on Ctrl+C.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Live, mutable model state a runner presence-reports (P2 — model-pool).
///
/// The model-pool node agent writes this on every load/unload command; the
/// presence task re-reads it each heartbeat tick so `{concurrency, models}`
/// reflect the current vLLM state without a re-enroll. A cheap-clone shared
/// handle (`Arc<Mutex<…>>`) — contention is trivial (a write per load/unload, a
/// read per heartbeat interval).
///
/// `concurrency` is the per-engine budget C (`=--max-num-seqs`); `models` is the
/// served model ids (base + loaded LoRA adapters). Both are advisory wire-truth
/// — mekhan keeps caps/namespace DB-authoritative.
#[derive(Clone, Default)]
pub struct LiveModelState {
    inner: Arc<Mutex<ModelStateInner>>,
}

#[derive(Default)]
struct ModelStateInner {
    concurrency: Option<u32>,
    models: Vec<String>,
}

impl LiveModelState {
    /// A new handle with no concurrency reported and an empty model set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the reported state (called by the node agent after a probe /
    /// load / unload). The next heartbeat re-serializes from here.
    pub fn set(&self, concurrency: Option<u32>, models: Vec<String>) {
        let mut guard = self.inner.lock().unwrap();
        guard.concurrency = concurrency;
        guard.models = models;
    }

    /// Snapshot the current `(concurrency, models)` for one heartbeat.
    pub fn snapshot(&self) -> (Option<u32>, Vec<String>) {
        let guard = self.inner.lock().unwrap();
        (guard.concurrency, guard.models.clone())
    }
}

/// The NATS subject a runner publishes its presence heartbeat to.
///
/// Matches the runner's Phase-2 JWT publish-allow (`runner.{id}.>`) and the
/// subject mekhan subscribes to for presence-pool liveness.
pub fn presence_subject(runner_id: &str) -> String {
    format!("runner.{runner_id}.presence")
}

/// Spawn the background presence heartbeat task.
///
/// Reuses `client` (the daemon's already-connected, runner-scoped NATS client)
/// — does **not** open a second connection. Publishes a
/// `{"runner_id": "<id>", "backends": ["python", ...], "concurrency": C,
/// "models": [...]}` payload to [`presence_subject`] every `interval`, starting
/// immediately, until `shutdown` is cancelled.
///
/// `backends` is the daemon's registered `ExecutorJob` wire-names (self-reported
/// wire-truth; see module docs) — caps/pool/namespace remain
/// mekhan-authoritative. `models` carries the optional live model-pool state
/// (P2): when `Some`, the per-engine concurrency C + served model ids are
/// re-read from the shared [`LiveModelState`] on EACH tick (so a load/unload
/// between heartbeats is reflected without a re-enroll) and added to the
/// payload; when `None` the payload omits both fields and the legacy
/// `{runner_id, backends}` shape is published. Both are advisory — mekhan reads
/// caps/namespace from the DB and ignores unknown fields, so this is additive.
/// Publish failures are logged at `warn` and do not abort the loop or the
/// daemon.
pub fn spawn_presence_task(
    client: async_nats::Client,
    runner_id: String,
    backends: Vec<String>,
    models: Option<LiveModelState>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    let subject = presence_subject(&runner_id);

    tokio::spawn(async move {
        debug!(
            %subject,
            interval_secs = interval.as_secs(),
            "runner presence heartbeat task started"
        );
        let mut ticker = tokio::time::interval(interval);
        // Default MissedTickBehavior::Burst would replay missed ticks after a
        // slow publish; Delay keeps a steady cadence instead.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    debug!(%subject, "runner presence heartbeat task stopping");
                    break;
                }
                _ = ticker.tick() => {
                    // Re-serialize INSIDE the tick so live model state (mutated
                    // by the node agent's load/unload between heartbeats) is
                    // picked up each cycle. caps/pool/namespace stay
                    // mekhan-authoritative; backends/concurrency/models are the
                    // runner's advisory self-report (see module docs).
                    let mut body = serde_json::json!({
                        "runner_id": runner_id,
                        "backends": backends,
                    });
                    if let Some(state) = &models {
                        let (concurrency, model_ids) = state.snapshot();
                        body["concurrency"] = serde_json::json!(concurrency);
                        body["models"] = serde_json::json!(model_ids);
                    }
                    let payload: Vec<u8> =
                        serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
                    match client.publish(subject.clone(), payload.into()).await {
                        Ok(()) => debug!(%subject, "published runner presence"),
                        Err(e) => warn!(
                            %subject,
                            error = %e,
                            "failed to publish runner presence; will retry next interval"
                        ),
                    }
                }
            }
        }
    });
}

/// The NATS subject a non-enrolled worker-pool daemon publishes its presence
/// heartbeat to.
///
/// Distinct from [`presence_subject`] (the `runner.{id}.presence` exclusive-
/// instrument path): worker-pool daemons have no runner identity and compete
/// as anonymous consumers, so they advertise on `worker.{worker_id}.presence`
/// where `worker_id` is a process-stable label (its config `name`), used purely
/// for operator log/debug visibility into which backend set each worker covers.
pub fn worker_presence_subject(worker_id: &str) -> String {
    format!("worker.{worker_id}.presence")
}

/// Spawn the background worker-presence heartbeat task for a worker-pool daemon.
///
/// Mirrors [`spawn_presence_task`] (same best-effort ticker, same reuse of the
/// daemon's existing NATS client, same `CancellationToken` shutdown) but
/// publishes a `{"worker_id": "<id>", "backends": ["python", ...], "group": ...}`
/// payload to [`worker_presence_subject`] advertising the backend set this
/// worker drains. Unlike the runner path, the backend set is the wire-truth
/// here — the worker pool routes by namespace partition, not by a DB-side
/// capability lookup — so the payload carries it for operator visibility.
///
/// `worker_id` is the presence-key mekhan dedupes on: for an enrolled worker
/// pass its `wkr_` control-plane UUID so mekhan correlates the heartbeat to the
/// DB row; for an anonymous worker pass the process-stable `config.name`.
/// `group` is the worker's routing group when enrolled (`None` for anonymous);
/// mekhan ignores unknown/extra fields, so carrying it is non-breaking.
///
/// Publish failures are logged at `warn` and never abort the loop or the daemon.
pub fn spawn_worker_presence_task(
    client: async_nats::Client,
    worker_id: String,
    backends: Vec<String>,
    group: Option<String>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    let subject = worker_presence_subject(&worker_id);
    let payload: Vec<u8> = serde_json::to_vec(&serde_json::json!({
        "worker_id": worker_id,
        "backends": backends,
        "group": group,
    }))
    .unwrap_or_else(|_| b"{}".to_vec());

    tokio::spawn(async move {
        debug!(
            %subject,
            interval_secs = interval.as_secs(),
            "worker presence heartbeat task started"
        );
        let mut ticker = tokio::time::interval(interval);
        // Steady cadence (see runner task): Delay over the default Burst so a
        // slow publish doesn't replay missed ticks back-to-back.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    debug!(%subject, "worker presence heartbeat task stopping");
                    break;
                }
                _ = ticker.tick() => {
                    match client.publish(subject.clone(), payload.clone().into()).await {
                        Ok(()) => debug!(%subject, "published worker presence"),
                        Err(e) => warn!(
                            %subject,
                            error = %e,
                            "failed to publish worker presence; will retry next interval"
                        ),
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_subject_matches_contract() {
        assert_eq!(presence_subject("rnr-abc"), "runner.rnr-abc.presence");
    }

    /// The runner presence payload carries the additive model-pool fields
    /// `{concurrency, models}` (P2) AND still parses as the legacy
    /// `{runner_id, backends}` shape — mekhan ignores the extra fields. This is
    /// the same forward-compat property `worker_presence_payload_carries_group`
    /// asserts for the worker path. We assert against the body the heartbeat
    /// serializes per tick (re-built from a `LiveModelState` snapshot).
    #[test]
    fn runner_presence_payload_carries_concurrency_and_models() {
        let state = LiveModelState::new();
        state.set(Some(256), vec!["base-x".to_string(), "lora-a".to_string()]);
        let (concurrency, model_ids) = state.snapshot();

        let mut body = serde_json::json!({
            "runner_id": "rnr-abc",
            "backends": ["python", "vllm"],
        });
        body["concurrency"] = serde_json::json!(concurrency);
        body["models"] = serde_json::json!(model_ids);

        // Additive fields present and correct.
        assert_eq!(body["runner_id"], "rnr-abc");
        assert_eq!(body["backends"][0], "python");
        assert_eq!(body["concurrency"], 256);
        assert_eq!(body["models"], serde_json::json!(["base-x", "lora-a"]));

        // Still parses as the legacy {runner_id, backends} shape (extra fields
        // ignored), proving the change is forward-compatible.
        #[derive(serde::Deserialize)]
        struct Legacy {
            runner_id: String,
            backends: Vec<String>,
        }
        let legacy: Legacy = serde_json::from_value(body).unwrap();
        assert_eq!(legacy.runner_id, "rnr-abc");
        assert_eq!(legacy.backends, vec!["python", "vllm"]);
    }

    /// With no live model state the payload omits `{concurrency, models}` —
    /// exactly the legacy shape, so non-model runners are unaffected.
    #[test]
    fn runner_presence_payload_omits_model_fields_when_absent() {
        let models: Option<LiveModelState> = None;
        let mut body = serde_json::json!({
            "runner_id": "rnr-abc",
            "backends": ["python"],
        });
        if let Some(state) = &models {
            let (c, m) = state.snapshot();
            body["concurrency"] = serde_json::json!(c);
            body["models"] = serde_json::json!(m);
        }
        assert!(body.get("concurrency").is_none());
        assert!(body.get("models").is_none());
    }

    /// A live load/unload between heartbeats mutates the shared handle and the
    /// next snapshot reflects it — the seam that lets presence track vLLM state
    /// without a re-enroll.
    #[test]
    fn live_model_state_reflects_latest_write() {
        let state = LiveModelState::new();
        state.set(Some(128), vec!["base".to_string()]);
        assert_eq!(state.snapshot(), (Some(128), vec!["base".to_string()]));
        // load adds an adapter:
        state.set(Some(128), vec!["base".to_string(), "lora".to_string()]);
        assert_eq!(
            state.snapshot(),
            (Some(128), vec!["base".to_string(), "lora".to_string()])
        );
    }

    #[test]
    fn worker_presence_subject_matches_contract() {
        assert_eq!(
            worker_presence_subject("exec-worker-1"),
            "worker.exec-worker-1.presence"
        );
    }

    /// The presence payload must carry `worker_id`, `backends`, and `group` so
    /// mekhan's `WorkerPresencePayload` (worker_id + backends; group ignored as
    /// an extra) parses it and the fleet view can render the routing group.
    #[test]
    fn worker_presence_payload_carries_group_when_enrolled() {
        let payload = serde_json::json!({
            "worker_id": "wkr-abc",
            "backends": ["python", "loki"],
            "group": Some("xrd_bench".to_string()),
        });
        assert_eq!(payload["worker_id"], "wkr-abc");
        assert_eq!(payload["backends"][0], "python");
        assert_eq!(payload["group"], "xrd_bench");
    }

    /// An anonymous worker serializes `group: null` — mekhan ignores the extra
    /// field, so the back-compat `{worker_id, backends}` shape still parses.
    #[test]
    fn worker_presence_payload_group_null_when_anonymous() {
        let group: Option<String> = None;
        let payload = serde_json::json!({
            "worker_id": "executor-host",
            "backends": ["process"],
            "group": group,
        });
        assert!(payload["group"].is_null());
    }
}
