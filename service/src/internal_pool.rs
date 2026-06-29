//! **Platform-tier internal LLM model pool** resources — the shared inference
//! router (`internal_pool_router`, `internal_llm`) and its model registry
//! (`internal_pool_registry`, `model_registry`).
//!
//! These are NOT demo content: the internal pool is a GLOBAL inference data
//! plane (the inference HTTP router + competing-consumer queue are already
//! cluster-wide). So the two control-plane resources every workflow binds to
//! (`resource_alias: "internal_pool_router"` / `"internal_pool_registry"`) live
//! at the **platform tier** (`scope_kind = 'platform'`,
//! `workspace_id = scope_id = PLATFORM_SCOPE_ID`) rather than in any single
//! tenant workspace. The resource resolver widens to
//! `WHERE (workspace_id = $caller OR scope_kind = 'platform')`
//! (`process/discover.rs`), so a platform-tier router/registry resolves for
//! workflows compiled in **any** workspace — the dev-user workspace, the prod
//! service-user workspace, the `demos` workspace, everywhere. A tenant row of
//! the same `path` deterministically shadows the platform default.
//!
//! This mirrors [`crate::model_serving_group::ensure_platform_model_serving_group`]
//! — same idempotent fast-path-resolve / build-`CreateResourceRequest` /
//! `create_resource_internal` / re-resolve-on-409 shape — but for the two
//! plain (non-`capacity`) resources of the model pool.

use uuid::Uuid;

use crate::models::asset::PLATFORM_SCOPE_ID;
use crate::models::error::ApiError;
use crate::AppState;

/// The platform-scoped resource path of the internal inference router. A
/// snake_case identifier (`^[a-z][a-z0-9_]*$`) so it is a legal resource path
/// and a legal workflow `resource_alias`. Workflows reference it as
/// `internal_pool_router.<field>`.
pub const INTERNAL_POOL_ROUTER_PATH: &str = "internal_pool_router";

/// The platform-scoped resource path of the internal model registry. Names the
/// router resource (`router_resource`) + the approved-models allowlist the
/// model-pool picker gates on.
pub const INTERNAL_POOL_REGISTRY_PATH: &str = "internal_pool_registry";

/// Default base URL of the internal inference router when `MEKHAN_ROUTER_URL`
/// is unset — the local dev inference-router slot. Mirrors the
/// `${MEKHAN_ROUTER_URL:-http://127.0.0.1:13200}` default the old
/// `demos/resources/internal_pool_router.json` fixture carried.
const INTERNAL_POOL_ROUTER_DEFAULT_URL: &str = "http://127.0.0.1:13200";

/// The seeder principal for the auto-provisioned internal-pool resources. A
/// fixed, out-of-band UUID (never a real OIDC subject) so the seed rows are
/// attributable without flowing through the BFF. Mirrors
/// `model_serving_group::MODEL_SERVING_GROUP_SEEDER_AUTHOR_ID`.
const INTERNAL_POOL_SEEDER_AUTHOR_ID: Uuid = uuid::uuid!("00000000-0000-0000-0000-000000001b07");

/// Resolve a **platform-tier** internal-pool resource (`scope_kind = 'platform'`,
/// `workspace_id = PLATFORM_SCOPE_ID`) at `path` of the given `resource_type` to
/// its row UUID. Returns `Ok(None)` when not yet seeded. DB read only.
async fn resolve_platform_internal_pool_uuid(
    db: &sqlx::PgPool,
    path: &str,
    resource_type: &str,
) -> Result<Option<Uuid>, ApiError> {
    let found: Option<(Uuid,)> = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM resources \
         WHERE scope_kind = 'platform' AND workspace_id = $1 AND path = $2 \
           AND resource_type = $3 AND deleted_at IS NULL",
    )
    .bind(PLATFORM_SCOPE_ID)
    .bind(path)
    .bind(resource_type)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::internal(format!("platform internal-pool lookup ({path}): {e}")))?;
    Ok(found.map(|(id,)| id))
}

/// Idempotently seed one platform-tier internal-pool resource at `path` with the
/// given `resource_type` + `config`. A no-op when one already exists. Returns the
/// resource's UUID either way.
///
/// Reuses [`crate::handlers::resources::create_resource_internal`] (the platform
/// `scope_kind` arm forces `workspace_id = scope_id = PLATFORM_SCOPE_ID`), so the
/// seeded row is byte-identical to a hand-created platform resource. A create
/// race surfaces as a `409` from the unique `(scope_kind, scope_id, path)`
/// constraint and is treated as "already present" — re-resolved.
async fn ensure_platform_internal_pool_resource(
    state: &AppState,
    path: &str,
    resource_type: &str,
    display_name: &str,
    config: serde_json::Value,
) -> Result<Uuid, ApiError> {
    // Fast path: already seeded.
    if let Some(id) = resolve_platform_internal_pool_uuid(&state.db, path, resource_type).await? {
        return Ok(id);
    }

    let req = crate::models::resource::CreateResourceRequest {
        path: path.to_string(),
        resource_type: resource_type.to_string(),
        display_name: Some(display_name.to_string()),
        config,
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
        INTERNAL_POOL_SEEDER_AUTHOR_ID,
    )
    .await
    {
        Ok(summary) => Ok(summary.id),
        Err(e) => {
            // A concurrent seed (409 on the unique path constraint) is benign —
            // re-resolve the row the other boot wrote. Any other error is real.
            if let Some(id) =
                resolve_platform_internal_pool_uuid(&state.db, path, resource_type).await?
            {
                return Ok(id);
            }
            Err(e)
        }
    }
}

/// Idempotently seed BOTH platform-tier internal-pool resources at boot: the
/// inference router (`internal_pool_router`, `internal_llm`) and its model
/// registry (`internal_pool_registry`, `model_registry`). Owned by the platform
/// tier so workflows in any workspace resolve them.
///
/// The router is seeded first (the registry's `router_resource` names it by
/// alias). Both are no-ops once present. Best-effort at the call site.
pub async fn ensure_platform_internal_pool(state: &AppState) -> Result<(), ApiError> {
    // The router endpoint varies per dev slot — honour `MEKHAN_ROUTER_URL` with
    // the local inference-router default, exactly as the old demo fixture did.
    let base_url = std::env::var("MEKHAN_ROUTER_URL")
        .unwrap_or_else(|_| INTERNAL_POOL_ROUTER_DEFAULT_URL.to_string());

    ensure_platform_internal_pool_resource(
        state,
        INTERNAL_POOL_ROUTER_PATH,
        "internal_llm",
        "Internal Model Pool Router",
        serde_json::json!({ "base_url": base_url }),
    )
    .await?;

    ensure_platform_internal_pool_resource(
        state,
        INTERNAL_POOL_REGISTRY_PATH,
        "model_registry",
        "Internal Model Registry",
        serde_json::json!({
            "router_resource": INTERNAL_POOL_ROUTER_PATH,
            "approved_models": [
                { "model_id": "qwen3.5:9b", "provider": "openai" },
                { "model_id": "llama3.2:1b", "provider": "openai" },
                { "model_id": "llama3.2-vision:11b", "provider": "openai" }
            ]
        }),
    )
    .await?;

    Ok(())
}
