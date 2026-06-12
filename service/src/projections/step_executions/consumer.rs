//! NATS-driven step-executions consumer.
//!
//! A [`crate::projections::framework::Projection`] with the default
//! [`BootstrapPolicy::ReplayHistory`](crate::projections::framework::BootstrapPolicy):
//! the per-net fold carries state the DB rows can't reproduce (entry-token
//! dedup sets, per-node iteration counters), so a cache miss bootstraps from
//! the net's full event log and subsequent events fold incrementally. Each
//! absorb is followed by the projector's two terminalization passes
//! (`close_open_rows` / `finalize_unreached`, both self-gated on the
//! terminal lifecycle event — exactly the tail of `project_step_executions`),
//! and only the rows the event actually touched (`take_dirty_rows`) are
//! upserted into `step_execution`.
//!
//! Bootstrap resolves the owning instance/template and decodes the compiler
//! `InterfaceRegistry` from `interface_json`. Nets that aren't workflow
//! instances, or whose template predates `interface_json`, return `Ok(None)`
//! — the event is ACKed and the net deliberately NOT cached, so non-instance
//! nets stay cheap misses rather than holding state.
//!
//! ## Incident tuning (2026-06-10 prod, 84k-message redelivery spiral)
//!
//! Two non-default consumer knobs plus the driver's pull-batch cap, all
//! preserved verbatim from the pre-framework consumer:
//! - `ack_wait: 120s` (default 30s) — processing an event can legitimately
//!   take seconds (bootstrap history fetch + fold + row upserts). With the
//!   default, prefetched messages expired in the client buffer faster than
//!   the loop could drain them: every message was redelivered, the ack floor
//!   froze, and the consumer made ~0 forward progress.
//! - batch cap 16 (the [`DriverTuning`](crate::projections::framework::DriverTuning)
//!   default) — at most a couple minutes of work is ever buffered ahead of
//!   the acks.
//! - `inactive_threshold: 30 days` — if this projection is ever removed (or
//!   the service decommissioned), the server reaps the durable instead of
//!   letting it accumulate pending forever (the fate of the orphaned
//!   `mekhan-{node,model}-replicas` durables).

use std::time::Duration;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use petri_domain::PersistedEvent;

use crate::compiler::InterfaceRegistry;
use crate::nats::subjects::{
    Subjects, EFFECT_COMPLETED_EVENTS_FILTER, EFFECT_FAILED_EVENTS_FILTER,
    NET_LIFECYCLE_EVENTS_FILTER, TOKEN_CREATED_EVENTS_FILTER, TRANSITION_FIRED_EVENTS_FILTER,
};
use crate::nats::{ConsumerSpec, MekhanNats, StreamSource};
use crate::projections::framework::{run_projection, Projection};

use super::projector::{Lookups, State as FoldState, StepExecutionRow, StepStatus};

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

/// Per-net cached state: the (stable) instance/template context, the decoded
/// interface registry, and the incremental fold.
struct NetState {
    ctx: InstanceContext,
    registry: InterfaceRegistry,
    fold: FoldState,
}

struct StepExecutionsProjection;

#[async_trait]
impl Projection for StepExecutionsProjection {
    type State = NetState;

    fn name(&self) -> &'static str {
        "step_executions"
    }

    fn spec(&self, _nats: &MekhanNats) -> ConsumerSpec {
        ConsumerSpec {
            stream: StreamSource::ExistingWithRetry(Subjects::STREAM_GLOBAL),
            durable_base: "mekhan-step-executions-v2",
            filter_subjects: vec![
                TOKEN_CREATED_EVENTS_FILTER.into(),
                TRANSITION_FIRED_EVENTS_FILTER.into(),
                EFFECT_COMPLETED_EVENTS_FILTER.into(),
                EFFECT_FAILED_EVENTS_FILTER.into(),
                NET_LIFECYCLE_EVENTS_FILTER.into(),
            ],
            ack_wait: Some(Duration::from_secs(120)),
            inactive_threshold: Some(Duration::from_secs(30 * 24 * 60 * 60)),
            migrate_from: Some("mekhan-step-executions"),
        }
    }

    async fn bootstrap(
        &self,
        db: &PgPool,
        net_id: &str,
        history: &[PersistedEvent],
    ) -> anyhow::Result<Option<NetState>> {
        let Some(ctx) = load_instance_context(db, net_id).await? else {
            return Ok(None);
        };
        let registry: InterfaceRegistry = match serde_json::from_value(ctx.interface_json.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(
                    net_id = %net_id,
                    template_id = %ctx.template_id,
                    "step-executions: cannot decode interface_json — skipping: {e}",
                );
                return Ok(None);
            }
        };

        let mut fold = FoldState::new();
        {
            let lookups = Lookups::build(&registry);
            for ev in history {
                fold.absorb(ev, &lookups);
            }
        }
        // Same tail as `project_step_executions`: both passes self-gate on a
        // terminal lifecycle event having been folded.
        fold.close_open_rows();
        fold.finalize_unreached(&registry);

        let rows = fold.take_dirty_rows();
        if !rows.is_empty() {
            upsert_rows(db, &ctx, &rows).await?;
        }
        Ok(Some(NetState {
            ctx,
            registry,
            fold,
        }))
    }

    async fn apply(
        &self,
        db: &PgPool,
        _net_id: &str,
        state: &mut NetState,
        ev: &PersistedEvent,
    ) -> anyhow::Result<()> {
        {
            let lookups = Lookups::build(&state.registry);
            state.fold.absorb(ev, &lookups);
        }
        state.fold.close_open_rows();
        state.fold.finalize_unreached(&state.registry);

        let rows = state.fold.take_dirty_rows();
        if !rows.is_empty() {
            upsert_rows(db, &state.ctx, &rows).await?;
        }
        Ok(())
    }
}

/// Start the step-executions ingest consumer. Spawned alongside the lifecycle
/// and causality consumers in `main.rs`. Runs until the message stream ends or
/// the consumer is dropped (process shutdown).
pub async fn start_step_executions_ingest(nats: MekhanNats, db: PgPool) {
    run_projection(StepExecutionsProjection, nats, db).await;
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
                execution_id, created_at, updated_at
            ) VALUES (
                $1, $2, $3,
                $4, $5, $6,
                $7, $8, $9, $10,
                $11, $12, $13, $14,
                $15, NOW(), NOW()
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
                execution_id = EXCLUDED.execution_id,
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
        .bind(row.execution_id.as_deref())
        .execute(db)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use petri_domain::{DomainEvent, PlaceId, Token, TokenColor, TransitionId};

    use super::*;
    use crate::projections::framework::subject_matches;

    fn filters() -> Vec<String> {
        vec![
            TOKEN_CREATED_EVENTS_FILTER.into(),
            TRANSITION_FIRED_EVENTS_FILTER.into(),
            EFFECT_COMPLETED_EVENTS_FILTER.into(),
            EFFECT_FAILED_EVENTS_FILTER.into(),
            NET_LIFECYCLE_EVENTS_FILTER.into(),
        ]
    }

    /// Every `DomainEvent` variant the step-executions fold matches must be
    /// covered by the durable's server-side filter list, or the projection
    /// would silently stop seeing its own input.
    #[test]
    fn filter_list_covers_every_projected_variant() {
        let matched_variants = [
            DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Unit),
                place_id: PlaceId("p_entry".into()),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            },
            DomainEvent::TransitionFired {
                transition_id: TransitionId("t_park".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            DomainEvent::EffectCompleted {
                transition_id: TransitionId("t_step".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "executor_submit".into(),
                effect_result: serde_json::json!({}),
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            DomainEvent::EffectFailed {
                transition_id: TransitionId("t_step".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "executor_submit".into(),
                error_message: "boom".into(),
                tokens_consumed: true,
                input_data: None,
                retryable: false,
            },
            DomainEvent::NetCompleted {
                net_id: "mekhan-x".into(),
                terminal_place_id: "p_end".into(),
                exit_code: None,
            },
            DomainEvent::NetCancelled {
                net_id: "mekhan-x".into(),
                reason: None,
                cancelled_by: None,
            },
            DomainEvent::NetFailed {
                net_id: "mekhan-x".into(),
                transition_id: TransitionId("t_x".into()),
                reason: "boom".into(),
                retryable: false,
            },
        ];

        for event in matched_variants {
            let subject = Subjects::for_event(&event, Some("mekhan-x"));
            assert!(
                filters().iter().any(|f| subject_matches(f, &subject)),
                "no filter matches {subject}"
            );
        }
    }
}
