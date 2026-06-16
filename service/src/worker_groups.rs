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
//! (the `worker` preset: `competing_consumer` liveness + `auto` acceptance). This
//! module is the single idempotent seeder + the alias→UUID resolver both the
//! compiler and the enroll handler use to turn the human group alias (or the
//! implicit "default") into the routing-partition UUID.

use uuid::Uuid;

use crate::models::asset::PLATFORM_SCOPE_ID;
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
/// `capacity` resource with `competing_consumer` liveness + `auto` acceptance.
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
           AND rv.public_config ->> 'acceptance' = 'auto'",
    )
    .bind(workspace_id)
    .bind(alias)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::internal(format!("worker-group lookup: {e}")))?;
    Ok(found.map(|(id,)| id))
}

/// Resolve the **platform-tier** default worker group at
/// [`DEFAULT_WORKER_GROUP_PATH`] to its capacity-resource UUID — the shared
/// competing-consumer routing partition every tenant's no-group steps land on.
/// Same `worker`-preset axes as [`resolve_worker_group_uuid`], but filtered to
/// the platform scope (`scope_kind = 'platform'`,
/// `workspace_id = scope_id = PLATFORM_SCOPE_ID`). Returns `Ok(None)` when not
/// yet seeded. DB read only.
pub async fn resolve_platform_default_worker_group_uuid(
    db: &sqlx::PgPool,
) -> Result<Option<Uuid>, ApiError> {
    let found: Option<(Uuid,)> = sqlx::query_as::<_, (Uuid,)>(
        "SELECT r.id FROM resources r \
         JOIN resource_versions rv \
           ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.scope_kind = 'platform' AND r.workspace_id = $1 AND r.path = $2 \
           AND r.resource_type = 'capacity' AND r.deleted_at IS NULL \
           AND rv.public_config ->> 'liveness' = 'competing_consumer' \
           AND rv.public_config ->> 'acceptance' = 'auto'",
    )
    .bind(PLATFORM_SCOPE_ID)
    .bind(DEFAULT_WORKER_GROUP_PATH)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::internal(format!("platform worker-group lookup: {e}")))?;
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
///
/// `id_override` pins the seeded capacity's id to a specific UUID. Production
/// and the boot seeder always pass `None` (a fresh random id per workspace).
/// The ONLY caller that sets it is the e2e test harness
/// (`tests/common::seed_dev_worker_partition`): a fresh-DB test must seed its
/// `default` group with the id the live dev executor is already bound to, so
/// the compiler stamps a partition a worker actually drains. It is a
/// single-workspace test affordance — within one fresh DB only one workspace is
/// seeded with the fixed id, so the resource PK stays unique.
pub async fn ensure_default_worker_group(
    state: &AppState,
    workspace_id: Uuid,
    id_override: Option<Uuid>,
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
        // The `worker` preset locks the competing_consumer/auto axes; the
        // create path expands it into the typed axis strings before persisting.
        config: serde_json::json!({ "preset": "worker" }),
        workspace_id: Some(workspace_id),
        scope_kind: None,
        scope_id: None,
        restricted: None,
    };

    match crate::handlers::resources::create_resource_internal_with_id(
        state,
        &req,
        workspace_id,
        WORKER_GROUP_SEEDER_AUTHOR_ID,
        id_override,
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

/// Idempotently seed the SINGLE **platform-tier** default worker group: one
/// `capacity` resource at [`DEFAULT_WORKER_GROUP_PATH`] with the `worker` preset,
/// owned by the platform tier (`scope_kind = 'platform'`,
/// `workspace_id = scope_id = PLATFORM_SCOPE_ID`). This is the shared
/// competing-consumer executor pool every tenant's no-group AutomatedStep routes
/// onto: the executor queue is already a global data plane (competing-consumer,
/// no lease/bridge net), so its control-plane routing-partition resource lives at
/// the platform tier rather than per workspace. "No group" now means "the shared
/// platform pool"; naming a group explicitly is the per-workspace escape hatch.
///
/// Reuses [`crate::handlers::resources::create_resource_internal`] (the platform
/// `scope_kind` arm forces `workspace_id = scope_id = PLATFORM_SCOPE_ID`), so the
/// seeded row is byte-identical to a hand-created platform worker capacity. A
/// no-op when one already exists; a create race surfaces as a `409` from the
/// unique `(scope_kind, scope_id, path)` constraint and is re-resolved.
pub async fn ensure_platform_default_worker_group(state: &AppState) -> Result<Uuid, ApiError> {
    // Fast path: already seeded.
    if let Some(id) = resolve_platform_default_worker_group_uuid(&state.db).await? {
        return Ok(id);
    }

    let req = crate::models::resource::CreateResourceRequest {
        path: DEFAULT_WORKER_GROUP_PATH.to_string(),
        resource_type: "capacity".to_string(),
        display_name: Some("Default workers".to_string()),
        // The `worker` preset locks the competing_consumer/auto axes; the create
        // path expands it into the typed axis strings before persisting.
        config: serde_json::json!({ "preset": "worker" }),
        // Platform tier: the create path forces workspace_id = scope_id =
        // PLATFORM_SCOPE_ID for `scope_kind = "platform"`.
        workspace_id: Some(PLATFORM_SCOPE_ID),
        scope_kind: Some("platform".to_string()),
        scope_id: Some(PLATFORM_SCOPE_ID),
        restricted: None,
    };

    match crate::handlers::resources::create_resource_internal(
        state,
        &req,
        PLATFORM_SCOPE_ID,
        WORKER_GROUP_SEEDER_AUTHOR_ID,
    )
    .await
    {
        Ok(summary) => Ok(summary.id),
        Err(e) => {
            // A concurrent seed (409 on the unique path constraint) is benign —
            // re-resolve the row the other boot wrote. Any other error is real.
            if let Some(id) = resolve_platform_default_worker_group_uuid(&state.db).await? {
                return Ok(id);
            }
            Err(e)
        }
    }
}
