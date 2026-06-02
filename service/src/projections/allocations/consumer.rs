//! NATS-driven allocations consumer.
//!
//! Subscribes to `petri.events.>` on PETRI_GLOBAL with the durable consumer
//! `mekhan-allocations`. On each event, buffers the per-net event log, runs the
//! pure projector, and upserts changed rows into `allocations`
//! (sequence-guarded by `(net_id, grant_id, kind)`).
//!
//! Modeled on `service/src/projections/step_executions/consumer.rs`. Unlike the
//! step-executions consumer it needs NO template `InterfaceRegistry` — the
//! projector derives every key from the engine events. It DOES resolve the
//! owning workflow `instance_id` (UUID) from the workflow net embedded in the
//! grant_id prefix (`<workflow_net_id>:<node_id>`), best-effort: pool-management
//! nets / unknown grants resolve to NULL `instance_id`, which is allowed.
//!
//! ## Tapping the accounting signal
//!
//! The enriched terminal accounting payload the per-cluster watcher publishes on
//! `petri.signal.{net_id}.>` lands in the SAME PETRI_GLOBAL event log as a
//! `TokenCreated` (the engine injects the signal payload as the token color,
//! tagging `signal_key == grant_id`). So a single `petri.events.>` consumer sees
//! it — no second `petri.signal.>` consumer is required. If a future engine
//! change ever slims the persisted signal token below what the projector needs,
//! add a second pull consumer here filtered on `petri.signal.>` and merge its
//! `ExternalSignal.payload` into the same upsert path.

use std::collections::HashMap;

use futures::StreamExt;
use sqlx::PgPool;
use uuid::Uuid;

use petri_domain::{DomainEvent, PersistedEvent};

use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;
use crate::petri::events::fetch_events;

use super::projector::{project_allocations, AllocationRow};

/// Upper bound on simultaneously-buffered nets. Mirrors the step-executions
/// consumer; terminal nets are evicted eagerly.
const MAX_BUFFERED_NETS: usize = 512;

/// Per-net in-memory projection input: the full event log for one net. Unlike
/// the step-executions buffer there's no registry/instance context to cache —
/// the projector is registry-free and `instance_id` is resolved per-upsert.
struct NetBuffer {
    events: Vec<PersistedEvent>,
}

/// Start the allocations ingest consumer. Spawned alongside the step-executions
/// and causality consumers in `main.rs`. Runs until the message stream ends.
pub async fn start_allocations_ingest(nats: MekhanNats, db: PgPool) {
    let consumer = match nats.allocations_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create allocations consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start allocations message stream: {e}");
            return;
        }
    };

    tracing::info!("allocations ingest started on petri.events.>");

    let mut buffers: HashMap<String, NetBuffer> = HashMap::new();

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("allocations ingest message error: {e}");
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
                tracing::error!(subject = %subject, "allocations processing failed: {e}");
                let _ = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        std::time::Duration::from_secs(2),
                    )))
                    .await;
            }
        }
    }

    tracing::warn!("allocations ingest stream ended");
}

async fn process_event(
    nats: &MekhanNats,
    db: &PgPool,
    buffers: &mut HashMap<String, NetBuffer>,
    subject: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    // Subject: petri.events.{net_id}.>
    let Some(net_id) = subject.split('.').nth(2) else {
        tracing::warn!("allocations: cannot extract net_id from subject: {subject}");
        return Ok(());
    };

    let incoming: PersistedEvent = match serde_json::from_slice(payload) {
        Ok(p) => p,
        Err(e) => {
            record_silent_drop_with(
                "allocations_envelope",
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
        // Cache miss: bootstrap the FULL event log once (covers events that
        // predate this process / the durable cursor).
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
    if !buf.events.is_empty() {
        let rows = project_allocations(&buf.events, net_id);
        if !rows.is_empty() {
            upsert_rows(db, &rows).await?;
        }
    }

    if is_terminal {
        buffers.remove(net_id);
    }
    Ok(())
}

/// Resolve the owning workflow `instance_id` (UUID) for a grant. The grant_id is
/// `<workflow_net_id>:<node_id>`; the workflow net is a `workflow_instances`
/// row. Returns `None` for pool-management / unknown grants (allowed — the
/// `instance_id` column is nullable).
async fn resolve_instance_id(db: &PgPool, grant_id: &str) -> Result<Option<Uuid>, sqlx::Error> {
    let Some((workflow_net_id, _)) = grant_id.split_once(':') else {
        return Ok(None);
    };
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM workflow_instances WHERE net_id = $1")
            .bind(workflow_net_id)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(id,)| id))
}

async fn upsert_rows(db: &PgPool, rows: &[AllocationRow]) -> Result<(), sqlx::Error> {
    for row in rows {
        let instance_id = resolve_instance_id(db, &row.grant_id).await?;
        let cluster_resource_id = row
            .cluster_resource_id
            .as_deref()
            .and_then(|s| Uuid::parse_str(s).ok());

        sqlx::query(
            r#"
            INSERT INTO allocations (
                kind, net_id, instance_id, node_id, grant_id,
                cluster_resource_id, scheduler_flavor, alloc_id, node,
                executor_namespace, status,
                requested_at, acquired_at, released_at, expiry,
                exit_code, queue_wait_ms, elapsed_ms,
                cpu_seconds, gpu_seconds, peak_rss_bytes,
                requested_tres, allocated_tres, last_error, last_sequence
            ) VALUES (
                $1, $2, $3, $4, $5,
                $6, $7, $8, $9,
                $10, $11,
                $12, $13, $14, $15,
                $16, $17, $18,
                $19, $20, $21,
                $22, $23, $24, $25
            )
            ON CONFLICT (net_id, grant_id, kind) DO UPDATE SET
                instance_id        = COALESCE(EXCLUDED.instance_id, allocations.instance_id),
                node_id            = COALESCE(EXCLUDED.node_id, allocations.node_id),
                cluster_resource_id= COALESCE(EXCLUDED.cluster_resource_id, allocations.cluster_resource_id),
                scheduler_flavor   = COALESCE(EXCLUDED.scheduler_flavor, allocations.scheduler_flavor),
                alloc_id           = COALESCE(EXCLUDED.alloc_id, allocations.alloc_id),
                node               = COALESCE(EXCLUDED.node, allocations.node),
                executor_namespace = COALESCE(EXCLUDED.executor_namespace, allocations.executor_namespace),
                status             = EXCLUDED.status,
                requested_at       = COALESCE(EXCLUDED.requested_at, allocations.requested_at),
                acquired_at        = COALESCE(EXCLUDED.acquired_at, allocations.acquired_at),
                released_at        = COALESCE(EXCLUDED.released_at, allocations.released_at),
                expiry             = COALESCE(EXCLUDED.expiry, allocations.expiry),
                exit_code          = COALESCE(EXCLUDED.exit_code, allocations.exit_code),
                queue_wait_ms      = COALESCE(EXCLUDED.queue_wait_ms, allocations.queue_wait_ms),
                elapsed_ms         = COALESCE(EXCLUDED.elapsed_ms, allocations.elapsed_ms),
                cpu_seconds        = COALESCE(EXCLUDED.cpu_seconds, allocations.cpu_seconds),
                gpu_seconds        = COALESCE(EXCLUDED.gpu_seconds, allocations.gpu_seconds),
                peak_rss_bytes     = COALESCE(EXCLUDED.peak_rss_bytes, allocations.peak_rss_bytes),
                requested_tres     = COALESCE(EXCLUDED.requested_tres, allocations.requested_tres),
                allocated_tres     = COALESCE(EXCLUDED.allocated_tres, allocations.allocated_tres),
                last_error         = COALESCE(EXCLUDED.last_error, allocations.last_error),
                last_sequence      = EXCLUDED.last_sequence
            WHERE allocations.last_sequence <= EXCLUDED.last_sequence
            "#,
        )
        .bind(row.kind.wire_str())
        .bind(&row.net_id)
        .bind(instance_id)
        .bind(row.node_id.as_deref())
        .bind(&row.grant_id)
        .bind(cluster_resource_id)
        .bind(row.scheduler_flavor.as_deref())
        .bind(row.alloc_id.as_deref())
        .bind(row.node.as_deref())
        .bind(row.executor_namespace.as_deref())
        .bind(row.status.wire_str())
        .bind(row.requested_at)
        .bind(row.acquired_at)
        .bind(row.released_at)
        .bind(row.expiry)
        .bind(row.exit_code)
        .bind(row.queue_wait_ms)
        .bind(row.elapsed_ms)
        .bind(row.cpu_seconds)
        .bind(row.gpu_seconds)
        .bind(row.peak_rss_bytes)
        .bind(row.requested_tres.as_ref())
        .bind(row.allocated_tres.as_ref())
        .bind(row.last_error.as_deref())
        .bind(row.last_sequence as i64)
        .execute(db)
        .await?;
    }
    Ok(())
}
