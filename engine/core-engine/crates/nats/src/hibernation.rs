//! Activity tracking and automatic hibernation for idle net instances.
//!
//! Uses a NATS KV bucket (`KV_NET_ACTIVITY`) to track last-active timestamps.
//! The [`HibernationMaster`] watches for expired entries and triggers hibernation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::kv::Store;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// NATS KV bucket name for net activity tracking.
pub const ACTIVITY_KV_BUCKET: &str = "KV_NET_ACTIVITY";

/// State stored in the activity KV entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub last_active: String,
    pub state: ActivityState,
}

/// Whether a net is currently hot (in-memory) or hibernating.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    Hot,
    Hibernating,
}

/// Tracks per-net activity timestamps in a NATS KV bucket.
///
/// Call [`touch`] after each eval step, signal injection, or command handling
/// to keep the net alive. The [`HibernationMaster`] watches for expired entries.
pub struct ActivityTracker {
    kv: Store,
    idle_timeout: Duration,
}

impl ActivityTracker {
    pub fn new(kv: Store, idle_timeout: Duration) -> Self {
        assert!(
            idle_timeout > Duration::ZERO,
            "idle_timeout must be positive"
        );
        Self { kv, idle_timeout }
    }

    /// Touch the activity timestamp for a net. Resets the idle timer.
    pub async fn touch(&self, net_id: &str) -> Result<(), String> {
        let entry = ActivityEntry {
            last_active: Utc::now().to_rfc3339(),
            state: ActivityState::Hot,
        };
        let value = serde_json::to_vec(&entry).map_err(|e| e.to_string())?;
        self.kv
            .put(net_id, value.into())
            .await
            .map_err(|e| format!("Failed to touch activity for {}: {}", net_id, e))?;
        Ok(())
    }

    /// Mark a net as hibernating (prevents re-wake during shutdown).
    pub async fn mark_hibernating(&self, net_id: &str) -> Result<(), String> {
        let entry = ActivityEntry {
            last_active: Utc::now().to_rfc3339(),
            state: ActivityState::Hibernating,
        };
        let value = serde_json::to_vec(&entry).map_err(|e| e.to_string())?;
        self.kv
            .put(net_id, value.into())
            .await
            .map_err(|e| format!("Failed to mark hibernating {}: {}", net_id, e))?;
        Ok(())
    }

    /// Remove a net's activity entry (net terminated or fully hibernated).
    pub async fn remove(&self, net_id: &str) -> Result<(), String> {
        self.kv
            .delete(net_id)
            .await
            .map_err(|e| format!("Failed to remove activity for {}: {}", net_id, e))?;
        Ok(())
    }

    /// Check if a net is currently marked as hot.
    pub async fn is_hot(&self, net_id: &str) -> Result<bool, String> {
        match self.kv.get(net_id).await {
            Ok(Some(entry)) => {
                let activity: ActivityEntry = serde_json::from_slice(&entry)
                    .map_err(|e| format!("Failed to parse activity entry: {}", e))?;
                Ok(activity.state == ActivityState::Hot)
            }
            Ok(None) => Ok(false),
            Err(e) => Err(format!("Failed to check activity for {}: {}", net_id, e)),
        }
    }

    /// Get the idle timeout duration.
    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    /// Parse an activity entry from raw KV value bytes.
    pub fn parse_entry(value: &[u8]) -> Result<ActivityEntry, String> {
        serde_json::from_slice(value).map_err(|e| format!("Failed to parse activity entry: {}", e))
    }

    /// Get the raw activity entry for a net (for timestamp checking).
    pub async fn get_entry(&self, net_id: &str) -> Result<Option<ActivityEntry>, String> {
        match self.kv.get(net_id).await {
            Ok(Some(entry)) => {
                let activity: ActivityEntry = serde_json::from_slice(&entry)
                    .map_err(|e| format!("Failed to parse activity entry: {}", e))?;
                Ok(Some(activity))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("Failed to get activity for {}: {}", net_id, e)),
        }
    }

    /// Provide access to the underlying KV store (for watchers).
    pub fn kv(&self) -> &Store {
        &self.kv
    }
}

/// Trait for decoupling hibernation logic from the NetRegistry.
#[async_trait::async_trait]
pub trait NetHibernator: Send + Sync {
    /// Hibernate a specific net instance.
    async fn hibernate(&self, net_id: &str) -> Result<(), String>;
}

/// Watches `KV_NET_ACTIVITY` and hibernates idle nets.
///
/// Uses the clockmaster pattern: bootstraps from existing KV entries,
/// watches for changes, and spawns sleep tasks that trigger hibernation
/// after the idle timeout expires.
pub struct HibernationMaster {
    activity: Arc<ActivityTracker>,
    hibernator: Arc<dyn NetHibernator>,
    sleep_tasks: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl HibernationMaster {
    pub fn new(activity: Arc<ActivityTracker>, hibernator: Arc<dyn NetHibernator>) -> Self {
        Self {
            activity,
            hibernator,
            sleep_tasks: Mutex::new(HashMap::new()),
        }
    }

    /// Run the hibernation master loop.
    ///
    /// 1. Bootstrap: scan existing KV entries, spawn sleep tasks.
    /// 2. Watch: listen for KV changes, (re)spawn sleep tasks on Put,
    ///    trigger hibernation on Delete (TTL expiry).
    pub async fn run(&self) -> Result<(), String> {
        use futures::StreamExt;

        let idle_timeout = self.activity.idle_timeout();

        // Bootstrap: scan existing entries
        match self.activity.kv.keys().await {
            Ok(mut keys) => {
                while let Some(key) = keys.next().await {
                    match key {
                        Ok(net_id) => {
                            tracing::debug!(net_id = %net_id, "Bootstrapping sleep task");
                            self.spawn_sleep_task(&net_id, idle_timeout).await;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Error reading KV key during bootstrap");
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to list activity KV keys during bootstrap");
            }
        }

        // Watch for changes
        let mut watcher = self
            .activity
            .kv
            .watch_all()
            .await
            .map_err(|e| format!("Failed to watch activity KV: {}", e))?;

        tracing::info!("HibernationMaster started, watching for idle nets");

        while let Some(entry) = watcher.next().await {
            match entry {
                Ok(entry) => {
                    let net_id = entry.key.clone();
                    match entry.operation {
                        async_nats::jetstream::kv::Operation::Put => {
                            // Activity refreshed — (re)spawn sleep task
                            self.spawn_sleep_task(&net_id, idle_timeout).await;
                        }
                        async_nats::jetstream::kv::Operation::Delete
                        | async_nats::jetstream::kv::Operation::Purge => {
                            // Entry expired or deleted — cancel sleep task if any
                            let mut tasks = self.sleep_tasks.lock().await;
                            if let Some(handle) = tasks.remove(&net_id) {
                                handle.abort();
                            }
                            // Trigger hibernation
                            tracing::info!(
                                net_id = %net_id,
                                "Activity entry expired/deleted, triggering hibernation"
                            );
                            if let Err(e) = self.hibernator.hibernate(&net_id).await {
                                tracing::debug!(
                                    net_id = %net_id,
                                    error = %e,
                                    "Failed to hibernate net (likely already removed by delete handler)"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Error watching activity KV");
                }
            }
        }

        tracing::info!("HibernationMaster stopped");
        Ok(())
    }

    /// Spawn (or replace) a sleep task for a net.
    ///
    /// When the timer expires, double-checks KV and hibernates if still idle.
    async fn spawn_sleep_task(&self, net_id: &str, timeout: Duration) {
        let mut tasks = self.sleep_tasks.lock().await;

        // Cancel existing task if any
        if let Some(handle) = tasks.remove(net_id) {
            handle.abort();
        }

        let net_id_key = net_id.to_string();
        let net_id = net_id.to_string();
        let activity = self.activity.clone();
        let hibernator = self.hibernator.clone();

        let handle = tokio::spawn(async move {
            tokio::time::sleep(timeout).await;

            // Double-check: the entry may have been refreshed while we slept
            match activity.get_entry(&net_id).await {
                Ok(Some(act)) => {
                    if act.state != ActivityState::Hot {
                        // Already hibernating or gone
                        return;
                    }
                    if let Ok(last) = chrono::DateTime::parse_from_rfc3339(&act.last_active) {
                        let elapsed = Utc::now().signed_duration_since(last.with_timezone(&Utc));
                        if elapsed.to_std().unwrap_or(timeout) < timeout {
                            // Refreshed recently — skip
                            tracing::debug!(
                                net_id = %net_id,
                                "Sleep task expired but net was recently active, skipping"
                            );
                            return;
                        }
                    }
                    // Actually idle — hibernate
                    tracing::info!(
                        net_id = %net_id,
                        "Idle timeout reached, triggering hibernation"
                    );
                    if let Err(e) = hibernator.hibernate(&net_id).await {
                        tracing::warn!(
                            net_id = %net_id,
                            error = %e,
                            "Failed to hibernate net after idle timeout"
                        );
                    }
                }
                Ok(None) => {
                    // Entry gone — already hibernated or terminated
                }
                Err(e) => {
                    tracing::warn!(
                        net_id = %net_id,
                        error = %e,
                        "Failed to check activity during sleep task"
                    );
                }
            }
        });

        tasks.insert(net_id_key, handle);
    }
}
