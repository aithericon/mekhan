//! Clockmaster: Durable timers using NATS KV.
//!
//! Two components:
//! 1. `NatsTimerClient` - Writes keys to a KV bucket.
//! 2. `Clockmaster` - Watches the KV bucket and schedules signals.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::jetstream::{self, kv};
use chrono::{TimeZone, Utc};
use futures::StreamExt;
use petri_domain::timer::{TimerCancelRequest, TimerClient, TimerError, TimerScheduleRequest};
use petri_domain::ExternalSignal;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use crate::Subjects;

/// Base name for the durable-timer KV bucket.
///
/// The live bucket is per-workspace: `KV_TIMERS_{ws}`, built via
/// [`crate::kv_bucket_for`]. Each workspace's clockmaster watches its own
/// bucket and fires signals on `petri.{ws}.{net}.signal.{place}`, so timers
/// can never leak across tenants.
pub const TIMER_KV_BUCKET: &str = "KV_TIMERS";

/// Build the NATS KV key for a durable timer.
///
/// A typo here would make `cancel` look up a different key than `schedule`
/// wrote, silently turning cancellation into a no-op — so both paths share
/// this single helper.
fn timer_kv_key(net_id: &str, place_id: &str, correlation_id: &uuid::Uuid) -> String {
    format!("timer.{}.{}.{}", net_id, place_id, correlation_id)
}

#[derive(Debug, Serialize, Deserialize)]
struct TimerValue {
    pub net_id: String,
    pub place_id: String,
    pub correlation_id: uuid::Uuid,
    pub expires_at_ms: u64,
    pub payload: serde_json::Value,
    /// Multi-tenancy: the workspace this timer belongs to, persisted at schedule
    /// time from the scheduling net's `service.workspace()`. The Clockmaster
    /// fires under THIS workspace (`petri.{workspace_id}.{net}.signal.{place}`)
    /// rather than its own process-level workspace, so a timer scheduled by a
    /// tenant-A net signals tenant A even while a single shared Clockmaster
    /// watches the process bucket. Defaults to `DEFAULT_WORKSPACE` for legacy
    /// entries written before this field existed.
    /// TODO(stream-per-ws): once a Clockmaster runs per workspace watching
    /// `KV_TIMERS_{ws}`, this field is redundant with the watcher's own ws.
    #[serde(default = "default_workspace")]
    pub workspace_id: String,
}

fn default_workspace() -> String {
    Subjects::DEFAULT_WORKSPACE.to_string()
}

/// Timer client that schedules delays by writing to a NATS KV bucket.
pub struct NatsTimerClient {
    kv: kv::Store,
}

impl NatsTimerClient {
    /// Open the timer client against a workspace's `KV_TIMERS_{ws}` bucket.
    pub async fn new(js: &jetstream::Context, workspace_id: &str) -> Result<Self, TimerError> {
        let bucket = crate::kv_bucket_for(TIMER_KV_BUCKET, workspace_id);
        Self::with_bucket(js, &bucket).await
    }

    /// Open the timer client against an explicit (already workspace-scoped) KV
    /// bucket name. Prefer [`Self::new`] which derives the per-workspace name.
    pub async fn with_bucket(js: &jetstream::Context, bucket: &str) -> Result<Self, TimerError> {
        let kv = js
            .get_key_value(bucket)
            .await
            .map_err(|e| TimerError::Fatal(format!("Failed to get KV bucket {}: {}", bucket, e)))?;

        Ok(Self { kv })
    }
}

#[async_trait::async_trait]
impl TimerClient for NatsTimerClient {
    async fn schedule(&self, request: TimerScheduleRequest) -> Result<(), TimerError> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let expires_at_ms = now_ms + request.delay_ms;

        let key = timer_kv_key(&request.net_id, &request.place_id, &request.correlation_id);

        let value = TimerValue {
            net_id: request.net_id,
            place_id: request.place_id,
            correlation_id: request.correlation_id,
            expires_at_ms,
            payload: request.payload,
            workspace_id: request.workspace_id,
        };

        let payload = serde_json::to_vec(&value).map_err(|e| TimerError::Fatal(e.to_string()))?;

        self.kv
            .put(&key, payload.into())
            .await
            .map_err(|e| TimerError::SchedulingFailed(e.to_string()))?;

        debug!(key = %key, delay_ms = request.delay_ms, "Scheduled durable timer");
        Ok(())
    }

    async fn cancel(&self, request: TimerCancelRequest) -> Result<bool, TimerError> {
        let key = timer_kv_key(&request.net_id, &request.place_id, &request.correlation_id);

        // Check if timer exists before deleting
        match self.kv.get(&key).await {
            Ok(Some(_)) => {
                self.kv
                    .delete(&key)
                    .await
                    .map_err(|e| TimerError::Fatal(format!("Failed to delete timer: {}", e)))?;
                debug!(key = %key, "Cancelled durable timer");
                Ok(true)
            }
            Ok(None) => {
                debug!(key = %key, "Timer not found (may have already fired)");
                Ok(false)
            }
            Err(e) => Err(TimerError::Fatal(format!("Failed to check timer: {}", e))),
        }
    }

    fn name(&self) -> &str {
        "nats_kv_timer"
    }
}

/// The Clockmaster service that watches for expired timers and publishes signals.
///
/// One clockmaster runs per workspace: it watches that workspace's
/// `KV_TIMERS_{ws}` bucket and fires expired timers as signals on
/// `petri.{ws}.{net}.signal.{place}`, so timers stay tenant-isolated.
pub struct Clockmaster {
    js: jetstream::Context,
    kv: kv::Store,
    workspace_id: String,
    /// Live per-timer sleep tasks, keyed by the KV timer key. Lets the watch
    /// loop abort a timer's sleeping task when the timer is cancelled or
    /// fired-and-deleted (KV Delete) or rescheduled (KV Put), instead of
    /// leaking a `tokio` task that sleeps to the ORIGINAL expiry while pinning
    /// its `TimerValue.payload`. Mirrors `HibernationMaster::sleep_tasks`.
    timers: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl Clockmaster {
    /// Build a clockmaster scoped to a workspace, watching `KV_TIMERS_{ws}`.
    pub async fn new(js: jetstream::Context, workspace_id: &str) -> Result<Self, String> {
        let bucket = crate::kv_bucket_for(TIMER_KV_BUCKET, workspace_id);
        Self::with_options(js, &bucket, workspace_id).await
    }

    /// Build a clockmaster against an explicit (already workspace-scoped) KV
    /// bucket. `workspace_id` drives the signal subject (`petri.{ws}.{net}.
    /// signal.{place}`); prefer [`Self::new`] which derives the bucket name.
    pub async fn with_options(
        js: jetstream::Context,
        bucket: &str,
        workspace_id: &str,
    ) -> Result<Self, String> {
        let kv = js
            .get_key_value(bucket)
            .await
            .map_err(|e| format!("Failed to get KV bucket: {}", e))?;

        Ok(Self {
            js,
            kv,
            workspace_id: workspace_id.to_string(),
            timers: Mutex::new(HashMap::new()),
        })
    }

    /// Number of live timer sleep tasks currently tracked. Test-only hook used
    /// to assert that cancelled/rescheduled timers don't leak their tasks.
    #[cfg(test)]
    pub(crate) async fn tracked_timer_count(&self) -> usize {
        self.timers.lock().await.len()
    }

    pub async fn run(&self) -> Result<(), String> {
        info!("Clockmaster starting up");

        // Hydrate existing timers
        let mut keys = self
            .kv
            .keys()
            .await
            .map_err(|e| format!("Failed to list keys: {}", e))?;
        while let Some(k) = keys.next().await {
            if let Ok(key) = k {
                if let Ok(Some(entry)) = self.kv.get(&key).await {
                    if let Ok(timer) = serde_json::from_slice::<TimerValue>(&entry) {
                        debug!(key = %key, "Hydrating existing timer");
                        self.schedule_timer_execution(key, timer).await;
                    }
                }
            }
        }

        // Watch for new timers
        let mut watcher = self
            .kv
            .watch_all()
            .await
            .map_err(|e| format!("Failed to watch KV bucket: {}", e))?;

        while let Some(entry_result) = watcher.next().await {
            let entry = match entry_result {
                Ok(e) => e,
                Err(e) => {
                    error!(error = %e, "Error watching KV");
                    continue;
                }
            };

            match entry.operation {
                kv::Operation::Put => {
                    if let Ok(timer) = serde_json::from_slice::<TimerValue>(&entry.value) {
                        // `schedule_timer_execution` aborts any prior task for
                        // this key before spawning, so a reschedule (Put with a
                        // new expiry) can't leave the old sleeping task behind.
                        info!(key = %entry.key, "Clockmaster observed new timer");
                        self.schedule_timer_execution(entry.key.clone(), timer)
                            .await;
                    }
                }
                kv::Operation::Delete | kv::Operation::Purge => {
                    // Timer cancelled, or fired-and-deleted by its own task:
                    // abort the sleep task so a cancelled timer doesn't linger
                    // to its original expiry pinning `TimerValue.payload`.
                    // Aborting an already-finished task is a no-op.
                    let mut timers = self.timers.lock().await;
                    if let Some(handle) = timers.remove(&entry.key) {
                        handle.abort();
                        debug!(key = %entry.key, "Aborted timer sleep task (cancel/expiry)");
                    }
                }
            }
        }

        Ok(())
    }

    async fn schedule_timer_execution(&self, key: String, timer: TimerValue) {
        let js = self.js.clone();
        let kv = self.kv.clone();
        // Fire under the timer's OWN workspace (persisted at schedule time), not
        // the Clockmaster's process-level workspace — a single shared Clockmaster
        // watching the process bucket would otherwise signal the wrong tenant.
        // Falls back to the watcher's ws only for a legacy entry whose
        // `workspace_id` deserialized to the default sentinel AND the watcher is
        // non-default (belt-and-suspenders; today both are `default` in dev).
        let workspace_id = if timer.workspace_id == Subjects::DEFAULT_WORKSPACE
            && self.workspace_id != Subjects::DEFAULT_WORKSPACE
        {
            self.workspace_id.clone()
        } else {
            timer.workspace_id.clone()
        };

        // Track (and de-duplicate) the sleep task. Abort any prior task for this
        // key before spawning so a reschedule never leaves an orphaned sleeper;
        // the Delete/Purge watch arm removes+aborts on cancel or fire-and-delete,
        // so the map only ever holds tasks for live timers.
        let map_key = key.clone();
        let mut timers = self.timers.lock().await;
        if let Some(prev) = timers.remove(&key) {
            prev.abort();
        }

        let handle = tokio::spawn(async move {
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;

            if timer.expires_at_ms > now_ms {
                let delay = Duration::from_millis(timer.expires_at_ms - now_ms);
                debug!(key = %key, delay_ms = delay.as_millis(), "Sleeping for timer");
                tokio::time::sleep(delay).await;
            }

            // Time to fire!
            // First, double check the timer hasn't been deleted or updated
            let current_entry = match kv.get(&key).await {
                Ok(Some(e)) => e,
                _ => return, // Timer gone or error
            };

            if let Ok(current_timer) = serde_json::from_slice::<TimerValue>(&current_entry) {
                if current_timer.expires_at_ms != timer.expires_at_ms {
                    return; // Timer was updated
                }
            } else {
                return;
            }

            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;

            let drift_ms = now_ms.saturating_sub(timer.expires_at_ms);

            // Convert to ISO 8601
            let scheduled_at_dt = match Utc
                .timestamp_millis_opt(timer.expires_at_ms as i64)
                .single()
            {
                Some(dt) => dt,
                None => {
                    error!(
                        net_id = %timer.net_id,
                        place_id = %timer.place_id,
                        expires_at_ms = timer.expires_at_ms,
                        "Timer scheduled_at timestamp out of range; skipping fire"
                    );
                    return;
                }
            };
            let triggered_at_dt = match Utc.timestamp_millis_opt(now_ms as i64).single() {
                Some(dt) => dt,
                None => {
                    error!(
                        net_id = %timer.net_id,
                        place_id = %timer.place_id,
                        now_ms = now_ms,
                        "Timer triggered_at timestamp out of range; skipping fire"
                    );
                    return;
                }
            };

            info!(
                net_id = %timer.net_id,
                place_id = %timer.place_id,
                drift_ms = drift_ms,
                "Timer expired, publishing signal"
            );

            // Inject metadata into the payload
            let mut payload = timer.payload;
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("drift_ms".to_string(), serde_json::json!(drift_ms));
                obj.insert(
                    "scheduled_at".to_string(),
                    serde_json::json!(scheduled_at_dt),
                );
                obj.insert(
                    "triggered_at".to_string(),
                    serde_json::json!(triggered_at_dt),
                );
            }

            let signal = ExternalSignal {
                source: "clockmaster".to_string(),
                signal_key: timer.correlation_id.to_string(),
                payload,
                timestamp: chrono::Utc::now(),
                // Timer firings are one-shot per correlation_id; dedup if redelivered.
                dedup_id: Some(format!("timer:{}", timer.correlation_id)),
            };

            let signal_subject =
                Subjects::signal_transfer(&workspace_id, &timer.net_id, &timer.place_id);
            let payload = match serde_json::to_vec(&signal) {
                Ok(p) => p,
                Err(e) => {
                    error!(error = %e, "Failed to serialize timer signal; skipping fire");
                    return;
                }
            };

            match js.publish(signal_subject, payload.into()).await {
                Ok(ack_future) => match ack_future.await {
                    Ok(_) => {
                        let _ = kv.delete(&key).await;
                    }
                    Err(e) => {
                        error!(error = %e, "Timer signal publish ack failed");
                    }
                },
                Err(e) => {
                    error!(error = %e, "Failed to publish timer signal");
                }
            }
        });

        timers.insert(map_key, handle);
    }
}
