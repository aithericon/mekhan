//! NATS-driven `template_stagings` consumer (B-staging, Phase 4).
//!
//! A [`crate::projections::framework::Projection`] with
//! [`BootstrapPolicy::Stateless`]: a staging net fires `stage_template`
//! exactly once, and each matching `EffectCompleted`/`EffectFailed` event
//! independently yields a self-contained row update — so there is no per-net
//! buffer, no history fetch, and no terminal tracking. The durable consumer
//! (`mekhan-template-stagings-v2`, cursor transplanted from the old
//! firehose durable) filters server-side to the two effect-event subjects
//! and pre-filters in-process to `staging-*` nets.
//!
//! The update always sets a TERMINAL status (the projector never emits
//! `staging`), and the net id (`staging-<row-id>`) targets exactly one row,
//! so the `UPDATE … WHERE id = $1` is naturally idempotent under replay.

use std::time::Duration;

use async_trait::async_trait;
use sqlx::PgPool;

use petri_domain::PersistedEvent;

use crate::nats::subjects::{
    Subjects, EFFECT_COMPLETED_EVENTS_FILTER, EFFECT_FAILED_EVENTS_FILTER,
};
use crate::nats::{ConsumerSpec, MekhanNats, StreamSource};
use crate::projections::framework::{run_projection, BootstrapPolicy, LazyHistory, Projection};

use super::projector::{project_staging, StagingUpdate};

struct TemplateStagingsProjection;

#[async_trait]
impl Projection for TemplateStagingsProjection {
    type State = ();

    fn name(&self) -> &'static str {
        "template_stagings"
    }

    fn spec(&self, _nats: &MekhanNats) -> ConsumerSpec {
        ConsumerSpec {
            stream: StreamSource::ExistingWithRetry(Subjects::STREAM_GLOBAL),
            durable_base: "mekhan-template-stagings-v2",
            filter_subjects: vec![
                EFFECT_COMPLETED_EVENTS_FILTER.into(),
                EFFECT_FAILED_EVENTS_FILTER.into(),
            ],
            ack_wait: Some(Duration::from_secs(120)),
            // Reap the durable if this projection is ever removed (see the
            // step-executions projection spec for the incident rationale).
            inactive_threshold: Some(Duration::from_secs(30 * 24 * 60 * 60)),
            migrate_from: Some("mekhan-template-stagings"),
        }
    }

    fn wants_net(&self, net_id: &str) -> bool {
        net_id.starts_with("staging-")
    }

    fn bootstrap_policy(&self) -> BootstrapPolicy {
        BootstrapPolicy::Stateless
    }

    async fn bootstrap(
        &self,
        _db: &PgPool,
        _net_id: &str,
        _history: &LazyHistory<'_>,
    ) -> anyhow::Result<Option<()>> {
        Ok(None)
    }

    async fn apply(
        &self,
        _db: &PgPool,
        _net_id: &str,
        _state: &mut (),
        _ev: &PersistedEvent,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn apply_stateless(
        &self,
        db: &PgPool,
        net_id: &str,
        ev: &PersistedEvent,
    ) -> anyhow::Result<()> {
        if let Some(update) = project_staging(std::slice::from_ref(ev), net_id) {
            apply_update(db, &update).await?;
        }
        Ok(())
    }
}

/// Start the template-stagings ingest consumer. Spawned alongside the other
/// projection consumers in `main.rs`. Runs until the message stream ends.
pub async fn start_template_stagings_ingest(nats: MekhanNats, db: PgPool) {
    run_projection(TemplateStagingsProjection, nats, db).await;
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use petri_domain::{DomainEvent, TransitionId};

    use super::*;
    use crate::projections::framework::subject_matches;

    fn filters() -> Vec<String> {
        vec![
            EFFECT_COMPLETED_EVENTS_FILTER.into(),
            EFFECT_FAILED_EVENTS_FILTER.into(),
        ]
    }

    /// Every `DomainEvent` variant `project_staging` matches must be covered
    /// by the durable's server-side filter list, or the projection would
    /// silently stop seeing its own input.
    #[test]
    fn filter_list_covers_every_projected_variant() {
        let matched_variants = [
            DomainEvent::EffectCompleted {
                transition_id: TransitionId("t_stage".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "stage_template".into(),
                effect_result: serde_json::json!({}),
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            DomainEvent::EffectFailed {
                transition_id: TransitionId("t_stage".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "stage_template".into(),
                error_message: "boom".into(),
                tokens_consumed: true,
                input_data: None,
                retryable: false,
            },
        ];

        for event in matched_variants {
            let subject = Subjects::for_event(&event, Some("staging-x"));
            assert!(
                filters().iter().any(|f| subject_matches(f, &subject)),
                "no filter matches {subject}"
            );
        }
    }

    /// The pure projector must actually fire on a single matching event —
    /// the stateless path hands it `&[ev]`, never a buffered log.
    #[test]
    fn single_event_projects_standalone() {
        let ev = PersistedEvent {
            sequence: 7,
            timestamp: Utc::now(),
            event: DomainEvent::EffectFailed {
                transition_id: TransitionId("t_stage".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "stage_template".into(),
                error_message: "missing request.slug".into(),
                tokens_consumed: true,
                input_data: None,
                retryable: false,
            },
            hash: String::new(),
            previous_hash: None,
        };
        let net = "staging-33333333-3333-3333-3333-333333333333";
        let update = project_staging(std::slice::from_ref(&ev), net).expect("update");
        assert_eq!(update.status, "failed");
    }
}
