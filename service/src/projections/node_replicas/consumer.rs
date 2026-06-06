//! NATS-driven `node_replicas` projection consumer (model-pool docs/31 Phase 2,
//! Loop 1).
//!
//! A near-verbatim clone of [`crate::projections::model_replicas::consumer`].
//! Subscribes to `petri.events.>` on PETRI_GLOBAL with the durable consumer
//! `mekhan-node-replicas`. For each event on a `node-pool-<id>-<gen>` net it buffers
//! the per-net event log, runs the pure projector, and folds the terminal
//! `stage_template` outcome onto the matching `node_replicas` row's
//! `status`/`node_slug`/`last_error`.
//!
//! The row's `observed_nodes`/`observed_slots` are NOT set here — they are
//! FleetLiveness-driven by Loop 1 (DERIVED-B; a `stage_template` success proves
//! "registered", not "serving"). We pre-filter to `node-pool-` nets by the
//! subject's net_id segment so the workflow/pool-net firehose is ignored.

use std::collections::HashMap;

use futures::StreamExt;
use sqlx::PgPool;

use petri_domain::{DomainEvent, PersistedEvent};

use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;
use crate::petri::events::fetch_events;

use super::projector::{project_node_pool, NodeReplicaUpdate};

const MAX_BUFFERED_NETS: usize = 256;

struct NetBuffer {
    events: Vec<PersistedEvent>,
}

/// Start the node-replicas ingest consumer. Spawned alongside the other projection
/// consumers in `main.rs`. Runs until the message stream ends.
pub async fn start_node_replicas_ingest(nats: MekhanNats, db: PgPool) {
    let consumer = match nats.node_replicas_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create node_replicas consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start node_replicas message stream: {e}");
            return;
        }
    };

    tracing::info!("node_replicas ingest started on petri.events.> (node-pool-* nets)");

    let mut buffers: HashMap<String, NetBuffer> = HashMap::new();

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("node_replicas ingest message error: {e}");
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
                tracing::error!(subject = %subject, "node_replicas processing failed: {e}");
                let _ = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        std::time::Duration::from_secs(2),
                    )))
                    .await;
            }
        }
    }

    tracing::warn!("node_replicas ingest stream ended");
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
    if !net_id.starts_with("node-pool-") {
        return Ok(());
    }

    let incoming: PersistedEvent = match serde_json::from_slice(payload) {
        Ok(p) => p,
        Err(e) => {
            record_silent_drop_with(
                "node_replicas_envelope",
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
    if let Some(update) = project_node_pool(&buf.events, net_id) {
        apply_node_pool_update(db, &update).await?;
    }

    if is_terminal {
        buffers.remove(net_id);
    }
    Ok(())
}

/// Apply a terminal node-pool-actuation outcome to its `node_replicas` row.
/// `status` is COALESCE'd (only a failure sets it — Loop 1 owns
/// `active`/`provisioning`); `node_slug` is set only if not already recorded;
/// `last_error` is set directly (cleared on success). The net id keys exactly one
/// row, so this targets `WHERE id = $1`. NEVER touches
/// `observed_nodes`/`observed_slots` (FleetLiveness-driven, DERIVED-B).
async fn apply_node_pool_update(db: &PgPool, update: &NodeReplicaUpdate) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE node_replicas \
         SET status = COALESCE($2, status), \
             node_slug = COALESCE(node_slug, $3), \
             last_error = $4, \
             updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(update.pool_resource_id)
    .bind(update.status.as_deref())
    .bind(update.node_slug.as_deref())
    .bind(update.last_error.as_deref())
    .execute(db)
    .await?;
    Ok(())
}
