//! Per-workspace **model-serving runner group** — the first-class home for
//! self-hosted LLM serving nodes (the inference pool).
//!
//! Identity of "this runner is part of the LLM pool" used to be an emergent
//! heuristic: a present runner whose `RunnerInterfaceCatalog` happened to carry a
//! `base_url`. That conflated the data-plane *endpoint* with pool *membership*.
//! This module makes membership first-class and consistent with the rest of the
//! fleet: model runners enrol into a presence-backed `capacity` resource at path
//! [`MODEL_SERVING_GROUP_PATH`] (the `instrument` preset: `presence` liveness +
//! `auto` acceptance), exactly like workers enrol into the `default` worker group.
//! The model-pool reads then gate on group membership (the runner's
//! `runner_group` alias, mirrored onto the presence snapshot) instead of sniffing
//! `base_url`; the catalog's `base_url`/`models`/`residency_zone` stay as the
//! data-plane payload the router needs.
//!
//! This is the sibling of [`crate::worker_groups`] — same idempotent-seed +
//! resolve shape, different preset (`instrument` vs `worker`) so the backing
//! resource sits at the presence-pool point in the capacity trait-space.

use uuid::Uuid;

use crate::models::asset::PLATFORM_SCOPE_ID;
use crate::models::error::ApiError;
use crate::AppState;

/// The workspace-scoped resource path of the model-serving runner group. A
/// snake_case identifier (`^[a-z][a-z0-9_]*$`) so it is a legal resource path
/// and a legal runner `group` alias. This is the alias `just dev up-model-runner`
/// enrols into and the alias the model-pool reads match present runners on.
pub const MODEL_SERVING_GROUP_PATH: &str = "model_serving";

/// The seeder principal for the auto-provisioned model-serving group. A fixed,
/// out-of-band UUID (never a real OIDC subject) so the seed row is attributable
/// without flowing through the BFF. Mirrors
/// `worker_groups::WORKER_GROUP_SEEDER_AUTHOR_ID`.
const MODEL_SERVING_GROUP_SEEDER_AUTHOR_ID: Uuid =
    uuid::uuid!("00000000-0000-0000-0000-000000000c1c");

/// Resolve the model-serving group ALIAS (workspace-scoped resource `path`) to
/// its capacity-resource UUID. Matches the `instrument` preset axes: a live
/// `capacity` resource with `presence` liveness + `auto` acceptance (which
/// excludes `consent` / human-roster pools). Returns `Ok(None)` when no such
/// backed group exists. DB read only.
pub async fn resolve_model_serving_group_uuid(
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
           AND rv.public_config ->> 'liveness' = 'presence' \
           AND rv.public_config ->> 'acceptance' = 'auto'",
    )
    .bind(workspace_id)
    .bind(alias)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::internal(format!("model-serving-group lookup: {e}")))?;
    Ok(found.map(|(id,)| id))
}

/// Resolve the **platform-tier** model-serving group at [`MODEL_SERVING_GROUP_PATH`]
/// to its capacity-resource UUID. Same `instrument`-preset axes as
/// [`resolve_model_serving_group_uuid`], but filtered to the platform scope
/// (`scope_kind = 'platform'`, `workspace_id = scope_id = PLATFORM_SCOPE_ID`) —
/// the single shared pool every tenant's serving runners enrol into. Returns
/// `Ok(None)` when not yet seeded. DB read only.
pub async fn resolve_platform_model_serving_group_uuid(
    db: &sqlx::PgPool,
) -> Result<Option<Uuid>, ApiError> {
    let found: Option<(Uuid,)> = sqlx::query_as::<_, (Uuid,)>(
        "SELECT r.id FROM resources r \
         JOIN resource_versions rv \
           ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.scope_kind = 'platform' AND r.workspace_id = $1 AND r.path = $2 \
           AND r.resource_type = 'capacity' AND r.deleted_at IS NULL \
           AND rv.public_config ->> 'liveness' = 'presence' \
           AND rv.public_config ->> 'acceptance' = 'auto'",
    )
    .bind(PLATFORM_SCOPE_ID)
    .bind(MODEL_SERVING_GROUP_PATH)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::internal(format!("platform model-serving-group lookup: {e}")))?;
    Ok(found.map(|(id,)| id))
}

/// Idempotently seed the per-workspace **model-serving** runner group: a
/// `capacity` resource at [`MODEL_SERVING_GROUP_PATH`] with the `instrument`
/// preset axes (presence-driven pool). A no-op when one already exists. Returns
/// the group's resource UUID either way.
///
/// Reuses [`crate::handlers::resources::create_resource_internal`] so the seeded
/// row is byte-identical to a hand-created presence-pool capacity: same preset
/// expansion, version write, ACL grant, audit row, and the presence-pool admission
/// net deployed at resource-create (the `instrument` preset resolves to the
/// `Presence` backend, which the runner's heartbeat injects a unit into).
///
/// Best-effort: a create race (two boots seeding the same workspace) surfaces as
/// a `409` from the unique `(workspace_id, path)` constraint — treated as
/// "already present" and re-resolved.
pub async fn ensure_model_serving_group(
    state: &AppState,
    workspace_id: Uuid,
) -> Result<Uuid, ApiError> {
    // Fast path: already seeded.
    if let Some(id) =
        resolve_model_serving_group_uuid(&state.db, workspace_id, MODEL_SERVING_GROUP_PATH).await?
    {
        return Ok(id);
    }

    let req = crate::models::resource::CreateResourceRequest {
        path: MODEL_SERVING_GROUP_PATH.to_string(),
        resource_type: "capacity".to_string(),
        display_name: Some("Model serving".to_string()),
        // The `instrument` preset locks the presence/push axes; the create path
        // expands it into the typed axis strings before persisting.
        config: serde_json::json!({ "preset": "instrument" }),
        workspace_id: Some(workspace_id),
        scope_kind: None,
        scope_id: None,
        restricted: None,
    };

    match crate::handlers::resources::create_resource_internal(
        state,
        &req,
        workspace_id,
        MODEL_SERVING_GROUP_SEEDER_AUTHOR_ID,
    )
    .await
    {
        Ok(summary) => Ok(summary.id),
        Err(e) => {
            // A concurrent seed (409 on the unique path constraint) is benign —
            // re-resolve the row the other boot wrote. Any other error is real.
            if let Some(id) =
                resolve_model_serving_group_uuid(&state.db, workspace_id, MODEL_SERVING_GROUP_PATH)
                    .await?
            {
                return Ok(id);
            }
            Err(e)
        }
    }
}

/// Idempotently seed the SINGLE **platform-tier** model-serving group: one
/// `capacity` resource at [`MODEL_SERVING_GROUP_PATH`] with the `instrument`
/// preset, owned by the platform tier (`scope_kind = 'platform'`,
/// `workspace_id = scope_id = PLATFORM_SCOPE_ID`). This is the shared inference
/// pool every tenant's self-hosted serving runners enrol into — the pool is a
/// global data plane (inference routing is already cluster-wide), so its
/// control-plane membership resource lives at the platform tier rather than in
/// any tenant workspace.
///
/// Reuses [`crate::handlers::resources::create_resource_internal`] (the platform
/// `scope_kind` arm forces `workspace_id = scope_id = PLATFORM_SCOPE_ID`), so the
/// seeded row is byte-identical to a hand-created platform presence-pool
/// capacity. A no-op when one already exists; a create race surfaces as a `409`
/// from the unique `(scope_kind, scope_id, path)` constraint and is re-resolved.
pub async fn ensure_platform_model_serving_group(state: &AppState) -> Result<Uuid, ApiError> {
    // Fast path: already seeded.
    if let Some(id) = resolve_platform_model_serving_group_uuid(&state.db).await? {
        return Ok(id);
    }

    let req = crate::models::resource::CreateResourceRequest {
        path: MODEL_SERVING_GROUP_PATH.to_string(),
        resource_type: "capacity".to_string(),
        display_name: Some("Model serving".to_string()),
        // The `instrument` preset locks the presence/push axes; the create path
        // expands it into the typed axis strings before persisting.
        config: serde_json::json!({ "preset": "instrument" }),
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
        MODEL_SERVING_GROUP_SEEDER_AUTHOR_ID,
    )
    .await
    {
        Ok(summary) => Ok(summary.id),
        Err(e) => {
            // A concurrent seed (409 on the unique path constraint) is benign —
            // re-resolve the row the other boot wrote. Any other error is real.
            if let Some(id) = resolve_platform_model_serving_group_uuid(&state.db).await? {
                return Ok(id);
            }
            Err(e)
        }
    }
}
