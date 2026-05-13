//! Checkpoint persistence via NATS KV.
//!
//! Scheduler watchers use [`CheckpointStore`] to persist their cursor position
//! so they can resume from the last processed event after a restart.

/// NATS KV bucket name for watcher checkpoints.
pub const KV_BUCKET: &str = "PETRI_WATCHER";

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
