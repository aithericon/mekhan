//! NATS-driven allocations consumer.
//!
//! A [`crate::projections::framework::Projection`] with the default
//! [`BootstrapPolicy::ReplayHistory`](crate::projections::framework::BootstrapPolicy):
//! the per-net [`State`] fold is incremental (lease releases correlate by
//! `alloc_id` against earlier acquires, so per-event state must be carried),
//! bootstrapped from the full event log on a cache miss and then fed one
//! event at a time. Only the rows an event actually touched
//! ([`State::take_dirty_rows`]) are upserted into `allocations`
//! (sequence-guarded by `(net_id, grant_id, kind)`).
//!
//! The durable consumer (`mekhan-allocations-v2`, cursor transplanted from
//! the old `petri.events.>` firehose durable) filters server-side to exactly
//! the subjects the fold matches — `effect.completed`, `token.created`,
//! `transition.fired` — plus `net.>` for the driver's terminal eviction.
//!
//! Unlike the step-executions projection it needs NO template
//! `InterfaceRegistry` — the projector derives every key from the engine
//! events. It DOES resolve the owning workflow `instance_id` (UUID) from the
//! workflow net embedded in the grant_id prefix (`<workflow_net_id>:<node_id>`),
//! best-effort: pool-management nets / unknown grants resolve to NULL
//! `instance_id`, which is allowed.
//!
//! ## Tapping the accounting signal
//!
//! The enriched terminal accounting payload the per-cluster watcher publishes on
//! `petri.signal.{net_id}.>` lands in the SAME PETRI_GLOBAL event log as a
//! `TokenCreated` (the engine injects the signal payload as the token color,
//! tagging `signal_key == grant_id`). So the `token.created` filter sees it —
//! no second `petri.signal.>` consumer is required. If a future engine
//! change ever slims the persisted signal token below what the projector needs,
//! add a second pull consumer here filtered on `petri.signal.>` and merge its
//! `ExternalSignal.payload` into the same upsert path.

use std::time::Duration;

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use petri_domain::PersistedEvent;

use crate::nats::subjects::{
    Subjects, EFFECT_COMPLETED_EVENTS_FILTER, NET_LIFECYCLE_EVENTS_FILTER,
    TOKEN_CREATED_EVENTS_FILTER, TRANSITION_FIRED_EVENTS_FILTER,
};
use crate::nats::{ConsumerSpec, MekhanNats, StreamSource};
use crate::projections::framework::{run_projection, Projection};

use super::projector::{AllocationRow, State};

struct AllocationsProjection;

#[async_trait]
impl Projection for AllocationsProjection {
    type State = State;

    fn name(&self) -> &'static str {
        "allocations"
    }

    fn spec(&self, _nats: &MekhanNats) -> ConsumerSpec {
        ConsumerSpec {
            stream: StreamSource::ExistingWithRetry(Subjects::STREAM_GLOBAL),
            durable_base: "mekhan-allocations-v2",
            filter_subjects: vec![
                EFFECT_COMPLETED_EVENTS_FILTER.into(),
                TOKEN_CREATED_EVENTS_FILTER.into(),
                TRANSITION_FIRED_EVENTS_FILTER.into(),
                NET_LIFECYCLE_EVENTS_FILTER.into(),
            ],
            ack_wait: Some(Duration::from_secs(120)),
            // Reap the durable if this projection is ever removed (see the
            // step-executions projection spec for the incident rationale).
            inactive_threshold: Some(Duration::from_secs(30 * 24 * 60 * 60)),
            migrate_from: Some("mekhan-allocations"),
        }
    }

    async fn bootstrap(
        &self,
        db: &PgPool,
        net_id: &str,
        history: &[PersistedEvent],
    ) -> anyhow::Result<Option<State>> {
        let mut state = State::default();
        for ev in history {
            state.absorb(ev, net_id);
        }
        let rows = state.take_dirty_rows();
        if !rows.is_empty() {
            upsert_rows(db, &rows).await?;
        }
        Ok(Some(state))
    }

    async fn apply(
        &self,
        db: &PgPool,
        net_id: &str,
        state: &mut State,
        ev: &PersistedEvent,
    ) -> anyhow::Result<()> {
        state.absorb(ev, net_id);
        let rows = state.take_dirty_rows();
        if !rows.is_empty() {
            upsert_rows(db, &rows).await?;
        }
        Ok(())
    }
}

/// Start the allocations ingest consumer. Spawned alongside the step-executions
/// and causality consumers in `main.rs`. Runs until the message stream ends.
pub async fn start_allocations_ingest(nats: MekhanNats, db: PgPool) {
    run_projection(AllocationsProjection, nats, db).await;
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

#[cfg(test)]
mod tests {
    use petri_domain::{DomainEvent, PlaceId, Token, TokenColor, TransitionId};

    use super::*;
    use crate::projections::framework::subject_matches;

    fn filters() -> Vec<String> {
        vec![
            EFFECT_COMPLETED_EVENTS_FILTER.into(),
            TOKEN_CREATED_EVENTS_FILTER.into(),
            TRANSITION_FIRED_EVENTS_FILTER.into(),
            NET_LIFECYCLE_EVENTS_FILTER.into(),
        ]
    }

    /// Every `DomainEvent` variant the allocations fold matches — plus the
    /// terminal lifecycle events the driver needs for cache eviction — must
    /// be covered by the durable's server-side filter list, or the projection
    /// would silently stop seeing its own input.
    #[test]
    fn filter_list_covers_every_projected_variant() {
        let matched_variants = [
            DomainEvent::EffectCompleted {
                transition_id: TransitionId("t_acquire".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "resource_lease_acquire".into(),
                effect_result: serde_json::json!({}),
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Data(serde_json::json!({}))),
                place_id: PlaceId("p_sig".into()),
                place_name: None,
                workflow_id: None,
                signal_key: Some("grant".into()),
                dedup_id: None,
            },
            DomainEvent::TransitionFired {
                transition_id: TransitionId("t_grant".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            DomainEvent::NetCompleted {
                net_id: "pool-x".into(),
                terminal_place_id: "p_end".into(),
                exit_code: None,
            },
            DomainEvent::NetCancelled {
                net_id: "pool-x".into(),
                reason: None,
                cancelled_by: None,
            },
            DomainEvent::NetFailed {
                net_id: "pool-x".into(),
                transition_id: TransitionId("t_x".into()),
                reason: "boom".into(),
                retryable: false,
            },
        ];

        for event in matched_variants {
            let subject = Subjects::for_event(&event, Some("pool-x"));
            assert!(
                filters().iter().any(|f| subject_matches(f, &subject)),
                "no filter matches {subject}"
            );
        }
    }
}
