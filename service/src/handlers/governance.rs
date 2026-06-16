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
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{map_to_api_error, require_role, AuthUser, Role};
use crate::compiler::derive_child_io;
use crate::handlers::require_template;
use crate::handlers::templates::graph_with_ydoc_fallback;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{
    is_known_library_category, FieldKind, Port, Presentation, WorkflowGraph, WorkflowTemplate,
    LIBRARY_CATEGORIES,
};
use crate::AppState;

/// Validate a `vendor/slug` coordinate: exactly two non-empty segments, each
/// lowercase `[a-z0-9-]` with no leading/trailing/double hyphen. Mirrors the
/// slug rules enforced on workspace slugs + seeded library packs so a
/// hand-promoted coordinate can never diverge from a GitOps-seeded one.
pub(crate) fn validate_coordinate(coordinate: &str) -> Result<(), ApiError> {
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
pub(crate) fn validate_category(presentation: &Presentation) -> Result<(), ApiError> {
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
    // Anchor the fork in the caller's active workspace; reject (403) rather than
    // forking into the nil tenant when the caller has no active workspace.
    let target_ws = user.require_workspace()?;

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
    let (graph, files) = graph_with_ydoc_fallback(&state, source.id, source.graph.clone(), |g| {
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

// ─── Phase 5: lifecycle ──────────────────────────────────────────────────────

/// The lifecycle states a library node can occupy. `active` is droppable;
/// `deprecated` stays droppable but the palette warns (and surfaces a successor
/// if `superseded_by` is set); `retired` is hidden from the palette entirely
/// (existing pinned embeds still resolve via their frozen version row — version
/// rows are never hard-deleted, decision 11).
fn valid_lifecycle_status(s: &str) -> bool {
    matches!(s, "active" | "deprecated" | "retired")
}

/// Body for `POST /api/v1/templates/{id}/lifecycle`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct LifecycleRequest {
    /// Target lifecycle state: `active` | `deprecated` | `retired`.
    pub status: String,
    /// Optional successor coordinate (`vendor/slug`) shown to consumers of a
    /// `deprecated`/`retired` node so they know what to migrate to. Cleared when
    /// omitted. Only meaningful for non-`active` states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

/// POST /api/v1/templates/{id}/lifecycle
///
/// Set a library node's lifecycle state across its whole version family
/// (decision 11). Admin/Owner on the node's workspace; seeded `system` nodes are
/// untouchable via the API. Never deletes version rows — `retired` only hides
/// the node from the palette while keeping pinned embeds resolvable.
#[utoipa::path(
    post,
    path = "/api/v1/templates/{id}/lifecycle",
    params(("id" = Uuid, Path, description = "Template id (any version in the family)")),
    request_body = LifecycleRequest,
    responses(
        (status = 200, description = "Lifecycle updated", body = WorkflowTemplate),
        (status = 400, description = "Invalid status / successor / system node", body = ErrorResponse),
        (status = 403, description = "Caller lacks workspace Admin/Owner", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 409, description = "Template is not a library node", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library",
)]
pub async fn set_lifecycle(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<LifecycleRequest>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    let existing = require_template(&state.db, id).await?;

    if existing.template_kind != "library_node" {
        return Err(ApiError::conflict("template is not a library node"));
    }
    if existing.origin.as_deref() == Some("system") {
        return Err(ApiError::bad_request(
            "seeded `system` library nodes cannot change lifecycle via the API",
        ));
    }
    if !valid_lifecycle_status(&req.status) {
        return Err(ApiError::bad_request(
            "status must be one of: active, deprecated, retired",
        ));
    }
    if let Some(succ) = req.superseded_by.as_deref() {
        validate_coordinate(succ)?;
    }

    require_role(&state.db, &user, existing.workspace_id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    let base_id = existing.chain_root_id();
    let principal = user.subject_as_uuid();

    sqlx::query(
        "UPDATE workflow_templates \
            SET lifecycle_status = $2, superseded_by = $3, \
                updated_by = $4, updated_at = NOW() \
          WHERE COALESCE(base_template_id, id) = $1",
    )
    .bind(base_id)
    .bind(&req.status)
    .bind(&req.superseded_by)
    .bind(principal)
    .execute(&state.db)
    .await?;

    tracing::info!(
        template_id = %id,
        family = %base_id,
        status = %req.status,
        superseded_by = ?req.superseded_by,
        principal = %principal,
        "governance: set library node lifecycle"
    );

    require_template(&state.db, id).await.map(Json)
}

// ─── Phase 5: upgrade preview (contract diff) ────────────────────────────────

/// A single field-level change between two contract versions. `from_kind` /
/// `to_kind` are the serde wire names of the [`FieldKind`]; a retype carries
/// both, an add carries only `to_kind`, a remove only `from_kind`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_kind: Option<String>,
    /// Whether the field is required in the target version (drives the
    /// breaking-vs-compatible call for newly-added inputs).
    pub required: bool,
}

/// Field-level diff of a library node's input + output [`Port`] contracts
/// between two versions. Inputs drive the breaking classification (a consumer's
/// `input_mapping`s target these); outputs are informational (the join just
/// maps whatever the child returns).
#[derive(Debug, Serialize, ToSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContractDiff {
    pub input_added: Vec<FieldChange>,
    pub input_removed: Vec<FieldChange>,
    pub input_retyped: Vec<FieldChange>,
    pub output_added: Vec<FieldChange>,
    pub output_removed: Vec<FieldChange>,
    pub output_retyped: Vec<FieldChange>,
}

/// Result of comparing a pinned embed's version against the family's latest.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpgradePreview {
    pub coordinate: String,
    pub from_version: i32,
    pub to_version: i32,
    /// `up_to_date` (already on latest), `compatible` (drop-in), or `breaking`
    /// (a consumed input was removed/retyped, or a new required input appeared —
    /// the consumer's input mappings need attention before adopting).
    pub classification: String,
    pub contract_diff: ContractDiff,
    /// Input field names a consumer must revisit on upgrade: removed, retyped,
    /// or newly-required-added. The editor cross-references these against the
    /// embedding node's `inputMapping` to flag exactly which rows to remap.
    pub affected_input_fields: Vec<String>,
}

/// Query params for `GET /api/v1/library/upgrade-preview`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UpgradePreviewQuery {
    /// Library node coordinate (`vendor/slug`). Query param rather than path
    /// because coordinates contain a slash.
    pub coordinate: String,
    /// The version the consumer is currently pinned to.
    pub from: i32,
}

/// Serde wire name of a [`FieldKind`] (e.g. `"text"`, `"json"`), for the diff.
fn kind_wire(kind: FieldKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Diff two ports into (added, removed, retyped) field changes, matched by name.
fn diff_ports(from: &Port, to: &Port) -> (Vec<FieldChange>, Vec<FieldChange>, Vec<FieldChange>) {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut retyped = Vec::new();

    for tf in &to.fields {
        match from.fields.iter().find(|ff| ff.name == tf.name) {
            None => added.push(FieldChange {
                name: tf.name.clone(),
                from_kind: None,
                to_kind: Some(kind_wire(tf.kind)),
                required: tf.required,
            }),
            Some(ff) if ff.kind != tf.kind => retyped.push(FieldChange {
                name: tf.name.clone(),
                from_kind: Some(kind_wire(ff.kind)),
                to_kind: Some(kind_wire(tf.kind)),
                required: tf.required,
            }),
            Some(_) => {}
        }
    }
    for ff in &from.fields {
        if !to.fields.iter().any(|tf| tf.name == ff.name) {
            removed.push(FieldChange {
                name: ff.name.clone(),
                from_kind: Some(kind_wire(ff.kind)),
                to_kind: None,
                required: ff.required,
            });
        }
    }
    (added, removed, retyped)
}

/// GET /api/v1/library/upgrade-preview?coordinate=vendor/slug&from=N
///
/// Classify the upgrade from version `from` to the family's latest visible
/// version of a library node, by diffing the derived SubWorkflow input/output
/// contracts (`derive_child_io` — the same derivation the publish path freezes,
/// so the preview can't drift). Drives the editor's "vN+1 available" prompt and
/// tells it which input mappings a breaking change touches.
#[utoipa::path(
    get,
    path = "/api/v1/library/upgrade-preview",
    params(UpgradePreviewQuery),
    responses(
        (status = 200, description = "Upgrade classification + contract diff", body = UpgradePreview),
        (status = 404, description = "Library node / from-version not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "library",
)]
pub async fn library_upgrade_preview(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<UpgradePreviewQuery>,
) -> Result<Json<UpgradePreview>, ApiError> {
    let workspace_id = user.require_workspace()?;

    // Latest visible library version for the coordinate (own ws or public).
    let latest = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates \
          WHERE coordinate = $1 AND template_kind = 'library_node' AND is_latest \
            AND (workspace_id = $2 OR visibility = 'public') \
          ORDER BY version DESC LIMIT 1",
    )
    .bind(&q.coordinate)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found(format!("library node `{}` not found", q.coordinate)))?;

    let base_id = latest.chain_root_id();

    // Already on (or ahead of) latest → nothing to do.
    if q.from >= latest.version {
        return Ok(Json(UpgradePreview {
            coordinate: q.coordinate,
            from_version: q.from,
            to_version: latest.version,
            classification: "up_to_date".to_string(),
            contract_diff: ContractDiff::default(),
            affected_input_fields: Vec::new(),
        }));
    }

    // The version the consumer is pinned to — same family, explicit version.
    let from_row = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates \
          WHERE (base_template_id = $1 OR id = $1) AND version = $2",
    )
    .bind(base_id)
    .bind(q.from)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| {
        ApiError::not_found(format!(
            "version {} of `{}` not found",
            q.from, q.coordinate
        ))
    })?;

    // Derive both contracts from the frozen `graph` column — exactly what the
    // io-contract endpoint and publish-time resolution read, so the diff can
    // never drift from the contract that gets embedded.
    let parse = |t: &WorkflowTemplate| -> Result<WorkflowGraph, ApiError> {
        serde_json::from_value(t.graph.clone())
            .map_err(|e| ApiError::internal(format!("child graph is invalid: {e}")))
    };
    let (from_in, from_out) = derive_child_io(&parse(&from_row)?);
    let (to_in, to_out) = derive_child_io(&parse(&latest)?);

    let (input_added, input_removed, input_retyped) = diff_ports(&from_in, &to_in);
    let (output_added, output_removed, output_retyped) = diff_ports(&from_out, &to_out);

    // Breaking iff a consumed input was removed/retyped, or a NEW required input
    // appeared. New optional inputs and any output change are drop-in compatible.
    let breaking = !input_removed.is_empty()
        || !input_retyped.is_empty()
        || input_added.iter().any(|f| f.required);
    let classification = if breaking { "breaking" } else { "compatible" };

    let affected_input_fields = input_removed
        .iter()
        .chain(input_retyped.iter())
        .chain(input_added.iter().filter(|f| f.required))
        .map(|f| f.name.clone())
        .collect();

    Ok(Json(UpgradePreview {
        coordinate: q.coordinate,
        from_version: q.from,
        to_version: latest.version,
        classification: classification.to_string(),
        contract_diff: ContractDiff {
            input_added,
            input_removed,
            input_retyped,
            output_added,
            output_removed,
            output_retyped,
        },
        affected_input_fields,
    }))
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

    fn fld(name: &str, kind: FieldKind, required: bool) -> crate::models::template::PortField {
        crate::models::template::PortField {
            name: name.to_string(),
            label: name.to_string(),
            kind,
            required,
            default: None,
            options: None,
            description: None,
            accept: None,
            schema: None,
        }
    }

    fn port(fields: Vec<crate::models::template::PortField>) -> Port {
        Port {
            id: "in".to_string(),
            label: "In".to_string(),
            fields,
        }
    }

    #[test]
    fn diff_detects_add_remove_retype() {
        let from = port(vec![
            fld("keep", FieldKind::Text, false),
            fld("gone", FieldKind::Text, false),
            fld("shift", FieldKind::Text, false),
        ]);
        let to = port(vec![
            fld("keep", FieldKind::Text, false),
            fld("shift", FieldKind::Number, false), // retyped
            fld("fresh", FieldKind::Text, false),   // added
        ]);
        let (added, removed, retyped) = diff_ports(&from, &to);
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].name, "fresh");
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].name, "gone");
        assert_eq!(retyped.len(), 1);
        assert_eq!(retyped[0].name, "shift");
        assert_eq!(retyped[0].from_kind.as_deref(), Some("text"));
        assert_eq!(retyped[0].to_kind.as_deref(), Some("number"));
    }

    #[test]
    fn classification_breaking_vs_compatible() {
        // Adding an OPTIONAL field is compatible.
        let base = port(vec![fld("a", FieldKind::Text, false)]);
        let add_opt = port(vec![
            fld("a", FieldKind::Text, false),
            fld("b", FieldKind::Text, false),
        ]);
        let (added, removed, retyped) = diff_ports(&base, &add_opt);
        let breaking =
            !removed.is_empty() || !retyped.is_empty() || added.iter().any(|f| f.required);
        assert!(!breaking, "optional add is compatible");

        // Adding a REQUIRED field is breaking.
        let add_req = port(vec![
            fld("a", FieldKind::Text, false),
            fld("b", FieldKind::Text, true),
        ]);
        let (added, _, _) = diff_ports(&base, &add_req);
        assert!(added.iter().any(|f| f.required), "required add is breaking");

        // Removing a field is breaking.
        let (_, removed, _) = diff_ports(&base, &port(vec![]));
        assert!(!removed.is_empty(), "remove is breaking");

        // Retype is breaking.
        let (_, _, retyped) = diff_ports(&base, &port(vec![fld("a", FieldKind::Number, false)]));
        assert!(!retyped.is_empty(), "retype is breaking");
    }

    #[test]
    fn lifecycle_status_validation() {
        assert!(valid_lifecycle_status("active"));
        assert!(valid_lifecycle_status("deprecated"));
        assert!(valid_lifecycle_status("retired"));
        assert!(!valid_lifecycle_status("archived"));
        assert!(!valid_lifecycle_status(""));
    }
}
