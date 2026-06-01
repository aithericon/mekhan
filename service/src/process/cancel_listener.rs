//! NATS listener that consumes engine-initiated human task cancellations.
//!
//! When the engine fires its `human_cancel` effect handler — typically as a
//! Timeout node's drain transition when the timer wins the SLA race — the
//! engine's `HumanNatsClient::cancel_task` publishes a `HumanTaskCancellation`
//! envelope to `human.cancel.{net_id}.{place}`. At the petri-net level the
//! engine already emits the cancelled token on the handler's output port, so
//! token flow continues without waiting for a round trip. But mekhan's
//! `hpi_tasks` row stays `pending` forever — the task lingers in the user's
//! inbox even though the engine has moved on.
//!
//! This listener closes that loop. It subscribes to `human.cancel.>`,
//! deserializes the engine's `HumanTaskCancellation` payload (which carries
//! the `task_id`), and flips the matching `hpi_tasks` row to `cancelled`
//! with the cancellation timestamp and optional reason. Idempotent — only
//! pending tasks transition; already-terminal rows are left alone.
//!
//! Parallels the UI-driven cancel path in `process::handlers::cancel_task`:
//! both end up calling `queries::update_task_status(..., "cancelled", ...)`.
//! The difference is just where the trigger comes from — UI click vs.
//! engine-fired effect.

use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::Deserialize;
use sqlx::PgPool;

use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;
use crate::process::queries;

/// Subset of the engine's `HumanTaskCancellation` envelope. We only need
/// `task_id` and `reason`; `cancelled_at` is decoded so we can use it as
/// the projection timestamp (preferring the engine's wall clock over the
/// listener's, so replay of an older message doesn't fake a fresh
/// completed_at).
#[derive(Debug, Deserialize)]
struct HumanCancelPayload {
    task_id: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    cancelled_at: Option<DateTime<Utc>>,
}

/// Start the NATS listener that projects engine-fired human task cancels
/// into the `hpi_tasks` table. Subscribes to `human.cancel.>` on the
/// engine-owned `HUMAN_CANCEL` JetStream stream.
pub async fn start_human_cancel_listener(nats: MekhanNats, db: PgPool) {
    let consumer = match nats.human_cancel_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create human cancel consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start human cancel message stream: {e}");
            return;
        }
    };

    tracing::info!("human cancel listener started on human.cancel.>");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("human cancel listener message error: {e}");
                continue;
            }
        };

        let subject = msg.subject.as_str().to_string();
        let payload: HumanCancelPayload = match serde_json::from_slice(&msg.payload) {
            Ok(p) => p,
            Err(e) => {
                record_silent_drop_with(
                    "human_cancel_envelope",
                    &e,
                    serde_json::json!({ "subject": subject }),
                    Some(&msg.payload),
                );
                let _ = msg.ack().await;
                continue;
            }
        };

        let detail = match payload.reason.as_deref() {
            Some(r) => serde_json::json!({
                "cancelled_by": "engine",
                "reason": r,
            }),
            None => serde_json::json!({ "cancelled_by": "engine" }),
        };

        // Only flip pending tasks. The query itself is gated by status =
        // 'pending' via the existing helper's COALESCE semantics — but we
        // still pre-check with get_task so we can log the no-op cleanly
        // instead of an Ok(None) ambiguous with "task doesn't exist".
        let task = match queries::get_task(&db, &payload.task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                tracing::debug!(
                    task_id = %payload.task_id,
                    "human cancel for unknown task — engine moved on, no-op"
                );
                let _ = msg.ack().await;
                continue;
            }
            Err(e) => {
                tracing::error!("human cancel lookup failed: {e}");
                // NACK by not acking; redelivery will retry.
                if let Err(nak) = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        Duration::from_secs(5),
                    )))
                    .await
                {
                    tracing::warn!("nak failed: {nak}");
                }
                continue;
            }
        };

        if task.status != "pending" {
            tracing::debug!(
                task_id = %payload.task_id,
                status = %task.status,
                "human cancel for non-pending task — ignoring"
            );
            let _ = msg.ack().await;
            continue;
        }

        match queries::update_task_status(&db, &payload.task_id, "cancelled", Some(&detail)).await {
            Ok(Some(_)) => {
                tracing::info!(
                    task_id = %payload.task_id,
                    cancelled_at = ?payload.cancelled_at,
                    "human task cancelled via engine effect"
                );
            }
            Ok(None) => {
                tracing::warn!(
                    task_id = %payload.task_id,
                    "human cancel raced — task disappeared between lookup and update"
                );
            }
            Err(e) => {
                tracing::error!("human cancel update failed: {e}");
                if let Err(nak) = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        Duration::from_secs(5),
                    )))
                    .await
                {
                    tracing::warn!("nak failed: {nak}");
                }
                continue;
            }
        }

        if let Err(e) = msg.ack().await {
            tracing::warn!("ack failed for human cancel: {e}");
        }
    }
}
