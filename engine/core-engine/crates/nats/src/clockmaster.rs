//! Clockmaster: Durable timers using NATS KV.
//!
//! Two components:
//! 1. `NatsTimerClient` - Writes keys to a KV bucket.
//! 2. `Clockmaster` - Watches the KV bucket and schedules signals.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::jetstream::{self, kv};
use chrono::{TimeZone, Utc};
use petri_domain::timer::{TimerCancelRequest, TimerClient, TimerError, TimerScheduleRequest};
use petri_domain::ExternalSignal;
use tracing::{debug, error, info};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::Subjects;

pub const TIMER_KV_BUCKET: &str = "PETRI_TIMERS";

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
}

/// Timer client that schedules delays by writing to a NATS KV bucket.
pub struct NatsTimerClient {
    kv: kv::Store,
}

impl NatsTimerClient {
    pub async fn new(js: &jetstream::Context) -> Result<Self, TimerError> {
        Self::with_bucket(js, TIMER_KV_BUCKET).await
    }

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
        };

        let payload = serde_json::to_vec(&value)
            .map_err(|e| TimerError::Fatal(e.to_string()))?;

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
pub struct Clockmaster {
    js: jetstream::Context,
    kv: kv::Store,
    signal_prefix: String,
}

impl Clockmaster {
    pub async fn new(js: jetstream::Context) -> Result<Self, String> {
        Self::with_options(js, TIMER_KV_BUCKET, Subjects::SIGNAL_PREFIX).await
    }

    pub async fn with_options(
        js: jetstream::Context,
        bucket: &str,
        signal_prefix: &str,
    ) -> Result<Self, String> {
        let kv = js.get_key_value(bucket).await
            .map_err(|e| format!("Failed to get KV bucket: {}", e))?;
        
        Ok(Self {
            js,
            kv,
            signal_prefix: signal_prefix.to_string(),
        })
    }

    pub async fn run(&self) -> Result<(), String> {
        info!("Clockmaster starting up");

        // Hydrate existing timers
        let mut keys = self.kv.keys().await.map_err(|e| format!("Failed to list keys: {}", e))?;
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
        let mut watcher = self.kv.watch_all().await
            .map_err(|e| format!("Failed to watch KV bucket: {}", e))?;

        while let Some(entry_result) = watcher.next().await {
            let entry = match entry_result {
                Ok(e) => e,
                Err(e) => {
                    error!(error = %e, "Error watching KV");
                    continue;
                }
            };

            // Only process PUT operations
            if entry.operation != kv::Operation::Put {
                continue;
            }

            if let Ok(timer) = serde_json::from_slice::<TimerValue>(&entry.value) {
                // Deduplication is handled inside schedule_timer_execution (it re-checks KV)
                // But simple check here: if we just hydrated it, we might double-schedule.
                // schedule_timer_execution spawns a task that sleeps.
                // If we spawn two tasks, both sleep. Both wake up.
                // Both check KV.
                // First one fires and deletes.
                // Second one checks KV -> Gone. Returns.
                // So it is safe!
                info!(key = %entry.key, "Clockmaster observed new timer");
                self.schedule_timer_execution(entry.key.clone(), timer).await;
            }
        }

        Ok(())
    }

    async fn schedule_timer_execution(&self, key: String, timer: TimerValue) {
        let js = self.js.clone();
        let kv = self.kv.clone();
        let prefix = self.signal_prefix.clone();
        
        tokio::spawn(async move {
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
            let scheduled_at_dt = match Utc.timestamp_millis_opt(timer.expires_at_ms as i64).single() {
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
                obj.insert("scheduled_at".to_string(), serde_json::json!(scheduled_at_dt));
                obj.insert("triggered_at".to_string(), serde_json::json!(triggered_at_dt));
            }

            let signal = ExternalSignal {
                source: "clockmaster".to_string(),
                signal_key: timer.correlation_id.to_string(),
                payload,
                timestamp: chrono::Utc::now(),
                // Timer firings are one-shot per correlation_id; dedup if redelivered.
                dedup_id: Some(format!("timer:{}", timer.correlation_id)),
            };

            let signal_subject = format!("{}.{}.{}", prefix, timer.net_id, timer.place_id);
            let payload = match serde_json::to_vec(&signal) {
                Ok(p) => p,
                Err(e) => {
                    error!(error = %e, "Failed to serialize timer signal; skipping fire");
                    return;
                }
            };

            match js.publish(signal_subject, payload.into()).await {
                Ok(ack_future) => {
                    match ack_future.await {
                        Ok(_) => {
                            let _ = kv.delete(&key).await;
                        }
                        Err(e) => {
                            error!(error = %e, "Timer signal publish ack failed");
                        }
                    }
                }
                Err(e) => {
                    error!(error = %e, "Failed to publish timer signal");
                }
            }
        });
    }
}
