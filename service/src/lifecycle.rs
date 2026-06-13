use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::AckKind;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use sqlx::PgPool;
use tracing;
use uuid::Uuid;

use petri_domain::{DomainEvent, PersistedEvent};

use crate::catalogue::subscriptions::SubscriptionManager;
use crate::config::CleanupConfig;
use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;
use crate::petri::client::PetriClient;
use crate::triggers::{ResultWaiters, TerminalOutcome, TriggerDispatcher};

/// Resolve a WaitForResult waiter for `net_id`'s instance, if one is
/// registered. Fast-pathed: the `net_id`→`id` lookup is skipped entirely when
/// no waiters exist (the common case — every terminal net calls this).
async fn resolve_waiter(
    db: &PgPool,
    waiters: &ResultWaiters,
    net_id: &str,
    status: &str,
    result: Option<serde_json::Value>,
) {
    if waiters.is_empty() {
        return;
    }
    if let Ok(Some((id,))) =
        sqlx::query_as::<_, (uuid::Uuid,)>("SELECT id FROM workflow_instances WHERE net_id = $1")
            .bind(net_id)
            .fetch_optional(db)
            .await
    {
        waiters.resolve(
            &id,
            TerminalOutcome {
                status: status.to_string(),
                result,
            },
        );
    }
}

/// A terminal net event can legitimately arrive before the instance row is
/// written (the create-instance API and the net's first events race). We NAK
/// such events for redelivery — but a net that will *never* have a Mekhan
/// instance row (test-harness nets, instances deleted out from under a running
/// net) would otherwise be NAK'd forever, pinning the consumer and DB in a
/// 1-second poison loop. After this many deliveries we give up and ack-drop
/// the orphan event. 10s comfortably covers the real race (the row is written
/// synchronously before the net is deployed).
const MAX_ORPHAN_EVENT_DELIVERIES: i64 = 10;

/// Apply a terminal status (`completed` / `cancelled` / `failed`) to the
/// instance row and run all post-update side-effects (waiter resolution,
/// subscription cleanup, trigger evaluation). Handles orphan-event retry +
/// poison-cutoff and DB-error retry inline — the caller never touches the
/// message ack on the failure paths.
///
/// Returns `true` if the message was already ack'd/NAK'd here (caller should
/// `continue` the outer loop), or `false` if the update landed and the caller
/// should fall through to the outer ack.
/// Instance metadata returned by the terminal-status UPDATE, used to fold the
/// run into the per-template usage rollups within the same transaction.
#[derive(sqlx::FromRow)]
struct TerminalInstanceMeta {
    template_id: Uuid,
    template_version: i32,
    /// `live` | `draft` | `test_run` — bucketed verbatim into the rollup.
    mode: String,
    /// The principal that launched the run (`workflow_instances.created_by`,
    /// NOT NULL) — the `template_user_runs` key.
    created_by: Uuid,
    /// Run identity timestamp; `bucket_hour` truncates this (NOT `now()`), so
    /// the rollup reflects WHEN the run happened even if it terminates much
    /// later (long-running / replayed instances).
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    /// Just-set `NOW()` from this UPDATE (terminal wall-clock).
    completed_at: Option<DateTime<Utc>>,
}

/// The single derived terminal outcome for the run rollup. Mirrors the SQL CASE
/// in the analytics reader: a `completed` net is a `success` unless its result
/// envelope explicitly carries `ok: false`; `failed` (or an explicit
/// `ok: false`) is a `failure`; `cancelled` stays `cancelled`.
fn derive_outcome(status: &str, result: Option<&serde_json::Value>) -> &'static str {
    let explicit_ok_false = result
        .and_then(|v| v.get("ok"))
        .and_then(|v| v.as_bool())
        .map(|ok| !ok)
        .unwrap_or(false);
    match status {
        "failed" => "failure",
        "cancelled" => "cancelled",
        "completed" if explicit_ok_false => "failure",
        "completed" => "success",
        // `handle_terminal_event` is only reached for the three terminal
        // statuses above; treat any other as a benign success rather than
        // panicking on the rollup path.
        _ => "success",
    }
}

/// Fold one terminal transition into the per-template usage rollups
/// (`template_run_rollup` + `template_user_runs`) inside the caller's
/// transaction. Fires exactly once per running→terminal transition — the
/// caller's `WHERE status = 'running'` gate matches only the single delivery
/// that flips the row, and this runs in the same tx as that flip.
async fn record_terminal_rollups(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    meta: &TerminalInstanceMeta,
    outcome: &str,
) -> Result<(), sqlx::Error> {
    // Wall-clock duration; NULL endpoints (instance never started, or a row
    // that somehow lacks completed_at) contribute 0 to BOTH sum and count so
    // the reader's mean (`sum / count`) excludes them rather than skewing low.
    let duration_ms: Option<i64> = match (meta.started_at, meta.completed_at) {
        (Some(s), Some(c)) => Some((c - s).num_milliseconds().max(0)),
        _ => None,
    };
    let (dur_sum, dur_count) = match duration_ms {
        Some(ms) => (ms, 1i64),
        None => (0i64, 0i64),
    };

    sqlx::query(
        "INSERT INTO template_run_rollup \
           (template_id, template_version, bucket_hour, mode, outcome, \
            run_count, duration_ms_sum, duration_ms_count) \
         VALUES ($1, $2, date_trunc('hour', $3::timestamptz), $4, $5, 1, $6, $7) \
         ON CONFLICT (template_id, template_version, bucket_hour, mode, outcome) \
         DO UPDATE SET \
            run_count = template_run_rollup.run_count + 1, \
            duration_ms_sum = template_run_rollup.duration_ms_sum + EXCLUDED.duration_ms_sum, \
            duration_ms_count = template_run_rollup.duration_ms_count + EXCLUDED.duration_ms_count",
    )
    .bind(meta.template_id)
    .bind(meta.template_version)
    .bind(meta.created_at)
    .bind(&meta.mode)
    .bind(outcome)
    .bind(dur_sum)
    .bind(dur_count)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO template_user_runs \
           (template_id, user_id, run_count, first_run, last_run) \
         VALUES ($1, $2, 1, $3, $3) \
         ON CONFLICT (template_id, user_id) DO UPDATE SET \
            run_count = template_user_runs.run_count + 1, \
            first_run = LEAST(template_user_runs.first_run, EXCLUDED.first_run), \
            last_run = GREATEST(template_user_runs.last_run, EXCLUDED.last_run)",
    )
    .bind(meta.template_id)
    .bind(meta.created_by)
    .bind(meta.created_at)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

async fn handle_terminal_event(
    db: &PgPool,
    msg: &async_nats::jetstream::Message,
    waiters: &ResultWaiters,
    subscription_manager: &SubscriptionManager,
    triggers: Option<&Arc<TriggerDispatcher>>,
    net_id: &str,
    status: &str,
    result_envelope: Option<serde_json::Value>,
) -> bool {
    // Single transaction: the terminal status flip AND the per-template usage
    // rollup fold commit together, so the rollup can never double-count (a
    // redelivery after commit no longer matches `status = 'running'`) nor drift
    // from the status it summarizes.
    let mut tx = match db.begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("failed to begin terminal tx for {net_id} ({status}): {e}");
            let _ = msg
                .ack_with(AckKind::Nak(Some(Duration::from_secs(1))))
                .await;
            return true;
        }
    };

    let updated: Result<Option<TerminalInstanceMeta>, sqlx::Error> = sqlx::query_as(
        // Projector-driven transition: advance `updated_at` but NULL `updated_by`
        // — this is the engine acting, not a request principal (FE renders
        // "System"). See Phase 2 audit/provenance design. RETURNING the rollup
        // dimensions so the fold below reads them without a second round-trip.
        "UPDATE workflow_instances \
         SET status = $2, completed_at = NOW(), result = COALESCE($3::jsonb, result), \
             updated_at = NOW(), updated_by = NULL \
         WHERE net_id = $1 AND status = 'running' \
         RETURNING template_id, template_version, mode, created_by, created_at, \
                   started_at, completed_at",
    )
    .bind(net_id)
    .bind(status)
    .bind(result_envelope.clone())
    .fetch_optional(&mut *tx)
    .await;

    let meta = match updated {
        Ok(None) => {
            let _ = tx.rollback().await;
            let delivered = msg.info().map(|i| i.delivered).unwrap_or(0);
            if delivered >= MAX_ORPHAN_EVENT_DELIVERIES {
                tracing::warn!(
                    "no instance for {net_id} after {delivered} deliveries; \
                     dropping orphan {status} lifecycle event"
                );
                let _ = msg.ack().await;
            } else {
                tracing::warn!(
                    "no running instance found for {net_id} ({status}), will retry \
                     (delivery {delivered})"
                );
                let _ = msg
                    .ack_with(AckKind::Nak(Some(Duration::from_secs(1))))
                    .await;
            }
            return true;
        }
        Err(e) => {
            let _ = tx.rollback().await;
            tracing::error!("failed to update instance status for {net_id} ({status}): {e}");
            let _ = msg
                .ack_with(AckKind::Nak(Some(Duration::from_secs(1))))
                .await;
            return true;
        }
        Ok(Some(meta)) => meta,
    };

    let outcome = derive_outcome(status, result_envelope.as_ref());
    if let Err(e) = record_terminal_rollups(&mut tx, &meta, outcome).await {
        let _ = tx.rollback().await;
        tracing::error!("failed to fold terminal rollups for {net_id} ({status}): {e}");
        let _ = msg
            .ack_with(AckKind::Nak(Some(Duration::from_secs(1))))
            .await;
        return true;
    }

    if let Err(e) = tx.commit().await {
        tracing::error!("failed to commit terminal tx for {net_id} ({status}): {e}");
        let _ = msg
            .ack_with(AckKind::Nak(Some(Duration::from_secs(1))))
            .await;
        return true;
    }

    resolve_waiter(db, waiters, net_id, status, result_envelope).await;
    subscription_manager.cleanup_net_subscriptions(net_id).await;

    // NetCompletion triggers fire on every terminal status (completed,
    // cancelled, failed). SingleActiveCoalesce: dispatch coalesced follow-up.
    if let Some(disp) = triggers {
        crate::triggers::sources::net_completion::evaluate(disp, db, net_id, status).await;
        disp.on_instance_terminal(net_id).await;
    }
    false
}

/// Start the NATS lifecycle event listener.
/// Subscribes to `petri.events.mekhan-*.net.>` and updates the DB
/// when NetCompleted or NetCancelled events arrive.
pub async fn start_lifecycle_listener(
    nats: MekhanNats,
    db: PgPool,
    subscription_manager: Arc<SubscriptionManager>,
    triggers: Option<Arc<TriggerDispatcher>>,
    waiters: Arc<ResultWaiters>,
) {
    let consumer = match nats.lifecycle_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create lifecycle consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start lifecycle message stream: {e}");
            return;
        }
    };

    tracing::info!("lifecycle listener started on petri.events.mekhan-*.net.>");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("lifecycle listener message error: {e}");
                continue;
            }
        };

        // Parse subject: petri.events.{net_id}.net.{event_type}
        let subject = msg.subject.as_str();
        let parts: Vec<&str> = subject.split('.').collect();

        if parts.len() < 5 {
            // Subject doesn't match the `petri.events.{net_id}.net.{event_type}`
            // shape this consumer is bound to — either a producer drift or a
            // subject filter misconfiguration. Either way the message will
            // never be processable, so ACK + loud.
            record_silent_drop_with(
                "lifecycle_subject",
                &format!("unexpected subject: {subject}"),
                serde_json::json!({ "subject": subject }),
                None, // subject-only failure — no payload to capture
            );
            let _ = msg.ack().await;
            continue;
        }

        let net_id = parts[2];
        let event_type = parts[parts.len() - 1];

        // The subject carries the terminal status; the payload carries the
        // structured result envelope (`NetCompleted.exit_code`). An *empty*
        // payload is intentional (bare-terminal workflows have no
        // `exit_code` — `result` stays NULL); a *garbage* payload is a
        // producer drift bug we want to surface.
        let persisted: Option<PersistedEvent> = if msg.payload.is_empty() {
            None
        } else {
            match serde_json::from_slice(&msg.payload) {
                Ok(p) => Some(p),
                Err(e) => {
                    record_silent_drop_with(
                        "lifecycle_envelope",
                        &e,
                        serde_json::json!({ "subject": subject, "net_id": net_id }),
                        Some(&msg.payload),
                    );
                    None
                }
            }
        };

        let (status, result_envelope): (Option<&'static str>, Option<serde_json::Value>) =
            match event_type {
                "completed" => {
                    tracing::info!("net {net_id} completed");
                    let envelope = persisted.as_ref().and_then(|p| match &p.event {
                        DomainEvent::NetCompleted { exit_code, .. } => exit_code.clone(),
                        _ => None,
                    });
                    (Some("completed"), envelope)
                }
                "cancelled" => {
                    tracing::info!("net {net_id} cancelled");
                    // Synthesize a uniform error envelope so the WaitForResult /
                    // SSE / poll contract is shape-stable across terminal kinds.
                    let reason = persisted.as_ref().and_then(|p| match &p.event {
                        DomainEvent::NetCancelled { reason, .. } => reason.clone(),
                        _ => None,
                    });
                    let envelope = serde_json::json!({
                        "ok": false,
                        "error": { "reason": reason, "value": serde_json::Value::Null }
                    });
                    (Some("cancelled"), Some(envelope))
                }
                "failed" => {
                    tracing::info!("net {net_id} failed");
                    let reason = persisted
                        .as_ref()
                        .and_then(|p| match &p.event {
                            DomainEvent::NetFailed { reason, .. } => Some(reason.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "net failed".to_string());
                    let envelope = serde_json::json!({
                        "ok": false,
                        "error": { "reason": reason, "value": serde_json::Value::Null }
                    });
                    (Some("failed"), Some(envelope))
                }
                _ => (None, None), // Ignore created, initialized, etc.
            };

        if let Some(status) = status {
            if handle_terminal_event(
                &db,
                &msg,
                &waiters,
                &subscription_manager,
                triggers.as_ref(),
                net_id,
                status,
                result_envelope,
            )
            .await
            {
                continue;
            }
        }

        let _ = msg.ack().await;
    }

    tracing::warn!("lifecycle listener stream ended");
}

/// Start the background cleanup sweep task.
/// Periodically scans for finished instances past the retention window and cleans them up.
pub async fn start_cleanup_sweep(
    config: CleanupConfig,
    db: PgPool,
    nats: MekhanNats,
    petri: PetriClient,
    s3: Arc<crate::s3::ArtifactStore>,
) {
    let interval_secs = config.sweep_interval_minutes * 60;
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));

    tracing::info!(
        "cleanup sweep started: retention={}h, interval={}m, purge_events={}",
        config.retention_hours,
        config.sweep_interval_minutes,
        config.purge_events
    );

    loop {
        interval.tick().await;
        cleanup_finished_instances(&config, &db, &nats, &petri, &s3).await;
    }
}

async fn cleanup_finished_instances(
    config: &CleanupConfig,
    db: &PgPool,
    nats: &MekhanNats,
    petri: &PetriClient,
    s3: &crate::s3::ArtifactStore,
) {
    let retention_interval = format!("{} hours", config.retention_hours);

    // Find instances that have been finished longer than the retention window
    let stale: Vec<(uuid::Uuid, String)> = match sqlx::query_as(
        r#"
        SELECT id, net_id FROM workflow_instances
        WHERE status IN ('completed', 'failed', 'cancelled')
        AND completed_at < NOW() - $1::interval
        "#,
    )
    .bind(&retention_interval)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("cleanup sweep query failed: {e}");
            return;
        }
    };

    if stale.is_empty() {
        return;
    }

    tracing::info!("cleanup sweep: {} instances to clean up", stale.len());

    for (instance_id, net_id) in &stale {
        cleanup_net(net_id, nats, petri, config.purge_events).await;

        // GC per-instance agent transcript blobs (the off-token conversation
        // side-channel, keyed by the bare instance id == `_instance_id` the
        // compiler emits — note `net_id` is `mekhan-{instance_id}`, a
        // different string). Best-effort: a failed delete must not block
        // archival, and a no-op when the instance ran no agent nodes.
        if let Err(e) = s3.delete_prefix(&format!("instances/{instance_id}/")).await {
            tracing::warn!("cleanup: failed to delete transcript blobs for {instance_id}: {e}");
        }

        // Update status to archived
        if let Err(e) = sqlx::query(
            "UPDATE workflow_instances SET status = 'archived', updated_at = NOW(), \
                     updated_by = NULL WHERE id = $1",
        )
        .bind(instance_id)
        .execute(db)
        .await
        {
            tracing::error!("failed to archive instance {instance_id}: {e}");
        }
    }

    tracing::info!("cleanup sweep complete: {} instances archived", stale.len());
}

/// Clean up a single net's resources. All operations are idempotent.
pub async fn cleanup_net(net_id: &str, nats: &MekhanNats, petri: &PetriClient, purge_events: bool) {
    // Step 1: Remove from petri-lab in-memory registry
    if let Err(e) = petri.delete_net(net_id).await {
        tracing::warn!("cleanup: failed to delete net {net_id} from engine: {e}");
    }

    // Step 2: Delete KV_NET_METADATA entry
    if let Err(e) = nats.delete_net_metadata(net_id).await {
        tracing::warn!("cleanup: failed to delete metadata for {net_id}: {e}");
    }

    // Step 3: Delete KV_NET_ACTIVITY entry
    if let Err(e) = nats.delete_net_activity(net_id).await {
        tracing::warn!("cleanup: failed to delete activity for {net_id}: {e}");
    }

    // Step 4: Purge NATS event stream data
    if purge_events {
        if let Err(e) = nats.purge_net_events(net_id).await {
            tracing::warn!("cleanup: failed to purge events for {net_id}: {e}");
        }

        // Step 5: Purge NATS signal data
        if let Err(e) = nats.purge_net_signals(net_id).await {
            tracing::warn!("cleanup: failed to purge signals for {net_id}: {e}");
        }
    }
}
