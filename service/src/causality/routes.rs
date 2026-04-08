use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::catalogue::model::CatalogueEntry;
use crate::process::model::{HpiLog, HpiMetric, HpiTask};
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
    pub place_name: Option<String>,
    pub transition_name: Option<String>,
    pub effect_handler: Option<String>,
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

    match run_provenance_cte(&state.db, &net_id, &token_id, depth).await {
        Ok(nodes) => Json(nodes).into_response(),
        Err(e) => {
            tracing::error!("provenance query failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Debug, Serialize, FromRow)]
pub struct CrossLink {
    pub signal_key: String,
    pub egress_net: Option<String>,
    pub egress_seq: Option<i64>,
    pub ingress_net: Option<String>,
    pub ingress_seq: Option<i64>,
    pub link_type: String,
}

/// GET /api/provenance/link/{signal_key}
///
/// Look up a cross-net bridge link by signal_key.
pub async fn cross_link(
    State(state): State<AppState>,
    Path(signal_key): Path<String>,
) -> impl IntoResponse {
    let result = sqlx::query_as::<_, CrossLink>(
        "SELECT * FROM causality_cross_links WHERE signal_key = $1",
    )
    .bind(&signal_key)
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

// ── Provenance from artifact ──────────────────────────────────────────────

/// GET /api/provenance/from-artifact/{artifact_id}?depth=10
///
/// Resolves a catalogue entry to its producing token and returns the full
/// ancestry chain. Uses `source_event_sequence` for direct lookup when
/// available, falling back to signal_key → cross-link resolution.
pub async fn provenance_from_artifact(
    State(state): State<AppState>,
    Path(artifact_id): Path<String>,
    Query(params): Query<ProvenanceParams>,
) -> impl IntoResponse {
    let depth = params.depth.min(50).max(1);

    // Look up the catalogue entry
    let entry: Option<(Option<String>, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT source_net, signal_key, source_event_sequence \
         FROM catalogue_entries WHERE id = $1",
    )
    .bind(&artifact_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let (source_net, signal_key, source_seq) = match entry {
        Some(e) => e,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let source_net = match source_net {
        Some(n) => n,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    // Resolve to (net_id, token_id) for the provenance CTE
    let resolved: Option<(String, String)> = if let Some(seq) = source_seq {
        // Fast path: direct event sequence lookup
        sqlx::query_as(
            "SELECT net_id, token_id FROM causality_event_tokens \
             WHERE net_id = $1 AND event_seq = $2 AND role = 'produced' LIMIT 1",
        )
        .bind(&source_net)
        .bind(seq)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None)
    } else if let Some(ref sk) = signal_key {
        // Fallback: signal_key → cross-link → egress token
        sqlx::query_as(
            "SELECT et.net_id, et.token_id \
             FROM causality_cross_links cl \
             JOIN causality_event_tokens et \
                 ON et.net_id = cl.egress_net AND et.event_seq = cl.egress_seq \
             WHERE cl.signal_key = $1 AND et.role = 'produced' LIMIT 1",
        )
        .bind(sk)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None)
    } else {
        None
    };

    let (net_id, token_id) = match resolved {
        Some(r) => r,
        None => {
            // No causality data yet — return empty chain
            return Json(Vec::<AncestryNode>::new()).into_response();
        }
    };

    // Run the standard provenance CTE
    let result = run_provenance_cte(&state.db, &net_id, &token_id, depth).await;
    match result {
        Ok(nodes) => Json(nodes).into_response(),
        Err(e) => {
            tracing::error!("provenance from artifact failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Shared provenance CTE query used by both `token_provenance` and
/// `provenance_from_artifact`.
///
/// Returns ancestry nodes enriched with consumed tokens. The CTE naturally
/// returns only `produced` role tokens (since it walks backwards from producer
/// to producer). We supplement with a second pass that adds consumed tokens
/// for every event in the ancestry, enabling the frontend to derive edges.
async fn run_provenance_cte(
    db: &sqlx::PgPool,
    net_id: &str,
    token_id: &str,
    depth: i32,
) -> Result<Vec<AncestryNode>, sqlx::Error> {
    // Phase 1: run the recursive CTE to discover all events in the ancestry
    let mut nodes = phase1_cte(db, net_id, token_id, depth).await?;

    // Phase 2: fetch consumed tokens for all discovered events
    let event_keys: Vec<(String, i64)> = nodes
        .iter()
        .map(|n| (n.net_id.clone(), n.event_seq))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if !event_keys.is_empty() {
        let net_ids: Vec<String> = event_keys.iter().map(|(n, _)| n.clone()).collect();
        let seqs: Vec<i64> = event_keys.iter().map(|(_, s)| *s).collect();

        let consumed: Vec<AncestryNode> = sqlx::query_as(
            r#"
            SELECT
                -1 AS depth, et.net_id, et.event_seq, ce.event_type,
                et.token_id, et.role, et.place_id, et.place_name,
                ce.transition_name, ce.effect_handler, ce.timestamp
            FROM causality_event_tokens et
            JOIN causality_events ce ON ce.net_id = et.net_id AND ce.event_seq = et.event_seq
            WHERE et.role = 'consumed'
              AND (et.net_id, et.event_seq) IN (
                  SELECT UNNEST($1::text[]), UNNEST($2::bigint[])
              )
            "#,
        )
        .bind(&net_ids)
        .bind(&seqs)
        .fetch_all(db)
        .await?;

        nodes.extend(consumed);
    }

    Ok(nodes)
}

async fn phase1_cte(
    db: &sqlx::PgPool,
    net_id: &str,
    token_id: &str,
    depth: i32,
) -> Result<Vec<AncestryNode>, sqlx::Error> {
    sqlx::query_as::<_, AncestryNode>(
        r#"
        WITH RECURSIVE ancestry AS (
            SELECT
                0 AS depth, et.net_id, et.event_seq, ce.event_type,
                et.token_id, et.role, et.place_id, et.place_name,
                ce.transition_name, ce.effect_handler, ce.timestamp
            FROM causality_event_tokens et
            JOIN causality_events ce ON ce.net_id = et.net_id AND ce.event_seq = et.event_seq
            WHERE et.token_id = $1 AND et.net_id = $2 AND et.role = 'produced'

            UNION ALL

            SELECT
                a.depth + 1, next.net_id, next.event_seq, next.event_type,
                next.token_id, next.role, next.place_id, next.place_name,
                next.transition_name, next.effect_handler, next.timestamp
            FROM ancestry a
            JOIN LATERAL (
                SELECT et2.net_id, et2.event_seq, ce2.event_type,
                       et2.token_id, et2.role, et2.place_id, et2.place_name,
                       ce2.transition_name, ce2.effect_handler, ce2.timestamp
                FROM causality_event_tokens consumed
                JOIN causality_event_tokens et2
                    ON et2.token_id = consumed.token_id AND et2.role = 'produced'
                JOIN causality_events ce2
                    ON ce2.net_id = et2.net_id AND ce2.event_seq = et2.event_seq
                WHERE consumed.net_id = a.net_id
                  AND consumed.event_seq = a.event_seq
                  AND consumed.role = 'consumed'

                UNION ALL

                -- Path 2: cross-net jump via bridge cross-link
                SELECT et3.net_id, et3.event_seq, ce3.event_type,
                       et3.token_id, et3.role, et3.place_id, et3.place_name,
                       ce3.transition_name, ce3.effect_handler, ce3.timestamp
                FROM causality_cross_links cl
                JOIN causality_event_tokens et3
                    ON et3.net_id = cl.egress_net AND et3.event_seq = cl.egress_seq
                JOIN causality_events ce3
                    ON ce3.net_id = et3.net_id AND ce3.event_seq = et3.event_seq
                WHERE cl.ingress_net = a.net_id
                  AND cl.ingress_seq = a.event_seq
                  AND a.event_type = 'TokenCreated'

                UNION ALL

                -- Path 3: signal-injected tokens (from executor/external systems)
                -- that have no cross-link — trace back via shared process tags
                -- to the most recent prior event in the same net.
                SELECT sub.net_id, sub.event_seq, sub.event_type,
                       sub.token_id, sub.role, sub.place_id, sub.place_name,
                       sub.transition_name, sub.effect_handler, sub.timestamp
                FROM (
                    SELECT et4.net_id, et4.event_seq, ce4.event_type,
                           et4.token_id, et4.role, et4.place_id, et4.place_name,
                           ce4.transition_name, ce4.effect_handler, ce4.timestamp,
                           ROW_NUMBER() OVER (ORDER BY ce4.event_seq DESC) AS rn
                    FROM causality_process_tags pt1
                    JOIN causality_process_tags pt2 ON pt2.process_id = pt1.process_id
                    JOIN causality_event_tokens et4
                        ON et4.token_id = pt2.token_id AND et4.role = 'produced'
                    JOIN causality_events ce4
                        ON ce4.net_id = et4.net_id AND ce4.event_seq = et4.event_seq
                    WHERE pt1.token_id = (
                        SELECT et_sig.token_id FROM causality_event_tokens et_sig
                        WHERE et_sig.net_id = a.net_id AND et_sig.event_seq = a.event_seq
                          AND et_sig.role = 'produced' LIMIT 1
                    )
                      AND a.event_type = 'TokenCreated'
                      AND et4.net_id = a.net_id
                      AND ce4.event_seq < a.event_seq
                      AND NOT EXISTS (
                        SELECT 1 FROM causality_cross_links cl2
                        WHERE cl2.ingress_net = a.net_id AND cl2.ingress_seq = a.event_seq
                      )
                ) sub WHERE sub.rn = 1
            ) next ON true
            WHERE a.depth < $3
        )
        SELECT DISTINCT depth, net_id, event_seq, event_type, token_id, role,
               place_id, place_name, transition_name, effect_handler, timestamp
        FROM ancestry
        ORDER BY depth, timestamp DESC
        "#,
    )
    .bind(token_id)
    .bind(net_id)
    .bind(depth)
    .fetch_all(db)
    .await
}

// ── Event detail ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, FromRow)]
pub struct TokenInfo {
    pub token_id: String,
    pub role: String,
    pub place_id: String,
    pub place_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EventDetail {
    pub net_id: String,
    pub event_seq: i64,
    pub event_type: String,
    pub transition_name: Option<String>,
    pub effect_handler: Option<String>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub tokens: Vec<TokenInfo>,
    pub task: Option<HpiTask>,
    pub artifact: Option<CatalogueEntry>,
    pub metrics: Vec<HpiMetric>,
    pub logs: Vec<HpiLog>,
}

/// GET /api/provenance/{net_id}/{event_seq}/detail
///
/// Returns the full context for a causality event by joining to domain
/// tables based on effect_handler. Enables rich detail views in the
/// provenance DAG visualization.
pub async fn event_detail(
    State(state): State<AppState>,
    Path((net_id, event_seq)): Path<(String, i64)>,
) -> impl IntoResponse {
    let db = &state.db;

    // Fetch the event
    let event: Option<(String, Option<String>, Option<String>, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            "SELECT event_type, transition_name, effect_handler, timestamp \
             FROM causality_events WHERE net_id = $1 AND event_seq = $2",
        )
        .bind(&net_id)
        .bind(event_seq)
        .fetch_optional(db)
        .await
        .unwrap_or(None);

    let (event_type, transition_name, effect_handler, timestamp) = match event {
        Some(e) => e,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    // Fetch all tokens involved in this event
    let tokens: Vec<TokenInfo> = sqlx::query_as(
        "SELECT token_id, role, place_id, place_name \
         FROM causality_event_tokens WHERE net_id = $1 AND event_seq = $2 \
         ORDER BY role, place_id",
    )
    .bind(&net_id)
    .bind(event_seq)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    // Resolve signal_key for this event (from cross-links where this is the egress)
    let signal_key: Option<String> = sqlx::query_scalar(
        "SELECT signal_key FROM causality_cross_links \
         WHERE egress_net = $1 AND egress_seq = $2 LIMIT 1",
    )
    .bind(&net_id)
    .bind(event_seq)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    // Resolve process_id from tokens
    let token_ids: Vec<String> = tokens
        .iter()
        .filter(|t| t.role == "consumed" || t.role == "read")
        .map(|t| t.token_id.clone())
        .collect();
    let process_id: Option<String> = if !token_ids.is_empty() {
        sqlx::query_scalar(
            "SELECT DISTINCT process_id FROM causality_process_tags \
             WHERE token_id = ANY($1) LIMIT 1",
        )
        .bind(&token_ids)
        .fetch_optional(db)
        .await
        .unwrap_or(None)
    } else {
        None
    };

    // Fetch handler-specific detail
    let mut task: Option<HpiTask> = None;
    let mut artifact: Option<CatalogueEntry> = None;
    let mut metrics: Vec<HpiMetric> = vec![];
    let mut logs: Vec<HpiLog> = vec![];

    match effect_handler.as_deref() {
        Some("human_task") => {
            if let Some(ref sk) = signal_key {
                task = sqlx::query_as(
                    "SELECT * FROM hpi_tasks WHERE id = $1",
                )
                .bind(sk)
                .fetch_optional(db)
                .await
                .unwrap_or(None);
            }
        }
        Some("catalogue_register") => {
            artifact = sqlx::query_as(
                "SELECT * FROM catalogue_entries \
                 WHERE source_net = $1 AND source_event_sequence = $2",
            )
            .bind(&net_id)
            .bind(event_seq)
            .fetch_optional(db)
            .await
            .unwrap_or(None);
        }
        Some("executor_submit") => {
            // Show artifacts, metrics, and logs for the execution
            if let Some(ref pid) = process_id {
                metrics = sqlx::query_as(
                    "SELECT * FROM hpi_metrics WHERE process_id = $1 \
                     ORDER BY timestamp LIMIT 100",
                )
                .bind(pid)
                .fetch_all(db)
                .await
                .unwrap_or_default();

                logs = sqlx::query_as(
                    "SELECT * FROM hpi_logs WHERE process_id = $1 \
                     ORDER BY timestamp LIMIT 50",
                )
                .bind(pid)
                .fetch_all(db)
                .await
                .unwrap_or_default();
            }
            // Also fetch catalogue artifacts produced downstream
            if let Some(ref sk) = signal_key {
                artifact = sqlx::query_as(
                    "SELECT * FROM catalogue_entries WHERE signal_key = $1 LIMIT 1",
                )
                .bind(sk)
                .fetch_optional(db)
                .await
                .unwrap_or(None);
            }
        }
        Some("process_log_metric") => {
            if let Some(ref pid) = process_id {
                metrics = sqlx::query_as(
                    "SELECT * FROM hpi_metrics WHERE process_id = $1 \
                     AND timestamp BETWEEN $2 - interval '1 second' AND $2 + interval '1 second' \
                     ORDER BY timestamp",
                )
                .bind(pid)
                .bind(timestamp)
                .fetch_all(db)
                .await
                .unwrap_or_default();
            }
        }
        Some("process_log_message") => {
            if let Some(ref pid) = process_id {
                logs = sqlx::query_as(
                    "SELECT * FROM hpi_logs WHERE process_id = $1 \
                     AND timestamp BETWEEN $2 - interval '1 second' AND $2 + interval '1 second' \
                     ORDER BY timestamp",
                )
                .bind(pid)
                .bind(timestamp)
                .fetch_all(db)
                .await
                .unwrap_or_default();
            }
        }
        _ => {}
    }

    Json(EventDetail {
        net_id,
        event_seq,
        event_type,
        transition_name,
        effect_handler,
        timestamp,
        tokens,
        task,
        artifact,
        metrics,
        logs,
    })
    .into_response()
}
