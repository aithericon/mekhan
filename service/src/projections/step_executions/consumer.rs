//! NATS-driven step-executions consumer.
//!
//! Subscribes to `petri.events.>` on PETRI_GLOBAL with the durable consumer
//! `mekhan-step-executions`. On each event, resolves the owning instance and
//! template, re-fetches that net's full event log from JetStream, runs the
//! pure projector, and upserts changed step rows into `step_execution`.
//!
//! Modeled on `service/src/causality/ingest.rs`. Single-task pull loop; ACK
//! on success, NAK on error (causality retries every 2s on failure — we
//! match that).
//!
//! ## Re-fetching events per arrival
//!
//! The simplest correct strategy: on each event, ask JetStream for the full
//! event log via `fetch_events` (same path used by `GET /instances/{id}/state`
//! to project the marking on demand). The projector is pure and the upsert is
//! by PK — duplicate work is invisible. At workflow volumes here this is
//! cheap; if it ever isn't, the obvious optimization is an in-memory per-net
//! buffer that backfills lazily on cache miss.

use std::collections::HashMap;

use sqlx::PgPool;

use futures::StreamExt;
use petri_domain::{DomainEvent, PersistedEvent};
use uuid::Uuid;

use crate::compiler::InterfaceRegistry;
use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;
use crate::petri::events::fetch_events;

use super::projector::{project_step_executions, StepExecutionRow, StepStatus};

/// The bare `snake_case` wire string for the `step_execution.status` text
/// column. `StepStatus` is now an alias for the canonical
/// `aithericon_executor_domain::PhaseStatus`, which has no inherent `as_str()`;
/// this match reproduces the prior projection's column values verbatim
/// (`"pending"`/`"running"`/`"completed"`/`"failed"`/`"skipped"`).
fn step_status_str(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Completed => "completed",
        StepStatus::Failed => "failed",
        StepStatus::Skipped => "skipped",
    }
}

/// Upper bound on simultaneously-buffered nets. Terminal nets are evicted
/// eagerly (see `process_event`); this only guards against unbounded growth
/// from many long-lived / never-terminating nets. An evicted net re-bootstraps
/// from `fetch_events` on its next event.
const MAX_BUFFERED_NETS: usize = 512;

/// Per-net in-memory projection input: the full event log for one net plus the
/// (stable) instance/template context and decoded interface registry.
///
/// The previous design re-fetched the entire net history from NATS — a fresh
/// ephemeral consumer with a blocking read timeout — on EVERY delivered
/// message, which could not keep up under load. Instead we bootstrap the log
/// once on cache miss, then append each subsequently-delivered event and
/// re-fold from memory.
struct NetBuffer {
    ctx: InstanceContext,
    registry: InterfaceRegistry,
    events: Vec<PersistedEvent>,
}

/// Start the step-executions ingest consumer. Spawned alongside the lifecycle
/// and causality consumers in `main.rs`. Runs until the message stream ends or
/// the consumer is dropped (process shutdown).
pub async fn start_step_executions_ingest(nats: MekhanNats, db: PgPool) {
    let consumer = match nats.step_executions_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create step-executions consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start step-executions message stream: {e}");
            return;
        }
    };

    tracing::info!("step-executions ingest started on petri.events.>");

    // Per-net event buffers, backfilled lazily on cache miss. Avoids re-reading
    // the whole net history from NATS on every message.
    let mut buffers: HashMap<String, NetBuffer> = HashMap::new();

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("step-executions ingest message error: {e}");
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
                tracing::error!(subject = %subject, "step-executions processing failed: {e}");
                let _ = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        std::time::Duration::from_secs(2),
                    )))
                    .await;
            }
        }
    }

    tracing::warn!("step-executions ingest stream ended");
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
        tracing::warn!("step-executions: cannot extract net_id from subject: {subject}");
        return Ok(());
    };

    let incoming: PersistedEvent = match serde_json::from_slice(payload) {
        Ok(p) => p,
        Err(e) => {
            record_silent_drop_with(
                "step_executions_envelope",
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
        // Cache miss: first event seen for this net since startup. Resolve the
        // owning instance/template + bootstrap the FULL event log once (covers
        // events that predate this process / the durable cursor). Nets that
        // aren't workflow instances, or whose template predates interface_json,
        // are skipped — and deliberately NOT cached, so non-instance nets stay
        // cheap misses rather than holding state.
        let Some(ctx) = load_instance_context(db, net_id).await? else {
            return Ok(());
        };
        let registry: InterfaceRegistry = match serde_json::from_value(ctx.interface_json.clone())
        {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    net_id = %net_id,
                    template_id = %ctx.template_id,
                    "step-executions: cannot decode interface_json — skipping: {e}",
                );
                return Ok(());
            }
        };
        let events = fetch_events(nats, net_id).await?;

        // Bound memory: if a flood of long-lived nets has accumulated, evict
        // one (it re-bootstraps on its next event).
        if buffers.len() >= MAX_BUFFERED_NETS {
            if let Some(victim) = buffers.keys().next().cloned() {
                buffers.remove(&victim);
            }
        }
        buffers.insert(
            net_id.to_string(),
            NetBuffer {
                ctx,
                registry,
                events,
            },
        );
    } else {
        // Cache hit: append the freshly-delivered event. The bootstrap fetch may
        // already include it (timing), so dedupe by sequence; keep the buffer
        // sequence-ordered for a correct re-fold.
        let buf = buffers.get_mut(net_id).expect("contains_key checked");
        if !buf.events.iter().any(|e| e.sequence == incoming.sequence) {
            buf.events.push(incoming);
            buf.events.sort_by_key(|e| e.sequence);
        }
    }

    let buf = buffers.get(net_id).expect("inserted/hit above");
    if !buf.events.is_empty() {
        // Pure fold over the in-memory buffer; idempotent upsert keys on PK.
        let rows = project_step_executions(&buf.events, &buf.registry);
        upsert_rows(db, &buf.ctx, &rows).await?;
    }

    // Free the buffer once the net is done — its rows are now final.
    if is_terminal {
        buffers.remove(net_id);
    }
    Ok(())
}

struct InstanceContext {
    instance_id: Uuid,
    template_id: Uuid,
    template_version: i32,
    interface_json: serde_json::Value,
}

async fn load_instance_context(
    db: &PgPool,
    net_id: &str,
) -> Result<Option<InstanceContext>, sqlx::Error> {
    let row: Option<(Uuid, Uuid, i32, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT wi.id, wi.template_id, wi.template_version, wt.interface_json \
         FROM workflow_instances wi \
         JOIN workflow_templates wt \
             ON wt.id = wi.template_id AND wt.version = wi.template_version \
         WHERE wi.net_id = $1",
    )
    .bind(net_id)
    .fetch_optional(db)
    .await?;

    let Some((instance_id, template_id, template_version, interface_json)) = row else {
        return Ok(None);
    };

    let Some(interface_json) = interface_json else {
        tracing::debug!(
            net_id = %net_id,
            template_id = %template_id,
            "step-executions: template has no interface_json — skipping",
        );
        return Ok(None);
    };

    Ok(Some(InstanceContext {
        instance_id,
        template_id,
        template_version,
        interface_json,
    }))
}

async fn upsert_rows(
    db: &PgPool,
    ctx: &InstanceContext,
    rows: &[StepExecutionRow],
) -> Result<(), sqlx::Error> {
    for row in rows {
        sqlx::query(
            r#"
            INSERT INTO step_execution (
                instance_id, node_id, iteration_index,
                template_id, template_version, node_kind,
                status, inputs, outputs, branch_taken,
                started_at, completed_at, error, last_sequence,
                created_at, updated_at
            ) VALUES (
                $1, $2, $3,
                $4, $5, $6,
                $7, $8, $9, $10,
                $11, $12, $13, $14,
                NOW(), NOW()
            )
            ON CONFLICT (instance_id, node_id, iteration_index) DO UPDATE SET
                status = EXCLUDED.status,
                inputs = EXCLUDED.inputs,
                outputs = EXCLUDED.outputs,
                branch_taken = EXCLUDED.branch_taken,
                started_at = EXCLUDED.started_at,
                completed_at = EXCLUDED.completed_at,
                error = EXCLUDED.error,
                last_sequence = EXCLUDED.last_sequence,
                updated_at = NOW()
            WHERE step_execution.last_sequence <= EXCLUDED.last_sequence
            "#,
        )
        .bind(ctx.instance_id)
        .bind(&row.node_id)
        .bind(row.iteration_index)
        .bind(ctx.template_id)
        .bind(ctx.template_version)
        .bind(row.node_kind.wire_str())
        .bind(step_status_str(row.status))
        .bind(row.inputs.as_ref())
        .bind(row.outputs.as_ref())
        .bind(row.branch_taken.as_deref())
        .bind(row.started_at)
        .bind(row.completed_at)
        .bind(row.error.as_ref())
        .bind(row.last_sequence as i64)
        .execute(db)
        .await?;
    }
    Ok(())
}

