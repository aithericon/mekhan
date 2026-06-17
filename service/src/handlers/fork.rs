//! Fork-to-workspace — deep-copy a shared (public / cross-workspace) template,
//! or a whole folder subtree, into the caller's active workspace.
//!
//! The motivating case is the system-owned `demos` workspace: its templates are
//! `visibility = 'public'` so every tenant can *discover* them, but a run's
//! tenancy is the template's workspace (`list_instances` scopes on
//! `wt.workspace_id`, instances carry no workspace_id of their own). A user who
//! isn't a member of `demos` therefore can't launch a demo in place. Forking
//! resolves that cleanly: it copies the definition into a workspace the caller
//! *does* own, so the fork — and every run of it — belongs to the caller's
//! tenant. Isolation is preserved; nothing is shared by reference.
//!
//! This mirrors `governance::fork_library_node` (the same Y.Doc-aware deep copy)
//! but generalises it: the fork is born **published & runnable** (it copies
//! `air_json` / `published`) rather than an editable draft, because the goal
//! here is "fork a demo so I can run it", not "drop a building block on a
//! canvas". The owner can still `new-version` to edit.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::{
    can_read_template, map_to_api_error, resolve_fork_target, AuthUser,
};
use crate::handlers::require_template;
use crate::handlers::templates::graph_with_ydoc_fallback;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{WorkflowGraph, WorkflowTemplate};
use crate::models::workspace::Folder;
use crate::AppState;

/// Optional placement for a single-template fork. Absent ⇒ the fork lands at the
/// caller's workspace root.
#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
pub struct ForkTemplateRequest {
    /// Workspace to fork INTO (must be one the caller can write). Absent ⇒ the
    /// active workspace when writable, else the caller's first writable
    /// workspace — so forking while *browsing* the read-only demos workspace
    /// still lands the copy somewhere the caller owns.
    #[serde(default)]
    pub target_workspace_id: Option<Uuid>,
    /// Home the fork in this folder of the target workspace. Must be a folder in
    /// that workspace. Absent ⇒ workspace root.
    #[serde(default)]
    pub folder_id: Option<Uuid>,
}

/// POST /api/v1/templates/{id}/fork
///
/// Deep-copy a readable template (the caller's own workspace OR any `public`
/// template, e.g. a built-in demo) into a fresh, runnable family in the caller's
/// active workspace. Returns the new template (201).
#[utoipa::path(
    post,
    path = "/api/v1/templates/{id}/fork",
    params(("id" = Uuid, Path, description = "Template id to fork (any readable version)")),
    request_body = ForkTemplateRequest,
    responses(
        (status = 201, description = "Forked into a runnable workspace template", body = WorkflowTemplate),
        (status = 403, description = "Caller cannot read the source / cannot create in their workspace", body = ErrorResponse),
        (status = 404, description = "Template not found / not readable", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn fork_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<ForkTemplateRequest>>,
) -> Result<(StatusCode, Json<WorkflowTemplate>), ApiError> {
    let req = body.map(|Json(r)| r).unwrap_or_default();

    // Resolve a workspace the caller can write into (the active one may be the
    // read-only demos workspace they're browsing). 400 when they have nowhere.
    let target_ws = resolve_fork_target(
        &state.db,
        &user,
        req.target_workspace_id,
        user.workspace_id,
    )
    .await
    .map_err(map_to_api_error)?
    .ok_or_else(|| ApiError::bad_request("no writable workspace to fork into"))?;

    // Read gate: the source must be readable — the caller's own workspace OR a
    // public definition. `can_read_template` is the single source of truth for
    // that rule (404 when the row doesn't exist).
    let source = require_template(&state.db, id).await?;
    if !can_read_template(&state.db, &user, id)
        .await
        .map_err(map_to_api_error)?
    {
        return Err(ApiError::not_found("template not found"));
    }

    // Validate the optional target folder belongs to the caller's workspace
    // before doing any copying.
    if let Some(folder_id) = req.folder_id {
        require_folder_in_workspace(&state, folder_id, target_ws).await?;
    }

    let forked = fork_one_template(&state, &source, target_ws, user.subject_as_uuid()).await?;

    if let Some(folder_id) = req.folder_id {
        home_template_in_folder(&state, forked.id, folder_id, target_ws, user.subject_as_uuid())
            .await?;
    }

    tracing::info!(
        new_template_id = %forked.id,
        source_template_id = %id,
        workspace = %target_ws,
        "fork: deep-copied template into workspace"
    );

    Ok((StatusCode::CREATED, Json(forked)))
}

/// POST /api/v1/folders/{id}/fork
///
/// Deep-copy a folder *subtree* into the caller's active workspace: recreate the
/// folder and its descendants, and fork every readable template homed anywhere
/// in the subtree into the matching new folder. The recreated subtree is nested
/// under a fresh root folder in the target workspace (collision-suffixed) so it
/// never disturbs the caller's existing tree. Returns the new root folder (201).
#[utoipa::path(
    post,
    path = "/api/v1/folders/{id}/fork",
    params(("id" = Uuid, Path, description = "Folder id to fork (any readable workspace)")),
    responses(
        (status = 201, description = "Folder subtree forked into the workspace", body = ForkFolderResponse),
        (status = 403, description = "Caller cannot create in their workspace", body = ErrorResponse),
        (status = 404, description = "Folder not found / contains nothing readable", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "folders",
)]
pub async fn fork_folder(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<ForkFolderResponse>), ApiError> {
    let principal = user.subject_as_uuid();
    // Fork into a workspace the caller can write (the active one may be the
    // read-only demos workspace they're browsing).
    let target_ws = resolve_fork_target(&state.db, &user, None, user.workspace_id)
        .await
        .map_err(map_to_api_error)?
        .ok_or_else(|| ApiError::bad_request("no writable workspace to fork into"))?;

    // Load the source folder (no membership gate on its workspace — a public
    // demos folder lives in a workspace the caller isn't a member of). Read
    // access is enforced per-template below via `can_read_template`: only
    // readable templates are copied.
    let source: Folder = sqlx::query_as(
        "SELECT id, workspace_id, parent_id, slug, display_name, description, path, \
                created_at, created_by, updated_at, updated_by \
           FROM folders WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("folder not found"))?;

    // Gather the subtree (the folder itself + every descendant) by materialized
    // path, deepest last so parents are always created before their children.
    let subtree: Vec<Folder> = sqlx::query_as(
        "SELECT id, workspace_id, parent_id, slug, display_name, description, path, \
                created_at, created_by, updated_at, updated_by \
           FROM folders \
          WHERE workspace_id = $1 AND (id = $2 OR path LIKE $3 || '/%') \
          ORDER BY length(path)",
    )
    .bind(source.workspace_id)
    .bind(source.id)
    .bind(&source.path)
    .fetch_all(&state.db)
    .await?;

    // Pick a collision-free root slug in the target workspace.
    let root_slug = unique_root_slug(&state, target_ws, &source.slug).await?;

    let mut tx = state.db.begin().await?;

    // Recreate the subtree. `id_map[source_folder_id] = new_folder_id`;
    // `path_map[source_path] = new_path` lets each child resolve its new parent
    // path. The source root is remapped to use `root_slug` at the workspace root.
    let mut id_map: std::collections::HashMap<Uuid, Uuid> = std::collections::HashMap::new();
    let mut path_map: std::collections::HashMap<String, (Uuid, String)> =
        std::collections::HashMap::new();

    for folder in &subtree {
        let is_root = folder.id == source.id;
        let (slug, parent_id, new_path) = if is_root {
            let p = format!("/{root_slug}");
            (root_slug.clone(), None, p)
        } else {
            // Parent path = strip this folder's own trailing `/slug`. Its remap
            // is already in `path_map` (parents sort before children by length).
            let parent_path = folder
                .path
                .rsplit_once('/')
                .map(|(head, _)| head.to_string())
                .unwrap_or_default();
            let (parent_new_id, parent_new_path) = path_map
                .get(&parent_path)
                .cloned()
                .ok_or_else(|| ApiError::internal("fork: parent folder missing in remap"))?;
            let p = format!("{parent_new_path}/{}", folder.slug);
            (folder.slug.clone(), Some(parent_new_id), p)
        };

        let new_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO folders \
                (id, workspace_id, parent_id, slug, display_name, description, path, created_by, updated_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
        )
        .bind(new_id)
        .bind(target_ws)
        .bind(parent_id)
        .bind(&slug)
        .bind(&folder.display_name)
        .bind(&folder.description)
        .bind(&new_path)
        .bind(principal)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::internal(format!("fork: failed to create folder: {e}")))?;

        id_map.insert(folder.id, new_id);
        path_map.insert(folder.path.clone(), (new_id, new_path));
    }

    tx.commit().await?;

    // Fork every readable, latest template homed anywhere in the source subtree,
    // homing each into the corresponding new folder. Template forks each seed a
    // Y.Doc (own connection) so they run outside the folder transaction.
    let subtree_ids: Vec<Uuid> = subtree.iter().map(|f| f.id).collect();
    let homed: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT wt.id, tf.folder_id \
           FROM template_folders tf \
           JOIN workflow_templates wt \
             ON wt.base_template_id = tf.base_template_id AND wt.is_latest \
          WHERE tf.folder_id = ANY($1)",
    )
    .bind(&subtree_ids)
    .fetch_all(&state.db)
    .await?;

    let mut forked_templates = 0u32;
    for (template_id, src_folder_id) in homed {
        if !can_read_template(&state.db, &user, template_id)
            .await
            .map_err(map_to_api_error)?
        {
            continue;
        }
        let Some(source_row) = sqlx::query_as::<_, WorkflowTemplate>(
            "SELECT * FROM workflow_templates WHERE id = $1",
        )
        .bind(template_id)
        .fetch_optional(&state.db)
        .await?
        else {
            continue;
        };
        // Skip non-runnable kinds (library nodes are forked via their own
        // coordinate endpoint; private sub-workflows ride embedded).
        if source_row.visibility == "private" {
            continue;
        }
        let new_folder_id = match id_map.get(&src_folder_id) {
            Some(fid) => *fid,
            None => continue,
        };
        let forked = fork_one_template(&state, &source_row, target_ws, principal).await?;
        home_template_in_folder(&state, forked.id, new_folder_id, target_ws, principal).await?;
        forked_templates += 1;
    }

    let new_root_id = *id_map
        .get(&source.id)
        .ok_or_else(|| ApiError::internal("fork: root folder missing in remap"))?;

    tracing::info!(
        new_root_folder_id = %new_root_id,
        source_folder_id = %id,
        workspace = %target_ws,
        folders = subtree.len(),
        templates = forked_templates,
        "fork: deep-copied folder subtree into workspace"
    );

    Ok((
        StatusCode::CREATED,
        Json(ForkFolderResponse {
            folder_id: new_root_id,
            workspace_id: target_ws,
            folders: subtree.len() as u32,
            templates: forked_templates,
        }),
    ))
}

/// Result of a folder fork — the new root folder plus how much it brought in.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ForkFolderResponse {
    /// Id of the new root folder created in the target workspace.
    pub folder_id: Uuid,
    /// Workspace the subtree was forked INTO (may differ from the active one
    /// when forking while browsing the read-only demos workspace).
    pub workspace_id: Uuid,
    /// Folders created (the source subtree size).
    pub folders: u32,
    /// Templates deep-copied into the new subtree.
    pub templates: u32,
}

// ─── shared internals ────────────────────────────────────────────────────────

/// Deep-copy one template row into `target_ws`, owned by `principal`, as a fresh
/// runnable family (new chain root, `version = 1`, `is_latest`). Copies the
/// authored graph from the source Y.Doc (not the stale `graph` column) and seeds
/// a Y.Doc for the fork so collaborative editing works immediately. `coordinate`
/// / `origin` / `pack_id` are deliberately dropped — a fork is plain workspace
/// content, never a coordinate-bearing library node (and copying `coordinate`
/// would collide on `uq_workflow_templates_origin_coordinate`).
async fn fork_one_template(
    state: &AppState,
    source: &WorkflowTemplate,
    target_ws: Uuid,
    principal: Uuid,
) -> Result<WorkflowTemplate, ApiError> {
    // Authored graph lives in the Y.Doc; the column is never written back on
    // edit/publish, so read the doc (legacy rows fall back to the column).
    let (graph, files) = graph_with_ydoc_fallback(state, source.id, source.graph.clone(), |g| {
        Ok(serde_json::from_value::<WorkflowGraph>(g).unwrap_or_else(|_| WorkflowGraph::default_graph()))
    })
    .await?;
    let graph_json = serde_json::to_value(&graph).map_err(|e| ApiError::internal(e.to_string()))?;

    let new_id = Uuid::new_v4();
    let forked_from = serde_json::json!({
        "template_id": source.chain_root_id(),
        "version": source.version,
        "workspace_id": source.workspace_id,
    });

    // Born `published` exactly when the source was, carrying its compiled AIR +
    // interface so the fork is immediately launchable. A `workspace`-visibility
    // `workflow` the caller owns; `forked_from` records provenance.
    let published = source.published;
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        INSERT INTO workflow_templates
            (id, name, description, base_template_id, version, is_latest, published,
             published_at, published_by, graph, air_json, interface_json, author_id,
             workspace_id, visibility, presentation, forked_from, template_kind,
             lifecycle_status, updated_by)
        VALUES ($1, $2, $3, $1, 1, TRUE, $4,
                CASE WHEN $4 THEN NOW() ELSE NULL END,
                CASE WHEN $4 THEN $5 ELSE NULL END,
                $6, $7, $8, $5, $9, 'workspace', $10, $11, 'workflow', 'active', $5)
        RETURNING *
        "#,
    )
    .bind(new_id)
    .bind(&source.name)
    .bind(&source.description)
    .bind(published)
    .bind(principal)
    .bind(&graph_json)
    .bind(&source.air_json)
    .bind(&source.interface_json)
    .bind(target_ws)
    .bind(&source.presentation)
    .bind(&forked_from)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("failed to fork template: {e}");
        ApiError::internal(e.to_string())
    })?;

    // Seed the Y.Doc for the new family so collaborative editing works
    // immediately, including the copied per-node files. Non-fatal: the row
    // exists; the Y.Doc can be re-initialized later.
    if let Err(e) = state
        .yjs
        .persistence
        .init_doc_from_graph_with_files(new_id, &graph, &files)
        .await
    {
        tracing::error!("failed to init Y.Doc for forked template {new_id}: {e}");
    }

    Ok(template)
}

/// Home a (just-forked) template in a folder of the target workspace. Keys on
/// the template's chain root, matching `set_template_folder`.
async fn home_template_in_folder(
    state: &AppState,
    template_id: Uuid,
    folder_id: Uuid,
    workspace_id: Uuid,
    principal: Uuid,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO template_folders (base_template_id, folder_id, workspace_id, moved_by) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (base_template_id) DO UPDATE SET folder_id = $2, moved_by = $4, moved_at = NOW()",
    )
    .bind(template_id)
    .bind(folder_id)
    .bind(workspace_id)
    .bind(principal)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("fork: failed to home template: {e}")))?;
    Ok(())
}

/// Verify a folder exists in `workspace_id` (a fork only homes into folders of
/// the caller's own workspace).
async fn require_folder_in_workspace(
    state: &AppState,
    folder_id: Uuid,
    workspace_id: Uuid,
) -> Result<(), ApiError> {
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM folders WHERE id = $1 AND workspace_id = $2")
            .bind(folder_id)
            .bind(workspace_id)
            .fetch_optional(&state.db)
            .await?;
    exists
        .map(|_| ())
        .ok_or_else(|| ApiError::bad_request("target folder is not in your workspace"))
}

/// A root-level folder slug in `workspace_id` that doesn't collide. Tries the
/// source slug first, then `slug-2`, `slug-3`, … (the materialized path of a
/// root folder is `/slug`, unique per workspace).
async fn unique_root_slug(
    state: &AppState,
    workspace_id: Uuid,
    base: &str,
) -> Result<String, ApiError> {
    for n in 1..=50 {
        let candidate = if n == 1 {
            base.to_string()
        } else {
            format!("{base}-{n}")
        };
        let taken: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM folders WHERE workspace_id = $1 AND parent_id IS NULL AND slug = $2",
        )
        .bind(workspace_id)
        .bind(&candidate)
        .fetch_optional(&state.db)
        .await?;
        if taken.is_none() {
            return Ok(candidate);
        }
    }
    Err(ApiError::internal(
        "fork: could not find a free folder slug after 50 tries",
    ))
}
