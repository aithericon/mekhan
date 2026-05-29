//! SlurmWatcher: poll-based observer that publishes job state changes to NATS.
//!
//! Polls `squeue` (active jobs) and `sacct` (completed jobs) at a configurable
//! interval, detects state transitions, and publishes `ExternalSignal` messages
//! to `petri.signal.{net_id}.{place_name}`.
//!
//! Net-agnostic — a single instance handles all nets via comment-based routing.
//!
//! Uses shared infrastructure from `petri-scheduler-bridge`:
//! - [`SignalPublisher`] for NATS signal delivery with JetStream dedup
//! - [`CheckpointStore`] for persisting the poll cursor across restarts
//! - [`RoutingMeta`] for per-status signal routing from job metadata
//! - [`run_with_reconnect`](petri_scheduler_bridge::backoff::run_with_reconnect) for the reconnect loop

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::RwLock;

use petri_domain::ExternalSignal;
use petri_scheduler_bridge::{signal_subject, CheckpointStore, RoutingMeta, SignalPublisher};

use crate::config::SlurmConfig;
use crate::models::{SacctEntry, SqueueEntry};
use crate::ssh::SshSession;
use crate::status_mapping;

/// Errors from the Slurm watcher.
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    /// SSH connection or command error.
    #[error("SSH error: {0}")]
    Ssh(#[from] crate::ssh::SshError),

    /// NATS communication error.
    #[error("NATS error: {0}")]
    Nats(String),
}

/// KV key for the last-polled sacct timestamp.
const CHECKPOINT_KEY: &str = "slurm.poll_cursor";

/// KV key for persisted tracked jobs (survives watcher restarts).
const TRACKED_JOBS_KEY: &str = "slurm.tracked_jobs";

/// Tracked state for a job the watcher is monitoring.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TrackedJob {
    /// Last known Slurm state string.
    last_state: String,
    /// Routing metadata extracted from the job comment.
    routing: RoutingMeta,
    /// Set when `last_state` first reaches a terminal status. Used to gate
    /// purging: keep the entry around long enough that a re-detect from
    /// sacct's rolling lookback window finds the cached terminal state and
    /// skips re-publishing. `None` for jobs that are still active. Defaults
    /// to None for backwards compat with persisted KV state from older
    /// watchers (which didn't have this field).
    #[serde(default)]
    terminal_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Compute `terminal_at = Some(now)` when `state` maps to a terminal status,
/// else `None`. Used at every TrackedJob insert site to mark when a job first
/// reached a terminal Slurm state.
fn terminal_at_for(state: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    status_mapping::map_slurm_state(state)
        .filter(|s| s.is_terminal())
        .map(|_| Utc::now())
}

/// Slurm poll-based watcher.
///
/// Polls `squeue` and `sacct` at a configurable interval, compares against
/// tracked state, and publishes signals to NATS on state transitions.
///
/// All mutable state is behind `RwLock` so `&self` methods work with
/// `run_with_reconnect`'s `FnMut` requirement.
pub struct SlurmWatcher {
    config: SlurmConfig,
    signal_publisher: SignalPublisher,
    checkpoint: CheckpointStore,
    /// Active jobs being tracked: job_id → tracked state.
    tracked: RwLock<HashMap<String, TrackedJob>>,
}

impl SlurmWatcher {
    /// Create a new watcher.
    ///
    /// Initializes the checkpoint KV bucket for restart resilience.
    pub async fn new(
        config: SlurmConfig,
        nats: async_nats::jetstream::Context,
    ) -> Result<Self, WatcherError> {
        let signal_publisher = SignalPublisher::new(nats.clone());
        let checkpoint = CheckpointStore::new(&nats).await;

        Ok(Self {
            config,
            signal_publisher,
            checkpoint,
            tracked: RwLock::new(HashMap::new()),
        })
    }

    /// Load the last checkpoint cursor (ISO timestamp).
    async fn load_checkpoint_cursor(&self) -> Option<String> {
        let value = self.checkpoint.load(CHECKPOINT_KEY).await?;
        tracing::info!(cursor = %value, "Loaded checkpoint cursor from NATS KV");
        Some(value)
    }

    /// Save the current poll cursor.
    async fn save_checkpoint_cursor(&self, cursor: &str) {
        self.checkpoint.save(CHECKPOINT_KEY, cursor).await;
    }

    /// Persist tracked jobs to NATS KV for restart recovery.
    ///
    /// When sacct is unavailable (accounting disabled), the tracked jobs map
    /// is the only way to detect completed jobs. Persisting it allows a
    /// restarted watcher to infer completion for jobs that left squeue
    /// during downtime.
    async fn save_tracked_jobs(&self) {
        let tracked = self.tracked.read().await;
        if tracked.is_empty() {
            // Clean up stale entry
            self.checkpoint.save(TRACKED_JOBS_KEY, "{}").await;
            return;
        }
        match serde_json::to_string(&*tracked) {
            Ok(json) => self.checkpoint.save(TRACKED_JOBS_KEY, &json).await,
            Err(e) => tracing::warn!(error = %e, "Failed to serialize tracked jobs"),
        }
    }

    /// Restore tracked jobs from NATS KV on startup.
    async fn restore_tracked_jobs(&self) {
        if let Some(json) = self.checkpoint.load(TRACKED_JOBS_KEY).await {
            match serde_json::from_str::<HashMap<String, TrackedJob>>(&json) {
                Ok(restored) if !restored.is_empty() => {
                    tracing::info!(
                        count = restored.len(),
                        "Restored tracked jobs from KV checkpoint"
                    );
                    *self.tracked.write().await = restored;
                }
                Ok(_) => {} // Empty map, nothing to restore
                Err(e) => tracing::warn!(error = %e, "Failed to deserialize tracked jobs from KV"),
            }
        }
    }

    /// Compute the sacct start time: either the checkpoint or lookback window.
    fn sacct_start_time(&self, checkpoint: Option<&str>) -> String {
        match checkpoint {
            Some(ts) => ts.to_string(),
            None => {
                let lookback = chrono::Duration::seconds(self.config.lookback_window_secs as i64);
                let start = Utc::now() - lookback;
                start.format("%Y-%m-%dT%H:%M:%S").to_string()
            }
        }
    }

    /// Run the watcher loop with automatic reconnection.
    ///
    /// This is a long-running async task. Connects via SSH, polls Slurm,
    /// and publishes signals to NATS. Reconnects with exponential backoff
    /// on SSH disconnection.
    ///
    /// # Shutdown
    /// Pass a `shutdown` receiver to gracefully stop the watcher.
    pub async fn run(&self, shutdown: tokio::sync::broadcast::Receiver<()>) {
        petri_scheduler_bridge::backoff::run_with_reconnect(shutdown, "Slurm", || self.poll_loop())
            .await;
    }

    /// Connect via SSH and poll in a loop until disconnected.
    async fn poll_loop(&self) -> Result<(), WatcherError> {
        let ssh = SshSession::connect(&self.config).await?;

        tracing::info!(
            destination = %self.config.destination(),
            poll_interval = self.config.poll_interval_secs,
            "Slurm watcher connected, starting poll loop"
        );

        let interval = Duration::from_secs(self.config.poll_interval_secs);
        let checkpoint = self.load_checkpoint_cursor().await;
        let mut sacct_start = self.sacct_start_time(checkpoint.as_deref());

        // Restore tracked jobs from previous watcher instance (if any).
        // Critical for sacct-disabled environments where the tracked map is
        // the only way to infer completion for jobs that left squeue during downtime.
        self.restore_tracked_jobs().await;

        loop {
            // Poll squeue for active jobs
            let squeue_entries = match Self::poll_squeue(&ssh).await {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::warn!(error = %e, "squeue poll failed");
                    return Err(e);
                }
            };

            // Poll sacct for completed/terminal jobs.
            // sacct may be unavailable (accounting storage disabled), so degrade
            // gracefully to squeue-only mode rather than aborting the poll loop.
            let sacct_entries = match Self::poll_sacct(&ssh, &sacct_start).await {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::debug!(error = %e, "sacct poll failed (accounting may be disabled), using squeue-only mode");
                    Vec::new()
                }
            };

            // Process squeue entries (active jobs)
            for entry in &squeue_entries {
                self.process_squeue_entry(entry).await;
            }

            // Process sacct entries (completed/terminal jobs)
            for entry in &sacct_entries {
                if entry.is_main_job() {
                    self.process_sacct_entry(entry).await;
                }
            }

            // Update checkpoint cursor to now
            let now = Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
            self.save_checkpoint_cursor(&now).await;
            sacct_start = now;

            // Handle jobs that disappeared from squeue (infer completion if sacct
            // is unavailable) and purge terminal entries from the tracker.
            self.handle_disappeared_jobs(&squeue_entries).await;

            // Persist tracked jobs so a restarted watcher can resume tracking.
            self.save_tracked_jobs().await;

            tokio::time::sleep(interval).await;
        }
    }

    /// Poll squeue for active jobs.
    ///
    /// Uses `-o` format string with `|` delimiter instead of `--parsable2`
    /// for compatibility with Slurm 21.08+ (older versions lack `--parsable2` on squeue).
    async fn poll_squeue(ssh: &SshSession) -> Result<Vec<SqueueEntry>, WatcherError> {
        // %500k: explicit width for Comment field to prevent default-width truncation.
        // Default %k width (20 chars) silently truncates the JSON routing metadata,
        // causing extract_routing() to fail and the watcher to ignore Petri-managed jobs.
        let output = ssh.exec("squeue -o '%i|%500k|%T' -h").await?;
        Ok(SqueueEntry::parse_all(&output))
    }

    /// Poll sacct for jobs that changed since the given start time.
    async fn poll_sacct(
        ssh: &SshSession,
        start_time: &str,
    ) -> Result<Vec<SacctEntry>, WatcherError> {
        let command = format!(
            "sacct -o 'JobID,Comment,State,ExitCode,NodeList' --parsable2 -n --noconvert -S {}",
            start_time
        );
        let output = ssh.exec(&command).await?;
        Ok(SacctEntry::parse_all(&output))
    }

    /// Process a squeue entry: track new jobs, detect state transitions for active jobs.
    async fn process_squeue_entry(&self, entry: &SqueueEntry) {
        let routing = match extract_routing(&entry.comment) {
            Some(r) => r,
            None => return, // Not a Petri-managed job
        };

        let should_signal = {
            let tracked = self.tracked.read().await;
            match tracked.get(&entry.job_id) {
                Some(t) => t.last_state != entry.state,
                None => true, // New job
            }
        };

        if should_signal {
            if let Some(job_status) = status_mapping::map_slurm_state(&entry.state) {
                let msg_id = format!("slurm-{}-{}", entry.job_id, job_status.as_str());
                self.publish_signal(&routing, &entry.job_id, &job_status, &msg_id, "", "")
                    .await;
            }
        }

        // Always update tracked state (or insert if new)
        let needs_update = {
            let tracked = self.tracked.read().await;
            match tracked.get(&entry.job_id) {
                Some(t) => t.last_state != entry.state,
                None => true,
            }
        };
        if needs_update {
            let terminal_at = terminal_at_for(&entry.state);
            self.tracked.write().await.insert(
                entry.job_id.clone(),
                TrackedJob {
                    last_state: entry.state.clone(),
                    routing,
                    terminal_at,
                },
            );
        }
    }

    /// Process a sacct entry: detect terminal state transitions.
    async fn process_sacct_entry(&self, entry: &SacctEntry) {
        let routing = match extract_routing(&entry.comment) {
            Some(r) => r,
            None => {
                // Try to get routing from tracked state (sacct comment may be empty
                // for jobs we already know about from squeue)
                let tracked = self.tracked.read().await;
                match tracked.get(&entry.job_id) {
                    Some(t) => t.routing.clone(),
                    None => return, // Not a Petri-managed job
                }
            }
        };

        let job_status = match status_mapping::map_slurm_state(&entry.state) {
            Some(s) => s,
            None => return,
        };

        // Only signal if this is a new state
        let should_signal = {
            let tracked = self.tracked.read().await;
            match tracked.get(&entry.job_id) {
                Some(t) => t.last_state != entry.state,
                None => true,
            }
        };

        if should_signal {
            // If the job reached a terminal state but was never seen as RUNNING,
            // publish a synthetic Running signal first so the Petri net can walk
            // the full state machine (submitted → running → completed).
            // NATS JetStream msg_id dedup prevents duplicates if Running was
            // already published via process_squeue_entry.
            if job_status.is_terminal() {
                let was_running = {
                    let tracked = self.tracked.read().await;
                    tracked
                        .get(&entry.job_id)
                        .and_then(|t| status_mapping::map_slurm_state(&t.last_state))
                        .map(|s| s == petri_domain::JobStatus::Running)
                        .unwrap_or(false)
                };
                if !was_running {
                    let running_status = petri_domain::JobStatus::Running;
                    let running_msg_id =
                        format!("slurm-{}-{}", entry.job_id, running_status.as_str());
                    self.publish_signal(
                        &routing,
                        &entry.job_id,
                        &running_status,
                        &running_msg_id,
                        "",
                        "",
                    )
                    .await;
                }
            }

            let msg_id = format!("slurm-{}-{}", entry.job_id, job_status.as_str());

            self.publish_signal(
                &routing,
                &entry.job_id,
                &job_status,
                &msg_id,
                &entry.exit_code,
                &entry.node_list,
            )
            .await;

            let terminal_at = terminal_at_for(&entry.state);
            self.tracked.write().await.insert(
                entry.job_id.clone(),
                TrackedJob {
                    last_state: entry.state.clone(),
                    routing,
                    terminal_at,
                },
            );
        }
    }

    /// Publish an ExternalSignal to NATS.
    ///
    /// When per-status signal routes are configured, only publishes for statuses
    /// that have an explicit route. Unmapped statuses (e.g. Queued) are silently
    /// dropped to prevent polluting unrelated signal places via the fallback.
    async fn publish_signal(
        &self,
        routing: &RoutingMeta,
        job_id: &str,
        job_status: &petri_domain::JobStatus,
        msg_id: &str,
        exit_code: &str,
        node_list: &str,
    ) {
        // When explicit signal routes are configured, only publish for mapped statuses.
        // This prevents e.g. "queued" signals from falling through to the fallback place.
        if !routing.signal_routes.is_empty()
            && !routing.signal_routes.contains_key(job_status.as_str())
        {
            tracing::debug!(
                job_id = %job_id,
                status = %job_status.as_str(),
                "No signal route for status, skipping publish"
            );
            return;
        }

        let target_place = routing.place_for_status(job_status.as_str());

        tracing::debug!(
            job_id = %job_id,
            status = %job_status.as_str(),
            target_place = %target_place,
            msg_id = %msg_id,
            "Signaling Slurm job state change"
        );

        let signal = ExternalSignal {
            source: "slurm".to_string(),
            signal_key: routing.signal_key.clone(),
            payload: serde_json::json!({
                "source": "slurm",
                "scheduler_job_id": job_id,
                "job_status": job_status,
                "exit_code": exit_code,
                "node_list": node_list,
            }),
            timestamp: Utc::now(),
            // dedup_id mirrors the JetStream `Nats-Msg-Id` so the engine
            // `DedupIndex` suppresses re-detected/redelivered signals beyond
            // the 120s stream window.
            dedup_id: Some(msg_id.to_string()),
        };

        let subject = signal_subject(&routing.net_id, target_place);
        self.signal_publisher
            .publish(&subject, &signal, msg_id)
            .await;
    }

    /// Handle jobs that disappeared from squeue and purge terminal entries.
    ///
    /// When a tracked non-terminal job (e.g. RUNNING) disappears from squeue
    /// without sacct providing the final state, we infer `Completed`. This
    /// handles Slurm deployments where accounting storage is disabled.
    async fn handle_disappeared_jobs(&self, active_entries: &[SqueueEntry]) {
        let active_ids: std::collections::HashSet<&str> =
            active_entries.iter().map(|e| e.job_id.as_str()).collect();

        // Find tracked non-terminal jobs that are no longer in squeue.
        let disappeared: Vec<(String, TrackedJob)> = {
            let tracked = self.tracked.read().await;
            tracked
                .iter()
                .filter(|(id, _)| !active_ids.contains(id.as_str()))
                .filter(|(_, t)| {
                    status_mapping::map_slurm_state(&t.last_state)
                        .map(|s| !s.is_terminal())
                        .unwrap_or(false)
                })
                .map(|(id, t)| (id.clone(), t.clone()))
                .collect()
        };

        // Infer completion for disappeared jobs and publish signals.
        // If a job was never seen as RUNNING (e.g. went PENDING → completed too fast
        // for us to catch the RUNNING state), we must also publish a Running signal
        // so the Petri net can walk the full state machine (submitted → running → completed).
        // NATS JetStream msg_id dedup prevents duplicates if Running was already published.
        for (job_id, tracked_job) in &disappeared {
            tracing::info!(
                job_id = %job_id,
                last_state = %tracked_job.last_state,
                "Job left squeue without sacct data, inferring completion"
            );

            // If last known state wasn't RUNNING, publish an intermediate Running signal first.
            let was_running = status_mapping::map_slurm_state(&tracked_job.last_state)
                .map(|s| s == petri_domain::JobStatus::Running)
                .unwrap_or(false);
            if !was_running {
                let running_status = petri_domain::JobStatus::Running;
                let running_msg_id = format!("slurm-{}-{}", job_id, running_status.as_str());
                self.publish_signal(
                    &tracked_job.routing,
                    job_id,
                    &running_status,
                    &running_msg_id,
                    "",
                    "",
                )
                .await;
            }

            let job_status = petri_domain::JobStatus::Completed;
            let msg_id = format!("slurm-{}-{}", job_id, job_status.as_str());
            self.publish_signal(&tracked_job.routing, job_id, &job_status, &msg_id, "", "")
                .await;

            self.tracked.write().await.insert(
                job_id.clone(),
                TrackedJob {
                    last_state: "COMPLETED".to_string(),
                    routing: tracked_job.routing.clone(),
                    terminal_at: Some(Utc::now()),
                },
            );
        }

        // Evict tracked jobs that reached a terminal state long enough ago that
        // sacct can no longer return them. Keeping terminal entries until past
        // the lookback window prevents the watcher from re-publishing the same
        // sig_completed every poll cycle when sacct's rolling window keeps
        // surfacing the same completed job. The 2x multiplier is a clock-skew
        // / cluster-lag buffer; min 600s for very small lookback configs.
        let ttl_secs = (self.config.lookback_window_secs * 2).max(600);
        let cutoff = Utc::now() - chrono::Duration::seconds(ttl_secs as i64);
        let mut tracked = self.tracked.write().await;
        tracked.retain(|job_id, tracked_job| {
            if active_ids.contains(job_id.as_str()) {
                return true; // Still active in squeue
            }
            match tracked_job.terminal_at {
                Some(at) if at < cutoff => {
                    tracing::debug!(
                        job_id = %job_id,
                        last_state = %tracked_job.last_state,
                        age_secs = (Utc::now() - at).num_seconds(),
                        "Evicting terminal job past sacct lookback window"
                    );
                    false
                }
                _ => true, // Recently terminal, or non-terminal disappeared (defensive)
            }
        });
    }
}

/// Extract routing metadata from a job comment (JSON-encoded meta tags).
///
/// Returns `None` for empty comments, Slurm's `(null)` placeholder, or
/// comments that don't contain valid Petri routing metadata.
fn extract_routing(comment: &str) -> Option<RoutingMeta> {
    let comment = comment.trim();
    if comment.is_empty() || comment == "(null)" {
        return None;
    }

    let meta: HashMap<String, String> = match serde_json::from_str(comment) {
        Ok(m) => m,
        Err(_) => return None,
    };

    RoutingMeta::from_meta_tags(&meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_routing_valid() {
        let comment = r#"{"petri_net_id":"test-net","petri_place":"inbox","petri_signal_key":"job:0"}"#;
        let routing = extract_routing(comment).unwrap();
        assert_eq!(routing.net_id, "test-net");
        assert_eq!(routing.fallback_place, "inbox");
        assert_eq!(routing.signal_key, "job:0");
    }

    #[test]
    fn test_extract_routing_with_signal_routes() {
        let comment = r#"{"petri_net_id":"test-net","petri_place":"inbox","petri_signal_key":"job:0","petri_signal_running":"running_inbox"}"#;
        let routing = extract_routing(comment).unwrap();
        assert_eq!(routing.place_for_status("running"), "running_inbox");
        assert_eq!(routing.place_for_status("completed"), "inbox");
    }

    #[test]
    fn test_extract_routing_empty() {
        assert!(extract_routing("").is_none());
    }

    #[test]
    fn test_extract_routing_null_placeholder() {
        assert!(extract_routing("(null)").is_none());
    }

    #[test]
    fn test_extract_routing_invalid_json() {
        assert!(extract_routing("not json").is_none());
    }

    #[test]
    fn test_extract_routing_missing_required() {
        let comment = r#"{"unrelated": "value"}"#;
        assert!(extract_routing(comment).is_none());
    }

    #[test]
    fn test_terminal_at_for_terminal_states() {
        assert!(terminal_at_for("COMPLETED").is_some());
        assert!(terminal_at_for("FAILED").is_some());
        assert!(terminal_at_for("CANCELLED").is_some());
        assert!(terminal_at_for("TIMEOUT").is_some());
    }

    #[test]
    fn test_terminal_at_for_non_terminal_states() {
        assert!(terminal_at_for("RUNNING").is_none());
        assert!(terminal_at_for("PENDING").is_none());
        assert!(terminal_at_for("COMPLETING").is_none());
    }

    #[test]
    fn test_terminal_at_for_unknown_state() {
        assert!(terminal_at_for("GARBAGE").is_none());
    }

    /// Mirrors the eviction predicate in handle_disappeared_jobs. Kept here as a
    /// pure function so we can unit-test the keep/evict decision without
    /// constructing a full SlurmWatcher (which needs NATS).
    fn should_keep(active: bool, terminal_at: Option<chrono::DateTime<Utc>>, cutoff: chrono::DateTime<Utc>) -> bool {
        if active {
            return true;
        }
        match terminal_at {
            Some(at) if at < cutoff => false,
            _ => true,
        }
    }

    #[test]
    fn test_eviction_keeps_active_regardless_of_terminal_at() {
        let cutoff = Utc::now();
        let very_old = Utc::now() - chrono::Duration::seconds(99_999);
        assert!(should_keep(true, Some(very_old), cutoff));
        assert!(should_keep(true, None, cutoff));
    }

    #[test]
    fn test_eviction_keeps_recent_terminal() {
        let now = Utc::now();
        let cutoff = now - chrono::Duration::seconds(60);
        let recent = now - chrono::Duration::seconds(10);
        assert!(should_keep(false, Some(recent), cutoff));
    }

    #[test]
    fn test_eviction_drops_stale_terminal() {
        let now = Utc::now();
        let cutoff = now - chrono::Duration::seconds(60);
        let stale = now - chrono::Duration::seconds(120);
        assert!(!should_keep(false, Some(stale), cutoff));
    }

    #[test]
    fn test_eviction_keeps_terminal_at_none_defensively() {
        // Defensive case: a tracked entry with no terminal_at (e.g. legacy KV
        // state from before this field existed) shouldn't be evicted just
        // because it left squeue. handle_disappeared_jobs promotes such
        // entries to COMPLETED + sets terminal_at before reaching purge.
        let cutoff = Utc::now();
        assert!(should_keep(false, None, cutoff));
    }

    #[test]
    fn test_tracked_job_serde_back_compat_no_terminal_at() {
        // Older watchers persisted TrackedJob without terminal_at. Ensure
        // those entries deserialize cleanly with terminal_at = None.
        let legacy = r#"{
            "last_state": "RUNNING",
            "routing": {
                "net_id": "test-net",
                "fallback_place": "inbox",
                "signal_routes": {},
                "signal_key": "job:0"
            }
        }"#;
        let parsed: TrackedJob = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.last_state, "RUNNING");
        assert!(parsed.terminal_at.is_none());
    }
}
