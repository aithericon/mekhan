//! Library-node governance (Phase 4): promote / demote / fork.
//!
//! These endpoints move a template across the `template_kind` / `origin`
//! coordinate axes that the catalogue + palette read (Phase 3). They change no
//! engine or compiler behaviour — a "library node" is still just a published
//! template wearing a brand, dropped onto the canvas as a `sub_workflow`
//! (decision 12). What governance adds is the *control* over which templates
//! are advertised in the Library palette and who may do the advertising.
//!
//! Authorization reuses the same role machinery as every other mutation:
//! workspace membership role (`require_role`) for promote/demote, object-read
//! visibility for fork. There is no dedicated audit table for templates — the
//! repo's convention for template-level governance is a structured `tracing`
//! record plus the `updated_by` stamp (see `templates.rs` publish-override
//! note), which we follow here.
//!
//! v1 scope (decision 4): only `origin = workspace` is settable via the API,
//! gated on workspace Admin/Owner. `community` (platform-admin review queue)
//! and `system` (seed-only) are deferred — the codebase has no platform-admin
//! primitive yet, so we reject those origins with a clear message rather than
//! inventing a half-gate.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::{map_to_api_error, require_role, AuthUser, Role};
use crate::handlers::require_template;
use crate::handlers::templates::graph_with_ydoc_fallback;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{
    is_known_library_category, Presentation, WorkflowGraph, WorkflowTemplate, LIBRARY_CATEGORIES,
};
use crate::AppState;

/// Validate a `vendor/slug` coordinate: exactly two non-empty segments, each
/// lowercase `[a-z0-9-]` with no leading/trailing/double hyphen. Mirrors the
/// slug rules enforced on workspace slugs + seeded library packs so a
/// hand-promoted coordinate can never diverge from a GitOps-seeded one.
fn validate_coordinate(coordinate: &str) -> Result<(), ApiError> {
    let parts: Vec<&str> = coordinate.split('/').collect();
    if parts.len() != 2 {
        return Err(ApiError::bad_request(
            "coordinate must be `vendor/slug` (exactly one slash)",
        ));
    }
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !s.starts_with('-')
            && !s.ends_with('-')
            && !s.contains("--")
    };
    if !segment_ok(parts[0]) || !segment_ok(parts[1]) {
        return Err(ApiError::bad_request(
            "coordinate segments must be lowercase [a-z0-9-] with no leading, \
             trailing, or doubled hyphens",
        ));
    }
    Ok(())
}

/// Validate that a library node's `presentation.category` is present and a
/// member of the controlled vocabulary (drives the two-level palette grouping).
fn validate_category(presentation: &Presentation) -> Result<(), ApiError> {
    match presentation.category.as_deref() {
        None => Err(ApiError::bad_request("presentation.category is required")),
        Some(c) if !is_known_library_category(c) => Err(ApiError::bad_request(format!(
            "unknown category `{c}`; allowed: {}",
            LIBRARY_CATEGORIES.join(", ")
        ))),
        Some(_) => Ok(()),
    }
}

/// Body for `POST /api/v1/templates/{id}/promote`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PromoteTemplateRequest {
    /// Origin axis. Only `workspace` (the default) is settable via the API in
    /// v1; `community` (platform-admin review) and `system` (seed-only) are
    /// rejected.
    #[serde(default)]
    pub origin: Option<String>,
    /// Stable `vendor/slug` coordinate, e.g. `acme/mesh-prep`. Unique among the
    /// current (`is_latest`) library nodes of this origin.
    pub coordinate: String,
    /// Branding + palette metadata. `category` must be a known vocabulary entry.
    pub presentation: Presentation,
}

/// POST /api/v1/templates/{id}/promote
///
/// Advertise a published template as a workspace library node: stamp
/// `template_kind = library_node` + `origin` + `coordinate` + `presentation`
/// across the whole version family so embeds pinned to any version resolve by
/// coordinate and carry the branding. Workspace Admin/Owner only.
#[utoipa::path(
    post,
    path = "/api/v1/templates/{id}/promote",
    params(("id" = Uuid, Path, description = "Template id (any version in the family)")),
    request_body = PromoteTemplateRequest,
    responses(
        (status = 200, description = "Template promoted to library node", body = WorkflowTemplate),
        (status = 400, description = "Invalid coordinate / category / origin", body = ErrorResponse),
        (status = 403, description = "Caller lacks workspace Admin/Owner", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 409, description = "Not published, or coordinate already in use", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library",
)]
pub async fn promote_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<PromoteTemplateRequest>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    let existing = require_template(&state.db, id).await?;

    // Origin gate — workspace only in v1.
    let origin = req.origin.as_deref().unwrap_or("workspace");
    match origin {
        "workspace" => {}
        "community" => {
            return Err(ApiError::bad_request(
                "community promotion requires platform-admin review, which is not yet available",
            ))
        }
        "system" => {
            return Err(ApiError::bad_request(
                "`system` origin is reserved for seeded vendor packs",
            ))
        }
        other => return Err(ApiError::bad_request(format!("unknown origin `{other}`"))),
    }

    // Only a published template can be pinned/embedded as a library node.
    if !existing.published {
        return Err(ApiError::conflict(
            "only a published template can be promoted to a library node",
        ));
    }

    // Workspace Admin/Owner on the template's own workspace.
    require_role(&state.db, &user, existing.workspace_id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    validate_coordinate(&req.coordinate)?;
    validate_category(&req.presentation)?;

    let base_id = existing.chain_root_id();

    // Coordinate uniqueness within the origin — enforced by the partial unique
    // index on the `is_latest` row, but pre-checked here for a friendly 409.
    // Re-promoting (or re-branding) one's OWN family is allowed; only a clash
    // with a different family is an error.
    let clash: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, base_template_id FROM workflow_templates \
          WHERE origin = $1 AND coordinate = $2 AND is_latest AND coordinate IS NOT NULL",
    )
    .bind(origin)
    .bind(&req.coordinate)
    .fetch_optional(&state.db)
    .await?;
    if let Some((clash_id, clash_base)) = clash {
        if clash_base.unwrap_or(clash_id) != base_id {
            return Err(ApiError::conflict(format!(
                "coordinate `{}` is already in use by another {origin} library node",
                req.coordinate
            )));
        }
    }

    let presentation_json =
        serde_json::to_value(&req.presentation).map_err(|e| ApiError::internal(e.to_string()))?;
    let principal = user.subject_as_uuid();

    // Stamp the WHOLE family. The partial unique index only constrains the
    // single `is_latest` row, so this is safe; stamping older versions too lets
    // a consumer pinned to v(N-1) still resolve the coordinate + branding when
    // `resolve_subworkflow_air` reads its pinned child row at publish time.
    sqlx::query(
        "UPDATE workflow_templates \
            SET template_kind = 'library_node', origin = $2, coordinate = $3, \
                presentation = $4, lifecycle_status = 'active', \
                updated_by = $5, updated_at = NOW() \
          WHERE COALESCE(base_template_id, id) = $1",
    )
    .bind(base_id)
    .bind(origin)
    .bind(&req.coordinate)
    .bind(&presentation_json)
    .bind(principal)
    .execute(&state.db)
    .await?;

    tracing::info!(
        template_id = %id,
        family = %base_id,
        coordinate = %req.coordinate,
        origin = %origin,
        principal = %principal,
        "governance: promoted template to library node"
    );

    require_template(&state.db, id).await.map(Json)
}

/// POST /api/v1/templates/{id}/demote
///
/// Reverse of promote: drop a workspace library node back to a plain workflow,
/// freeing its coordinate and removing it from the Library palette. Existing
/// embeds are frozen (the presentation is snapshotted into the consumer's graph
/// at publish time) and are unaffected. Workspace Admin/Owner only; seeded
/// `system` nodes cannot be demoted via the API.
#[utoipa::path(
    post,
    path = "/api/v1/templates/{id}/demote",
    params(("id" = Uuid, Path, description = "Template id (any version in the family)")),
    responses(
        (status = 200, description = "Library node demoted to workflow", body = WorkflowTemplate),
        (status = 400, description = "Seeded system node", body = ErrorResponse),
        (status = 403, description = "Caller lacks workspace Admin/Owner", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 409, description = "Template is not a library node", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library",
)]
pub async fn demote_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    let existing = require_template(&state.db, id).await?;

    if existing.template_kind != "library_node" {
        return Err(ApiError::conflict("template is not a library node"));
    }
    if existing.origin.as_deref() == Some("system") {
        return Err(ApiError::bad_request(
            "seeded `system` library nodes cannot be demoted via the API",
        ));
    }

    require_role(&state.db, &user, existing.workspace_id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    let base_id = existing.chain_root_id();
    let principal = user.subject_as_uuid();

    // Reverse the promote stamp across the family. Keep `presentation` (harmless
    // and lets a re-promote reuse the branding); clearing `coordinate` frees the
    // unique-index slot.
    sqlx::query(
        "UPDATE workflow_templates \
            SET template_kind = 'workflow', origin = NULL, coordinate = NULL, \
                updated_by = $2, updated_at = NOW() \
          WHERE COALESCE(base_template_id, id) = $1",
    )
    .bind(base_id)
    .bind(principal)
    .execute(&state.db)
    .await?;

    tracing::info!(
        template_id = %id,
        family = %base_id,
        principal = %principal,
        "governance: demoted library node to workflow"
    );

    require_template(&state.db, id).await.map(Json)
}

/// Body for `POST /api/v1/library/fork`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ForkLibraryRequest {
    /// Coordinate of the library node to fork (`vendor/slug`).
    pub coordinate: String,
}

/// POST /api/v1/library/fork
///
/// Deep-copy a (readable) library node's current version into a fresh, editable
/// `workspace`-visibility template family in the caller's active workspace,
/// recording `forked_from` provenance (decision 5). The fork is born a `workflow`
/// (the owner edits, then may re-promote it). Branding is copied so the fork
/// stays recognisable while editing. Body-based rather than `{coordinate}` in
/// the path because coordinates contain a slash.
#[utoipa::path(
    post,
    path = "/api/v1/library/fork",
    request_body = ForkLibraryRequest,
    responses(
        (status = 201, description = "Forked into a new editable template", body = WorkflowTemplate),
        (status = 403, description = "Caller cannot create in their workspace", body = ErrorResponse),
        (status = 404, description = "Library node not found / not readable", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library",
)]
pub async fn fork_library_node(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<ForkLibraryRequest>,
) -> Result<(StatusCode, Json<WorkflowTemplate>), ApiError> {
    // Anchor the fork in the caller's active workspace (Uuid::nil fallback keeps
    // the no-DB test resolver writing into a valid workspace, as elsewhere).
    let target_ws = user.workspace_id.unwrap_or_else(Uuid::nil);

    // Must be able to create in the target workspace.
    require_role(&state.db, &user, target_ws, Role::Editor)
        .await
        .map_err(map_to_api_error)?;

    // Resolve coordinate -> latest library-node version readable by the caller
    // (own workspace OR public). This snapshot is what the fork copies.
    let source = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates \
          WHERE coordinate = $1 AND template_kind = 'library_node' AND is_latest \
            AND (workspace_id = $2 OR visibility = 'public') \
          ORDER BY version DESC LIMIT 1",
    )
    .bind(&req.coordinate)
    .bind(target_ws)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found(format!("library node `{}` not found", req.coordinate)))?;

    // The authored graph lives in the source's Y.Doc, not the `graph` column
    // (publish/edit never write the column back — see `new_version`). Copy from
    // the Y.Doc, falling back to the column for legacy rows.
    let (graph, files) =
        graph_with_ydoc_fallback(&state, source.id, source.graph.clone(), |g| {
            Ok(serde_json::from_value(g).unwrap_or_else(|_| WorkflowGraph::default_graph()))
        })
        .await?;
    let graph_json = serde_json::to_value(&graph).map_err(|e| ApiError::internal(e.to_string()))?;

    let new_id = Uuid::new_v4();
    let principal = user.subject_as_uuid();
    let forked_from = serde_json::json!({
        "coordinate": source.coordinate,
        "template_id": source.chain_root_id(),
        "version": source.version,
    });
    let name = format!("{} (fork)", source.name);

    // New family, born an editable `workspace`-visibility draft the caller owns
    // (NOT `private` — that tier is reserved for owned sub-workflow children).
    // Plain `workflow` until re-promoted; `forked_from` records provenance.
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        INSERT INTO workflow_templates
            (id, name, description, base_template_id, version, is_latest, graph,
             author_id, workspace_id, visibility, presentation, forked_from,
             template_kind, lifecycle_status, updated_by)
        VALUES ($1, $2, $3, $1, 1, TRUE, $4, $5, $6, 'workspace', $7, $8,
                'workflow', 'active', $5)
        RETURNING *
        "#,
    )
    .bind(new_id)
    .bind(&name)
    .bind(&source.description)
    .bind(&graph_json)
    .bind(principal)
    .bind(target_ws)
    .bind(&source.presentation)
    .bind(&forked_from)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("failed to fork library node: {e}");
        ApiError::internal(e.to_string())
    })?;

    // Seed the Y.Doc for the new family so WS collaboration works immediately,
    // including the copied per-node files.
    if let Err(e) = state
        .yjs
        .persistence
        .init_doc_from_graph_with_files(new_id, &graph, &files)
        .await
    {
        tracing::error!("failed to init Y.Doc for forked template {new_id}: {e}");
        // Non-fatal: the row exists; the Y.Doc can be initialized later.
    }

    tracing::info!(
        new_template_id = %new_id,
        source_coordinate = %req.coordinate,
        workspace = %target_ws,
        principal = %principal,
        "governance: forked library node into workspace"
    );

    Ok((StatusCode::CREATED, Json(template)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinate_validation() {
        assert!(validate_coordinate("openfoam/solid-displacement").is_ok());
        assert!(validate_coordinate("acme/mesh-prep").is_ok());
        assert!(validate_coordinate("a1/b2").is_ok());

        // wrong segment count
        assert!(validate_coordinate("noslash").is_err());
        assert!(validate_coordinate("too/many/slashes").is_err());
        // empty segment
        assert!(validate_coordinate("/slug").is_err());
        assert!(validate_coordinate("vendor/").is_err());
        // illegal chars / casing
        assert!(validate_coordinate("Vendor/slug").is_err());
        assert!(validate_coordinate("vendor/Slug").is_err());
        assert!(validate_coordinate("vendor/slug_underscore").is_err());
        // hyphen edges
        assert!(validate_coordinate("-vendor/slug").is_err());
        assert!(validate_coordinate("vendor/slug-").is_err());
        assert!(validate_coordinate("ven--dor/slug").is_err());
    }

    #[test]
    fn category_validation() {
        let mut p = Presentation::default();
        assert!(validate_category(&p).is_err()); // missing
        p.category = Some("CFD".to_string());
        assert!(validate_category(&p).is_ok());
        p.category = Some("cfd".to_string());
        assert!(validate_category(&p).is_err()); // case-sensitive
        p.category = Some("Frobnication".to_string());
        assert!(validate_category(&p).is_err()); // unknown
    }
}
