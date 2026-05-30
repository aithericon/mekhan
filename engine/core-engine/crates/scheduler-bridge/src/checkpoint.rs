//! Checkpoint persistence via NATS KV.
//!
//! Scheduler watchers use [`CheckpointStore`] to persist their cursor position
//! so they can resume from the last processed event after a restart.
//!
//! # Per-cluster keying (multi-cluster correctness)
//!
//! All cluster watchers in one engine share the single [`KV_BUCKET`] KV bucket.
//! Each watcher therefore MUST namespace its checkpoint keys by the cluster it
//! observes, or two clusters of the same flavor clobber each other's cursor and
//! the next restart skips or replays events (the dup-seq failure class).
//!
//! The namespace is the datacenter `resource_id` (a UUID), or the reserved
//! [`DEV_BOOTSTRAP_CLUSTER_KEY`] literal for the env/dev-bootstrap watcher. The
//! key builders below ([`slurm_poll_cursor_key`], [`slurm_tracked_jobs_key`],
//! [`nomad_event_index_key`]) are the SINGLE source of truth for the scheme so
//! the Slurm and Nomad watchers cannot drift, and so the cluster-scoping is
//! unit-testable without standing up a watcher (which needs NATS).

/// NATS KV bucket name for watcher checkpoints.
pub const KV_BUCKET: &str = "PETRI_WATCHER";

/// Reserved cluster key for the env/dev-bootstrap watcher (the single
/// `from_env`-built client). A real datacenter `resource_id` is a UUID and can
/// never collide with this literal, so the dev-bootstrap cursor stays disjoint
/// from every resource-driven cluster's cursor.
pub const DEV_BOOTSTRAP_CLUSTER_KEY: &str = "_env";

/// Per-cluster KV key for a Slurm watcher's last-polled sacct cursor (ISO ts).
///
/// `cluster_key` is the datacenter `resource_id` (or [`DEV_BOOTSTRAP_CLUSTER_KEY`]).
pub fn slurm_poll_cursor_key(cluster_key: &str) -> String {
    format!("slurm.{}.poll_cursor", cluster_key)
}

/// Per-cluster KV key for a Slurm watcher's persisted tracked-jobs map.
///
/// Distinct from [`slurm_poll_cursor_key`] under the SAME `cluster_key` so the
/// cursor and the tracked-jobs map are namespaced independently per cluster.
pub fn slurm_tracked_jobs_key(cluster_key: &str) -> String {
    format!("slurm.{}.tracked_jobs", cluster_key)
}

/// Per-cluster KV key for a Nomad watcher's last-processed event-stream index.
///
/// `cluster_key` is the datacenter `resource_id` (or [`DEV_BOOTSTRAP_CLUSTER_KEY`]).
pub fn nomad_event_index_key(cluster_key: &str) -> String {
    format!("nomad.{}.event_index", cluster_key)
}

/// Persists and loads watcher cursor positions from NATS KV.
///
/// Wraps `Option<kv::Store>` for graceful degradation — if the KV bucket
/// cannot be created, the watcher still works but without persisted checkpoints.
pub struct CheckpointStore {
    kv: Option<async_nats::jetstream::kv::Store>,
}

impl CheckpointStore {
    /// Initialize the checkpoint store, creating the KV bucket if needed.
    ///
    /// If KV initialization fails, logs a warning and continues without
    /// checkpoint persistence.
    pub async fn new(jetstream: &async_nats::jetstream::Context) -> Self {
        let kv = match jetstream
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: KV_BUCKET.to_string(),
                history: 1,
                ..Default::default()
            })
            .await
        {
            Ok(store) => {
                tracing::info!(bucket = KV_BUCKET, "Checkpoint KV bucket ready");
                Some(store)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    bucket = KV_BUCKET,
                    "Failed to create checkpoint KV bucket — restart resilience disabled"
                );
                None
            }
        };

        Self { kv }
    }

    /// Create a checkpoint store without KV backing (for testing or when KV is unavailable).
    pub fn disabled() -> Self {
        Self { kv: None }
    }

    /// Whether the KV store is available.
    pub fn is_available(&self) -> bool {
        self.kv.is_some()
    }

    /// Load a checkpoint value by key.
    ///
    /// Returns `None` if the key does not exist, KV is unavailable, or the
    /// value cannot be decoded as UTF-8.
    pub async fn load(&self, key: &str) -> Option<String> {
        let kv = self.kv.as_ref()?;
        match kv.get(key).await {
            Ok(Some(bytes)) => {
                let s = std::str::from_utf8(&bytes).ok()?;
                tracing::debug!(key = key, value = s, "Loaded checkpoint from NATS KV");
                Some(s.to_string())
            }
            Ok(None) => {
                tracing::debug!(key = key, "No checkpoint found (first run)");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, key = key, "Failed to read checkpoint from NATS KV");
                None
            }
        }
    }

    /// Save a checkpoint value by key.
    pub async fn save(&self, key: &str, value: &str) {
        let Some(kv) = self.kv.as_ref() else {
            return;
        };
        if let Err(e) = kv.put(key, value.to_string().into()).await {
            tracing::warn!(
                error = %e,
                key = key,
                "Failed to save checkpoint to NATS KV"
            );
        }
    }

    /// Clear (purge) a checkpoint key.
    pub async fn clear(&self, key: &str) {
        let Some(kv) = self.kv.as_ref() else {
            return;
        };
        if let Err(e) = kv.purge(key).await {
            tracing::warn!(error = %e, key = key, "Failed to clear checkpoint");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slurm_keys_are_cluster_scoped() {
        let a = "11111111-2222-3333-4444-555555555555";
        let b = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

        // Two distinct clusters never share a poll-cursor key. A shared key is
        // the dup-seq failure: one cluster's cursor would clobber the other's,
        // and the next restart would skip or replay events on both.
        assert_ne!(slurm_poll_cursor_key(a), slurm_poll_cursor_key(b));
        assert_ne!(slurm_tracked_jobs_key(a), slurm_tracked_jobs_key(b));

        // The cluster_key threads into BOTH the cursor key AND the tracked-jobs
        // key (design doc §5.2 adversarial note) — neither falls back to a global.
        assert!(slurm_poll_cursor_key(a).contains(a));
        assert!(slurm_tracked_jobs_key(a).contains(a));
    }

    #[test]
    fn nomad_key_is_cluster_scoped() {
        let a = "11111111-2222-3333-4444-555555555555";
        let b = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        assert_ne!(nomad_event_index_key(a), nomad_event_index_key(b));
        assert!(nomad_event_index_key(a).contains(a));
    }

    #[test]
    fn cursor_and_tracked_jobs_keys_are_disjoint() {
        // Within ONE cluster the cursor and the tracked-jobs map must use
        // distinct keys, or saving the tracked-jobs blob overwrites the cursor.
        let c = "11111111-2222-3333-4444-555555555555";
        assert_ne!(slurm_poll_cursor_key(c), slurm_tracked_jobs_key(c));
    }

    #[test]
    fn flavor_prefixes_are_disjoint() {
        // A slurm cluster and a nomad cluster could in principle carry the same
        // resource_id namespace (they won't — one resource is one flavor — but
        // the keys are still disjoint by the flavor prefix, defence in depth).
        let c = "11111111-2222-3333-4444-555555555555";
        assert_ne!(slurm_poll_cursor_key(c), nomad_event_index_key(c));
    }

    #[test]
    fn dev_bootstrap_key_never_collides_with_a_uuid() {
        // The dev-bootstrap watcher uses the reserved "_env" namespace. A real
        // datacenter resource_id is a UUID, which can never equal the literal,
        // so the env cursor stays disjoint from every resource-driven cluster.
        let uuid = "11111111-2222-3333-4444-555555555555";
        assert_ne!(DEV_BOOTSTRAP_CLUSTER_KEY, uuid);
        assert_ne!(
            slurm_poll_cursor_key(DEV_BOOTSTRAP_CLUSTER_KEY),
            slurm_poll_cursor_key(uuid)
        );
        assert_ne!(
            nomad_event_index_key(DEV_BOOTSTRAP_CLUSTER_KEY),
            nomad_event_index_key(uuid)
        );
    }
}
