//! Runner presence heartbeat (Phase 3 — presence-lease pool capacity).
//!
//! A registered lab-runner advertises liveness by publishing a minimal payload
//! to `runner.{runner_id}.presence` on a fixed interval. mekhan watches that
//! subject and keeps the runner's presence-pool unit alive (injecting a
//! `presence_acquire` into the pool net on first sight, and a
//! `presence_expired` signal when heartbeats stop). Capabilities / pool /
//! executor-namespace are looked up by mekhan from the runner's DB row — they
//! are **not** trusted from the wire — so the payload here is deliberately
//! minimal (just the runner id, for log/debug correlation on the broker side).
//!
//! The task reuses the daemon's existing NATS connection (which already carries
//! the runner's Phase-2 scoped creds); it never opens a second connection. It
//! is best-effort: a publish error is logged at `warn` and the loop continues —
//! a transient broker hiccup must not crash the daemon. It shuts down when the
//! supplied `CancellationToken` fires (same token the cancel/chunk listeners
//! use), so it stops cleanly on Ctrl+C.

use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

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
/// — does **not** open a second connection. Publishes a minimal
/// `{"runner_id": "<id>"}` payload to [`presence_subject`] every `interval`,
/// starting immediately, until `shutdown` is cancelled. Publish failures are
/// logged at `warn` and do not abort the loop or the daemon.
pub fn spawn_presence_task(
    client: async_nats::Client,
    runner_id: String,
    interval: Duration,
    shutdown: CancellationToken,
) {
    let subject = presence_subject(&runner_id);
    // Minimal payload — caps/pool/namespace are authoritative on mekhan's side.
    let payload: Vec<u8> = serde_json::to_vec(&serde_json::json!({ "runner_id": runner_id }))
        .unwrap_or_else(|_| b"{}".to_vec());

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
                    match client.publish(subject.clone(), payload.clone().into()).await {
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
/// publishes a `{"worker_id": "<id>", "backends": ["python", ...]}` payload to
/// [`worker_presence_subject`] advertising the backend set this worker drains.
/// Unlike the runner path, the backend set is the wire-truth here — the worker
/// pool routes by namespace partition, not by a DB-side capability lookup — so
/// the payload carries it for operator visibility. Publish failures are logged
/// at `warn` and never abort the loop or the daemon.
pub fn spawn_worker_presence_task(
    client: async_nats::Client,
    worker_id: String,
    backends: Vec<String>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    let subject = worker_presence_subject(&worker_id);
    let payload: Vec<u8> = serde_json::to_vec(&serde_json::json!({
        "worker_id": worker_id,
        "backends": backends,
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

    #[test]
    fn worker_presence_subject_matches_contract() {
        assert_eq!(
            worker_presence_subject("exec-worker-1"),
            "worker.exec-worker-1.presence"
        );
    }
}
