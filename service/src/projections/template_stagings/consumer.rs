//! NATS-driven `template_stagings` consumer (B-staging, Phase 4).
//!
//! Subscribes to `petri.events.>` on PETRI_GLOBAL with the durable consumer
//! `mekhan-template-stagings`. For each event on a `staging-<id>` net it buffers
//! the per-net event log, runs the pure projector, and updates the matching
//! `template_stagings` row's status (`staged` / `failed`), `remote_ref`,
//! `staged_at`, and `last_error`.
//!
//! Modeled on `service/src/projections/allocations/consumer.rs`, but simpler:
//! a staging net fires `stage_template` exactly once, so the projector yields at
//! most ONE terminal update per net. The update always sets a TERMINAL status
//! (the projector never emits `staging`), and the net id (`staging-<row-id>`)
//! targets exactly one row, so the `UPDATE … WHERE id = $1` is naturally
//! idempotent under replay — no `last_sequence` guard column is needed.
//!
//! We cheaply pre-filter to `staging-` nets by the subject's net_id segment so
//! the (much larger) workflow/pool-net event firehose is ignored without a
//! buffer entry.

use std::collections::HashMap;

use futures::StreamExt;
use sqlx::PgPool;

use petri_domain::{DomainEvent, PersistedEvent};

use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;
use crate::petri::events::fetch_events;

use super::projector::{project_staging, StagingUpdate};

/// Upper bound on simultaneously-buffered staging nets. Terminal nets are
/// evicted eagerly; staging runs are short-lived so this rarely fills.
const MAX_BUFFERED_NETS: usize = 256;

struct NetBuffer {
    events: Vec<PersistedEvent>,
}

/// Start the template-stagings ingest consumer. Spawned alongside the other
/// projection consumers in `main.rs`. Runs until the message stream ends.
pub async fn start_template_stagings_ingest(nats: MekhanNats, db: PgPool) {
    let consumer = match nats.template_stagings_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create template_stagings consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start template_stagings message stream: {e}");
            return;
        }
    };

    tracing::info!("template_stagings ingest started on petri.events.> (staging-* nets)");

    let mut buffers: HashMap<String, NetBuffer> = HashMap::new();

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("template_stagings ingest message error: {e}");
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
                tracing::error!(subject = %subject, "template_stagings processing failed: {e}");
                let _ = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        std::time::Duration::from_secs(2),
                    )))
                    .await;
            }
        }
    }

    tracing::warn!("template_stagings ingest stream ended");
}

async fn process_event(
    nats: &MekhanNats,
    db: &PgPool,
    buffers: &mut HashMap<String, NetBuffer>,
    subject: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    // Subject: petri.events.{net_id}.> — only staging nets are our concern.
    let Some(net_id) = subject.split('.').nth(2) else {
        return Ok(());
    };
    if !net_id.starts_with("staging-") {
        return Ok(());
    }

    let incoming: PersistedEvent = match serde_json::from_slice(payload) {
        Ok(p) => p,
        Err(e) => {
            record_silent_drop_with(
                "template_stagings_envelope",
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
    if let Some(update) = project_staging(&buf.events, net_id) {
        apply_update(db, &update).await?;
    }

    if is_terminal {
        buffers.remove(net_id);
    }
    Ok(())
}

/// Apply a terminal staging outcome to its `template_stagings` row. Sets the
/// terminal status + `remote_ref`/`staged_at`; `last_error` is set directly (so a
/// successful re-stage after a prior failure CLEARS the error). The net id keys
/// exactly one row, so this targets `WHERE id = $1`.
async fn apply_update(db: &PgPool, update: &StagingUpdate) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE template_stagings \
         SET status = $2, \
             remote_ref = COALESCE($3, remote_ref), \
             staged_at = COALESCE($4, staged_at), \
             last_error = $5, \
             updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(update.staging_id)
    .bind(&update.status)
    .bind(update.remote_ref.as_deref())
    .bind(update.staged_at)
    .bind(update.last_error.as_deref())
    .execute(db)
    .await?;
    Ok(())
}
