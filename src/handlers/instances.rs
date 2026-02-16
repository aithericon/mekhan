use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::json;
use uuid::Uuid;

use crate::models::instance::{
    CreateInstanceRequest, InstanceStateResponse, ListInstancesQuery, WorkflowInstance,
};
use crate::models::template::{PaginatedResponse, WorkflowTemplate};
use crate::petri::instance::{deploy_instance, parameterize_air};
use crate::AppState;

/// POST /api/instances
pub async fn create_instance(
    State(state): State<AppState>,
    Json(req): Json<CreateInstanceRequest>,
) -> impl IntoResponse {
    // Fetch the template (must be published)
    let template = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(req.template_id)
    .fetch_optional(&state.db)
    .await;

    let template = match template {
        Ok(Some(t)) if t.published => t,
        Ok(Some(_)) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "template is not published"}))).into_response();
        }
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "template not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    let air_json = match &template.air_json {
        Some(air) => air.clone(),
        None => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "published template has no AIR JSON"}))).into_response();
        }
    };

    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{instance_id}");
    let metadata = req.metadata.clone().unwrap_or(json!({}));

    // Parameterize AIR JSON for this instance
    let parameterized_air = parameterize_air(
        &air_json,
        instance_id,
        template.id,
        template.version,
        req.created_by,
        req.metadata.as_ref(),
    );

    // Insert instance record FIRST so the lifecycle listener can find it
    // if the net completes before we return.
    let instance = match sqlx::query_as::<_, WorkflowInstance>(
        r#"
        INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, started_at, metadata)
        VALUES ($1, $2, $3, $4, 'running', $5, NOW(), $6)
        RETURNING *
        "#,
    )
    .bind(instance_id)
    .bind(template.id)
    .bind(template.version)
    .bind(&net_id)
    .bind(req.created_by)
    .bind(&metadata)
    .fetch_one(&state.db)
    .await
    {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("failed to insert instance: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    // Deploy to petri-lab (DB row already exists for lifecycle listener)
    if let Err(e) = deploy_instance(&state.petri, &net_id, &parameterized_air).await {
        tracing::error!("failed to deploy instance to petri-lab: {e}");
        // Clean up the DB row
        let _ = sqlx::query("DELETE FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .execute(&state.db)
            .await;
        return (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("failed to deploy to engine: {e}")}))).into_response();
    }

    (StatusCode::CREATED, Json(json!(instance))).into_response()
}

/// GET /api/instances
pub async fn list_instances(
    State(state): State<AppState>,
    Query(params): Query<ListInstancesQuery>,
) -> impl IntoResponse {
    let offset = (params.page - 1) * params.per_page;

    // All filter parameters are bound safely to prevent SQL injection
    let (items, total) = match (params.template_id, &params.status) {
        (Some(template_id), Some(status)) => {
            let items = sqlx::query_as::<_, WorkflowInstance>(
                "SELECT * FROM workflow_instances WHERE template_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
            )
            .bind(template_id)
            .bind(status)
            .bind(params.per_page)
            .bind(offset)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();

            let total: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM workflow_instances WHERE template_id = $1 AND status = $2",
            )
            .bind(template_id)
            .bind(status)
            .fetch_one(&state.db)
            .await
            .unwrap_or((0,));

            (items, total.0)
        }
        (Some(template_id), None) => {
            let items = sqlx::query_as::<_, WorkflowInstance>(
                "SELECT * FROM workflow_instances WHERE template_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            )
            .bind(template_id)
            .bind(params.per_page)
            .bind(offset)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();

            let total: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM workflow_instances WHERE template_id = $1",
            )
            .bind(template_id)
            .fetch_one(&state.db)
            .await
            .unwrap_or((0,));

            (items, total.0)
        }
        (None, Some(status)) => {
            let items = sqlx::query_as::<_, WorkflowInstance>(
                "SELECT * FROM workflow_instances WHERE status = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            )
            .bind(status)
            .bind(params.per_page)
            .bind(offset)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();

            let total: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM workflow_instances WHERE status = $1",
            )
            .bind(status)
            .fetch_one(&state.db)
            .await
            .unwrap_or((0,));

            (items, total.0)
        }
        (None, None) => {
            let items = sqlx::query_as::<_, WorkflowInstance>(
                "SELECT * FROM workflow_instances ORDER BY created_at DESC LIMIT $1 OFFSET $2",
            )
            .bind(params.per_page)
            .bind(offset)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();

            let total: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM workflow_instances",
            )
            .fetch_one(&state.db)
            .await
            .unwrap_or((0,));

            (items, total.0)
        }
    };

    Json(PaginatedResponse {
        items,
        total,
        page: params.page,
        per_page: params.per_page,
    })
}

/// GET /api/instances/:id
pub async fn get_instance(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let result = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    match result {
        Ok(Some(instance)) => Json(json!(instance)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "instance not found"}))).into_response(),
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// GET /api/instances/:id/state
pub async fn get_instance_state(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let instance = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    let instance = match instance {
        Ok(Some(i)) => i,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "instance not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    // Proxy to petri-lab engine
    let engine_state = match state.petri.get_state(&instance.net_id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to get state from petri-lab: {e}");
            return (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("engine error: {e}")}))).into_response();
        }
    };

    // Extract marking and enabled transitions from engine response
    let marking = engine_state
        .get("marking")
        .cloned()
        .unwrap_or(json!({}));
    let enabled_transitions: Vec<String> = engine_state
        .get("enabled_transitions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Json(json!(InstanceStateResponse {
        instance_id: instance.id,
        net_id: instance.net_id,
        status: instance.status,
        marking,
        enabled_transitions,
        current_step: instance.current_step,
    }))
    .into_response()
}

/// GET /api/instances/:id/events
pub async fn get_instance_events(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let instance = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    let instance = match instance {
        Ok(Some(i)) => i,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "instance not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    // Return the SSE stream URL for the frontend to connect to directly
    // In production, this would proxy the SSE stream. For MVP, redirect.
    let events_url = state.petri.events_stream_url(&instance.net_id);
    Json(json!({
        "events_url": events_url,
        "net_id": instance.net_id,
    }))
    .into_response()
}

/// DELETE /api/instances/:id
pub async fn cancel_instance(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let instance = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    let instance = match instance {
        Ok(Some(i)) => i,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "instance not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    if instance.status == "completed" || instance.status == "cancelled" {
        return (StatusCode::CONFLICT, Json(json!({"error": format!("instance is already {}", instance.status)}))).into_response();
    }

    // Terminate the net in petri-lab (pause + delete)
    if let Err(e) = state.petri.terminate_net(&instance.net_id).await {
        tracing::warn!("failed to terminate net in petri-lab: {e}");
    }

    // Update instance status
    let result = sqlx::query_as::<_, WorkflowInstance>(
        r#"
        UPDATE workflow_instances
        SET status = 'cancelled', completed_at = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(id)
    .fetch_one(&state.db)
    .await;

    match result {
        Ok(instance) => Json(json!(instance)).into_response(),
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}
