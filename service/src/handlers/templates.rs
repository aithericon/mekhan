use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::compiler::{
    compile_to_air, compile_to_air_with_subworkflows_inline, generate_py_io_files,
    node_files_inline, node_files_storage_path, node_input_scopes, node_namespace_scopes,
    node_output_fields, TyDescriptor,
};
use crate::handlers::template_tests::{run_test, RunContext};
use crate::lifecycle::cleanup_net;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{
    ApplyTemplateRequest, CompileRequest, CreateTemplateRequest, ExecutionBackendType,
    ListTemplatesQuery, PaginatedResponse, UpdateTemplateRequest, WorkflowGraph, WorkflowNodeData,
    WorkflowTemplate,
};
use crate::models::template_test::{FailingTestInfo, PublishGateBlockedResponse, TemplateTest};
use crate::process::publish::{resolve_subworkflow_air, CompiledArtifacts, PublishService};
use crate::AppState;

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
    let graph_json = serde_json::to_value(&graph).unwrap();
    let description = req.description.unwrap_or_default();

    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        INSERT INTO workflow_templates (id, name, description, base_template_id, version, is_latest, graph, author_id)
        VALUES ($1, $2, $3, $1, 1, TRUE, $4, $5)
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(&req.name)
    .bind(&description)
    .bind(&graph_json)
    .bind(user.subject_as_uuid())
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
        .map(|(node_id, files)| format!("{node_id}=[{}]", files.keys().cloned().collect::<Vec<_>>().join(",")))
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

/// GET /api/v1/templates
#[utoipa::path(
    get,
    path = "/api/v1/templates",
    params(ListTemplatesQuery),
    responses(
        (status = 200, description = "Paginated list of templates", body = PaginatedResponse<WorkflowTemplate>),
    ),
    tag = "templates",
)]
pub async fn list_templates(
    State(state): State<AppState>,
    Query(params): Query<ListTemplatesQuery>,
) -> Json<PaginatedResponse<WorkflowTemplate>> {
    let offset = (params.page - 1) * params.per_page;

    // Build dynamic query based on filters
    let (items, total): (Vec<WorkflowTemplate>, i64) = if let Some(base_id) = params.base_template_id {
        // List versions for a specific template chain
        let items = sqlx::query_as::<_, WorkflowTemplate>(
            "SELECT * FROM workflow_templates WHERE base_template_id = $1 ORDER BY version DESC LIMIT $2 OFFSET $3",
        )
        .bind(base_id)
        .bind(params.per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        let total: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM workflow_templates WHERE base_template_id = $1",
        )
        .bind(base_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or((0,));

        (items, total.0)
    } else {
        // List latest versions, optionally filtered — all parameters bound safely
        match (params.published, &params.search) {
            (Some(published), Some(search)) => {
                let pattern = format!("%{search}%");
                let items = sqlx::query_as::<_, WorkflowTemplate>(
                    "SELECT * FROM workflow_templates WHERE is_latest = TRUE AND published = $1 AND (name ILIKE $2 OR description ILIKE $2) ORDER BY updated_at DESC LIMIT $3 OFFSET $4",
                )
                .bind(published)
                .bind(&pattern)
                .bind(params.per_page)
                .bind(offset)
                .fetch_all(&state.db)
                .await
                .unwrap_or_default();

                let total: (i64,) = sqlx::query_as(
                    "SELECT COUNT(*) FROM workflow_templates WHERE is_latest = TRUE AND published = $1 AND (name ILIKE $2 OR description ILIKE $2)",
                )
                .bind(published)
                .bind(&pattern)
                .fetch_one(&state.db)
                .await
                .unwrap_or((0,));

                (items, total.0)
            }
            (Some(published), None) => {
                let items = sqlx::query_as::<_, WorkflowTemplate>(
                    "SELECT * FROM workflow_templates WHERE is_latest = TRUE AND published = $1 ORDER BY updated_at DESC LIMIT $2 OFFSET $3",
                )
                .bind(published)
                .bind(params.per_page)
                .bind(offset)
                .fetch_all(&state.db)
                .await
                .unwrap_or_default();

                let total: (i64,) = sqlx::query_as(
                    "SELECT COUNT(*) FROM workflow_templates WHERE is_latest = TRUE AND published = $1",
                )
                .bind(published)
                .fetch_one(&state.db)
                .await
                .unwrap_or((0,));

                (items, total.0)
            }
            (None, Some(search)) => {
                let pattern = format!("%{search}%");
                let items = sqlx::query_as::<_, WorkflowTemplate>(
                    "SELECT * FROM workflow_templates WHERE is_latest = TRUE AND (name ILIKE $1 OR description ILIKE $1) ORDER BY updated_at DESC LIMIT $2 OFFSET $3",
                )
                .bind(&pattern)
                .bind(params.per_page)
                .bind(offset)
                .fetch_all(&state.db)
                .await
                .unwrap_or_default();

                let total: (i64,) = sqlx::query_as(
                    "SELECT COUNT(*) FROM workflow_templates WHERE is_latest = TRUE AND (name ILIKE $1 OR description ILIKE $1)",
                )
                .bind(&pattern)
                .fetch_one(&state.db)
                .await
                .unwrap_or((0,));

                (items, total.0)
            }
            (None, None) => {
                let items = sqlx::query_as::<_, WorkflowTemplate>(
                    "SELECT * FROM workflow_templates WHERE is_latest = TRUE ORDER BY updated_at DESC LIMIT $1 OFFSET $2",
                )
                .bind(params.per_page)
                .bind(offset)
                .fetch_all(&state.db)
                .await
                .unwrap_or_default();

                let total: (i64,) = sqlx::query_as(
                    "SELECT COUNT(*) FROM workflow_templates WHERE is_latest = TRUE",
                )
                .fetch_one(&state.db)
                .await
                .unwrap_or((0,));

                (items, total.0)
            }
        }
    };

    Json(PaginatedResponse {
        items,
        total,
        page: params.page,
        per_page: params.per_page,
    })
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
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("failed to get template: {e}");
        ApiError::internal(e.to_string())
    })?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    Ok(Json(template))
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
    Path(id): Path<Uuid>,
) -> Result<Json<TemplateBundle>, ApiError> {
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    let (graph, files) = match reconstruct_graph_from_ydoc(&state, id).await {
        Ok(Some((g, f))) => (g, f),
        Ok(None) => {
            let g = serde_json::from_value(existing.graph.clone())
                .map_err(|e| ApiError::internal(format!("invalid graph: {e}")))?;
            (g, HashMap::new())
        }
        Err(e) => {
            tracing::error!("failed to load Y.Doc for template {id}: {e}");
            let g = serde_json::from_value(existing.graph.clone())
                .map_err(|e| ApiError::internal(format!("invalid graph: {e}")))?;
            (g, HashMap::new())
        }
    };

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
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTemplateRequest>,
) -> Result<Json<WorkflowTemplate>, ApiError> {
    // Check if template exists and is not published
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    if existing.published {
        return Err(ApiError::conflict("cannot edit a published template"));
    }

    let name = req.name.unwrap_or(existing.name);
    let description = req.description.unwrap_or(existing.description);
    let graph = req
        .graph
        .map(|g| serde_json::to_value(&g).unwrap())
        .unwrap_or(existing.graph);

    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        UPDATE workflow_templates
        SET name = $2, description = $3, graph = $4, updated_at = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(&name)
    .bind(&description)
    .bind(&graph)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("failed to update template: {e}");
        ApiError::internal(e.to_string())
    })?;

    Ok(Json(template))
}

/// DELETE /api/v1/templates/{id}
/// Per Section 11.7: cascade cleanup for published templates with finished instances.
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
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    let base_id = existing.base_template_id.unwrap_or(existing.id);

    if existing.published {
        // Check for running instances across all versions in this chain
        let running_count: (i64,) = sqlx::query_as(
            r#"SELECT COUNT(*) FROM workflow_instances
               WHERE template_id IN (SELECT id FROM workflow_templates WHERE base_template_id = $1)
               AND status = 'running'"#,
        )
        .bind(base_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or((0,));

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
        .await
        .unwrap_or_default();

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
    }

    // Capture every version id in the chain before the delete so we can drop
    // their triggers from the in-memory dispatcher afterwards (otherwise a
    // deleted template's triggers keep firing until the next restart).
    let version_ids: Vec<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM workflow_templates WHERE base_template_id = $1")
            .bind(base_id)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();

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
    }

    Ok(StatusCode::NO_CONTENT)
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
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    if existing.published {
        return Err(ApiError::conflict("template is already published"));
    }

    // Try to reconstruct graph + files from Y.Doc first (collaborative editing source of truth),
    // falling back to the DB graph column for legacy templates.
    let (graph, mut ydoc_files): (WorkflowGraph, HashMap<String, HashMap<String, String>>) =
        match reconstruct_graph_from_ydoc(&state, id).await {
            Ok(Some((g, f))) => (g, f),
            Ok(None) => {
                // No Y.Doc exists — fall back to DB graph
                let g = serde_json::from_value(existing.graph.clone())
                    .map_err(|e| ApiError::bad_request(format!("invalid graph: {e}")))?;
                (g, HashMap::new())
            }
            Err(e) => {
                tracing::error!("failed to load Y.Doc for template {id}: {e}");
                // Fall back to DB graph
                let g = serde_json::from_value(existing.graph.clone())
                    .map_err(|e| ApiError::bad_request(format!("invalid graph: {e}")))?;
                (g, HashMap::new())
            }
        };

    let publisher = PublishService::new(&state);

    // Synthesize Python IO stubs + compile AIR + serialize the graph in one
    // shared step (identical to `apply`). `ydoc_files` is mutated so the S3
    // upload below stages exactly what was compiled against.
    let CompiledArtifacts {
        air_json,
        graph_json,
        interface_json,
        node_configs,
    } = publisher
        .compile_artifacts(
            &graph,
            &existing.name,
            &existing.description,
            id,
            existing.version,
            Some(existing.base_template_id.unwrap_or(existing.id)),
            &mut ydoc_files,
            principal_id,
        )
        .await?;

    // Upload node file contents to S3 so the executor can stage them at
    // runtime. Non-fatal for UI publish (legacy behavior).
    if let Err(e) = publisher.upload_files(id, existing.version, &ydoc_files).await {
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
    let failing = run_publish_gate(&state, &existing, &air_json, &graph, user.subject_as_uuid()).await?;
    if !failing.is_empty() {
        if query.force {
            tracing::warn!(
                template_id = %id,
                failing = failing.len(),
                "publish gate bypassed via ?force=true"
            );
        } else {
            let failing_json = serde_json::to_value(&failing)
                .map_err(|e| ApiError::internal(e.to_string()))?;
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
    let template =
        finalize_publish_row(&state.db, id, &air_json, &graph_json, &interface_json, None).await?;

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

    let result = tokio::task::spawn_blocking(move || -> Result<(WorkflowGraph, HashMap<String, HashMap<String, String>>), String> {
        use crate::yjs::persistence::YjsPersistence;
        use crate::yjs::doc_ops;

        let doc = YjsPersistence::build_doc_from_raw(snapshot.as_deref(), &updates)
            .map_err(|e| e.to_string())?;

        let graph = doc_ops::doc_to_graph(&doc)?;
        let files = doc_ops::extract_files_from_doc(&doc);
        Ok((graph, files))
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))??;

    Ok(Some(result))
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
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

/// The publish-finalize UPDATE: freeze the row, store compiled AIR + the
/// just-compiled graph, and stamp git provenance. `source_ref` is `None` for a
/// UI publish (column stays NULL) and `Some` for `apply`'s seed path. Generic
/// over the executor so publish (pool) and apply (txn) share one statement —
/// the single place the `source_ref` column is threaded on an UPDATE.
async fn finalize_publish_row<'e, E>(
    exec: E,
    id: Uuid,
    air_json: &serde_json::Value,
    graph_json: &serde_json::Value,
    interface_json: &serde_json::Value,
    source_ref: Option<&serde_json::Value>,
) -> Result<WorkflowTemplate, ApiError>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        UPDATE workflow_templates
        SET published = TRUE, published_at = NOW(), air_json = $2, graph = $3,
            interface_json = $4, source_ref = $5, updated_at = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(air_json)
    .bind(graph_json)
    .bind(interface_json)
    .bind(source_ref)
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
    source_ref: Option<&serde_json::Value>,
) -> Result<WorkflowTemplate, ApiError>
where
    E: sqlx::PgExecutor<'e>,
{
    let base_id = src.base_template_id.unwrap_or(src.id);
    sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        INSERT INTO workflow_templates
            (id, name, description, base_template_id, parent_id, version,
             is_latest, published, published_at, graph, air_json,
             interface_json, source_ref, author_id)
        VALUES ($1, $2, $3, $4, $5, $6, TRUE, TRUE, NOW(), $7, $8, $9, $10, $11)
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
    .fetch_one(exec)
    .await
    .map_err(|e| {
        tracing::error!("failed to insert published version: {e}");
        ApiError::internal(e.to_string())
    })
}

/// Fetch the newest version (highest `version`) in a template's chain.
async fn latest_in_chain(
    pool: &sqlx::PgPool,
    base_id: Uuid,
) -> Result<WorkflowTemplate, ApiError> {
    sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE base_template_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(base_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
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
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    if !existing.published {
        return Err(ApiError::conflict(
            "can only create new version from a published template",
        ));
    }

    let new_id = Uuid::new_v4();
    let new_version = existing.version + 1;
    let base_id = existing.base_template_id.unwrap_or(existing.id);

    // The authored workflow lives in the *source's Y.Doc*, not the `graph`
    // column — publish/edit never write the column back, so copying
    // `existing.graph` would fork a blank canvas. Reconstruct the real graph
    // + per-node files from the Y.Doc (same source of truth publish uses),
    // falling back to the column only for legacy templates with no Y.Doc.
    let (graph, files): (WorkflowGraph, HashMap<String, HashMap<String, String>>) =
        match reconstruct_graph_from_ydoc(&state, id).await {
            Ok(Some((g, f))) => (g, f),
            Ok(None) => (
                serde_json::from_value(existing.graph.clone())
                    .unwrap_or_else(|_| WorkflowGraph::default_graph()),
                HashMap::new(),
            ),
            Err(e) => {
                tracing::error!(
                    "failed to load source Y.Doc for new version of {id}: {e}"
                );
                (
                    serde_json::from_value(existing.graph.clone())
                        .unwrap_or_else(|_| WorkflowGraph::default_graph()),
                    HashMap::new(),
                )
            }
        };
    let graph_json =
        serde_json::to_value(&graph).map_err(|e| ApiError::internal(e.to_string()))?;

    // Start a transaction
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Mark old version as not latest
    mark_not_latest(&mut *tx, id).await?;

    // Create new version
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        INSERT INTO workflow_templates (id, name, description, base_template_id, parent_id, version, is_latest, graph, author_id)
        VALUES ($1, $2, $3, $4, $5, $6, TRUE, $7, $8)
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
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("failed to create new version: {e}");
        ApiError::internal(e.to_string())
    })?;

    tx.commit()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

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
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    let base_id = existing.base_template_id.unwrap_or(existing.id);
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
    } = publisher
        .compile_artifacts(
            &graph,
            &latest.name,
            &latest.description,
            target_id,
            target_version,
            Some(latest.base_template_id.unwrap_or(latest.id)),
            &mut files_map,
            user.subject_as_uuid(),
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
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let applied = match mode {
        ApplyMode::Seed => {
            finalize_publish_row(
                &mut *tx,
                latest.id,
                &air_json,
                &graph_json,
                &interface_json,
                source_ref_json.as_ref(),
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
                source_ref_json.as_ref(),
            )
            .await?
        }
    };

    tx.commit()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

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
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    let base_id = existing.base_template_id.unwrap_or(existing.id);

    let versions = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE base_template_id = $1 ORDER BY version DESC",
    )
    .bind(base_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

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
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    let base_id = existing.base_template_id.unwrap_or(existing.id);
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
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

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
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

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

    let publishing_family = Some(existing.base_template_id.unwrap_or(existing.id));
    let sub_air = resolve_subworkflow_air(&state, publishing_family, &graph).await?;

    let air = compile_to_air_with_subworkflows_inline(
        &graph,
        &existing.name,
        &existing.description,
        &files,
        &ydoc_files,
        &sub_air,
    )
    .map_err(|e| {
        let view = e.to_view();
        ApiError::compile(format!("compilation failed: {e}"), vec![view])
    })?;

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
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

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
    let air = compile_to_air(&body.graph, &body.name, description, &files)
        .map_err(|e| {
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
    Json(body): Json<CompileRequest>,
) -> Result<Json<TypeSurfaceResponse>, ApiError> {
    let s = crate::compiler::surface_types(&body.graph);
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
    let family = existing.base_template_id.unwrap_or(existing.id);

    let tests: Vec<TemplateTest> = sqlx::query_as::<_, TemplateTest>(
        "SELECT * FROM template_tests \
         WHERE template_id = $1 AND enabled = TRUE \
         ORDER BY created_at ASC",
    )
    .bind(family)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;

    if tests.is_empty() {
        return Ok(Vec::new());
    }

    let ctx = RunContext {
        template_id: existing.id,
        template_version: existing.version,
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

    fn tmpl(version: i32, published: bool, parent_id: Option<Uuid>) -> WorkflowTemplate {
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
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
