use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::json;
use uuid::Uuid;

use crate::compiler::compile_to_air;
use crate::lifecycle::cleanup_net;
use crate::models::template::{
    CreateTemplateRequest, ListTemplatesQuery, PaginatedResponse, UpdateTemplateRequest,
    WorkflowGraph, WorkflowTemplate,
};
use crate::AppState;

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
        Ok(template) => (StatusCode::CREATED, Json(json!(template))).into_response(),
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

    // Parse graph and compile to AIR
    let graph: WorkflowGraph = match serde_json::from_value(existing.graph.clone()) {
        Ok(g) => g,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid graph: {e}")}))).into_response();
        }
    };

    let air_json = match compile_to_air(&graph, &existing.name, &existing.description) {
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

    match compile_to_air(&graph, &existing.name, &existing.description) {
        Ok(air) => Json(air).into_response(),
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(json!({"error": format!("compilation failed: {e}")}))).into_response()
        }
    }
}
