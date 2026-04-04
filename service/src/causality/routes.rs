use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct ProvenanceParams {
    #[serde(default = "default_depth")]
    pub depth: i32,
}

fn default_depth() -> i32 {
    10
}

#[derive(Debug, Serialize, FromRow)]
pub struct AncestryNode {
    pub depth: i32,
    pub net_id: String,
    pub event_seq: i64,
    pub event_type: String,
    pub token_id: String,
    pub role: String,
    pub place_id: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// GET /api/provenance/{net_id}/{token_id}?depth=10
///
/// Recursive CTE walking token ancestry: for a given token, find which events
/// produced it, what tokens those events consumed, and recurse.
pub async fn token_provenance(
    State(state): State<AppState>,
    Path((net_id, token_id)): Path<(String, String)>,
    Query(params): Query<ProvenanceParams>,
) -> impl IntoResponse {
    let depth = params.depth.min(50).max(1);

    let result = sqlx::query_as::<_, AncestryNode>(
        r#"
        WITH RECURSIVE ancestry AS (
            -- Base: the event that produced the target token
            SELECT
                0 AS depth,
                et.net_id,
                et.event_seq,
                ce.event_type,
                et.token_id,
                et.role,
                et.place_id,
                ce.timestamp
            FROM causality_event_tokens et
            JOIN causality_events ce ON ce.net_id = et.net_id AND ce.event_seq = et.event_seq
            WHERE et.token_id = $1 AND et.net_id = $2 AND et.role = 'produced'

            UNION ALL

            -- Recurse: for each producing event, find consumed tokens, then find
            -- the events that produced THOSE tokens
            SELECT
                a.depth + 1,
                et2.net_id,
                et2.event_seq,
                ce2.event_type,
                et2.token_id,
                et2.role,
                et2.place_id,
                ce2.timestamp
            FROM ancestry a
            -- Find consumed tokens in the same event
            JOIN causality_event_tokens consumed
                ON consumed.net_id = a.net_id
                AND consumed.event_seq = a.event_seq
                AND consumed.role = 'consumed'
            -- Find the event that produced each consumed token
            JOIN causality_event_tokens et2
                ON et2.token_id = consumed.token_id
                AND et2.role = 'produced'
            JOIN causality_events ce2
                ON ce2.net_id = et2.net_id AND ce2.event_seq = et2.event_seq
            WHERE a.depth < $3
        )
        SELECT DISTINCT depth, net_id, event_seq, event_type, token_id, role, place_id, timestamp
        FROM ancestry
        ORDER BY depth, timestamp DESC
        "#,
    )
    .bind(&token_id)
    .bind(&net_id)
    .bind(depth)
    .fetch_all(&state.db)
    .await;

    match result {
        Ok(nodes) => Json(nodes).into_response(),
        Err(e) => {
            tracing::error!("provenance query failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Debug, Serialize, FromRow)]
pub struct CrossLink {
    pub correlation_id: String,
    pub egress_net: Option<String>,
    pub egress_seq: Option<i64>,
    pub ingress_net: Option<String>,
    pub ingress_seq: Option<i64>,
    pub link_type: String,
}

/// GET /api/provenance/link/{correlation_id}
///
/// Look up a cross-net bridge link by correlation_id.
pub async fn cross_link(
    State(state): State<AppState>,
    Path(correlation_id): Path<String>,
) -> impl IntoResponse {
    let result = sqlx::query_as::<_, CrossLink>(
        "SELECT * FROM causality_cross_links WHERE correlation_id = $1",
    )
    .bind(&correlation_id)
    .fetch_optional(&state.db)
    .await;

    match result {
        Ok(Some(link)) => Json(link).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("cross-link query failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
