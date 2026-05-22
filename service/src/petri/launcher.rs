//! Instance-launch seam.
//!
//! `handlers::instances::create_instance` (user POST) and
//! `triggers::dispatcher::fire_spawn` (a Spawn trigger firing) both ran the
//! identical sequence: parameterize the template's AIR, INSERT the
//! `workflow_instances` row *before* deploying (so the lifecycle listener can
//! find it if the net finishes first), deploy to petri-lab, and on a deploy
//! failure DELETE the row so lifecycle never observes a phantom.
//!
//! That ordering — and especially the rollback-on-deploy-failure invariant —
//! lived twice, once in an HTTP handler and once in the trigger dispatcher.
//! The dispatcher additionally reached directly into the `petri::instance`
//! free functions. [`InstanceLauncher`] owns the sequence once; both callers
//! depend on this seam instead of re-implementing it.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::instance::{StartToken, WorkflowInstance};
use crate::models::template::WorkflowGraph;
use crate::petri::client::PetriClient;
use crate::petri::instance::{
    deploy_instance, parameterize_air, parameterize_for_place, ParameterizeError,
    ParameterizeForPlaceError,
};

/// Why a launch failed. Each caller maps these to its own surface:
/// `create_instance` turns [`LaunchError::Parameterize`] into a 400 and
/// [`LaunchError::Deploy`] into a 502; `fire_spawn` folds both into
/// `TriggerError::InstanceFailed`. The launcher itself is surface-agnostic.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// `parameterize_air` rejected the start tokens (missing/unknown/duplicate
    /// start block, wrong field kind, ...). No row was inserted.
    #[error(transparent)]
    Parameterize(#[from] ParameterizeError),

    /// `parameterize_for_place` rejected the pre-AIR direct-place seeding
    /// (place id not found in AIR, or AIR has no `places` array).
    #[error(transparent)]
    ParameterizeForPlace(#[from] ParameterizeForPlaceError),

    /// The instance row could not be inserted. Nothing was deployed.
    #[error("instance row insert failed: {0}")]
    Database(String),

    /// petri-lab deploy failed. The just-inserted row has already been rolled
    /// back so the lifecycle listener never observes a never-deployed
    /// instance.
    #[error("deploy failed: {0}")]
    Deploy(String),
}

/// What the caller wants run.
///
/// Two variants, one per authoring path:
/// - [`LaunchSpec::Templated`] — graph-authored template (`Start` blocks,
///   typed ports, payload-mapping validated at the launcher boundary). The
///   path the visual editor produces; consumed by `create_instance` and
///   by graph-authored triggers in `fire_spawn`.
/// - [`LaunchSpec::PreAir`] — clinic-style headless template. The trigger
///   names an AIR place id directly (no `Start`, no graph-level port
///   shape). Consumed by `fire_spawn` when the trigger record carries
///   `air_target_place_id`. Per
///   `feedback_no_mode_framing_for_the_direction` this is a first-class
///   variant, not an `Option<&WorkflowGraph>` mode-flag on the templated
///   path.
pub enum LaunchSpec<'a> {
    Templated {
        instance_id: Uuid,
        net_id: String,
        template_id: Uuid,
        template_version: i32,
        created_by: Uuid,
        /// Audit-only blob stored on the instance row (not merged into tokens).
        metadata: Value,
        air_json: &'a Value,
        graph: &'a WorkflowGraph,
        start_tokens: &'a [StartToken],
    },
    PreAir {
        instance_id: Uuid,
        net_id: String,
        template_id: Uuid,
        template_version: i32,
        created_by: Uuid,
        metadata: Value,
        air_json: &'a Value,
        /// The AIR place id whose `initial_tokens` will be seeded with the
        /// supplied token + system fields. Resolved at the trigger
        /// boundary from the Trigger node's `air_target_place_id`.
        air_target_place_id: &'a str,
        /// Opaque payload. Clinic AIR transitions consume opaque tokens
        /// (task_kind / required_capabilities / system_prompt live in
        /// `transition.logic.config`); no port-shape validation here.
        token: &'a Value,
    },
}

/// Owns the deploy-an-instance sequence. Behavior-identical to the code that
/// was inlined in `create_instance` and `fire_spawn` — pure relocation, now
/// extended with the pre-AIR variant.
#[derive(Clone, Copy)]
pub struct InstanceLauncher<'a> {
    db: &'a PgPool,
    petri: &'a PetriClient,
}

impl<'a> InstanceLauncher<'a> {
    pub fn new(db: &'a PgPool, petri: &'a PetriClient) -> Self {
        Self { db, petri }
    }

    /// Parameterize, insert the row, deploy, and roll the row back if the
    /// deploy fails. Returns the persisted instance on success.
    ///
    /// Ordering is load-bearing and preserved exactly: the row is inserted
    /// *before* the deploy so the lifecycle listener can find it if the net
    /// completes before this returns; a deploy failure deletes the row before
    /// the error propagates so lifecycle never sees a phantom.
    pub async fn launch(&self, spec: LaunchSpec<'_>) -> Result<WorkflowInstance, LaunchError> {
        // Per-variant: parameterize and capture the row-write inputs in a
        // single tuple so the DB-write / deploy / rollback tail is shared
        // byte-for-byte across both paths (the launcher's load-bearing
        // invariant — see the doc-comment above).
        let (parameterized, instance_id, net_id, template_id, template_version, created_by, metadata) =
            match spec {
                LaunchSpec::Templated {
                    instance_id,
                    net_id,
                    template_id,
                    template_version,
                    created_by,
                    metadata,
                    air_json,
                    graph,
                    start_tokens,
                } => {
                    let parameterized = parameterize_air(
                        air_json,
                        instance_id,
                        template_id,
                        template_version,
                        created_by,
                        graph,
                        start_tokens,
                    )?;
                    (parameterized, instance_id, net_id, template_id, template_version, created_by, metadata)
                }
                LaunchSpec::PreAir {
                    instance_id,
                    net_id,
                    template_id,
                    template_version,
                    created_by,
                    metadata,
                    air_json,
                    air_target_place_id,
                    token,
                } => {
                    let parameterized = parameterize_for_place(
                        air_json,
                        instance_id,
                        template_id,
                        template_version,
                        created_by,
                        air_target_place_id,
                        token,
                    )?;
                    (parameterized, instance_id, net_id, template_id, template_version, created_by, metadata)
                }
            };

        let instance = sqlx::query_as::<_, WorkflowInstance>(
            r#"
            INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, started_at, metadata)
            VALUES ($1, $2, $3, $4, 'running', $5, NOW(), $6)
            RETURNING *
            "#,
        )
        .bind(instance_id)
        .bind(template_id)
        .bind(template_version)
        .bind(&net_id)
        .bind(created_by)
        .bind(&metadata)
        .fetch_one(self.db)
        .await
        .map_err(|e| {
            tracing::error!("failed to insert instance: {e}");
            LaunchError::Database(e.to_string())
        })?;

        if let Err(e) = deploy_instance(self.petri, &net_id, &parameterized).await {
            tracing::error!("failed to deploy instance to petri-lab: {e}");
            // Roll the row back so lifecycle never observes a phantom /
            // never-deployed instance.
            let _ = sqlx::query("DELETE FROM workflow_instances WHERE id = $1")
                .bind(instance_id)
                .execute(self.db)
                .await;
            return Err(LaunchError::Deploy(e.to_string()));
        }

        Ok(instance)
    }
}
