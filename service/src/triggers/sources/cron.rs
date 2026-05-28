//! Cron trigger source (Phase 5b).
//!
//! The cron source schedules trigger fires per their `CronTrigger.schedule`
//! (cron string) and `CronTrigger.timezone` (IANA zone). The dispatcher owns
//! one tokio task per registered cron trigger; on every tick the task evaluates
//! the next-fire time, sleeps until then, then calls
//! `TriggerDispatcher::fire` with a payload of `{ fire_time, scheduled_time }`.
//!
//! Missed-fire replay (`CronCatchup::FireMissed`) consults a NATS KV bucket
//! that records the last fire timestamp per trigger node id, so a service
//! restart doesn't drop schedules silently. KV is the same persistence layer
//! `CatalogueSubscription` already uses — keep the wire in one place.
//!
//! Phase 5b scope: schedule parsing, periodic firing, basic catch-up. Jitter
//! and full at-most-once semantics across the fleet land when triggers grow
//! cross-replica leases (out of scope for this proposal).

use std::str::FromStr;
use std::sync::Arc;

use async_nats::jetstream::kv::Store;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::task::JoinHandle;

use crate::models::template::{CronCatchup, CronTrigger, TriggerSource};
use crate::triggers::dispatcher::TriggerDispatcher;
use crate::triggers::model::TriggerRecord;

const LAST_FIRE_PREFIX: &str = "cron.last_fire.";

/// Stored per-trigger so the dispatcher can replay missed fires after restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LastFireRecord {
    last_fire: DateTime<Utc>,
}

/// Parse and validate a `CronTrigger`'s schedule + timezone. Returns the
/// (Schedule, Tz) pair on success or a stable error string for the compiler /
/// editor to surface.
pub fn parse_cron(trigger: &CronTrigger) -> Result<(Schedule, Tz), String> {
    let schedule = Schedule::from_str(&trigger.schedule)
        .map_err(|e| format!("invalid cron schedule '{}': {e}", trigger.schedule))?;
    let tz: Tz = trigger
        .timezone
        .parse()
        .map_err(|e: chrono_tz::ParseError| {
            format!("invalid timezone '{}': {e}", trigger.timezone)
        })?;
    Ok((schedule, tz))
}

/// Compute the next fire time for a cron trigger, after `after`. Returns
/// `None` if the schedule has no future occurrences (unusual but possible for
/// extremely restrictive schedules).
pub fn next_fire_after(
    trigger: &CronTrigger,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, String> {
    let (schedule, tz) = parse_cron(trigger)?;
    let after_tz = after.with_timezone(&tz);
    Ok(schedule.after(&after_tz).next().map(|t| t.with_timezone(&Utc)))
}

/// Persistent loop that fires a single cron trigger on schedule. Spawned per
/// trigger by `register`. Replaces any prior task for the same trigger so
/// the latest published config wins.
pub fn spawn_loop(
    dispatcher: Arc<TriggerDispatcher>,
    kv: Option<Store>,
    record: TriggerRecord,
    cron: CronTrigger,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let node_id = record.node_id.clone();

        // Catch-up: load last-fire timestamp from KV (if any). Each new tick
        // we recompute the next-fire after `max(now, last_fire)` so a service
        // that came up after a missed window can immediately fire (FireMissed)
        // or skip ahead (SkipMissed — default).
        let mut last_fire: Option<DateTime<Utc>> =
            if let Some(ref kv) = kv {
                read_last_fire(kv, &node_id).await
            } else {
                None
            };

        // On boot, if FireMissed and we have a last_fire, replay the most
        // recent missed window (one fire, not all of them — that's safer for
        // schedules with short intervals).
        if matches!(cron.catchup, CronCatchup::FireMissed) {
            if let Some(lf) = last_fire {
                if let Ok(Some(next)) = next_fire_after(&cron, lf) {
                    if next <= Utc::now() {
                        fire_once(&dispatcher, &node_id, next, next).await;
                        if let Some(ref kv) = kv {
                            write_last_fire(kv, &node_id, next).await;
                        }
                        last_fire = Some(next);
                    }
                }
            }
        }

        loop {
            let from = last_fire.unwrap_or_else(Utc::now);
            let next = match next_fire_after(&cron, from) {
                Ok(Some(t)) => t,
                Ok(None) => {
                    tracing::warn!(
                        node_id = %node_id,
                        "cron schedule has no future occurrences — loop exiting"
                    );
                    return;
                }
                Err(e) => {
                    tracing::error!(
                        node_id = %node_id,
                        "cron schedule parse failed: {e}"
                    );
                    return;
                }
            };
            let now = Utc::now();
            let sleep_for = (next - now)
                .to_std()
                .unwrap_or(std::time::Duration::from_secs(1));
            tokio::time::sleep(sleep_for).await;

            // After waking up, the dispatcher may have forgotten this trigger
            // (template superseded / unpublished). Check before firing.
            if dispatcher.get(&node_id).is_none() {
                tracing::debug!(node_id = %node_id, "cron trigger forgotten — loop exiting");
                return;
            }

            let fire_time = Utc::now();
            fire_once(&dispatcher, &node_id, next, fire_time).await;
            if let Some(ref kv) = kv {
                write_last_fire(kv, &node_id, next).await;
            }
            last_fire = Some(next);
        }
    })
}

/// Convenience wrapper: spawn a cron loop for every Cron trigger in the
/// dispatcher's current registry. Called once at startup; subsequent
/// re-registrations on publish should call `spawn_loop` directly.
pub async fn register_all(dispatcher: Arc<TriggerDispatcher>, kv: Option<Store>) -> usize {
    let mut spawned = 0;
    for rec in dispatcher.list_all() {
        if let TriggerSource::Cron(ref c) = rec.source {
            if rec.enabled {
                let _handle = spawn_loop(dispatcher.clone(), kv.clone(), rec.clone(), c.clone());
                spawned += 1;
            }
        }
    }
    if spawned > 0 {
        tracing::info!(spawned, "registered cron triggers");
    }
    spawned
}

async fn fire_once(
    dispatcher: &TriggerDispatcher,
    node_id: &str,
    scheduled_time: DateTime<Utc>,
    fire_time: DateTime<Utc>,
) {
    let payload = json!({
        "fire_time": fire_time.to_rfc3339(),
        "scheduled_time": scheduled_time.to_rfc3339(),
    });
    match dispatcher
        .fire(node_id, payload, petri_api_types::DispatchOptions::default())
        .await
    {
        Ok(result) => {
            tracing::info!(
                node_id = %node_id,
                outcome = ?result.outcome,
                "cron trigger fired"
            );
        }
        Err(e) => {
            tracing::warn!(
                node_id = %node_id,
                "cron trigger fire failed: {e}"
            );
        }
    }
}

async fn read_last_fire(kv: &Store, node_id: &str) -> Option<DateTime<Utc>> {
    let key = format!("{LAST_FIRE_PREFIX}{node_id}");
    match kv.get(&key).await {
        Ok(Some(bytes)) => serde_json::from_slice::<LastFireRecord>(&bytes)
            .ok()
            .map(|r| r.last_fire),
        _ => None,
    }
}

async fn write_last_fire(kv: &Store, node_id: &str, ts: DateTime<Utc>) {
    let key = format!("{LAST_FIRE_PREFIX}{node_id}");
    let value = match serde_json::to_vec(&LastFireRecord { last_fire: ts }) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("cron last_fire serialize failed: {e}");
            return;
        }
    };
    if let Err(e) = kv.put(&key, value.into()).await {
        tracing::warn!(node_id = %node_id, "cron last_fire write failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::CronCatchup;

    fn t(schedule: &str, timezone: &str) -> CronTrigger {
        CronTrigger {
            schedule: schedule.to_string(),
            timezone: timezone.to_string(),
            jitter_secs: 0,
            catchup: CronCatchup::SkipMissed,
        }
    }

    #[test]
    fn parse_valid_cron() {
        let trigger = t("0 0 9 * * *", "UTC");
        assert!(parse_cron(&trigger).is_ok());
    }

    #[test]
    fn parse_rejects_bad_schedule() {
        let trigger = t("not a cron", "UTC");
        assert!(parse_cron(&trigger).is_err());
    }

    #[test]
    fn parse_rejects_bad_timezone() {
        let trigger = t("0 0 9 * * *", "Not/A/Zone");
        assert!(parse_cron(&trigger).is_err());
    }

    #[test]
    fn next_fire_advances() {
        // 9:00 UTC every day → next fire after midnight should be the same day's 9:00.
        let trigger = t("0 0 9 * * *", "UTC");
        let after = chrono::DateTime::parse_from_rfc3339("2026-05-15T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = next_fire_after(&trigger, after).unwrap().unwrap();
        assert_eq!(next.format("%H:%M").to_string(), "09:00");
    }
}
