//! NATS-driven `image_materializations` consumer (docs/22 container staging).
//!
//! Subscribes to `petri.events.>` on PETRI_GLOBAL with the durable consumer
//! `mekhan-image-materializations`. For each event on a `materialize-<id>` net it
//! buffers the per-net event log, runs the pure projector, and updates the
//! matching `image_materializations` row (`ready`/`failed`, `digest`, `sif_path`,
//! `size_bytes`, `last_error`).
//!
//! Direct clone of `template_stagings::consumer` — a materialize net fires
//! `materialize_image` exactly once, so the projector yields at most ONE terminal
//! update per net, applied as an idempotent `UPDATE … WHERE id = $1`.

use std::collections::HashMap;

use futures::StreamExt;
use sqlx::PgPool;

use petri_domain::{DomainEvent, PersistedEvent};

use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;
use crate::petri::events::fetch_events;

use super::projector::{project_materialize, MaterializeUpdate};

const MAX_BUFFERED_NETS: usize = 256;

struct NetBuffer {
    events: Vec<PersistedEvent>,
}

/// Start the image-materializations ingest consumer. Spawned alongside the other
/// projection consumers in `main.rs`. Runs until the message stream ends.
pub async fn start_image_materializations_ingest(nats: MekhanNats, db: PgPool) {
    let consumer = match nats.image_materializations_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create image_materializations consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start image_materializations message stream: {e}");
            return;
        }
    };

    tracing::info!("image_materializations ingest started on petri.events.> (materialize-* nets)");

    let mut buffers: HashMap<String, NetBuffer> = HashMap::new();

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("image_materializations ingest message error: {e}");
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
                tracing::error!(subject = %subject, "image_materializations processing failed: {e}");
                let _ = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        std::time::Duration::from_secs(2),
                    )))
                    .await;
            }
        }
    }

    tracing::warn!("image_materializations ingest stream ended");
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
    if !net_id.starts_with("materialize-") {
        return Ok(());
    }

    let incoming: PersistedEvent = match serde_json::from_slice(payload) {
        Ok(p) => p,
        Err(e) => {
            record_silent_drop_with(
                "image_materializations_envelope",
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
    if let Some(update) = project_materialize(&buf.events, net_id) {
        apply_update(db, &update).await?;
    }

    if is_terminal {
        buffers.remove(net_id);
    }
    Ok(())
}

/// Apply a terminal materialization outcome to its `image_materializations` row.
/// `last_error` is set directly so a successful re-materialize after a prior
/// failure clears it. The net id keys exactly one row.
async fn apply_update(db: &PgPool, update: &MaterializeUpdate) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE image_materializations \
         SET status = $2, \
             digest = COALESCE($3, digest), \
             sif_path = COALESCE($4, sif_path), \
             size_bytes = COALESCE($5, size_bytes), \
             last_error = $6, \
             updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(update.materialize_id)
    .bind(&update.status)
    .bind(update.digest.as_deref())
    .bind(update.sif_path.as_deref())
    .bind(update.size_bytes)
    .bind(update.last_error.as_deref())
    .execute(db)
    .await?;
    Ok(())
}
