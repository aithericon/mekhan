use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::{
    annotate_roles_keep_all, map_to_api_error, require_object_role, AuthUser, ObjectKind,
    ObjectRef, Role,
};
use crate::compiler::{
    compile_to_air, compile_to_air_with_options, derive_child_io, generate_py_io_files,
    node_files_inline, node_files_storage_path, node_input_scopes, node_namespace_scopes,
    node_output_fields, CompileOptions, TyDescriptor,
};
use crate::handlers::require_template;
use crate::handlers::template_tests::{run_test, RunContext};
use crate::lifecycle::cleanup_net;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{
    ApplyAirTemplateRequest, ApplyTemplateRequest, CompileRequest, CreateTemplateRequest,
    DiscardDraftResponse, ExecutionBackendType, Port, Position, Presentation, TemplateListExtras,
    UpdateTemplateRequest, WorkflowGraph, WorkflowNode, WorkflowNodeData, WorkflowTemplate,
    WorkflowTemplateSummary,
};
use crate::models::template_test::{FailingTestInfo, PublishGateBlockedResponse, TemplateTest};
use crate::process::publish::{
    resolve_subworkflow_air, ArtifactKeySpace, CompiledArtifacts, PublishService,
};
use crate::query::builder::{self, QueryError};
use crate::query::extractor::QueryParams;
use crate::query::pagination::Paginated;
use crate::AppState;

/// Direct columns on `workflow_templates` exposed to the generic
/// `filter[field][op]=value` DSL. Anything not in this whitelist is rejected.
const TEMPLATE_FILTER_FIELDS: &[&str] = &[
    "id",
    "name",
    "description",
    "version",
    "published",
    "visibility",
    "author_id",
    "created_at",
    "updated_at",
    "published_at",
];

/// Columns the list may be sorted by (`sort=-updated_at`, `sort=name`, …).
const TEMPLATE_SORT_FIELDS: &[&str] = &[
    "name",
    "version",
    "created_at",
    "updated_at",
    "published_at",
];

/// Map a query-builder error to the right HTTP shape: bad DSL → 400,
/// underlying DB failure → 500 (don't leak it as a client error).
fn query_err_to_api(e: QueryError) -> ApiError {
    match e {
        QueryError::Database(db) => {
            tracing::error!("templates list db error: {db}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        }
        other => ApiError::bad_request(other.to_string()),
    }
}

/// Visibility-aware read gate: passes when the template is `public` OR the
/// caller is at least a `viewer` member of the template's workspace. Maps
/// the underlying membership errors to standard `ApiError` shapes.
async fn gate_template_read(
    state: &AppState,
    user: &AuthUser,
    template: &WorkflowTemplate,
) -> Result<(), ApiError> {
    if template.visibility == "public" {
        return Ok(());
    }
    // Object-ACL: effective role ≥ Viewer (workspace floor + folder/object
    // grants). A folder-scoped Viewer grant now reads a template their active
    // workspace doesn't otherwise expose.
    require_object_role(
        &state.db,
        user,
        ObjectRef::template(template.id),
        Role::Viewer,
    )
    .await
    .map_err(map_to_api_error)
    .map(|_| ())
}

/// Write gate for mutate paths (update/delete/publish): requires the caller's
/// effective role on the template (workspace floor + folder/object grants) to
/// be at least `editor`. Public visibility does NOT grant write — cross-
/// workspace reads of public templates are read-only by design.
async fn gate_template_write(
    state: &AppState,
    user: &AuthUser,
    template: &WorkflowTemplate,
) -> Result<(), ApiError> {
    require_object_role(
        &state.db,
        user,
        ObjectRef::template(template.id),
        Role::Editor,
    )
    .await
    .map_err(map_to_api_error)
    .map(|_| ())
}

/// Fill `my_effective_role` on a page of templates with one role-resolution
/// query. Keyed by the per-version row id (the resolver collapses to the chain
/// root internally), so each row gets the caller's effective role for the
/// SPA's edit-affordance hinting.
async fn annotate_template_roles<T: crate::auth::AclAnnotated>(
    state: &AppState,
    user: &AuthUser,
    workspace_id: Uuid,
    items: &mut [T],
) -> Result<(), ApiError> {
    // Keep-all on purpose: a template only becomes restricted via an ancestor
    // folder, and detail access is gated by `require_object_role`.
    annotate_roles_keep_all(&state.db, user, ObjectKind::Template, workspace_id, items)
        .await
        .map_err(map_to_api_error)
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct PublishQuery {
    /// Bypass the template-test gate. Failing or stale tests do not block
    /// publish when `true`; an audit-level log records the override. Use
    /// only when a test itself is broken and you need to ship.
    #[serde(default)]
    pub force: bool,
}

/// POST /api/v1/templates
#[utoipa::path(
    post,
    path = "/api/v1/templates",
    request_body = CreateTemplateRequest,
    responses(
        (status = 201, description = "Template created", body = WorkflowTemplate),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn create_template(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateTemplateRequest>,
) -> Result<(StatusCode, Json<WorkflowTemplate>), ApiError> {
    let id = Uuid::new_v4();
    let graph = req.graph.unwrap_or_else(WorkflowGraph::default_graph);
    let graph_json = serde_json::to_value(&graph).map_err(|e| ApiError::internal(e.to_string()))?;
    let description = req.description.unwrap_or_default();

    // Anchor the new template in the caller's active workspace; reject (403)
    // rather than creating in the nil tenant when the caller has no active
    // workspace.
    let workspace_id = user.require_workspace()?;

    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        INSERT INTO workflow_templates (id, name, description, base_template_id, version, is_latest, graph, author_id, workspace_id, updated_by)
        VALUES ($1, $2, $3, $1, 1, TRUE, $4, $5, $6, $5)
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(&req.name)
    .bind(&description)
    .bind(&graph_json)
    .bind(user.subject_as_uuid())
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("failed to create template: {e}");
        ApiError::internal(e.to_string())
    })?;

    // Initialize Y.Doc from the graph for real-time collaboration. Seed any
    // inline files the caller supplied so the template lands ready-to-publish
    // for Python and other file-bearing backends.
    let file_summary: Vec<String> = req
        .files
        .iter()
        .map(|(node_id, files)| {
            format!(
                "{node_id}=[{}]",
                files.keys().cloned().collect::<Vec<_>>().join(",")
            )
        })
        .collect();
    tracing::info!(
        template_id = %id,
        name = %req.name,
        files = %file_summary.join("; "),
        "seeding template files into Y.Doc"
    );
    if let Err(e) = state
        .yjs
        .persistence
        .init_doc_from_graph_with_files(id, &graph, &req.files)
        .await
    {
        tracing::error!("failed to init Y.Doc for template {id}: {e}");
        // Non-fatal: template is created, Y.Doc can be initialized later
    }

    Ok((StatusCode::CREATED, Json(template)))
}

/// Append the shared FROM-tail for the latest-version template listing:
/// optional folder/tag JOINs, the mandatory workspace+visibility gate, the
/// private-children rule, the generic typed filters, and free-text search.
/// Used identically by the COUNT and the SELECT query so the two can't drift.
fn append_template_where(
    qb: &mut sqlx::QueryBuilder<'_, sqlx::Postgres>,
    workspace_id: Uuid,
    extras: &TemplateListExtras,
    params: &QueryParams,
) -> Result<(), QueryError> {
    if let Some(folder_id) = extras.folder_id {
        // Direct membership: the template's home folder is exactly the selected
        // folder. Recursive: the home folder is the selected folder OR any
        // descendant, resolved by materialized-path prefix via a self-join on
        // `folders` (so the caller need not pre-resolve the path).
        qb.push(
            " JOIN template_folders tf ON tf.base_template_id = COALESCE(t.base_template_id, t.id)",
        );
        if extras.recursive {
            qb.push(" JOIN folders f ON f.id = tf.folder_id");
            qb.push(" JOIN folders sel ON sel.id = ");
            qb.push_bind(folder_id);
            qb.push(" AND (f.path = sel.path OR f.path LIKE sel.path || '/%')");
        } else {
            qb.push(" AND tf.folder_id = ");
            qb.push_bind(folder_id);
        }
    }
    if let Some(ref tag) = extras.tag {
        qb.push(" JOIN template_tags tt ON tt.base_template_id = COALESCE(t.base_template_id, t.id) AND tt.workspace_id = ");
        qb.push_bind(workspace_id);
        qb.push(" AND tt.tag = ");
        qb.push_bind(tag.clone());
    }

    qb.push(" WHERE t.is_latest = TRUE AND (t.workspace_id = ");
    qb.push_bind(workspace_id);
    qb.push(" OR t.visibility = 'public')");

    // Private sub-workflows are hidden unless explicitly enumerated by their
    // owning parent family.
    match extras.owner_template_id {
        Some(owner) => {
            qb.push(" AND t.owner_template_id = ");
            qb.push_bind(owner);
        }
        None => {
            qb.push(" AND t.visibility <> 'private'");
        }
    }

    // Generic typed filters (e.g. filter[published][eq]=true), prefixed to the
    // joined `t` alias and validated against the column whitelist.
    if let Some(ref filter) = params.filter {
        if !filter.is_empty() {
            qb.push(" AND ");
            builder::build_where_conditions_with_prefix(
                qb,
                filter,
                TEMPLATE_FILTER_FIELDS,
                Some("t."),
            )?;
        }
    }

    // Free-text search across name + description (OR — can't be expressed via
    // the AND-only typed filter DSL, so it stays a dedicated param).
    if let Some(ref search) = params.search {
        let pattern = format!("%{search}%");
        qb.push(" AND (t.name ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR t.description ILIKE ");
        qb.push_bind(pattern);
        qb.push(")");
    }

    Ok(())
}

/// Two correlated `text[]` subquery columns (`io_inputs`, `io_outputs`) that
/// compute a template's compact I/O preview straight from its `graph` JSONB —
/// Start-node input field names and deduped End-node result-mapping targets.
/// This is the Rust/SQL mirror of the frontend's old `summarize(graph)` pass;
/// doing it server-side lets `GET /api/v1/templates` keep the per-card I/O
/// badges WITHOUT shipping the whole graph to the browser. `graph_expr` is the
/// table-qualified column (`"graph"` or `"t.graph"`). The `jsonb_typeof` guard
/// keeps a malformed node from erroring the whole list. Node shape mirrors
/// `WorkflowNodeData` (serde `tag = "type"`, camelCase: `initial.fields[].name`,
/// `resultMapping[].targetField`).
fn io_summary_cols(graph_expr: &str) -> String {
    format!(
        "COALESCE((SELECT array_agg(f->>'name') \
            FROM jsonb_array_elements({g}->'nodes') n \
            CROSS JOIN LATERAL jsonb_array_elements( \
              CASE WHEN n->'data'->>'type' = 'start' \
                     AND jsonb_typeof(n->'data'->'initial'->'fields') = 'array' \
                   THEN n->'data'->'initial'->'fields' ELSE '[]'::jsonb END) f \
            WHERE f->>'name' IS NOT NULL), ARRAY[]::text[]) AS io_inputs, \
         COALESCE((SELECT array_agg(DISTINCT m->>'targetField') \
            FROM jsonb_array_elements({g}->'nodes') n \
            CROSS JOIN LATERAL jsonb_array_elements( \
              CASE WHEN n->'data'->>'type' = 'end' \
                     AND jsonb_typeof(n->'data'->'resultMapping') = 'array' \
                   THEN n->'data'->'resultMapping' ELSE '[]'::jsonb END) m \
            WHERE m->>'targetField' IS NOT NULL), ARRAY[]::text[]) AS io_outputs",
        g = graph_expr
    )
}

/// GET /api/v1/templates
///
/// Latest-version catalogue listing driven by the generic list DSL:
///   - `page`, `page_size` — pagination (0-based)
///   - `sort` — e.g. `-updated_at`, `name`, `version`
///   - `search` — free-text across name + description
///   - `filter[field][op]=value` — typed filters over direct columns
///     (published, version, visibility, created_at, …)
///
/// plus the template-specific relational/security params in
/// [`TemplateListExtras`] (`folder_id` + `recursive`, `tag`,
/// `base_template_id`, `owner_template_id`).
#[utoipa::path(
    get,
    path = "/api/v1/templates",
    params(TemplateListExtras),
    responses(
        (status = 200, description = "Paginated list of template summaries", body = Paginated<WorkflowTemplateSummary>),
        (status = 400, description = "Invalid query DSL", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn list_templates(
    State(state): State<AppState>,
    user: AuthUser,
    params: QueryParams,
    Query(extras): Query<TemplateListExtras>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use sqlx::{Postgres, QueryBuilder};

    // Summary projection — every `WorkflowTemplateSummary` column (i.e. all of
    // `WorkflowTemplate` MINUS the heavy `graph`/`air_json`/`interface_json`/
    // `source_ref` JSONB blobs the detail endpoint serves). Selecting these
    // explicitly instead of `*`/`t.*` keeps a 20-row page from dragging the
    // full compiled graphs over the wire (~20 MB → a few KB).
    const SUMMARY_COLS: &str = "id, name, description, base_template_id, parent_id, version, \
        is_latest, published, published_at, published_by, author_id, updated_by, created_at, \
        updated_at, workspace_id, visibility, owner_template_id";

    let workspace_id = user.require_workspace()?;

    // The version-chain listing (base_template_id != None) is a separate
    // mode: it shows every version of a template chain regardless of
    // is_latest. Workspace gate still applies — but on the chain root's
    // workspace (versions inherit it, since `new_version` keeps the same
    // workspace_id by default per the DB column DEFAULT).
    if let Some(base_id) = extras.base_template_id {
        let mut items = sqlx::query_as::<_, WorkflowTemplateSummary>(&format!(
            "SELECT {SUMMARY_COLS}, {io} FROM workflow_templates \
              WHERE base_template_id = $1 \
                AND (workspace_id = $2 OR visibility = 'public') \
              ORDER BY version DESC LIMIT $3 OFFSET $4",
            io = io_summary_cols("graph")
        ))
        .bind(base_id)
        .bind(workspace_id)
        .bind(params.page.limit())
        .bind(params.page.offset())
        .fetch_all(&state.db)
        .await
        .map_err(|e| query_err_to_api(QueryError::Database(e)))?;

        let total: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM workflow_templates \
              WHERE base_template_id = $1 \
                AND (workspace_id = $2 OR visibility = 'public')",
        )
        .bind(base_id)
        .bind(workspace_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| query_err_to_api(QueryError::Database(e)))?;

        annotate_template_roles(&state, &user, workspace_id, &mut items).await?;
        let resp = Paginated::new(items, total.0, &params.page);
        return Ok(Json(
            serde_json::to_value(resp).unwrap_or_else(|_| serde_json::json!({})),
        ));
    }

    // Latest-version listing with composable filter / sort / pagination. The
    // COUNT and SELECT share `append_template_where` so their predicates can't
    // drift; the workspace clause is mandatory.
    let count: i64 = {
        let mut qb =
            QueryBuilder::<Postgres>::new("SELECT COUNT(*)::bigint FROM workflow_templates t");
        append_template_where(&mut qb, workspace_id, &extras, &params).map_err(query_err_to_api)?;
        qb.build_query_as::<(i64,)>()
            .fetch_one(&state.db)
            .await
            .map_err(|e| query_err_to_api(QueryError::Database(e)))?
            .0
    };

    let mut items: Vec<WorkflowTemplateSummary> = {
        // `t.`-prefixed summary projection (the `FROM ... t` alias is shared
        // with the COUNT + `append_template_where` predicates).
        let select = format!(
            "SELECT {}, {io} FROM workflow_templates t",
            SUMMARY_COLS
                .split(", ")
                .map(|c| format!("t.{c}"))
                .collect::<Vec<_>>()
                .join(", "),
            io = io_summary_cols("t.graph")
        );
        let mut qb = QueryBuilder::<Postgres>::new(select);
        append_template_where(&mut qb, workspace_id, &extras, &params).map_err(query_err_to_api)?;
        match params.sort {
            Some(ref sort) => {
                builder::build_order_by_with_prefix(
                    &mut qb,
                    sort,
                    TEMPLATE_SORT_FIELDS,
                    Some("t."),
                )
                .map_err(query_err_to_api)?;
            }
            None => {
                qb.push(" ORDER BY t.updated_at DESC");
            }
        }
        builder::build_pagination(&mut qb, &params.page);
        qb.build_query_as::<WorkflowTemplateSummary>()
            .fetch_all(&state.db)
            .await
            .map_err(|e| query_err_to_api(QueryError::Database(e)))?
    };

    annotate_template_roles(&state, &user, workspace_id, &mut items).await?;

    let resp = Paginated::new(items, count, &params.page);
    Ok(Json(
        serde_json::to_value(resp).unwrap_or_else(|_| serde_json::json!({})),
    ))
}

/// GET /api/v1/templates/{id}
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}",
    params(("id" = Uuid, Path, description = "Template id")),
    responses(
        (status = 200, description = "Template", body = WorkflowTemplate),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn get_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    let mut template = require_template(&state.db, id).await?;

    gate_template_read(&state, &user, &template).await?;
    let ws = template.workspace_id;
    annotate_template_roles(&state, &user, ws, std::slice::from_mut(&mut template)).await?;
    Ok(Json(template))
}

/// SubWorkflow input/output contract derived from a child template's graph.
/// Mirrors exactly what the publish path freezes (see
/// [`crate::compiler::derive_child_io`]): `input` is the child's Start
/// `initial` port, `output` is the union of its End `result_mapping` targets
/// (Json-typed). The SubWorkflow editor reads this to render fixed, read-only
/// ports and one input-mapping row per child Start field.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct TemplateIoContract {
    pub input: Port,
    pub output: Port,
    /// Child's display name — lets the editor brand a sub-workflow card with
    /// the real template name instead of a truncated UUID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Stable `vendor/slug` coordinate when the child is a library node
    /// (decision 7). Frozen onto the embedding node so the canvas can brand the
    /// card and the upgrade prompt can track the source. Absent for plain
    /// (non-library) sub-workflows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinate: Option<String>,
    /// Child's branding (decisions 9, 12) when it is a library node. Frozen onto
    /// the embedding node alongside the contract; display-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<Presentation>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct IoContractQuery {
    /// Pin to a specific child version. Omit for the family's latest published
    /// row (matches a SubWorkflow node's `versionPin: latest`).
    pub version: Option<i32>,
}

/// GET /api/v1/templates/{id}/io-contract
///
/// Resolve a child template family (by base id or any version-row id) per the
/// optional `version` pin — the SAME resolution `resolve_subworkflow_air` uses
/// at publish — and return its derived SubWorkflow contract. The editor's
/// preview therefore cannot drift from the contract frozen into the parent.
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/io-contract",
    params(
        ("id" = Uuid, Path, description = "Child template family id (base or any version row)"),
        IoContractQuery,
    ),
    responses(
        (status = 200, description = "Derived SubWorkflow input/output contract", body = TemplateIoContract),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn get_io_contract(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<IoContractQuery>,
) -> Result<Json<TemplateIoContract>, ApiError> {
    let child = match q.version {
        None => sqlx::query_as::<_, WorkflowTemplate>(
            "SELECT * FROM workflow_templates \
             WHERE (base_template_id = $1 OR id = $1) AND is_latest = TRUE",
        )
        .bind(id),
        Some(v) => sqlx::query_as::<_, WorkflowTemplate>(
            "SELECT * FROM workflow_templates \
             WHERE (base_template_id = $1 OR id = $1) AND version = $2",
        )
        .bind(id)
        .bind(v),
    }
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    gate_template_read(&state, &user, &child).await?;

    // Capture the branding surface before `child.graph` is moved into the
    // parser. Only library nodes carry a coordinate/presentation; plain
    // sub-workflows leave these None and the card renders generically.
    let name = Some(child.name.clone());
    let coordinate = child.coordinate.clone();
    let presentation = child
        .presentation
        .clone()
        .and_then(|v| serde_json::from_value::<Presentation>(v).ok());

    let graph: WorkflowGraph = serde_json::from_value(child.graph)
        .map_err(|e| ApiError::internal(format!("child graph is invalid: {e}")))?;
    let (input, output) = derive_child_io(&graph);
    Ok(Json(TemplateIoContract {
        input,
        output,
        name,
        coordinate,
        presentation,
    }))
}

/// Authoring bundle for a template — graph plus inline per-node files. This
/// is the same `(graph, files)` pair the publish/new-version paths feed into
/// the compiler, served as plain JSON so non-collaborative clients (the CLI,
/// CI jobs) don't need a Yjs/WSS channel just to read a published template.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct TemplateBundle {
    pub graph: WorkflowGraph,
    pub files: HashMap<String, HashMap<String, String>>,
}

/// GET /api/v1/templates/{id}/bundle
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/bundle",
    params(("id" = Uuid, Path, description = "Template id")),
    responses(
        (status = 200, description = "Template authoring bundle (graph + per-node inline files)", body = TemplateBundle),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn get_template_bundle(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<TemplateBundle>, ApiError> {
    let existing = require_template(&state.db, id).await?;

    gate_template_read(&state, &user, &existing).await?;

    let (graph, files) = graph_with_ydoc_fallback(&state, id, existing.graph.clone(), |g| {
        serde_json::from_value(g).map_err(|e| ApiError::internal(format!("invalid graph: {e}")))
    })
    .await?;

    Ok(Json(TemplateBundle { graph, files }))
}

/// PUT /api/v1/templates/{id}
#[utoipa::path(
    put,
    path = "/api/v1/templates/{id}",
    params(("id" = Uuid, Path, description = "Template id")),
    request_body = UpdateTemplateRequest,
    responses(
        (status = 200, description = "Template updated", body = WorkflowTemplate),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 409, description = "Template is published and locked", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn update_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTemplateRequest>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    // Check if template exists and is not published
    let existing = require_template(&state.db, id).await?;

    gate_template_write(&state, &user, &existing).await?;

    if existing.published {
        return Err(ApiError::conflict("cannot edit a published template"));
    }

    let name = req.name.unwrap_or(existing.name);
    let description = req.description.unwrap_or(existing.description);
    let graph = match req.graph {
        Some(g) => serde_json::to_value(&g).map_err(|e| ApiError::internal(e.to_string()))?,
        None => existing.graph,
    };

    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        UPDATE workflow_templates
        SET name = $2, description = $3, graph = $4, updated_at = NOW(), updated_by = $5
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(&name)
    .bind(&description)
    .bind(&graph)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("failed to update template: {e}");
        ApiError::internal(e.to_string())
    })?;

    Ok(Json(template))
}

/// DELETE /api/v1/templates/{id}
/// Per Section 11.7: cascade cleanup of the chain's instances (published runs
/// AND draft test runs) before the template rows.
#[utoipa::path(
    delete,
    path = "/api/v1/templates/{id}",
    params(("id" = Uuid, Path, description = "Template id")),
    responses(
        (status = 204, description = "Template deleted"),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 409, description = "Template has active instances", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn delete_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let existing = require_template(&state.db, id).await?;

    gate_template_write(&state, &user, &existing).await?;

    let base_id = existing.chain_root_id();

    // Instance cleanup is UNCONDITIONAL — never-published drafts get
    // instances too (the publish gate runs template tests against the draft
    // id, leaving `mode = 'test_run'` rows behind), and gating this on
    // `existing.published` left FK-violating rows that 500'd the final
    // template DELETE.
    //
    // Check for running instances across all versions in this chain.
    let running_count: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*) FROM workflow_instances
           WHERE template_id IN (SELECT id FROM workflow_templates WHERE base_template_id = $1)
           AND status = 'running'"#,
    )
    .bind(base_id)
    .fetch_one(&state.db)
    .await?;

    if running_count.0 > 0 {
        return Err(ApiError::conflict(
            "cannot delete template with active instances",
        ));
    }

    // Cascade cleanup: clean up all finished instances for this template chain
    let instances: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        r#"SELECT id, net_id FROM workflow_instances
           WHERE template_id IN (SELECT id FROM workflow_templates WHERE base_template_id = $1)"#,
    )
    .bind(base_id)
    .fetch_all(&state.db)
    .await?;

    let purge_events = state.config.cleanup.purge_events;
    for (_instance_id, net_id) in &instances {
        cleanup_net(net_id, &state.nats, &state.petri, purge_events).await;
    }

    // Delete all instances for this template chain
    if let Err(e) = sqlx::query(
        "DELETE FROM workflow_instances WHERE template_id IN (SELECT id FROM workflow_templates WHERE base_template_id = $1)"
    )
    .bind(base_id)
    .execute(&state.db)
    .await
    {
        tracing::error!("failed to delete instances for template chain: {e}");
    }

    // Capture every version id in the chain before the delete so we can drop
    // their triggers from the in-memory dispatcher afterwards (otherwise a
    // deleted template's triggers keep firing until the next restart).
    let version_ids: Vec<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM workflow_templates WHERE base_template_id = $1")
            .bind(base_id)
            .fetch_all(&state.db)
            .await?;

    // Polymorphic object-grant cleanup (object_grants.object_id has no FK):
    // drop the template chain-root grant and any instance grants for instances
    // of this chain before the rows vanish.
    if let Err(e) = sqlx::query(
        "DELETE FROM object_grants \
          WHERE (object_type = 'template'::object_kind AND object_id = $1) \
             OR (object_type = 'instance'::object_kind AND object_id IN \
                 (SELECT id FROM workflow_instances \
                   WHERE template_id IN (SELECT id FROM workflow_templates WHERE base_template_id = $1)))",
    )
    .bind(base_id)
    .execute(&state.db)
    .await
    {
        tracing::error!("failed to clean object_grants for template chain: {e}");
    }

    // Delete all versions in the template chain
    sqlx::query("DELETE FROM workflow_templates WHERE base_template_id = $1")
        .bind(base_id)
        .execute(&state.db)
        .await
        .map_err(|e| {
            tracing::error!("failed to delete template: {e}");
            ApiError::internal(e.to_string())
        })?;

    for (vid,) in version_ids {
        state.triggers.forget_template(vid);
        // Kick any collaborator still connected to a deleted version's Yjs
        // room — their socket would otherwise keep accepting edits whose
        // persistence INSERTs fail on the now-deleted rows.
        state.yjs.close_room(vid).await;
        // Cascade replacement: the migration that generalized the Yjs tables
        // (doc_id + doc_kind) DROPPED the `workflow_templates` FK, so the old
        // `ON DELETE CASCADE` no longer reaps these rows. Delete them
        // explicitly (the graph doc is keyed on the version id).
        if let Err(e) = sqlx::query("DELETE FROM yjs_documents WHERE doc_id = $1")
            .bind(vid)
            .execute(&state.db)
            .await
        {
            tracing::error!("failed to delete yjs_documents for template version {vid}: {e}");
        }
        if let Err(e) = sqlx::query("DELETE FROM yjs_snapshots WHERE doc_id = $1")
            .bind(vid)
            .execute(&state.db)
            .await
        {
            tracing::error!("failed to delete yjs_snapshots for template version {vid}: {e}");
        }
    }

    // Clean attached pages (polymorphic `pages.attached_id` has no FK): the
    // template's chain-root "Notes" page plus any deleted instance's "Report"
    // page. Each page's Yjs doc rows (no FK on `doc_id`) and in-memory room go
    // too. Page ids are gathered first so the rooms can be closed after the
    // rows are gone.
    let instance_ids: Vec<uuid::Uuid> = instances.iter().map(|(id, _)| *id).collect();
    let page_ids: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM pages \
          WHERE (attached_kind = 'template' AND attached_id = $1) \
             OR (attached_kind = 'instance' AND attached_id = ANY($2))",
    )
    .bind(base_id)
    .bind(&instance_ids)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for (pid,) in &page_ids {
        for stmt in [
            "DELETE FROM yjs_documents WHERE doc_id = $1",
            "DELETE FROM yjs_snapshots WHERE doc_id = $1",
            "DELETE FROM pages WHERE id = $1",
        ] {
            if let Err(e) = sqlx::query(stmt).bind(pid).execute(&state.db).await {
                tracing::error!("failed to clean attached page {pid} for template chain: {e}");
            }
        }
        state.yjs.close_room(*pid).await;
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Which discard path a draft selects. Pure decision (mirrors `apply_mode`)
/// so it can be unit-tested without a DB. `Err` carries the 409 message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscardMode {
    /// Never-published root draft (parent NULL): the draft IS the chain —
    /// delete the whole template.
    DeleteChain,
    /// Delete the draft row and restore this parent as the chain head.
    RestoreParent(Uuid),
}

pub(crate) fn discard_mode(draft: &WorkflowTemplate) -> Result<DiscardMode, String> {
    if draft.published {
        return Err("cannot discard a published version".into());
    }
    match draft.parent_id {
        None => Ok(DiscardMode::DeleteChain),
        Some(parent_id) if draft.is_latest => Ok(DiscardMode::RestoreParent(parent_id)),
        // An unpublished non-head row shouldn't exist (drafts are only ever
        // the chain head); refuse rather than mint a second `is_latest` row.
        Some(_) => Err("draft is not the chain head".into()),
    }
}

/// DELETE /api/v1/templates/{id}/draft
///
/// Discard a single unpublished draft version — the reverse of `new_version`.
/// The draft's instances (publish-gate `test_run`s bind to the draft id and
/// their FK has no cascade) and the draft row are deleted (its
/// `yjs_documents`/`yjs_snapshots` cascade via FK) and the parent version is
/// restored as the chain head (`is_latest = TRUE`) in one transaction; the
/// instances' engine nets are purged after commit. A never-published v1 root draft
/// has no parent: the draft IS the chain, so the whole template is deleted
/// via the same path as `DELETE /api/v1/templates/{id}` (which also owns the
/// chain-root `object_grants` cleanup).
#[utoipa::path(
    delete,
    path = "/api/v1/templates/{id}/draft",
    params(("id" = Uuid, Path, description = "Draft template version id")),
    responses(
        (status = 200, description = "Draft discarded", body = DiscardDraftResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 409, description = "Version is published, not the chain head, or has running instances", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn discard_draft(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<DiscardDraftResponse>, ApiError> {
    let existing = require_template(&state.db, id).await?;

    gate_template_write(&state, &user, &existing).await?;

    let parent_id = match discard_mode(&existing).map_err(ApiError::conflict)? {
        DiscardMode::DeleteChain => {
            // Never-published root draft (only `create_template` makes one):
            // delegate to the chain delete — for a single-version chain it
            // deletes exactly this row plus the chain-root grants.
            delete_template(State(state), user, Path(id)).await?;
            return Ok(Json(DiscardDraftResponse {
                template_deleted: true,
                restored_head: None,
            }));
        }
        DiscardMode::RestoreParent(parent_id) => parent_id,
    };

    let mut tx = state.db.begin().await?;

    // Re-check the draft state under a row lock: `discard_mode` decided from
    // a pre-transaction snapshot, and a concurrent publish flips
    // `published = TRUE` between its own pre-checks and `finalize_publish_row`
    // (a seconds-long compile + test-gate window). Without this guard the
    // DELETE below would silently destroy the freshly published version.
    let still_draft: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM workflow_templates \
          WHERE id = $1 AND published = FALSE AND is_latest = TRUE FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if still_draft.is_none() {
        return Err(ApiError::conflict(
            "version was published or superseded concurrently",
        ));
    }

    // The publish gate runs template tests against the DRAFT id, leaving
    // `mode = 'test_run'` workflow_instances rows behind. Their template_id
    // FK has no ON DELETE action, so they (and their polymorphic instance
    // grants) must go before the template row — mirroring delete_template.
    let running: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM workflow_instances WHERE template_id = $1 AND status = 'running'",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if running.0 > 0 {
        return Err(ApiError::conflict(
            "cannot discard a draft with running instances",
        ));
    }

    let instances: Vec<(uuid::Uuid, String)> =
        sqlx::query_as("SELECT id, net_id FROM workflow_instances WHERE template_id = $1")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;

    sqlx::query(
        "DELETE FROM object_grants \
          WHERE object_type = 'instance'::object_kind \
            AND object_id IN (SELECT id FROM workflow_instances WHERE template_id = $1)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;

    // Clean attached "Report" pages on the test-run instances about to vanish
    // (polymorphic `pages.attached_id` has no FK → no cascade). Gather the page
    // ids so their in-memory rooms can be closed after commit.
    let instance_ids: Vec<uuid::Uuid> = instances.iter().map(|(iid, _)| *iid).collect();
    let draft_page_ids: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM pages WHERE attached_kind = 'instance' AND attached_id = ANY($1)",
    )
    .bind(&instance_ids)
    .fetch_all(&mut *tx)
    .await?;
    for (pid,) in &draft_page_ids {
        sqlx::query("DELETE FROM yjs_documents WHERE doc_id = $1")
            .bind(pid)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM yjs_snapshots WHERE doc_id = $1")
            .bind(pid)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM pages WHERE id = $1")
            .bind(pid)
            .execute(&mut *tx)
            .await?;
    }

    sqlx::query("DELETE FROM workflow_instances WHERE template_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM workflow_templates WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;

    let restored = sqlx::query_as::<_, WorkflowTemplate>(
        "UPDATE workflow_templates SET is_latest = TRUE WHERE id = $1 RETURNING *",
    )
    .bind(parent_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("failed to restore parent version on draft discard: {e}");
        ApiError::internal(e.to_string())
    })?;

    tx.commit().await?;

    // Engine-side teardown AFTER commit, so a rollback never leaves DB rows
    // pointing at already-purged nets; the gate's test-run nets would
    // otherwise leak in the engine.
    let purge_events = state.config.cleanup.purge_events;
    for (_instance_id, net_id) in &instances {
        cleanup_net(net_id, &state.nats, &state.petri, purge_events).await;
    }

    // Kick any collaborator still connected to the draft's Yjs room — their
    // socket would otherwise keep accepting edits whose persistence INSERTs
    // fail on the now-deleted rows, silently losing their work.
    state.yjs.close_room(id).await;
    // Same for any attached-page rooms whose rows were deleted above.
    for (pid,) in &draft_page_ids {
        state.yjs.close_room(*pid).await;
    }

    // Cascade replacement: the Yjs-table generalization (doc_id + doc_kind)
    // DROPPED the `workflow_templates` FK, so the discarded draft's graph doc
    // rows are no longer reaped by `ON DELETE CASCADE`. Delete them explicitly
    // (the doc is keyed on the draft's own id).
    if let Err(e) = sqlx::query("DELETE FROM yjs_documents WHERE doc_id = $1")
        .bind(id)
        .execute(&state.db)
        .await
    {
        tracing::error!("failed to delete yjs_documents for discarded draft {id}: {e}");
    }
    if let Err(e) = sqlx::query("DELETE FROM yjs_snapshots WHERE doc_id = $1")
        .bind(id)
        .execute(&state.db)
        .await
    {
        tracing::error!("failed to delete yjs_snapshots for discarded draft {id}: {e}");
    }

    // Drafts have no registered triggers, but mirror delete_template's
    // dispatcher hygiene in case of drift.
    state.triggers.forget_template(id);

    // `new_version` forgot the parent's triggers when it was superseded;
    // restoring it as the head re-registers them (same gate as `hydrate`:
    // published + latest + non-private, no backfill — nothing is new).
    if restored.published && restored.visibility != "private" {
        state.triggers.register_template(&restored, false).await;
    }

    Ok(Json(DiscardDraftResponse {
        template_deleted: false,
        restored_head: Some(restored),
    }))
}

/// POST /api/v1/templates/{id}/publish
#[utoipa::path(
    post,
    path = "/api/v1/templates/{id}/publish",
    params(
        ("id" = Uuid, Path, description = "Template id"),
        PublishQuery,
    ),
    responses(
        (status = 200, description = "Template published; AIR compiled and stored", body = WorkflowTemplate),
        (status = 400, description = "Compilation failed or graph invalid", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 409, description = "Template already published", body = ErrorResponse),
        (status = 412, description = "Template tests failing; publish blocked", body = PublishGateBlockedResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn publish_template(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(query): Query<PublishQuery>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    let principal_id = user.subject_as_uuid();
    let existing = require_template(&state.db, id).await?;

    gate_template_write(&state, &user, &existing).await?;

    if existing.published {
        return Err(ApiError::conflict("template is already published"));
    }

    // Try to reconstruct graph + files from Y.Doc first (collaborative editing source of truth),
    // falling back to the DB graph column for legacy templates.
    let (graph, mut ydoc_files): (WorkflowGraph, HashMap<String, HashMap<String, String>>) =
        graph_with_ydoc_fallback(&state, id, existing.graph.clone(), |g| {
            serde_json::from_value(g)
                .map_err(|e| ApiError::bad_request(format!("invalid graph: {e}")))
        })
        .await?;

    let publisher = PublishService::new(&state);

    // Synthesize Python IO stubs + compile AIR + serialize the graph in one
    // shared step (identical to `apply`). `ydoc_files` is mutated so the S3
    // upload below stages exactly what was compiled against.
    let CompiledArtifacts {
        air_json,
        graph_json,
        interface_json,
        node_configs,
        metrics,
    } = publisher
        .compile_artifacts(
            &graph,
            &existing.name,
            &existing.description,
            id,
            existing.version,
            ArtifactKeySpace::Version,
            Some(existing.chain_root_id()),
            &mut ydoc_files,
            principal_id,
            existing.workspace_id,
        )
        .await?;

    // Upload node file contents to S3 so the executor can stage them at
    // runtime. Non-fatal for UI publish (legacy behavior).
    if let Err(e) = publisher
        .upload_files(id, existing.version, &ydoc_files)
        .await
    {
        tracing::warn!("S3 file upload failed (non-fatal): {e}");
    }
    // Upload the per-node static configs the compiler offloaded so the
    // executor's `FetchConfigHook` can resolve `config_ref` at run time.
    // Non-fatal in UI publish — matches the upload_files behavior so a
    // transient S3 hiccup doesn't strand a draft.
    if let Err(e) = publisher
        .upload_node_configs(id, existing.version, &node_configs)
        .await
    {
        tracing::warn!("S3 node-config upload failed (non-fatal): {e}");
    }

    // Template-test gate. Run every enabled test for this template family
    // against the freshly-compiled AIR before flipping `published`. Failing
    // (or erroring) tests block the publish unless `?force=true`.
    let failing =
        run_publish_gate(&state, &existing, &air_json, &graph, user.subject_as_uuid()).await?;
    if !failing.is_empty() {
        if query.force {
            tracing::warn!(
                template_id = %id,
                failing = failing.len(),
                "publish gate bypassed via ?force=true"
            );
        } else {
            let failing_json =
                serde_json::to_value(&failing).map_err(|e| ApiError::internal(e.to_string()))?;
            return Err(ApiError {
                status: StatusCode::PRECONDITION_FAILED,
                body: Some(
                    ErrorResponse::new(format!(
                        "{} template test(s) failed; publish blocked. Pass ?force=true to override.",
                        failing.len()
                    ))
                    .with_code("publish-gate")
                    .with_failing_tests(failing_json),
                ),
            });
        }
    }

    // Persist the Y.Doc-reconstructed graph we just compiled into the `graph`
    // column. Publish previously wrote only `air_json`, leaving `graph` stale:
    // every consumer that reads `template.graph` — the trigger dispatcher
    // (`hydrate`, `register_template`, and `fire`, which reloads the graph to
    // read a trigger's `payload_mapping`) and the create-instance dialog —
    // would otherwise operate on a pre-Y.Doc graph that lacks newly-authored
    // nodes (a published trigger fired with "trigger node missing in graph").
    //
    // UI publish: no git provenance (column stays NULL).
    let template = finalize_publish_row(
        &state.db,
        id,
        &air_json,
        &graph_json,
        &interface_json,
        &metrics,
        None,
        user.subject_as_uuid(),
    )
    .await?;

    // Make the just-published template's triggers live immediately. The
    // dispatcher's in-memory registry is otherwise only filled by `hydrate()`
    // at service startup, so without this a freshly-published trigger 404s
    // ("not found in any published template") until the next restart.
    // `template.graph` is now the freshly-persisted compiled graph.
    let registered = publisher.register_triggers(&template).await;
    if registered > 0 {
        tracing::info!(template_id = %id, registered, "registered triggers on publish");
    }

    Ok(Json(template))
}

/// Try to reconstruct a WorkflowGraph and file contents from the Y.Doc stored for this template.
/// Returns Ok(None) if no Y.Doc exists.
///
/// Reads from the new schema: Y.Map("nodes"), Y.Array("edges"), Y.Map("viewport").
/// Also extracts Y.Text file entries from `nodes[nodeId].files`.
#[allow(clippy::type_complexity)]
async fn reconstruct_graph_from_ydoc(
    state: &AppState,
    template_id: Uuid,
) -> Result<Option<(WorkflowGraph, HashMap<String, HashMap<String, String>>)>, String> {
    // Prefer the LIVE in-memory room when an editor session is open. The room
    // holds the authoritative collaborative state — every connected canvas sees
    // it, and it's updated synchronously on each WS message. The persisted
    // `yjs_documents` rows can lag it: most acutely across a background
    // compaction (a COMPACTION_THRESHOLD crossing snapshots + deletes update
    // rows, and any edit that races that pass is dropped from the DB while the
    // room keeps the full state). Publish AND the draft dev-run both reconstruct
    // through here, so reading the live room is what lets "Run draft" capture
    // the canvas exactly as authored instead of a stale snapshot. Only adopt it
    // when it actually carries nodes — a just-connected (not-yet-seeded) or
    // emptied room must still fall through to persistence / the legacy `graph`
    // column rather than compile an empty graph.
    if let Some(room) = state.yjs.get_room_if_exists(template_id) {
        let full_state = room.encode_full_state().await;
        let live = tokio::task::spawn_blocking(
            move || -> Result<(WorkflowGraph, HashMap<String, HashMap<String, String>>), String> {
                use crate::yjs::doc_ops;
                use crate::yjs::persistence::YjsPersistence;

                // `encode_full_state` is `encode_state_as_update_v1`, which
                // decodes like any persisted snapshot — feed it as the snapshot
                // with no trailing incremental updates.
                let doc = YjsPersistence::build_doc_from_raw(Some(&full_state), &[])
                    .map_err(|e| e.to_string())?;
                let graph = doc_ops::doc_to_graph(&doc)?;
                let files = doc_ops::extract_files_from_doc(&doc);
                Ok((graph, files))
            },
        )
        .await
        .map_err(|e| format!("spawn_blocking: {e}"))??;

        if !live.0.nodes.is_empty() {
            return Ok(Some(live));
        }
    }

    let has_doc = state
        .yjs
        .persistence
        .has_doc(template_id)
        .await
        .map_err(|e| e.to_string())?;

    if !has_doc {
        return Ok(None);
    }

    // Load raw updates and build the doc in spawn_blocking (yrs Doc is !Send)
    let (snapshot, updates) = state
        .yjs
        .persistence
        .load_raw_updates(template_id)
        .await
        .map_err(|e| e.to_string())?;

    let result = tokio::task::spawn_blocking(
        move || -> Result<(WorkflowGraph, HashMap<String, HashMap<String, String>>), String> {
            use crate::yjs::doc_ops;
            use crate::yjs::persistence::YjsPersistence;

            let doc = YjsPersistence::build_doc_from_raw(snapshot.as_deref(), &updates)
                .map_err(|e| e.to_string())?;

            let graph = doc_ops::doc_to_graph(&doc)?;
            let files = doc_ops::extract_files_from_doc(&doc);
            Ok((graph, files))
        },
    )
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))??;

    Ok(Some(result))
}

/// Resolve the authored `(graph, files)` for a template, preferring the live
/// Y.Doc (collaborative source of truth) and falling back to the persisted
/// `graph` column ONLY when no Y.Doc exists (legacy templates).
///
/// A Y.Doc that exists but fails to reconstruct is surfaced as an error rather
/// than silently serving the stale DB column — the previous per-call-site
/// behavior (log + serve `existing.graph`) could publish / fork the wrong graph
/// when collaborative edits hadn't been flushed to the column (they never are).
///
/// `parse_db_graph` decodes the `Ok(None)` legacy column, letting each caller
/// keep its own invalid-graph contract (hard `internal`/`bad_request` error vs.
/// the new-version path's silent `default_graph()` tolerance).
/// `pub(crate)`: the draft dev-run path (`create_instance`) compiles a draft
/// per-launch from the same authored source publish reads.
pub(crate) async fn graph_with_ydoc_fallback<F>(
    state: &AppState,
    id: Uuid,
    db_graph: serde_json::Value,
    parse_db_graph: F,
) -> Result<(WorkflowGraph, HashMap<String, HashMap<String, String>>), ApiError>
where
    F: FnOnce(serde_json::Value) -> Result<WorkflowGraph, ApiError>,
{
    match reconstruct_graph_from_ydoc(state, id).await {
        Ok(Some((g, f))) => Ok((g, f)),
        Ok(None) => Ok((parse_db_graph(db_graph)?, HashMap::new())),
        Err(e) => Err(ApiError::internal(format!(
            "failed to load Y.Doc for template {id}: {e}"
        ))),
    }
}

/// Mark a template row as no longer the latest in its version chain. Generic
/// over the executor so it works on the pool or inside a transaction.
async fn mark_not_latest<'e, E>(exec: E, id: Uuid) -> Result<(), ApiError>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query("UPDATE workflow_templates SET is_latest = FALSE WHERE id = $1")
        .bind(id)
        .execute(exec)
        .await?;
    Ok(())
}

/// The publish-finalize UPDATE: freeze the row, store compiled AIR + the
/// just-compiled graph, and stamp git provenance. `source_ref` is `None` for a
/// UI publish (column stays NULL) and `Some` for `apply`'s seed path. Generic
/// over the executor so publish (pool) and apply (txn) share one statement —
/// the single place the `source_ref` column is threaded on an UPDATE.
#[allow(clippy::too_many_arguments)]
async fn finalize_publish_row<'e, E>(
    exec: E,
    id: Uuid,
    air_json: &serde_json::Value,
    graph_json: &serde_json::Value,
    interface_json: &serde_json::Value,
    metrics: &serde_json::Value,
    source_ref: Option<&serde_json::Value>,
    updated_by: Uuid,
) -> Result<WorkflowTemplate, ApiError>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        UPDATE workflow_templates
        SET published = TRUE, published_at = NOW(), air_json = $2, graph = $3,
            interface_json = $4, source_ref = $5, updated_at = NOW(), updated_by = $6,
            metrics = $7
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(air_json)
    .bind(graph_json)
    .bind(interface_json)
    .bind(source_ref)
    .bind(updated_by)
    .bind(metrics)
    .fetch_one(exec)
    .await
    .map_err(|e| {
        tracing::error!("failed to publish template: {e}");
        ApiError::internal(e.to_string())
    })
}

/// Insert a new chain version that is *born published* — the atomic primitive
/// behind `apply`'s bump path. Unlike `new_version`'s draft INSERT, the row
/// lands `published = TRUE` in a single statement so there is no persisted
/// latest-but-unpublished intermediate state. Caller must `mark_not_latest`
/// the source within the same transaction.
#[allow(clippy::too_many_arguments)]
async fn insert_published_version<'e, E>(
    exec: E,
    src: &WorkflowTemplate,
    new_id: Uuid,
    version: i32,
    air_json: &serde_json::Value,
    graph_json: &serde_json::Value,
    interface_json: &serde_json::Value,
    metrics: &serde_json::Value,
    source_ref: Option<&serde_json::Value>,
    updated_by: Uuid,
) -> Result<WorkflowTemplate, ApiError>
where
    E: sqlx::PgExecutor<'e>,
{
    let base_id = src.chain_root_id();
    sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        INSERT INTO workflow_templates
            (id, name, description, base_template_id, parent_id, version,
             is_latest, published, published_at, graph, air_json,
             interface_json, source_ref, author_id,
             workspace_id, visibility, owner_template_id, updated_by, metrics)
        VALUES ($1, $2, $3, $4, $5, $6, TRUE, TRUE, NOW(), $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
        RETURNING *
        "#,
    )
    .bind(new_id)
    .bind(&src.name)
    .bind(&src.description)
    .bind(base_id)
    .bind(src.id)
    .bind(version)
    .bind(graph_json)
    .bind(air_json)
    .bind(interface_json)
    .bind(source_ref)
    .bind(src.author_id)
    .bind(src.workspace_id)
    .bind(&src.visibility)
    .bind(src.owner_template_id)
    .bind(updated_by)
    .bind(metrics)
    .fetch_one(exec)
    .await
    .map_err(|e| {
        tracing::error!("failed to insert published version: {e}");
        ApiError::internal(e.to_string())
    })
}

/// Fetch the newest version (highest `version`) in a template's chain.
async fn latest_in_chain(pool: &sqlx::PgPool, base_id: Uuid) -> Result<WorkflowTemplate, ApiError> {
    sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE base_template_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(base_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::not_found("template chain not found"))
}

/// POST /api/v1/templates/{id}/new-version
#[utoipa::path(
    post,
    path = "/api/v1/templates/{id}/new-version",
    params(("id" = Uuid, Path, description = "Source template id")),
    responses(
        (status = 201, description = "New draft version created from published source", body = WorkflowTemplate),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 409, description = "Source must be published", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn new_version(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<WorkflowTemplate>), ApiError> {
    let existing = require_template(&state.db, id).await?;

    if !existing.published {
        return Err(ApiError::conflict(
            "can only create new version from a published template",
        ));
    }

    let new_id = Uuid::new_v4();
    let new_version = existing.version + 1;
    let base_id = existing.chain_root_id();

    // The authored workflow lives in the *source's Y.Doc*, not the `graph`
    // column — publish/edit never write the column back, so copying
    // `existing.graph` would fork a blank canvas. Reconstruct the real graph
    // + per-node files from the Y.Doc (same source of truth publish uses),
    // falling back to the column only for legacy templates with no Y.Doc.
    let (graph, files): (WorkflowGraph, HashMap<String, HashMap<String, String>>) =
        graph_with_ydoc_fallback(&state, id, existing.graph.clone(), |g| {
            // Legacy no-Y.Doc fork: an unparseable column degrades to a blank
            // canvas rather than failing the new-version create.
            Ok(serde_json::from_value(g).unwrap_or_else(|_| WorkflowGraph::default_graph()))
        })
        .await?;
    let graph_json = serde_json::to_value(&graph).map_err(|e| ApiError::internal(e.to_string()))?;

    // Start a transaction
    let mut tx = state.db.begin().await?;

    // Mark old version as not latest
    mark_not_latest(&mut *tx, id).await?;

    // Create new version
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        INSERT INTO workflow_templates (id, name, description, base_template_id, parent_id, version, is_latest, graph, author_id, workspace_id, visibility, owner_template_id, updated_by)
        VALUES ($1, $2, $3, $4, $5, $6, TRUE, $7, $8, $9, $10, $11, $8)
        RETURNING *
        "#,
    )
    .bind(new_id)
    .bind(&existing.name)
    .bind(&existing.description)
    .bind(base_id)
    .bind(existing.id)
    .bind(new_version)
    .bind(&graph_json)
    .bind(existing.author_id)
    .bind(existing.workspace_id)
    .bind(&existing.visibility)
    .bind(existing.owner_template_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("failed to create new version: {e}");
        ApiError::internal(e.to_string())
    })?;

    tx.commit().await?;

    // The previous version is now superseded (is_latest = FALSE). Its triggers
    // must stop firing immediately, not linger in the in-memory dispatcher
    // until the next restart — `hydrate()` already excludes non-latest
    // versions, so this just makes the running process match restart state.
    state.triggers.forget_template(existing.id);

    // Seed Y.Doc for the new version so WS collaboration works immediately,
    // including the copied per-node files.
    if let Err(e) = state
        .yjs
        .persistence
        .init_doc_from_graph_with_files(new_id, &graph, &files)
        .await
    {
        tracing::error!("failed to init Y.Doc for new version {new_id}: {e}");
        // Non-fatal: template is created, Y.Doc can be initialized later
    }

    Ok((StatusCode::CREATED, Json(template)))
}

/// Which `apply` path the chain head selects. Pure decision so it can be
/// unit-tested without a DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyMode {
    /// Seed-publish a fresh `mekhan init` draft in place as v1.
    Seed,
    /// Bump the published head to a new born-published version.
    Bump,
}

/// Decide the apply path from the chain's latest version. `Err` carries the
/// 409 message when the head is a UI-created unpublished draft. Only
/// `create_template` (via `mekhan init`) yields a v1 / parent-NULL /
/// unpublished row — `new_version` always sets `parent_id` — so that triple
/// uniquely identifies an untouched init draft.
pub(crate) fn apply_mode(latest: &WorkflowTemplate) -> Result<ApplyMode, String> {
    if latest.published {
        Ok(ApplyMode::Bump)
    } else if latest.version == 1 && latest.parent_id.is_none() {
        Ok(ApplyMode::Seed)
    } else {
        Err(format!(
            "latest version v{} is an unpublished web-editor draft; \
             resolve or detach before apply (out of scope)",
            latest.version
        ))
    }
}

/// POST /api/v1/templates/{id}/apply
///
/// GitOps entry point: atomically publish a new version of the chain straight
/// from a git-authored artifact. The supplied `graph` REPLACES the chain head
/// wholesale (no CRDT merge). Either seeds-and-publishes a fresh `mekhan init`
/// draft as v1 in place, or bumps the published head to a new born-published
/// version. The collaborative Y.Doc draft window is collapsed to nothing, so
/// publish-freeze itself is the isolation between git- and web-authored
/// templates.
#[utoipa::path(
    post,
    path = "/api/v1/templates/{id}/apply",
    params(("id" = Uuid, Path, description = "Any template id in the target chain")),
    request_body = ApplyTemplateRequest,
    responses(
        (status = 200, description = "Applied: seeded v1 or a new born-published version", body = WorkflowTemplate),
        (status = 400, description = "Compilation failed or graph invalid", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 409, description = "Chain head is an unpublished web-editor draft", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn apply_template(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<Uuid>,
    Json(req): Json<ApplyTemplateRequest>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    // 1. Resolve the chain head and pick the bootstrap branch. Read-only —
    //    nothing is written until the compile has passed.
    let existing = require_template(&state.db, id).await?;

    let base_id = existing.chain_root_id();
    let latest = latest_in_chain(&state.db, base_id).await?;

    let mode = apply_mode(&latest).map_err(ApiError::conflict)?;

    let target_id = match mode {
        ApplyMode::Seed => latest.id,
        ApplyMode::Bump => Uuid::new_v4(),
    };
    let target_version = match mode {
        ApplyMode::Seed => latest.version,
        ApplyMode::Bump => latest.version + 1,
    };

    // 2. Compile AIR FIRST — before any write. Pure; a failure leaves zero
    //    side effects (no draft, no S3, no Y.Doc). Same shared step as
    //    `publish_template`.
    let graph = req.graph;
    let mut files_map = req.files;
    let publisher = PublishService::new(&state);

    let CompiledArtifacts {
        air_json,
        graph_json,
        interface_json,
        node_configs,
        metrics,
    } = publisher
        .compile_artifacts(
            &graph,
            &latest.name,
            &latest.description,
            target_id,
            target_version,
            ArtifactKeySpace::Version,
            Some(latest.chain_root_id()),
            &mut files_map,
            user.subject_as_uuid(),
            // Apply (no-version-bump) hits the same workspace as the latest
            // template row; reuse it directly to keep the resource lookup
            // tenant-correct.
            latest.workspace_id,
        )
        .await?;
    let source_ref_json = req
        .source_ref
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| ApiError::internal(format!("serialize source_ref: {e}")))?;

    // 3. Upload node files to S3 under the *target* version key, before the
    //    DB row. A failure here leaves only inert orphan objects (nothing
    //    points at them) — no dangling row. Fatal for apply (unlike publish's
    //    logged-warning legacy behavior).
    if let Err(e) = publisher
        .upload_files(target_id, target_version, &files_map)
        .await
    {
        return Err(ApiError::internal(format!("S3 file upload failed: {e}")));
    }
    // Per-node static configs offloaded by the compiler. Fatal for apply —
    // the executor `FetchConfigHook` would fail at run-time if a node's
    // blob is missing, leaving a hard-to-trace runtime breakage.
    if let Err(e) = publisher
        .upload_node_configs(target_id, target_version, &node_configs)
        .await
    {
        return Err(ApiError::internal(format!(
            "S3 node-config upload failed: {e}"
        )));
    }

    // 4. Single transaction: the only persisted, queryable transition. The
    //    row is born in its final published+latest form — there is no
    //    intermediate latest-but-unpublished state to strand on failure.
    let mut tx = state.db.begin().await?;

    let applied = match mode {
        ApplyMode::Seed => {
            finalize_publish_row(
                &mut *tx,
                latest.id,
                &air_json,
                &graph_json,
                &interface_json,
                &metrics,
                source_ref_json.as_ref(),
                user.subject_as_uuid(),
            )
            .await?
        }
        ApplyMode::Bump => {
            mark_not_latest(&mut *tx, latest.id).await?;
            insert_published_version(
                &mut *tx,
                &latest,
                target_id,
                target_version,
                &air_json,
                &graph_json,
                &interface_json,
                &metrics,
                source_ref_json.as_ref(),
                user.subject_as_uuid(),
            )
            .await?
        }
    };

    tx.commit().await?;

    tracing::info!(
        template_id = %applied.id,
        version = applied.version,
        actor = %user.subject,
        "applied template from git"
    );

    // 5. Post-commit, non-fatal: the executor runs from AIR/S3 and the
    //    `graph` column (both durable above), not the Y.Doc. Bump mints a
    //    brand-new id, so this is a clean Y.Doc init exactly like
    //    `new_version`. Seed reuses the existing v1 id whose Y.Doc would
    //    *merge* (store_update appends) rather than replace — and the row is
    //    now published⇒read-only — so the seeded v1's editor view stays at
    //    its init seed (cosmetic only; the published graph/AIR are correct).
    if mode == ApplyMode::Bump {
        if let Err(e) = state
            .yjs
            .persistence
            .init_doc_from_graph_with_files(applied.id, &graph, &files_map)
            .await
        {
            tracing::error!(
                "failed to init Y.Doc for applied version {}: {e}",
                applied.id
            );
        }
    }

    // 6. Trigger registry (process-local, post-commit so it can only ever
    //    reflect a row that truly landed published+latest).
    if mode == ApplyMode::Bump {
        state.triggers.forget_template(latest.id);
    }
    let registered = publisher.register_triggers(&applied).await;
    if registered > 0 {
        tracing::info!(template_id = %applied.id, registered, "registered triggers on apply");
    }

    Ok(Json(applied))
}

/// POST /api/v1/templates/apply-air
///
/// Clinic-style headless template upload: accepts pre-compiled AIR
/// (`ScenarioDefinition` shape — `{places[], transitions[]}`) directly,
/// bypassing the editor's `WorkflowGraph` → AIR compile pass entirely.
/// The supplied `air_json` is stored verbatim into the `air_json` column;
/// a synthetic stub graph containing just the `Trigger` node is stored
/// into the `graph` column so the trigger dispatcher's `register_triggers`
/// finds it post-commit.
///
/// Idempotency: name-based, scoped per workspace. A first apply with a
/// given `name` in the caller's workspace Seeds a fresh v1 chain
/// (`is_latest = true`); subsequent applies with the same `(name,
/// workspace_id)` pair Bump the chain. Cross-workspace name collisions
/// are independent chains.
///
/// Distinct from `POST /api/v1/templates/{id}/apply` (the GitOps path for
/// graph-authored templates): that one demands an existing `{id}` and a
/// `WorkflowGraph`, then runs the compile pass. This endpoint takes
/// neither.
#[utoipa::path(
    post,
    path = "/api/v1/templates/apply-air",
    request_body = ApplyAirTemplateRequest,
    responses(
        (status = 200, description = "Applied: seeded v1 or a new born-published version", body = WorkflowTemplate),
        (status = 400, description = "Invalid AIR or trigger spec", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn apply_air_template(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<ApplyAirTemplateRequest>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    // Validate inputs before touching state. The endpoint is born-published
    // and pre-AIR; the AIR is opaque to mekhan-service so we only assert
    // shape-level invariants (places exist, named trigger target exists).
    if !req.air_json.is_object() {
        return Err(ApiError::bad_request("air_json must be a JSON object"));
    }
    let places_array = req
        .air_json
        .get("places")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ApiError::bad_request("air_json.places must be an array"))?;
    let target_place_exists = places_array.iter().any(|p| {
        p.get("id").and_then(|v| v.as_str()) == Some(req.trigger.air_target_place_id.as_str())
    });
    if !target_place_exists {
        return Err(ApiError::bad_request(format!(
            "trigger.air_target_place_id '{}' not found in air_json.places",
            req.trigger.air_target_place_id
        )));
    }
    if req.trigger.node_id.trim().is_empty() {
        return Err(ApiError::bad_request("trigger.node_id must be non-empty"));
    }

    // Synthesize the stub graph: one Trigger node, no edges. `register_triggers`
    // walks this; the dispatcher's pre-AIR branch reads
    // `air_target_place_id` directly from the node data.
    let stub_node = WorkflowNode {
        id: req.trigger.node_id.clone(),
        node_type: "trigger".to_string(),
        slug: None,
        position: Position { x: 0.0, y: 0.0 },
        data: WorkflowNodeData::Trigger {
            label: req.trigger.label.clone(),
            description: None,
            source: req.trigger.source.clone(),
            concurrency: Default::default(),
            payload_mapping: req.trigger.payload_mapping.clone(),
            enabled: req.trigger.enabled,
            air_target_place_id: Some(req.trigger.air_target_place_id.clone()),
        },
        parent_id: None,
        width: None,
        height: None,
    };
    let stub_graph = WorkflowGraph {
        nodes: vec![stub_node],
        edges: vec![],
        viewport: None,
        // Pre-AIR templates have no graph-level resource declarations or
        // template-level concurrency policy — both default-empty.
        definitions: Default::default(),
        instance_concurrency: Default::default(),
        default_scheduler: None,
    };
    let stub_graph_json = serde_json::to_value(&stub_graph)
        .map_err(|e| ApiError::internal(format!("synthesize stub graph: {e}")))?;
    let source_ref_json = req
        .source_ref
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| ApiError::internal(format!("serialize source_ref: {e}")))?;
    let description = req.description.clone().unwrap_or_default();
    let author_id = user.subject_as_uuid();
    // Multi-tenant scoping per upstream's workspaces+visibility migration
    // (`5ac9e72` + 9 commits). Pre-AIR apply is workspace-private by
    // construction; `public` visibility is admin-only via
    // `PATCH /api/v1/templates/{id}/visibility` (`38642db`).
    let workspace_id = user.require_workspace()?;
    let visibility = "workspace";

    // Name-based chain lookup, scoped per workspace. The pre-AIR endpoint
    // uses `(name, workspace_id)` as the stable chain key so the deploy
    // recipe can re-apply idempotently from git without owning a UUID,
    // and cross-tenant name collisions don't Bump the wrong chain.
    let latest: Option<WorkflowTemplate> = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates \
            WHERE name = $1 AND workspace_id = $2 AND is_latest = TRUE",
    )
    .bind(&req.name)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let applied = match latest {
        Some(latest) => {
            // Bump: mark prior latest as not-latest, insert new born-published
            // version. The pre-AIR endpoint diverges from `insert_published_version`
            // by binding `published_by` (apply-air's caller is always an
            // authenticated service user; `apply_template` doesn't bind it
            // for historical reasons).
            mark_not_latest(&mut *tx, latest.id).await?;
            let base_id = latest.base_template_id.unwrap_or(latest.id);
            let new_id = Uuid::new_v4();
            let new_version = latest.version + 1;
            sqlx::query_as::<_, WorkflowTemplate>(
                r#"
                INSERT INTO workflow_templates
                    (id, name, description, base_template_id, parent_id, version,
                     is_latest, published, published_at, published_by,
                     graph, air_json, source_ref, author_id, workspace_id, visibility, updated_by)
                VALUES ($1, $2, $3, $4, $5, $6, TRUE, TRUE, NOW(), $7, $8, $9, $10, $11, $12, $13, $7)
                RETURNING *
                "#,
            )
            .bind(new_id)
            .bind(&req.name)
            .bind(&description)
            .bind(base_id)
            .bind(latest.id)
            .bind(new_version)
            .bind(author_id)
            .bind(&stub_graph_json)
            .bind(&req.air_json)
            .bind(source_ref_json.as_ref())
            .bind(author_id)
            .bind(workspace_id)
            .bind(visibility)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| {
                tracing::error!("failed to insert pre-AIR bump version: {e}");
                ApiError::internal(e.to_string())
            })?
        }
        None => {
            // Seed: fresh chain at v1, born-published. `base_template_id = id`
            // matches `create_template`'s convention.
            let new_id = Uuid::new_v4();
            sqlx::query_as::<_, WorkflowTemplate>(
                r#"
                INSERT INTO workflow_templates
                    (id, name, description, base_template_id, version,
                     is_latest, published, published_at, published_by,
                     graph, air_json, source_ref, author_id, workspace_id, visibility, updated_by)
                VALUES ($1, $2, $3, $1, 1, TRUE, TRUE, NOW(), $4, $5, $6, $7, $8, $9, $10, $4)
                RETURNING *
                "#,
            )
            .bind(new_id)
            .bind(&req.name)
            .bind(&description)
            .bind(author_id)
            .bind(&stub_graph_json)
            .bind(&req.air_json)
            .bind(source_ref_json.as_ref())
            .bind(author_id)
            .bind(workspace_id)
            .bind(visibility)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| {
                tracing::error!("failed to seed pre-AIR template: {e}");
                ApiError::internal(e.to_string())
            })?
        }
    };

    tx.commit()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    tracing::info!(
        template_id = %applied.id,
        version = applied.version,
        name = %applied.name,
        actor = %user.subject,
        "applied pre-AIR template"
    );

    // Post-commit: in-memory trigger registry. On Bump, the prior version's
    // triggers stay registered under the old template id until we forget
    // them — do that explicitly. Belt-and-suspenders per Q4 disposition: we
    // also defensively call `forget_template` for the applied id in case a
    // re-apply against a stale dispatcher leaked a record (operationally
    // unreachable post-Q3 collision check, but cheap).
    let publisher = PublishService::new(&state);
    if applied.version > 1 {
        if let Some(parent_id) = applied.parent_id {
            state.triggers.forget_template(parent_id);
        }
    }
    state.triggers.forget_template(applied.id);
    let registered = publisher.register_triggers(&applied).await;
    if registered > 0 {
        tracing::info!(
            template_id = %applied.id,
            registered,
            "registered triggers on apply-air"
        );
    }

    Ok(Json(applied))
}

/// GET /api/v1/templates/{id}/versions
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/versions",
    params(("id" = Uuid, Path, description = "Any template id in the version chain")),
    responses(
        (status = 200, description = "All versions in the template's chain, newest first", body = Vec<WorkflowTemplate>),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn list_versions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<WorkflowTemplate>>, ApiError> {
    // First find the base_template_id for this template
    let existing = require_template(&state.db, id).await?;

    let base_id = existing.chain_root_id();

    let versions = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE base_template_id = $1 ORDER BY version DESC",
    )
    .bind(base_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(versions))
}

/// GET /api/v1/templates/{id}/latest
///
/// Resolve any id in a template's version chain to the row currently flagged
/// `is_latest`. Accepts the chain root (`base_template_id`) — the stable
/// identifier the CLI's `mekhan.lock.json` pins — or any historical version
/// id; both resolve through the same `base_template_id` column.
///
/// CLI commands that need "the chain head right now" (`run`, `test`, the
/// post-pull bundle fetch) call this first, then operate on the returned id.
/// The split keeps `/bundle`, `/instances`, `/tests/...` semantics
/// strictly version-id-scoped (you can still pull a historical version by
/// passing its concrete id) — the resolver layer is the only place that
/// follows the chain.
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/latest",
    params(("id" = Uuid, Path, description = "Any template id in the version chain (base or a specific version)")),
    responses(
        (status = 200, description = "The latest version in the template's chain", body = WorkflowTemplate),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn get_latest(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    let existing = require_template(&state.db, id).await?;

    let base_id = existing.chain_root_id();
    let latest = latest_in_chain(&state.db, base_id).await?;
    Ok(Json(latest))
}

/// GET /api/v1/templates/{id}/air
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/air",
    params(("id" = Uuid, Path, description = "Template id")),
    responses(
        (status = 200, description = "Compiled AIR JSON for the published template", body = serde_json::Value),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 409, description = "Template is not published", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn get_air(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let existing = require_template(&state.db, id).await?;

    if !existing.published {
        return Err(ApiError::conflict("template is not published"));
    }

    let air = existing
        .air_json
        .ok_or_else(|| ApiError::internal("published template has no AIR JSON"))?;

    Ok(Json(air))
}

/// POST /api/v1/templates/{id}/compile
#[utoipa::path(
    post,
    path = "/api/v1/templates/{id}/compile",
    params(("id" = Uuid, Path, description = "Template id")),
    responses(
        (status = 200, description = "Compiled AIR JSON preview from current draft graph", body = serde_json::Value),
        (status = 400, description = "Compilation failed or graph invalid", body = ErrorResponse),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn compile_preview(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let existing = require_template(&state.db, id).await?;
    let publishing_family = Some(existing.chain_root_id());

    let graph: WorkflowGraph = serde_json::from_value(existing.graph)
        .map_err(|e| ApiError::bad_request(format!("invalid graph: {e}")))?;

    // Try to pull files from the Y.Doc so the preview AIR matches what publish
    // would emit. If the Y.Doc has nothing, the empty map yields a compile
    // error for any automated_step (same as publish would, just earlier).
    let ydoc_files = match reconstruct_graph_from_ydoc(&state, id).await {
        Ok(Some((_, f))) => f,
        _ => HashMap::new(),
    };
    // Mirror the publish path: storage_path NodeFiles for executor
    // staging + the same inline `ydoc_files` map passed to the borrow
    // planner so the preview AIR matches what publish would emit.
    let files = node_files_storage_path(id, existing.version, &ydoc_files);

    let sub_air = resolve_subworkflow_air(&state, publishing_family, &graph).await?;

    let air = compile_to_air_with_options(
        &graph,
        &existing.name,
        &existing.description,
        &files,
        CompileOptions {
            inline_sources: &ydoc_files,
            sub_air: &sub_air,
            ..Default::default()
        },
    )
    .map_err(|e| {
        let view = e.to_view();
        ApiError::compile(format!("compilation failed: {e}"), vec![view])
    })?
    .air;

    Ok(Json(air))
}

/// GET /api/v1/templates/{id}/io-stubs
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/io-stubs",
    params(("id" = Uuid, Path, description = "Template id")),
    responses(
        (status = 200, description = "Per-node generated `_aithericon_io` files", body = serde_json::Value),
        (status = 404, description = "Template not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "templates",
)]
/// Generated `_aithericon_io` pair (`.py` SDK delegate + typed `.pyi` overlay)
/// per Python automated step, derived from each node's input scope. An
/// authoring aid: unlike compile it does NOT require the graph to be
/// publishable (missing entrypoints / dangling edges are fine) — it only needs
/// a DAG. The IDE surfaces these read-only so step code gets typed
/// `load_input()` before publish. Non-fatal by design: a graph that can't be
/// scoped yields an empty map, never an error, so the editor never breaks on
/// this.
pub async fn io_stubs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let existing = require_template(&state.db, id).await?;

    // Prefer the live Y.Doc graph (what the author sees in the IDE). `diagnostic`
    // is surfaced to the editor so an empty scope explains itself instead of
    // looking broken. Crucially: if the Y.Doc exists but won't reconstruct we do
    // NOT silently serve the stale DB graph — that showed pre-edit scopes and is
    // exactly why "I added a field but it doesn't show up" happened. Honest-empty
    // beats confidently-wrong for an authoring aid.
    let (graph, mut diagnostic): (Option<WorkflowGraph>, String) =
        match reconstruct_graph_from_ydoc(&state, id).await {
            Ok(Some((g, _))) => (Some(g), "ok".to_string()),
            Ok(None) => (
                serde_json::from_value(existing.graph).ok(),
                "no_ydoc_using_saved_graph".to_string(),
            ),
            Err(e) => {
                tracing::warn!(template = %id, error = %e, "io_stubs: Y.Doc unreadable");
                (None, format!("ydoc_unreadable: {e}"))
            }
        };

    let mut generated: HashMap<String, HashMap<String, String>> = HashMap::new();
    // Structured per-node input scope so the editor's reference panel can
    // render `token.<field>` without parsing the generated `.pyi`. Ordered
    // (BTreeMap) for stable display.
    let mut scopes_out: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    if let Some(graph) = graph {
        let ns_scopes = node_namespace_scopes(&graph).ok();
        let outputs = node_output_fields(&graph);
        match node_input_scopes(&graph) {
            Ok(scopes) => {
                for node in &graph.nodes {
                    if let WorkflowNodeData::AutomatedStep { execution_spec, .. } = &node.data {
                        if execution_spec.backend_type == ExecutionBackendType::Python {
                            if let Some(scope) = scopes.get(&node.id) {
                                let entry = generated.entry(node.id.clone()).or_default();
                                let empty: std::collections::BTreeMap<
                                    String,
                                    std::collections::BTreeMap<
                                        String,
                                        crate::models::template::FieldKind,
                                    >,
                                > = std::collections::BTreeMap::new();
                                let empty_out: std::collections::BTreeMap<
                                    String,
                                    crate::models::template::FieldKind,
                                > = std::collections::BTreeMap::new();
                                let ns = ns_scopes
                                    .as_ref()
                                    .and_then(|m| m.get(&node.id))
                                    .unwrap_or(&empty);
                                let out = outputs.get(&node.id).unwrap_or(&empty_out);
                                for (filename, source) in generate_py_io_files(scope, ns, out) {
                                    entry.insert(filename.to_string(), source);
                                }
                                scopes_out.insert(
                                    node.id.clone(),
                                    scope
                                        .iter()
                                        .map(|(name, kind)| {
                                            serde_json::json!({ "name": name, "kind": kind })
                                        })
                                        .collect(),
                                );
                            }
                        }
                    }
                }
            }
            // A mid-authoring graph that isn't yet a clean DAG (no Start, a
            // cycle, dangling edges) can't be scoped — say so rather than
            // showing a misleading empty panel.
            Err(e) => {
                diagnostic = format!("graph_not_scopable: {e}");
            }
        }
    }

    Ok(Json(serde_json::json!({
        "generated": generated,
        "scopes": scopes_out,
        "diagnostic": diagnostic,
    })))
}

/// POST /api/v1/compile
/// POST /api/v1/compile
///
/// Stateless compilation: accepts a graph (and optional inline file contents)
/// and returns AIR JSON without database access. Used by the editor's "Preview
/// AIR" button before publish.
#[utoipa::path(
    post,
    path = "/api/v1/compile",
    request_body = CompileRequest,
    responses(
        (status = 200, description = "Compiled AIR JSON", body = serde_json::Value),
        (status = 400, description = "Compilation failed", body = ErrorResponse),
    ),
    tag = "templates",
)]
pub async fn compile_graph(
    Json(body): Json<CompileRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let description = body.description.as_deref().unwrap_or("");
    let files = node_files_inline(&body.files);
    let air = compile_to_air(&body.graph, &body.name, description, &files).map_err(|e| {
        let view = e.to_view();
        ApiError::compile(format!("compilation failed: {e}"), vec![view])
    })?;
    Ok(Json(air))
}

/// One reachable, producer-attributed reference the guard picker should
/// offer at a node. The single source of truth for editor scope —
/// replaces the deleted client-side `computeScopes` reimplementation.
///
/// `ty` is the recursive [`TyDescriptor`] tree so the picker can drill into
/// nested objects and array element shapes without additional calls; for
/// File-anchored containers the tree's root carries `selectable: true` so
/// the row is pickable as a whole while its children are individually
/// pickable too.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct ScopeEntryDto {
    /// What you'd type in a guard, e.g. `input.data.invoice_amount`.
    pub path: String,
    /// Recursive type descriptor. Single source of truth for the picker's
    /// nested drill-down and (later) array `[*]` iteration affordance.
    pub ty: TyDescriptor,
    pub producer_node: String,
    pub producer_label: String,
    pub note: String,
}

/// Flattened guard diagnostic (`node_id` is highlighted in the editor).
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct GuardDiagnosticDto {
    pub kind: String,
    pub node_id: String,
    pub message: String,
}

/// Shape-aware analysis surface — per-node scope + diagnostics. Pure and
/// graph-only: works on drafts that can't compile/publish yet.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct TypeSurfaceResponse {
    pub graph_ok: bool,
    pub scopes: HashMap<String, Vec<ScopeEntryDto>>,
    pub diagnostics: Vec<GuardDiagnosticDto>,
}

/// Stateless shape analysis: the editor's single source of truth for guard
/// scope + diagnostics. Independent of `compile_to_air` succeeding (no files
/// needed) so feedback lands while editing, not at publish.
#[utoipa::path(
    post,
    path = "/api/v1/analyze",
    request_body = CompileRequest,
    responses(
        (status = 200, description = "Shape-aware scope + diagnostics", body = TypeSurfaceResponse),
    ),
    tag = "templates",
)]
pub async fn analyze_graph(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<CompileRequest>,
) -> Result<Json<TypeSurfaceResponse>, ApiError> {
    // Server-authoritative named-global registry: when the editor sends the
    // draft's workspace (resources) / template (assets) ids, resolve them so the
    // picker + diagnostics see `<resource>.<field>` / `<asset>.<field>` as a
    // "Globals" scope rather than a false-unresolved error. This is the
    // **registry-only** discovery half — no graph mutation, no DB writes. It must
    // never 500 the analyze endpoint: a discovery failure (transient DB error,
    // mid-edit draft) degrades to the empty registry (producer-only surface), so
    // the editor keeps getting feedback on every keystroke.
    let known_globals = if body.workspace_id.is_some() || body.template_id.is_some() {
        // Registry-only: empty inline sources (no Python bodies on the editor
        // path) and non-strict (a mid-edit draft must never hard-fail).
        match crate::process::discover::discover_named_globals(
            &state,
            &body.graph,
            body.workspace_id,
            body.template_id,
            &std::collections::HashMap::new(),
            false,
        )
        .await
        {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(
                    "analyze named-global discovery failed (degrading to empty registry): {e:?}"
                );
                Default::default()
            }
        }
    } else {
        Default::default()
    };
    let s = crate::compiler::surface_types(&body.graph, &known_globals);
    let scopes = s
        .scopes
        .into_iter()
        .map(|(node_id, entries)| {
            let mapped = entries
                .into_iter()
                .map(|e| ScopeEntryDto {
                    path: e.path,
                    ty: e.ty,
                    producer_node: e.producer_node,
                    producer_label: e.producer_label,
                    note: e.note,
                })
                .collect();
            (node_id, mapped)
        })
        .collect();
    let diagnostics = s
        .diagnostics
        .iter()
        .map(|d| {
            let (kind, node_id, message) = d.dto();
            GuardDiagnosticDto {
                kind: kind.to_string(),
                node_id,
                message,
            }
        })
        .collect();
    Ok(Json(TypeSurfaceResponse {
        graph_ok: s.graph_ok,
        scopes,
        diagnostics,
    }))
}

/// Run every enabled test for `existing`'s template family against a
/// freshly-compiled AIR. Returns the list of failing (or erroring, or
/// stale) tests so the publish handler can either block or be overridden
/// with `?force=true`.
///
/// "Stale" only matters in the strict sense after publish — pre-publish we
/// always re-run, so the version-staleness check folds into the live result.
async fn run_publish_gate(
    state: &AppState,
    existing: &WorkflowTemplate,
    air_json: &serde_json::Value,
    graph: &WorkflowGraph,
    created_by: Uuid,
) -> Result<Vec<FailingTestInfo>, ApiError> {
    let family = existing.chain_root_id();

    let tests: Vec<TemplateTest> = sqlx::query_as::<_, TemplateTest>(
        "SELECT * FROM template_tests \
         WHERE template_id = $1 AND enabled = TRUE \
         ORDER BY created_at ASC",
    )
    .bind(family)
    .fetch_all(&state.db)
    .await?;

    if tests.is_empty() {
        return Ok(Vec::new());
    }

    let ctx = RunContext {
        template_id: existing.id,
        template_version: existing.version,
        workspace_id: existing.workspace_id,
        air_json: air_json.clone(),
        graph: graph.clone(),
        created_by,
    };

    let mut failing = Vec::new();
    for test in &tests {
        let run = run_test(state, &ctx, test).await?;
        if run.status != "passed" {
            let reason = match run.status.as_str() {
                "failed" => "assertion failed".to_string(),
                "error" => run
                    .failure_detail
                    .as_ref()
                    .and_then(|d| d.get("reason"))
                    .and_then(|r| r.as_str())
                    .unwrap_or("runtime error")
                    .to_string(),
                other => format!("unexpected status '{other}'"),
            };
            failing.push(FailingTestInfo {
                test_id: test.id,
                name: test.name.clone(),
                reason,
                run_id: Some(run.id),
            });
        }
    }
    Ok(failing)
}

#[cfg(test)]
mod apply_mode_tests {
    use super::{apply_mode, ApplyMode, WorkflowTemplate};
    use chrono::Utc;
    use uuid::Uuid;

    pub(super) fn tmpl(version: i32, published: bool, parent_id: Option<Uuid>) -> WorkflowTemplate {
        WorkflowTemplate {
            id: Uuid::new_v4(),
            name: "t".into(),
            description: String::new(),
            base_template_id: None,
            parent_id,
            version,
            is_latest: true,
            published,
            published_at: None,
            published_by: None,
            graph: serde_json::json!({}),
            air_json: None,
            interface_json: None,
            source_ref: None,
            author_id: Uuid::new_v4(),
            updated_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            workspace_id: Uuid::nil(),
            visibility: "workspace".into(),
            owner_template_id: None,
            template_kind: "workflow".into(),
            origin: None,
            coordinate: None,
            presentation: None,
            lifecycle_status: "active".into(),
            superseded_by: None,
            forked_from: None,
            my_effective_role: None,
        }
    }

    #[test]
    fn fresh_init_draft_seeds() {
        // v1 / parent NULL / unpublished — the only shape `mekhan init` makes.
        assert_eq!(apply_mode(&tmpl(1, false, None)).unwrap(), ApplyMode::Seed);
    }

    #[test]
    fn published_head_bumps() {
        let t = tmpl(3, true, Some(Uuid::new_v4()));
        assert_eq!(apply_mode(&t).unwrap(), ApplyMode::Bump);
    }

    #[test]
    fn ui_new_version_draft_conflicts() {
        // unpublished, version > 1 → a web-editor `new_version` draft → 409.
        let err = apply_mode(&tmpl(2, false, Some(Uuid::new_v4()))).unwrap_err();
        assert!(err.contains("web-editor draft"), "got: {err}");
    }

    #[test]
    fn unpublished_v1_with_parent_is_not_seed() {
        // Defensive: v1 but parent set is not a fresh init → must not Seed.
        assert!(apply_mode(&tmpl(1, false, Some(Uuid::new_v4()))).is_err());
    }
}

#[cfg(test)]
mod discard_mode_tests {
    use super::apply_mode_tests::tmpl;
    use super::{discard_mode, DiscardMode};
    use uuid::Uuid;

    #[test]
    fn published_version_refused() {
        let err = discard_mode(&tmpl(2, true, Some(Uuid::new_v4()))).unwrap_err();
        assert!(err.contains("published"), "got: {err}");
    }

    #[test]
    fn root_draft_deletes_chain() {
        // Never-published v1 (parent NULL) — the only shape `create_template`
        // makes; the draft IS the chain.
        assert_eq!(
            discard_mode(&tmpl(1, false, None)).unwrap(),
            DiscardMode::DeleteChain
        );
    }

    #[test]
    fn forked_draft_restores_parent() {
        let parent = Uuid::new_v4();
        assert_eq!(
            discard_mode(&tmpl(3, false, Some(parent))).unwrap(),
            DiscardMode::RestoreParent(parent)
        );
    }

    #[test]
    fn non_head_draft_refused() {
        // Defensive: an unpublished non-latest row shouldn't exist; restoring
        // its parent would mint a second `is_latest` head.
        let mut t = tmpl(2, false, Some(Uuid::new_v4()));
        t.is_latest = false;
        let err = discard_mode(&t).unwrap_err();
        assert!(err.contains("chain head"), "got: {err}");
    }
}
