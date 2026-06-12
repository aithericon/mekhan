//! NATS-driven `image_materializations` consumer (docs/22 container staging).
//!
//! A [`crate::projections::framework::Projection`] with
//! [`BootstrapPolicy::Stateless`] — the direct sibling of
//! `template_stagings::consumer`: a materialize net fires `materialize_image`
//! exactly once, and each matching `EffectCompleted`/`EffectFailed` event
//! independently yields a self-contained row update, applied as an idempotent
//! `UPDATE … WHERE id = $1` (`ready`/`failed`, `digest`, `sif_path`,
//! `size_bytes`, `last_error`). The durable consumer
//! (`mekhan-image-materializations-v2`, cursor transplanted from the old
//! firehose durable) filters server-side to the two effect-event subjects and
//! pre-filters in-process to `materialize-*` nets.

use std::time::Duration;

use async_trait::async_trait;
use sqlx::PgPool;

use petri_domain::PersistedEvent;

use crate::nats::subjects::{
    Subjects, EFFECT_COMPLETED_EVENTS_FILTER, EFFECT_FAILED_EVENTS_FILTER,
};
use crate::nats::{ConsumerSpec, MekhanNats, StreamSource};
use crate::projections::framework::{run_projection, BootstrapPolicy, Projection};

use super::projector::{project_materialize, MaterializeUpdate};

struct ImageMaterializationsProjection;

#[async_trait]
impl Projection for ImageMaterializationsProjection {
    type State = ();

    fn name(&self) -> &'static str {
        "image_materializations"
    }

    fn spec(&self, _nats: &MekhanNats) -> ConsumerSpec {
        ConsumerSpec {
            stream: StreamSource::ExistingWithRetry(Subjects::STREAM_GLOBAL),
            durable_base: "mekhan-image-materializations-v2",
            filter_subjects: vec![
                EFFECT_COMPLETED_EVENTS_FILTER.into(),
                EFFECT_FAILED_EVENTS_FILTER.into(),
            ],
            ack_wait: Some(Duration::from_secs(120)),
            // Reap the durable if this projection is ever removed (see the
            // step-executions projection spec for the incident rationale).
            inactive_threshold: Some(Duration::from_secs(30 * 24 * 60 * 60)),
            migrate_from: Some("mekhan-image-materializations"),
        }
    }

    fn wants_net(&self, net_id: &str) -> bool {
        net_id.starts_with("materialize-")
    }

    fn bootstrap_policy(&self) -> BootstrapPolicy {
        BootstrapPolicy::Stateless
    }

    async fn bootstrap(
        &self,
        _db: &PgPool,
        _net_id: &str,
        _history: &[PersistedEvent],
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
        if let Some(update) = project_materialize(std::slice::from_ref(ev), net_id) {
            apply_update(db, &update).await?;
        }
        Ok(())
    }
}

/// Start the image-materializations ingest consumer. Spawned alongside the other
/// projection consumers in `main.rs`. Runs until the message stream ends.
pub async fn start_image_materializations_ingest(nats: MekhanNats, db: PgPool) {
    run_projection(ImageMaterializationsProjection, nats, db).await;
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

    /// Every `DomainEvent` variant `project_materialize` matches must be
    /// covered by the durable's server-side filter list, or the projection
    /// would silently stop seeing its own input.
    #[test]
    fn filter_list_covers_every_projected_variant() {
        let matched_variants = [
            DomainEvent::EffectCompleted {
                transition_id: TransitionId("t_materialize".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "materialize_image".into(),
                effect_result: serde_json::json!({}),
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            DomainEvent::EffectFailed {
                transition_id: TransitionId("t_materialize".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "materialize_image".into(),
                error_message: "boom".into(),
                tokens_consumed: true,
                input_data: None,
                retryable: false,
            },
        ];

        for event in matched_variants {
            let subject = Subjects::for_event(&event, Some("materialize-x"));
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
                transition_id: TransitionId("t_materialize".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "materialize_image".into(),
                error_message: "missing image_ref".into(),
                tokens_consumed: true,
                input_data: None,
                retryable: false,
            },
            hash: String::new(),
            previous_hash: None,
        };
        let net = "materialize-44444444-4444-4444-4444-444444444444";
        let update = project_materialize(std::slice::from_ref(&ev), net).expect("update");
        assert_eq!(update.status, "failed");
    }
}
