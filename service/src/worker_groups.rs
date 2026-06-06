//! Per-workspace **default worker group** — the always-seeded landing partition
//! for the unified single-stream executor-worker dispatch model.
//!
//! In the unified model there is no anonymous worker path: EVERY worker enrolls
//! and EVERY executor job routes through a GROUP partition on the parallel
//! `executor-<wire>-grp` stream. A step that names no group is stamped with its
//! workspace's "default" worker group at compile time; a worker that enrolls
//! without naming a group inherits "default". The partition token is the
//! group's capacity-resource UUID (workspace-safe by construction — two
//! workspaces can both own a "default" group without colliding on a queue).
//!
//! For that to work, every workspace must own a `capacity` resource at path
//! [`DEFAULT_WORKER_GROUP_PATH`] sitting at the WORKER point in the trait-space
//! (the `worker` preset: `competing_consumer` liveness + `pull` dispatch). This
//! module is the single idempotent seeder + the alias→UUID resolver both the
//! compiler and the enroll handler use to turn the human group alias (or the
//! implicit "default") into the routing-partition UUID.

use uuid::Uuid;

use crate::models::error::ApiError;
use crate::AppState;

/// The workspace-scoped resource path of the default worker group. A
/// snake_case identifier (`^[a-z][a-z0-9_]*$`) so it is a legal resource path
/// and a legal step `group` alias.
pub const DEFAULT_WORKER_GROUP_PATH: &str = "default";

/// The seeder principal for the auto-provisioned default worker group. A fixed,
/// out-of-band UUID (never a real OIDC subject) so the seed row is attributable
/// without flowing through the BFF. Mirrors `demos::DEMO_SEEDER_AUTHOR_ID`.
const WORKER_GROUP_SEEDER_AUTHOR_ID: Uuid = uuid::uuid!("00000000-0000-0000-0000-000000000bbb");

/// Resolve a worker-group ALIAS (workspace-scoped resource `path`) to its
/// capacity-resource UUID — the routing partition token. Matches the same
/// worker axes the enroll gate (`worker_group_exists`) checks: a live
/// `capacity` resource with `competing_consumer` liveness + `pull` dispatch.
///
/// Returns `Ok(None)` when no such backed group exists (the caller decides
/// whether that is a hard error). DB read only.
pub async fn resolve_worker_group_uuid(
    db: &sqlx::PgPool,
    workspace_id: Uuid,
    alias: &str,
) -> Result<Option<Uuid>, ApiError> {
    let found: Option<(Uuid,)> = sqlx::query_as::<_, (Uuid,)>(
        "SELECT r.id FROM resources r \
         JOIN resource_versions rv \
           ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.workspace_id = $1 AND r.path = $2 \
           AND r.resource_type = 'capacity' AND r.deleted_at IS NULL \
           AND rv.public_config ->> 'liveness' = 'competing_consumer' \
           AND rv.public_config ->> 'dispatch' = 'pull'",
    )
    .bind(workspace_id)
    .bind(alias)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::internal(format!("worker-group lookup: {e}")))?;
    Ok(found.map(|(id,)| id))
}

/// Idempotently seed the per-workspace **default** worker group: a `capacity`
/// resource at [`DEFAULT_WORKER_GROUP_PATH`] with the `worker` preset axes. A
/// no-op when one already exists. Returns the group's resource UUID (the
/// routing partition) either way.
///
/// Reuses the normal resource-create machinery
/// ([`crate::handlers::resources::create_resource_internal`]) so the seeded row
/// is byte-identical to a hand-created worker capacity: same preset expansion,
/// version write, ACL grant, audit row, and pool-net hook (the `worker` preset
/// resolves to the `Queue` backend, which deploys NO admission net — exactly
/// like a hand-created worker group).
///
/// Best-effort: a create race (two boots seeding the same workspace) surfaces
/// as a `409` from the unique `(workspace_id, path)` constraint — we treat that
/// as "already present" and re-resolve.
pub async fn ensure_default_worker_group(
    state: &AppState,
    workspace_id: Uuid,
) -> Result<Uuid, ApiError> {
    // Fast path: already seeded.
    if let Some(id) =
        resolve_worker_group_uuid(&state.db, workspace_id, DEFAULT_WORKER_GROUP_PATH).await?
    {
        return Ok(id);
    }

    let req = crate::models::resource::CreateResourceRequest {
        path: DEFAULT_WORKER_GROUP_PATH.to_string(),
        resource_type: "capacity".to_string(),
        display_name: Some("Default workers".to_string()),
        // The `worker` preset locks the competing_consumer/pull axes; the
        // create path expands it into the typed axis strings before persisting.
        config: serde_json::json!({ "preset": "worker" }),
        workspace_id: Some(workspace_id),
    };

    match crate::handlers::resources::create_resource_internal(
        state,
        &req,
        workspace_id,
        WORKER_GROUP_SEEDER_AUTHOR_ID,
    )
    .await
    {
        Ok(summary) => Ok(summary.id),
        Err(e) => {
            // A concurrent seed (409 on the unique path constraint) is benign —
            // re-resolve the row the other boot wrote. Any other error is real.
            if let Some(id) =
                resolve_worker_group_uuid(&state.db, workspace_id, DEFAULT_WORKER_GROUP_PATH)
                    .await?
            {
                return Ok(id);
            }
            Err(e)
        }
    }
}

/// Seed the default worker group for EVERY existing workspace at startup, so
/// installs that never reboot through a workspace-create path (and any
/// workspace created before this seeder existed) still get their default group.
/// Idempotent + best-effort per workspace: a single workspace's failure logs a
/// warning and does not abort the others.
pub async fn ensure_default_worker_group_all_workspaces(state: &AppState) {
    let workspaces: Vec<(Uuid,)> = match sqlx::query_as::<_, (Uuid,)>("SELECT id FROM workspaces")
        .fetch_all(&state.db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "default worker-group seeder: could not list workspaces");
            return;
        }
    };

    let mut seeded = 0usize;
    for (workspace_id,) in workspaces {
        match ensure_default_worker_group(state, workspace_id).await {
            Ok(_) => seeded += 1,
            Err(e) => tracing::warn!(
                workspace_id = %workspace_id,
                error = ?e,
                "default worker-group seed failed for workspace"
            ),
        }
    }
    tracing::info!(workspaces = seeded, "default worker-group seeder finished");
}
