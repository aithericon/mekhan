use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::json;
use uuid::Uuid;

use serde::Deserialize;

use aithericon_executor_domain::InputSource;

use crate::compiler::compile_to_air;
use crate::lifecycle::cleanup_net;
use crate::models::template::{
    CreateTemplateRequest, ListTemplatesQuery, PaginatedResponse, UpdateTemplateRequest,
    WorkflowGraph, WorkflowTemplate,
};
use crate::AppState;

/// Request body for stateless compilation.
#[derive(Debug, Deserialize)]
pub struct CompileRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub graph: WorkflowGraph,
    /// Per-node, per-filename inline file contents. The compiler emits
    /// `InputSource::Raw` entries for these so the preview AIR shows the same
    /// staging shape that publish produces (with `StoragePath` keys).
    #[serde(default)]
    pub files: HashMap<String, HashMap<String, String>>,
}

/// POST /api/templates
pub async fn create_template(
    State(state): State<AppState>,
    Json(req): Json<CreateTemplateRequest>,
) -> impl IntoResponse {
    let id = Uuid::new_v4();
    let graph = req.graph.unwrap_or_else(WorkflowGraph::default_graph);
    let graph_json = serde_json::to_value(&graph).unwrap();
    let description = req.description.unwrap_or_default();

    let result = sqlx::query_as::<_, WorkflowTemplate>(
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
    .bind(req.author_id)
    .fetch_one(&state.db)
    .await;

    match result {
        Ok(template) => {
            // Initialize Y.Doc from the graph for real-time collaboration
            if let Err(e) = state
                .yjs
                .persistence
                .init_doc_from_graph(id, &graph)
                .await
            {
                tracing::error!("failed to init Y.Doc for template {id}: {e}");
                // Non-fatal: template is created, Y.Doc can be initialized later
            }

            (StatusCode::CREATED, Json(json!(template))).into_response()
        }
        Err(e) => {
            tracing::error!("failed to create template: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// GET /api/templates
pub async fn list_templates(
    State(state): State<AppState>,
    Query(params): Query<ListTemplatesQuery>,
) -> impl IntoResponse {
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

/// GET /api/templates/:id
pub async fn get_template(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let result = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    match result {
        Ok(Some(template)) => Json(json!(template)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "template not found"}))).into_response(),
        Err(e) => {
            tracing::error!("failed to get template: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// PUT /api/templates/:id
pub async fn update_template(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTemplateRequest>,
) -> impl IntoResponse {
    // Check if template exists and is not published
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    let existing = match existing {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "template not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    if existing.published {
        return (StatusCode::CONFLICT, Json(json!({"error": "cannot edit a published template"}))).into_response();
    }

    let name = req.name.unwrap_or(existing.name);
    let description = req.description.unwrap_or(existing.description);
    let graph = req
        .graph
        .map(|g| serde_json::to_value(&g).unwrap())
        .unwrap_or(existing.graph);

    let result = sqlx::query_as::<_, WorkflowTemplate>(
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
    .await;

    match result {
        Ok(template) => Json(json!(template)).into_response(),
        Err(e) => {
            tracing::error!("failed to update template: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// DELETE /api/templates/:id
/// Per Section 11.7: cascade cleanup for published templates with finished instances.
pub async fn delete_template(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    let existing = match existing {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "template not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

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
            return (StatusCode::CONFLICT, Json(json!({"error": "cannot delete template with active instances"}))).into_response();
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

    // Delete all versions in the template chain
    let result = sqlx::query("DELETE FROM workflow_templates WHERE base_template_id = $1")
        .bind(base_id)
        .execute(&state.db)
        .await;

    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!("failed to delete template: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// POST /api/templates/:id/publish
pub async fn publish_template(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    let existing = match existing {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "template not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    if existing.published {
        return (StatusCode::CONFLICT, Json(json!({"error": "template is already published"}))).into_response();
    }

    // Try to reconstruct graph + files from Y.Doc first (collaborative editing source of truth),
    // falling back to the DB graph column for legacy templates.
    let (graph, ydoc_files): (WorkflowGraph, HashMap<String, HashMap<String, String>>) = match reconstruct_graph_from_ydoc(&state, id).await {
        Ok(Some((g, f))) => (g, f),
        Ok(None) => {
            // No Y.Doc exists — fall back to DB graph
            match serde_json::from_value(existing.graph.clone()) {
                Ok(g) => (g, HashMap::new()),
                Err(e) => {
                    return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid graph: {e}")}))).into_response();
                }
            }
        }
        Err(e) => {
            tracing::error!("failed to load Y.Doc for template {id}: {e}");
            // Fall back to DB graph
            match serde_json::from_value(existing.graph.clone()) {
                Ok(g) => (g, HashMap::new()),
                Err(e) => {
                    return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid graph: {e}")}))).into_response();
                }
            }
        }
    };

    // Upload node file contents to S3 so the executor can stage them at runtime.
    if let Err(e) = upload_node_files(&state, id, existing.version, &ydoc_files).await {
        tracing::warn!("S3 file upload failed (non-fatal): {e}");
    }

    // Build the per-node input source map. Files have just been uploaded to S3
    // under `templates/{tid}/v{ver}/{node_id}/{filename}`, so each one is a
    // StoragePath input — the executor's worker downloads it via the global
    // ArtifactStore at staging time.
    let files = storage_path_files(id, existing.version, &ydoc_files);

    // Compile to AIR
    let air_json = match compile_to_air(&graph, &existing.name, &existing.description, &files) {
        Ok(air) => air,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("compilation failed: {e}")}))).into_response();
        }
    };

    let result = sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        UPDATE workflow_templates
        SET published = TRUE, published_at = NOW(), air_json = $2, updated_at = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .bind(&air_json)
    .fetch_one(&state.db)
    .await;

    match result {
        Ok(template) => Json(json!(template)).into_response(),
        Err(e) => {
            tracing::error!("failed to publish template: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// Try to reconstruct a WorkflowGraph and file contents from the Y.Doc stored for this template.
/// Returns Ok(None) if no Y.Doc exists.
///
/// Reads from the new schema: Y.Map("nodes"), Y.Array("edges"), Y.Map("viewport").
/// Also extracts Y.Text file entries from `nodes[nodeId].files`.
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

/// Upload file contents from nodes to S3 for archival.
///
/// Upload each Y.Text file under the deterministic key
/// `templates/{template_id}/v{version}/{node_id}/{filename}`. The compiler
/// emits `InputSource::StoragePath` references that resolve back to these keys
/// at execution time via the executor worker's global ArtifactStore.
async fn upload_node_files(
    state: &AppState,
    template_id: Uuid,
    version: i32,
    ydoc_files: &HashMap<String, HashMap<String, String>>,
) -> Result<(), String> {
    for (node_id, node_files) in ydoc_files {
        for (filename, content) in node_files {
            match state
                .s3
                .upload_file(template_id, version, node_id, filename, content.as_bytes())
                .await
            {
                Ok(key) => {
                    tracing::info!(
                        node_id = %node_id,
                        filename,
                        key = %key,
                        "uploaded node file to S3"
                    );
                }
                Err(e) => {
                    return Err(format!("upload {}/{}: {}", node_id, filename, e));
                }
            }
        }
    }
    Ok(())
}

/// Build the per-node `name -> InputSource::StoragePath` map that the compiler
/// uses to emit executor inputs. Mirrors the layout written by
/// [`upload_node_files`].
fn storage_path_files(
    template_id: Uuid,
    version: i32,
    ydoc_files: &HashMap<String, HashMap<String, String>>,
) -> HashMap<String, HashMap<String, InputSource>> {
    ydoc_files
        .iter()
        .map(|(node_id, files)| {
            let sources = files
                .keys()
                .map(|filename| {
                    let path =
                        format!("templates/{template_id}/v{version}/{node_id}/{filename}");
                    (
                        filename.clone(),
                        InputSource::StoragePath {
                            path,
                            storage: None,
                        },
                    )
                })
                .collect();
            (node_id.clone(), sources)
        })
        .collect()
}

/// Materialize a per-node `name -> InputSource::Raw` map straight from inline
/// file contents. Used by the stateless preview compile, where files haven't
/// been uploaded to S3 yet.
fn inline_files(
    inline: &HashMap<String, HashMap<String, String>>,
) -> HashMap<String, HashMap<String, InputSource>> {
    inline
        .iter()
        .map(|(node_id, files)| {
            let sources = files
                .iter()
                .map(|(filename, content)| {
                    (
                        filename.clone(),
                        InputSource::Raw {
                            content: content.clone(),
                        },
                    )
                })
                .collect();
            (node_id.clone(), sources)
        })
        .collect()
}

/// POST /api/templates/:id/new-version
pub async fn new_version(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    let existing = match existing {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "template not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    if !existing.published {
        return (StatusCode::CONFLICT, Json(json!({"error": "can only create new version from a published template"}))).into_response();
    }

    let new_id = Uuid::new_v4();
    let new_version = existing.version + 1;
    let base_id = existing.base_template_id.unwrap_or(existing.id);

    // Start a transaction
    let mut tx = match state.db.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    // Mark old version as not latest
    if let Err(e) = sqlx::query("UPDATE workflow_templates SET is_latest = FALSE WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }

    // Create new version
    let result = sqlx::query_as::<_, WorkflowTemplate>(
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
    .bind(&existing.graph)
    .bind(existing.author_id)
    .fetch_one(&mut *tx)
    .await;

    match result {
        Ok(template) => {
            if let Err(e) = tx.commit().await {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
            }

            // Seed Y.Doc for the new version so WS collaboration works immediately
            let graph: WorkflowGraph = serde_json::from_value(existing.graph.clone())
                .unwrap_or_else(|_| WorkflowGraph::default_graph());
            if let Err(e) = state
                .yjs
                .persistence
                .init_doc_from_graph(new_id, &graph)
                .await
            {
                tracing::error!("failed to init Y.Doc for new version {new_id}: {e}");
                // Non-fatal: template is created, Y.Doc can be initialized later
            }

            (StatusCode::CREATED, Json(json!(template))).into_response()
        }
        Err(e) => {
            tracing::error!("failed to create new version: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// GET /api/templates/:id/versions
pub async fn list_versions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    // First find the base_template_id for this template
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    let existing = match existing {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "template not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    let base_id = existing.base_template_id.unwrap_or(existing.id);

    let versions = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE base_template_id = $1 ORDER BY version DESC",
    )
    .bind(base_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    Json(json!(versions)).into_response()
}

/// GET /api/templates/:id/air
pub async fn get_air(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    match existing {
        Ok(Some(t)) if t.published => {
            if let Some(air) = t.air_json {
                Json(air).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "published template has no AIR JSON"}))).into_response()
            }
        }
        Ok(Some(_)) => {
            (StatusCode::CONFLICT, Json(json!({"error": "template is not published"}))).into_response()
        }
        Ok(None) => {
            (StatusCode::NOT_FOUND, Json(json!({"error": "template not found"}))).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// POST /api/templates/:id/compile
pub async fn compile_preview(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    let existing = match existing {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "template not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    let graph: WorkflowGraph = match serde_json::from_value(existing.graph) {
        Ok(g) => g,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid graph: {e}")}))).into_response();
        }
    };

    // Try to pull files from the Y.Doc so the preview AIR matches what publish
    // would emit. If the Y.Doc has nothing, the empty map yields a compile
    // error for any automated_step (same as publish would, just earlier).
    let ydoc_files = match reconstruct_graph_from_ydoc(&state, id).await {
        Ok(Some((_, f))) => f,
        _ => HashMap::new(),
    };
    let files = storage_path_files(id, existing.version, &ydoc_files);

    match compile_to_air(&graph, &existing.name, &existing.description, &files) {
        Ok(air) => Json(air).into_response(),
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(json!({"error": format!("compilation failed: {e}")}))).into_response()
        }
    }
}

/// POST /api/compile
/// Stateless compilation: accepts a graph (and optional inline file contents)
/// and returns AIR JSON without database access. Used by the editor's "Preview
/// AIR" button before publish.
pub async fn compile_graph(
    Json(body): Json<CompileRequest>,
) -> impl IntoResponse {
    let description = body.description.as_deref().unwrap_or("");
    let files = inline_files(&body.files);
    match compile_to_air(&body.graph, &body.name, description, &files) {
        Ok(air) => Json(air).into_response(),
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(json!({"error": format!("compilation failed: {e}")}))).into_response()
        }
    }
}
