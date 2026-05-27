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

use sqlx::PgPool;

use futures::StreamExt;
use petri_domain::PersistedEvent;
use uuid::Uuid;

use crate::compiler::InterfaceRegistry;
use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;
use crate::petri::events::fetch_events;

use super::projector::{project_step_executions, StepExecutionRow};

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

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("step-executions ingest message error: {e}");
                continue;
            }
        };

        let subject = msg.subject.as_str();
        let result = process_event(&nats, &db, subject, &msg.payload).await;

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
    subject: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    // Subject: petri.events.{net_id}.>
    let Some(net_id) = subject.split('.').nth(2) else {
        tracing::warn!("step-executions: cannot extract net_id from subject: {subject}");
        return Ok(());
    };

    // Parse only to validate the wire shape; we re-fetch the full log below.
    let _envelope: PersistedEvent = match serde_json::from_slice(payload) {
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

    // Look up the owning instance + template (skip events for nets that
    // aren't workflow instances — e.g. demo nets started outside the
    // service).
    let Some(ctx) = load_instance_context(db, net_id).await? else {
        return Ok(());
    };

    // Re-fetch the full event log and project. The projector is pure;
    // duplicate replay is harmless since upsert keys on PK.
    let events = fetch_events(nats, net_id).await?;
    if events.is_empty() {
        return Ok(());
    }

    let registry: InterfaceRegistry = match serde_json::from_value(ctx.interface_json.clone()) {
        Ok(r) => r,
        Err(e) => {
            // Pre-prototype templates (interface_json missing or in an old
            // shape) just skip — there's no attribution path without the
            // registry. The instance view falls back to the raw petri canvas.
            tracing::debug!(
                net_id = %net_id,
                template_id = %ctx.template_id,
                "step-executions: cannot decode interface_json — skipping: {e}",
            );
            return Ok(());
        }
    };

    let rows = project_step_executions(&events, &registry);
    upsert_rows(db, &ctx, &rows).await?;
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
        .bind(node_kind_to_str(row.node_kind))
        .bind(row.status.as_str())
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

fn node_kind_to_str(kind: crate::compiler::NodeKind) -> &'static str {
    match kind {
        crate::compiler::NodeKind::Start => "start",
        crate::compiler::NodeKind::End => "end",
        crate::compiler::NodeKind::HumanTask => "human_task",
        crate::compiler::NodeKind::AutomatedStep => "automated_step",
        crate::compiler::NodeKind::Decision => "decision",
        crate::compiler::NodeKind::Loop => "loop",
        crate::compiler::NodeKind::ParallelSplit => "parallel_split",
        crate::compiler::NodeKind::ParallelJoin => "parallel_join",
        crate::compiler::NodeKind::Join => "join",
        crate::compiler::NodeKind::Scope => "scope",
        crate::compiler::NodeKind::SubWorkflow => "sub_workflow",
        crate::compiler::NodeKind::PhaseUpdate => "phase_update",
        crate::compiler::NodeKind::ProgressUpdate => "progress_update",
        crate::compiler::NodeKind::Failure => "failure",
        crate::compiler::NodeKind::Trigger => "trigger",
    }
}
