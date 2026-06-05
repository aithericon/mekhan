//! NATS-driven `model_replicas` projection consumer (model-pool P4, docs/29 §6').
//!
//! Subscribes to `petri.events.>` on PETRI_GLOBAL with the durable consumer
//! `mekhan-model-replicas`. For each event on a `model-replica-<id>` net it
//! buffers the per-net event log, runs the pure projector, and folds the terminal
//! `stage_template` outcome onto the matching `model_replicas` row's
//! `status`/`replica_slug`/`last_error`.
//!
//! Mirrors `template_stagings/consumer.rs`. The row's `observed_count` is NOT set
//! here — it is roster-driven by the autoscaler loop (a `stage_template` success
//! proves "registered", not "serving"). We pre-filter to `model-replica-` nets by
//! the subject's net_id segment so the workflow/pool-net firehose is ignored.

use std::collections::HashMap;

use futures::StreamExt;
use sqlx::PgPool;

use petri_domain::{DomainEvent, PersistedEvent};

use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;
use crate::petri::events::fetch_events;

use super::projector::{project_replica, ReplicaUpdate};

const MAX_BUFFERED_NETS: usize = 256;

struct NetBuffer {
    events: Vec<PersistedEvent>,
}

/// Start the model-replicas ingest consumer. Spawned alongside the other
/// projection consumers in `main.rs`. Runs until the message stream ends.
pub async fn start_model_replicas_ingest(nats: MekhanNats, db: PgPool) {
    let consumer = match nats.model_replicas_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create model_replicas consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start model_replicas message stream: {e}");
            return;
        }
    };

    tracing::info!("model_replicas ingest started on petri.events.> (model-replica-* nets)");

    let mut buffers: HashMap<String, NetBuffer> = HashMap::new();

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("model_replicas ingest message error: {e}");
                continue;
            }
        };

        let subject = msg.subject.as_str();
        let result = process_event(&nats, &db, &mut buffers, subject, &msg.payload).await;

        match result {
            Ok(()) => {
                let _ = msg.ack().await;
            }
            Err(e) => {
                tracing::error!(subject = %subject, "model_replicas processing failed: {e}");
                let _ = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        std::time::Duration::from_secs(2),
                    )))
                    .await;
            }
        }
    }

    tracing::warn!("model_replicas ingest stream ended");
}

async fn process_event(
    nats: &MekhanNats,
    db: &PgPool,
    buffers: &mut HashMap<String, NetBuffer>,
    subject: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    let Some(net_id) = subject.split('.').nth(2) else {
        return Ok(());
    };
    if !net_id.starts_with("model-replica-") {
        return Ok(());
    }

    let incoming: PersistedEvent = match serde_json::from_slice(payload) {
        Ok(p) => p,
        Err(e) => {
            record_silent_drop_with(
                "model_replicas_envelope",
                &e,
                serde_json::json!({ "subject": subject, "net_id": net_id }),
                Some(payload),
            );
            return Ok(());
        }
    };

    let is_terminal = matches!(
        incoming.event,
        DomainEvent::NetCompleted { .. }
            | DomainEvent::NetCancelled { .. }
            | DomainEvent::NetFailed { .. }
    );

    if !buffers.contains_key(net_id) {
        let events = fetch_events(nats, net_id).await?;
        if buffers.len() >= MAX_BUFFERED_NETS {
            if let Some(victim) = buffers.keys().next().cloned() {
                buffers.remove(&victim);
            }
        }
        buffers.insert(net_id.to_string(), NetBuffer { events });
    } else {
        let buf = buffers.get_mut(net_id).expect("contains_key checked");
        if !buf.events.iter().any(|e| e.sequence == incoming.sequence) {
            buf.events.push(incoming);
            buf.events.sort_by_key(|e| e.sequence);
        }
    }

    let buf = buffers.get(net_id).expect("inserted/hit above");
    if let Some(update) = project_replica(&buf.events, net_id) {
        apply_update(db, &update).await?;
    }

    if is_terminal {
        buffers.remove(net_id);
    }
    Ok(())
}

/// Apply a terminal replica-actuation outcome to its `model_replicas` row.
/// `status` is COALESCE'd (only a failure sets it — the autoscaler owns
/// `active`/`provisioning`); `replica_slug` is set only if not already recorded;
/// `last_error` is set directly (cleared on success). The net id keys exactly one
/// row, so this targets `WHERE id = $1`. NEVER touches `observed_count`.
async fn apply_update(db: &PgPool, update: &ReplicaUpdate) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE model_replicas \
         SET status = COALESCE($2, status), \
             replica_slug = COALESCE(replica_slug, $3), \
             last_error = $4, \
             updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(update.replica_id)
    .bind(update.status.as_deref())
    .bind(update.remote_ref.as_deref())
    .bind(update.last_error.as_deref())
    .execute(db)
    .await?;
    Ok(())
}
