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
use crate::petri::instance::{deploy_instance, parameterize_air, ParameterizeError};

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

    /// The instance row could not be inserted. Nothing was deployed.
    #[error("instance row insert failed: {0}")]
    Database(String),

    /// petri-lab deploy failed. The just-inserted row has already been rolled
    /// back so the lifecycle listener never observes a never-deployed
    /// instance.
    #[error("deploy failed: {0}")]
    Deploy(String),
}

/// What the caller wants run. `created_by` and `metadata` are the only inputs
/// that genuinely differ between the user-POST and trigger-fire paths, so they
/// stay parameters; everything else (parameterize → insert → deploy →
/// rollback) is owned by the launcher.
pub struct LaunchSpec<'a> {
    pub instance_id: Uuid,
    pub net_id: String,
    pub template_id: Uuid,
    pub template_version: i32,
    pub created_by: Uuid,
    /// Audit-only blob stored on the instance row (not merged into tokens).
    pub metadata: Value,
    pub air_json: &'a Value,
    pub graph: &'a WorkflowGraph,
    pub start_tokens: &'a [StartToken],
    /// Categorizes the instance. `None` ⇒ `'live'` (the historical default).
    /// `Some("draft")` for user-initiated experiments; `Some("test_run")` is
    /// reserved for the template-test runner.
    pub mode: Option<&'a str>,
    /// Set when `mode == "test_run"`. Forwards into the instance row so the
    /// run can be reconciled with its originating `template_tests` row.
    pub test_id: Option<Uuid>,
}

/// Owns the deploy-an-instance sequence. Behavior-identical to the code that
/// was inlined in `create_instance` and `fire_spawn` — pure relocation.
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
        let parameterized = parameterize_air(
            spec.air_json,
            spec.instance_id,
            spec.template_id,
            spec.template_version,
            spec.created_by,
            spec.graph,
            spec.start_tokens,
        )?;

        let mode = spec.mode.unwrap_or("live");
        let instance = sqlx::query_as::<_, WorkflowInstance>(
            r#"
            INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, started_at, metadata, mode, test_id)
            VALUES ($1, $2, $3, $4, 'running', $5, NOW(), $6, $7, $8)
            RETURNING *
            "#,
        )
        .bind(spec.instance_id)
        .bind(spec.template_id)
        .bind(spec.template_version)
        .bind(&spec.net_id)
        .bind(spec.created_by)
        .bind(&spec.metadata)
        .bind(mode)
        .bind(spec.test_id)
        .fetch_one(self.db)
        .await
        .map_err(|e| {
            tracing::error!("failed to insert instance: {e}");
            LaunchError::Database(e.to_string())
        })?;

        if let Err(e) = deploy_instance(self.petri, &spec.net_id, &parameterized).await {
            tracing::error!("failed to deploy instance to petri-lab: {e}");
            // Roll the row back so lifecycle never observes a phantom /
            // never-deployed instance.
            let _ = sqlx::query("DELETE FROM workflow_instances WHERE id = $1")
                .bind(spec.instance_id)
                .execute(self.db)
                .await;
            return Err(LaunchError::Deploy(e.to_string()));
        }

        Ok(instance)
    }
}
