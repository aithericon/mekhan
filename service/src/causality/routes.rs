use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::{IntoParams, ToSchema};

use crate::catalogue::model::CatalogueEntry;
use crate::models::error::{ApiError, ErrorResponse};
use crate::process::model::{HpiLog, HpiMetric, HpiTask};
use crate::AppState;

#[derive(Debug, Deserialize, IntoParams)]
pub struct ProvenanceParams {
    /// Max ancestry depth (clamped to [1, 50]; default 10).
    #[serde(default = "default_depth")]
    pub depth: i32,
}

fn default_depth() -> i32 {
    10
}

#[derive(Debug, Serialize, FromRow, ToSchema)]
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

/// Explicit cross-net edge resolved from causality_cross_links.
#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct CrossNetEdge {
    pub signal_key: String,
    pub egress_net: String,
    pub egress_seq: i64,
    pub ingress_net: String,
    pub ingress_seq: i64,
    pub link_type: String,
}

/// Full provenance response: ancestry nodes + explicit cross-net edges.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProvenanceResponse {
    pub nodes: Vec<AncestryNode>,
    pub cross_net_edges: Vec<CrossNetEdge>,
}

/// GET /api/v1/provenance/{net_id}/{token_id}?depth=10
///
/// Recursive CTE walking token ancestry: for a given token, find which events
/// produced it, what tokens those events consumed, and recurse.
#[utoipa::path(
    get,
    path = "/api/v1/provenance/{net_id}/{token_id}",
    params(
        ("net_id" = String, Path, description = "Net id"),
        ("token_id" = String, Path, description = "Token id to walk ancestry from"),
        ProvenanceParams,
    ),
    responses(
        (status = 200, description = "Ancestry nodes + cross-net edges", body = ProvenanceResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "provenance",
)]
pub async fn token_provenance(
    State(state): State<AppState>,
    Path((net_id, token_id)): Path<(String, String)>,
    Query(params): Query<ProvenanceParams>,
) -> Result<Json<ProvenanceResponse>, ApiError> {
    let depth = params.depth.clamp(1, 50);

    let resp = run_provenance_cte(&state.db, &net_id, &token_id, depth)
        .await
        .map_err(|e| {
            tracing::error!("provenance query failed: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?;
    Ok(Json(resp))
}

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct CrossLink {
    pub signal_key: String,
    pub egress_net: Option<String>,
    pub egress_seq: Option<i64>,
    pub ingress_net: Option<String>,
    pub ingress_seq: Option<i64>,
    pub link_type: String,
}

/// GET /api/v1/provenance/link/{signal_key}
///
/// Look up a cross-net bridge link by signal_key.
#[utoipa::path(
    get,
    path = "/api/v1/provenance/link/{signal_key}",
    params(("signal_key" = String, Path, description = "Signal key identifying the bridge")),
    responses(
        (status = 200, description = "Cross-net link", body = CrossLink),
        (status = 404, description = "Signal key not found"),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "provenance",
)]
pub async fn cross_link(
    State(state): State<AppState>,
    Path(signal_key): Path<String>,
) -> Result<Json<CrossLink>, ApiError> {
    let link = sqlx::query_as::<_, CrossLink>(
        "SELECT * FROM causality_cross_links WHERE signal_key = $1",
    )
    .bind(&signal_key)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("cross-link query failed: {e}");
        ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
    })?
    .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;
    Ok(Json(link))
}

// ── Provenance from artifact ──────────────────────────────────────────────

/// GET /api/v1/provenance/from-artifact/{execution_id}/{artifact_id}?depth=10
///
/// Resolves a catalogue entry to its producing token and returns the full
/// ancestry chain. Uses `source_event_sequence` for direct lookup when
/// available, falling back to signal_key → cross-link resolution.
#[utoipa::path(
    get,
    path = "/api/v1/provenance/from-artifact/{execution_id}/{artifact_id}",
    params(
        ("execution_id" = String, Path, description = "Execution id from the catalogue entry"),
        ("artifact_id" = String, Path, description = "Catalogue entry id"),
        ProvenanceParams,
    ),
    responses(
        (status = 200, description = "Ancestry walked back from the producing token", body = ProvenanceResponse),
        (status = 404, description = "Catalogue entry not found"),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "provenance",
)]
pub async fn provenance_from_artifact(
    State(state): State<AppState>,
    Path((execution_id, artifact_id)): Path<(String, String)>,
    Query(params): Query<ProvenanceParams>,
) -> Result<Json<ProvenanceResponse>, ApiError> {
    let depth = params.depth.clamp(1, 50);

    // Look up the catalogue entry by (execution_id, id) — unique key
    let entry: Option<(Option<String>, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT source_net, signal_key, source_event_sequence \
         FROM catalogue_entries WHERE execution_id = $1 AND id = $2",
    )
    .bind(&execution_id)
    .bind(&artifact_id)
    .fetch_optional(&state.db)
    .await
    .unwrap_or(None);

    let (source_net, signal_key, source_seq) = entry
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;

    let source_net = source_net
        .ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;

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
            // No causality data yet — return empty response
            return Ok(Json(ProvenanceResponse {
                nodes: vec![],
                cross_net_edges: vec![],
            }));
        }
    };

    // Run the standard provenance CTE
    let resp = run_provenance_cte(&state.db, &net_id, &token_id, depth)
        .await
        .map_err(|e| {
            tracing::error!("provenance from artifact failed: {e}");
            ApiError::status_only(StatusCode::INTERNAL_SERVER_ERROR)
        })?;
    Ok(Json(resp))
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
) -> Result<ProvenanceResponse, sqlx::Error> {
    // Phase 1: run the recursive CTE to discover all events in the ancestry
    let mut nodes = phase1_cte(db, net_id, token_id, depth).await?;

    // Phase 2: fetch consumed tokens for all discovered events
    let event_keys: Vec<(String, i64)> = nodes
        .iter()
        .map(|n| (n.net_id.clone(), n.event_seq))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let mut cross_net_edges = vec![];

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

        // Phase 3: fetch cross-net edges from causality_cross_links AND
        // signal-injection lineage where both endpoints are in our ancestry.
        // Lineage rows are exposed under the synthetic link_type 'signal'
        // so the frontend can render them alongside cross-net edges.
        cross_net_edges = sqlx::query_as::<_, CrossNetEdge>(
            r#"
            SELECT cl.signal_key, cl.egress_net, cl.egress_seq,
                   cl.ingress_net, cl.ingress_seq, cl.link_type
            FROM causality_cross_links cl
            WHERE (cl.egress_net, cl.egress_seq) IN (
                SELECT UNNEST($1::text[]), UNNEST($2::bigint[])
            )
            AND (cl.ingress_net, cl.ingress_seq) IN (
                SELECT UNNEST($1::text[]), UNNEST($2::bigint[])
            )
            UNION ALL
            SELECT sl.signal_key, sl.dispatch_net AS egress_net, sl.dispatch_seq AS egress_seq,
                   sl.ingress_net, sl.ingress_seq, 'signal' AS link_type
            FROM causality_signal_lineage sl
            WHERE (sl.dispatch_net, sl.dispatch_seq) IN (
                SELECT UNNEST($1::text[]), UNNEST($2::bigint[])
            )
            AND (sl.ingress_net, sl.ingress_seq) IN (
                SELECT UNNEST($1::text[]), UNNEST($2::bigint[])
            )
            "#,
        )
        .bind(&net_ids)
        .bind(&seqs)
        .fetch_all(db)
        .await?;
    }

    Ok(ProvenanceResponse {
        nodes,
        cross_net_edges,
    })
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

                -- Path 4: signal-injected tokens with a known dispatch in
                -- causality_signal_lineage. Used for the executor lifecycle
                -- where one executor_submit emits many sig_* TokenCreated events.
                SELECT et5.net_id, et5.event_seq, ce5.event_type,
                       et5.token_id, et5.role, et5.place_id, et5.place_name,
                       ce5.transition_name, ce5.effect_handler, ce5.timestamp
                FROM causality_signal_lineage sl
                JOIN causality_event_tokens et5
                    ON et5.net_id = sl.dispatch_net AND et5.event_seq = sl.dispatch_seq
                JOIN causality_events ce5
                    ON ce5.net_id = et5.net_id AND ce5.event_seq = et5.event_seq
                WHERE sl.ingress_net = a.net_id
                  AND sl.ingress_seq = a.event_seq
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
                      -- Skip other signal TokenCreated events to reach structural transitions
                      AND NOT (ce4.event_type = 'TokenCreated' AND et4.place_id LIKE 'sig_%')
                      AND NOT EXISTS (
                        SELECT 1 FROM causality_cross_links cl2
                        WHERE cl2.ingress_net = a.net_id AND cl2.ingress_seq = a.event_seq
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM causality_signal_lineage sl2
                        WHERE sl2.ingress_net = a.net_id AND sl2.ingress_seq = a.event_seq
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

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct TokenInfo {
    pub token_id: String,
    pub role: String,
    pub place_id: String,
    pub place_name: Option<String>,
    /// Full token payload (color). Null for `consumed` role when the
    /// producer's row isn't available for join-back.
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BridgeTarget {
    pub target_net: String,
    pub target_place: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SignalDispatch {
    pub dispatch_net: String,
    pub dispatch_seq: i64,
    pub signal_key: String,
}

#[derive(Debug, Serialize, ToSchema)]
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
    /// Raw JSON returned by the effect handler (EffectCompleted) or a
    /// failure envelope (EffectFailed). Null for other event types.
    pub effect_result: Option<serde_json::Value>,
    /// Present only for `TokenBridgedOut`.
    pub bridge: Option<BridgeTarget>,
    /// Present when this event is a signal-injected `TokenCreated` whose
    /// signal_key matches a row in `causality_signal_dispatches` — i.e.
    /// we know which effect originally dispatched this signal.
    pub signal_dispatch: Option<SignalDispatch>,
}

/// GET /api/v1/provenance/{net_id}/{event_seq}/detail
///
/// Returns the full context for a causality event by joining to domain
/// tables based on effect_handler. Enables rich detail views in the
/// provenance DAG visualization.
#[utoipa::path(
    get,
    path = "/api/v1/provenance/{net_id}/{event_seq}/detail",
    params(
        ("net_id" = String, Path, description = "Net id"),
        ("event_seq" = i64, Path, description = "Event sequence number within the net"),
    ),
    responses(
        (status = 200, description = "Full event detail with related rows", body = EventDetail),
        (status = 404, description = "Event not found"),
    ),
    tag = "provenance",
)]
pub async fn event_detail(
    State(state): State<AppState>,
    Path((net_id, event_seq)): Path<(String, i64)>,
) -> Result<Json<EventDetail>, ApiError> {
    let db = &state.db;

    // Fetch the event + new payload columns in one go.
    type EventRow = (
        String,                                // event_type
        Option<String>,                        // transition_name
        Option<String>,                        // effect_handler
        chrono::DateTime<chrono::Utc>,         // timestamp
        Option<serde_json::Value>,             // effect_result
        Option<String>,                        // bridge_target_net
        Option<String>,                        // bridge_target_place
    );
    let event: Option<EventRow> = sqlx::query_as(
        "SELECT event_type, transition_name, effect_handler, timestamp, \
                effect_result, bridge_target_net, bridge_target_place \
         FROM causality_events WHERE net_id = $1 AND event_seq = $2",
    )
    .bind(&net_id)
    .bind(event_seq)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    let (
        event_type,
        transition_name,
        effect_handler,
        timestamp,
        effect_result,
        bridge_target_net,
        bridge_target_place,
    ) = event.ok_or_else(|| ApiError::status_only(StatusCode::NOT_FOUND))?;

    let bridge = match (bridge_target_net, bridge_target_place) {
        (Some(n), Some(p)) => Some(BridgeTarget {
            target_net: n,
            target_place: p,
        }),
        _ => None,
    };

    // Fetch all tokens involved in this event. For consumed-role rows we
    // fall back to the producer's token_data (same token_id, role='produced')
    // so the UI can display payload for tokens that entered as inputs.
    let tokens: Vec<TokenInfo> = sqlx::query_as(
        "SELECT t.token_id, t.role, t.place_id, t.place_name, \
                COALESCE( \
                    t.token_data, \
                    (SELECT p.token_data FROM causality_event_tokens p \
                     WHERE p.token_id = t.token_id AND p.role = 'produced' \
                       AND p.token_data IS NOT NULL LIMIT 1) \
                ) AS data \
         FROM causality_event_tokens t \
         WHERE t.net_id = $1 AND t.event_seq = $2 \
         ORDER BY t.role, t.place_id",
    )
    .bind(&net_id)
    .bind(event_seq)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    // Resolve the signal_key that identifies this event's downstream work.
    // Tried in order: (1) cross_links where this is the egress — works for
    // most handlers; (2) causality_signal_dispatches — works for
    // executor_submit, whose cross_links entry is overwritten by a later
    // catalogue_register re-use of the same key; (3) the event's own
    // effect_result — final fallback.
    let signal_key: Option<String> = match sqlx::query_scalar::<_, String>(
        "SELECT signal_key FROM causality_cross_links \
         WHERE egress_net = $1 AND egress_seq = $2 LIMIT 1",
    )
    .bind(&net_id)
    .bind(event_seq)
    .fetch_optional(db)
    .await
    {
        Ok(Some(sk)) => Some(sk),
        _ => {
            let from_dispatch: Option<String> = sqlx::query_scalar(
                "SELECT signal_key FROM causality_signal_dispatches \
                 WHERE dispatch_net = $1 AND dispatch_seq = $2 LIMIT 1",
            )
            .bind(&net_id)
            .bind(event_seq)
            .fetch_optional(db)
            .await
            .unwrap_or(None);
            from_dispatch.or_else(|| {
                effect_result
                    .as_ref()
                    .and_then(|r| r.get("signal_key"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
        }
    };

    // For signal-injected TokenCreated events, look up the dispatch event
    // so the UI can show "Emitted by {effect} at {net}#{seq}".
    let signal_dispatch: Option<SignalDispatch> = if event_type == "TokenCreated" {
        sqlx::query_as::<_, (String, i64, String)>(
            "SELECT sl.dispatch_net, sl.dispatch_seq, sl.signal_key \
             FROM causality_signal_lineage sl \
             WHERE sl.ingress_net = $1 AND sl.ingress_seq = $2 LIMIT 1",
        )
        .bind(&net_id)
        .bind(event_seq)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|(dn, ds, sk)| SignalDispatch {
            dispatch_net: dn,
            dispatch_seq: ds,
            signal_key: sk,
        })
    } else {
        None
    };

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
            // Filter metrics and logs to exactly this execution by signal_key.
            // In a BO campaign every iteration shares one process_id, so
            // falling back to `WHERE process_id = ...` would leak metrics
            // from every other iteration into this view.
            if let Some(ref sk) = signal_key {
                metrics = sqlx::query_as(
                    "SELECT * FROM hpi_metrics WHERE signal_key = $1 \
                     ORDER BY timestamp LIMIT 200",
                )
                .bind(sk)
                .fetch_all(db)
                .await
                .unwrap_or_default();

                logs = sqlx::query_as(
                    "SELECT * FROM hpi_logs WHERE signal_key = $1 \
                     ORDER BY timestamp LIMIT 200",
                )
                .bind(sk)
                .fetch_all(db)
                .await
                .unwrap_or_default();

                // Also fetch the catalogue artifact this execution produced.
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

    Ok(Json(EventDetail {
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
        effect_result,
        bridge,
        signal_dispatch,
    }))
}
