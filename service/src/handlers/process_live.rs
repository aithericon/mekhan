//! SSE + backfill endpoints for live process metrics and logs.
//!
//! Four endpoints:
//! - GET `/api/v1/processes/{pid}/metrics/series` — DB backfill, adaptive
//!   `time_bucket()` downsampling when the requested window exceeds `max_points`.
//! - GET `/api/v1/processes/{pid}/metrics/stream` — SSE: ring-buffer snapshot (skip
//!   ≤since_seq), then live broadcast filtered by process_id (+ optional
//!   signal_key / key whitelist). Emits `resync` on broadcast lag, `gap` when
//!   the client's since_seq predates the buffer.
//! - GET `/api/v1/processes/{pid}/logs/tail` — DB backfill of recent rows.
//! - GET `/api/v1/processes/{pid}/logs/stream` — SSE live tail (same shape as metrics).

use std::collections::HashSet;
use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use chrono::{DateTime, Utc};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use utoipa::{IntoParams, ToSchema};

use crate::causality::live::{LiveArtifactEvent, LiveLogEvent, LiveMetricEvent};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::responses::{ArtifactsListResponse, LogsTailResponse};
use crate::AppState;

// ─── metrics/series (DB backfill with adaptive downsampling) ───────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct MetricsSeriesQuery {
    /// Comma-separated metric keys.
    #[serde(default)]
    pub keys: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub signal_key: Option<String>,
    #[serde(default = "default_max_points")]
    pub max_points: i64,
}

fn default_max_points() -> i64 {
    2000
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MetricPoint {
    pub t: DateTime<Utc>,
    pub v: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MetricsSeriesResponse {
    /// Seconds per bucket (0 = raw rows).
    pub bucket_seconds: i64,
    pub series: std::collections::BTreeMap<String, Vec<MetricPoint>>,
}

/// Choose a sane bucket width in seconds so the total point count stays under
/// `max_points` per series. Returns 0 when raw rows fit.
fn choose_bucket_seconds(window_seconds: i64, max_points: i64) -> i64 {
    if window_seconds <= 0 || max_points <= 0 {
        return 0;
    }
    let ideal = window_seconds / max_points;
    if ideal <= 1 {
        return 0;
    }
    // Snap to a human-friendly step.
    const STEPS: &[i64] = &[
        1, 5, 15, 30, 60, 300, 900, 1800, 3600, 7200, 21600, 43200, 86400,
    ];
    for &s in STEPS {
        if s >= ideal {
            return s;
        }
    }
    86400
}

#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/metrics/series",
    params(
        ("process_id" = String, Path, description = "Process id"),
        MetricsSeriesQuery,
    ),
    responses(
        (status = 200, description = "Backfilled metric series (adaptive bucket when window > max_points)", body = MetricsSeriesResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "processes-live",
)]
pub async fn metrics_series(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
    Query(q): Query<MetricsSeriesQuery>,
) -> Result<Json<MetricsSeriesResponse>, ApiError> {
    let until = q.until.unwrap_or_else(Utc::now);
    let since = q
        .since
        .unwrap_or_else(|| until - chrono::Duration::hours(1));
    let keys: Vec<String> = q
        .keys
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let window_seconds = (until - since).num_seconds().max(0);
    let bucket_seconds = choose_bucket_seconds(window_seconds, q.max_points.max(1));

    let result: Result<Vec<(String, DateTime<Utc>, f64)>, sqlx::Error> = if bucket_seconds == 0 {
        // Raw rows.
        let sql = "SELECT key, timestamp AS t, value AS v \
                   FROM hpi_metrics \
                   WHERE process_id = $1 \
                     AND timestamp >= $2 AND timestamp < $3 \
                     AND ($4::text[] IS NULL OR key = ANY($4)) \
                     AND ($5::text IS NULL OR signal_key = $5) \
                   ORDER BY key, timestamp ASC";
        let keys_opt: Option<Vec<String>> = if keys.is_empty() {
            None
        } else {
            Some(keys.clone())
        };
        let rows = sqlx::query(sql)
            .bind(&process_id)
            .bind(since)
            .bind(until)
            .bind(keys_opt)
            .bind(q.signal_key.as_deref())
            .fetch_all(&state.db)
            .await;
        rows.map(|rs| {
            rs.into_iter()
                .map(|r| {
                    (
                        r.get::<String, _>("key"),
                        r.get::<DateTime<Utc>, _>("t"),
                        r.get::<f64, _>("v"),
                    )
                })
                .collect()
        })
    } else {
        let bucket = format!("{} seconds", bucket_seconds);
        let sql = "SELECT key, \
                          time_bucket($6::interval, timestamp) AS t, \
                          avg(value) AS v \
                   FROM hpi_metrics \
                   WHERE process_id = $1 \
                     AND timestamp >= $2 AND timestamp < $3 \
                     AND ($4::text[] IS NULL OR key = ANY($4)) \
                     AND ($5::text IS NULL OR signal_key = $5) \
                   GROUP BY key, t \
                   ORDER BY key, t ASC";
        let keys_opt: Option<Vec<String>> = if keys.is_empty() {
            None
        } else {
            Some(keys.clone())
        };
        let rows = sqlx::query(sql)
            .bind(&process_id)
            .bind(since)
            .bind(until)
            .bind(keys_opt)
            .bind(q.signal_key.as_deref())
            .bind(&bucket)
            .fetch_all(&state.db)
            .await;
        rows.map(|rs| {
            rs.into_iter()
                .map(|r| {
                    (
                        r.get::<String, _>("key"),
                        r.get::<DateTime<Utc>, _>("t"),
                        r.get::<f64, _>("v"),
                    )
                })
                .collect()
        })
    };

    let rows = result.map_err(|e| {
        tracing::warn!(process_id = %process_id, "metrics_series: {e}");
        ApiError::internal(e.to_string())
    })?;

    let mut series: std::collections::BTreeMap<String, Vec<MetricPoint>> = Default::default();
    for (key, t, v) in rows {
        series.entry(key).or_default().push(MetricPoint { t, v });
    }

    Ok(Json(MetricsSeriesResponse {
        bucket_seconds,
        series,
    }))
}

// ─── metrics/stream (SSE) ───────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct MetricsStreamQuery {
    #[serde(default)]
    pub since_seq: Option<u64>,
    pub signal_key: Option<String>,
    /// Comma-separated metric keys to filter.
    pub keys: Option<String>,
}

/// SSE: emits `connected`, optional `gap`/`resync`, then `metric` events
/// whose `data` field is a JSON-stringified `LiveMetricEvent`.
#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/metrics/stream",
    params(
        ("process_id" = String, Path, description = "Process id"),
        MetricsStreamQuery,
    ),
    responses(
        (status = 200, description = "SSE stream of metric events", content_type = "text/event-stream"),
    ),
    tag = "processes-live",
)]
pub async fn metrics_stream(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
    Query(q): Query<MetricsStreamQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let since_seq = q.since_seq.unwrap_or(0);
    let signal_filter = q.signal_key.clone();
    let key_filter: Option<HashSet<String>> = q.keys.as_deref().map(|s| {
        s.split(',')
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect()
    });

    // Subscribe BEFORE snapshotting to avoid missing events in between.
    let rx = state.live.subscribe_metrics();
    let (snapshot, first_buf_seq) = state.live.metrics_snapshot();

    // Detect gap: client resuming from a seq that predates our ring buffer.
    let gap = since_seq > 0 && first_buf_seq > since_seq + 1;

    // Filter the snapshot.
    let pid = process_id.clone();
    let sig_for_backfill = signal_filter.clone();
    let keys_for_backfill = key_filter.clone();
    let backfill: Vec<Result<Event, Infallible>> = snapshot
        .into_iter()
        .filter(|e| e.seq > since_seq)
        .filter(|e| e.process_id == pid)
        .filter(|e| {
            sig_for_backfill
                .as_deref()
                .is_none_or(|s| e.signal_key.as_deref() == Some(s))
        })
        .filter(|e| {
            keys_for_backfill
                .as_ref()
                .is_none_or(|set| set.contains(&e.key))
        })
        .map(|e| Ok(metric_event_to_sse(&e)))
        .collect();

    let max_backfill_seq = state.live.metrics_snapshot().1; // reasonable upper bound

    let pid2 = process_id.clone();
    let live_stream = futures::stream::unfold(
        (
            rx,
            signal_filter,
            key_filter,
            pid2,
            since_seq.max(max_backfill_seq),
        ),
        |(mut rx, sig, keys, pid, skip_until)| async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if event.seq <= skip_until {
                            continue;
                        }
                        if event.process_id != pid {
                            continue;
                        }
                        if let Some(ref s) = sig {
                            if event.signal_key.as_deref() != Some(s.as_str()) {
                                continue;
                            }
                        }
                        if let Some(ref set) = keys {
                            if !set.contains(&event.key) {
                                continue;
                            }
                        }
                        let seq = event.seq;
                        return Some((Ok(metric_event_to_sse(&event)), (rx, sig, keys, pid, seq)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        let data = serde_json::json!({ "missed": n }).to_string();
                        return Some((
                            Ok(Event::default().event("resync").data(data)),
                            (rx, sig, keys, pid, skip_until),
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );

    let mut prelude: Vec<Result<Event, Infallible>> = Vec::new();
    prelude.push(Ok(Event::default().event("connected").data("ok")));
    if gap {
        let data = serde_json::json!({ "since_seq": since_seq, "first_buf_seq": first_buf_seq })
            .to_string();
        prelude.push(Ok(Event::default().event("gap").data(data)));
    }

    use futures::StreamExt;
    let stream = futures::stream::iter(prelude)
        .chain(futures::stream::iter(backfill))
        .chain(live_stream);

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(5))
            .text("ping"),
    )
}

fn metric_event_to_sse(e: &LiveMetricEvent) -> Event {
    let data = serde_json::to_string(e).unwrap_or_default();
    Event::default()
        .event("metric")
        .id(e.seq.to_string())
        .data(data)
}

// ─── logs/tail (DB backfill) ────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct LogsTailQuery {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub level: Option<String>,
    pub signal_key: Option<String>,
    pub q: Option<String>,
    #[serde(default = "default_log_limit")]
    pub limit: i64,
}

fn default_log_limit() -> i64 {
    500
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct LogRow {
    pub id: i64,
    pub process_id: String,
    pub level: String,
    pub source: Option<String>,
    pub message: String,
    pub detail: serde_json::Value,
    pub timestamp: DateTime<Utc>,
    pub signal_key: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/logs/tail",
    params(
        ("process_id" = String, Path, description = "Process id"),
        LogsTailQuery,
    ),
    responses(
        (status = 200, description = "Recent log rows", body = LogsTailResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "processes-live",
)]
pub async fn logs_tail(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
    Query(qp): Query<LogsTailQuery>,
) -> Result<Json<LogsTailResponse>, ApiError> {
    let sql = "SELECT id, process_id, level, source, message, detail, timestamp, signal_key \
               FROM hpi_logs \
               WHERE process_id = $1 \
                 AND ($2::timestamptz IS NULL OR timestamp >= $2) \
                 AND ($3::timestamptz IS NULL OR timestamp < $3) \
                 AND ($4::text IS NULL OR level = $4) \
                 AND ($5::text IS NULL OR signal_key = $5) \
                 AND ($6::text IS NULL OR message ILIKE '%' || $6 || '%') \
               ORDER BY timestamp DESC \
               LIMIT $7";

    let rows: Result<Vec<LogRow>, sqlx::Error> = sqlx::query_as::<_, LogRow>(sql)
        .bind(&process_id)
        .bind(qp.since)
        .bind(qp.until)
        .bind(qp.level.as_deref())
        .bind(qp.signal_key.as_deref())
        .bind(qp.q.as_deref())
        .bind(qp.limit.clamp(1, 5000))
        .fetch_all(&state.db)
        .await;

    let mut rs = rows.map_err(|e| {
        tracing::warn!(process_id = %process_id, "logs_tail: {e}");
        ApiError::internal(e.to_string())
    })?;

    // Client wants ascending timeline for tail rendering.
    rs.reverse();
    Ok(Json(LogsTailResponse { logs: rs }))
}

// ─── logs/stream (SSE) ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct LogsStreamQuery {
    #[serde(default)]
    pub since_seq: Option<u64>,
    pub signal_key: Option<String>,
    pub level: Option<String>,
    pub q: Option<String>,
}

/// SSE: emits `log` events whose `data` field is a JSON-stringified `LiveLogEvent`.
#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/logs/stream",
    params(
        ("process_id" = String, Path, description = "Process id"),
        LogsStreamQuery,
    ),
    responses(
        (status = 200, description = "SSE stream of log events", content_type = "text/event-stream"),
    ),
    tag = "processes-live",
)]
pub async fn logs_stream(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
    Query(qp): Query<LogsStreamQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let since_seq = qp.since_seq.unwrap_or(0);
    let signal_filter = qp.signal_key.clone();
    let level_filter = qp.level.clone();
    let text_filter = qp.q.clone().map(|s| s.to_lowercase());

    let rx = state.live.subscribe_logs();
    let (snapshot, first_buf_seq) = state.live.logs_snapshot();
    let gap = since_seq > 0 && first_buf_seq > since_seq + 1;

    let pid = process_id.clone();
    let sig_bf = signal_filter.clone();
    let lvl_bf = level_filter.clone();
    let txt_bf = text_filter.clone();
    let backfill: Vec<Result<Event, Infallible>> = snapshot
        .into_iter()
        .filter(|e| e.seq > since_seq)
        .filter(|e| e.process_id == pid)
        .filter(|e| {
            sig_bf
                .as_deref()
                .is_none_or(|s| e.signal_key.as_deref() == Some(s))
        })
        .filter(|e| lvl_bf.as_deref().is_none_or(|l| e.level == l))
        .filter(|e| {
            txt_bf
                .as_deref()
                .is_none_or(|t| e.message.to_lowercase().contains(t))
        })
        .map(|e| Ok(log_event_to_sse(&e)))
        .collect();

    let max_backfill_seq = state.live.logs_snapshot().1;
    let pid2 = process_id.clone();
    let live_stream = futures::stream::unfold(
        (
            rx,
            signal_filter,
            level_filter,
            text_filter,
            pid2,
            since_seq.max(max_backfill_seq),
        ),
        |(mut rx, sig, lvl, txt, pid, skip_until)| async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if event.seq <= skip_until {
                            continue;
                        }
                        if event.process_id != pid {
                            continue;
                        }
                        if let Some(ref s) = sig {
                            if event.signal_key.as_deref() != Some(s.as_str()) {
                                continue;
                            }
                        }
                        if let Some(ref l) = lvl {
                            if event.level != *l {
                                continue;
                            }
                        }
                        if let Some(ref t) = txt {
                            if !event.message.to_lowercase().contains(t) {
                                continue;
                            }
                        }
                        let seq = event.seq;
                        return Some((Ok(log_event_to_sse(&event)), (rx, sig, lvl, txt, pid, seq)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        let data = serde_json::json!({ "missed": n }).to_string();
                        return Some((
                            Ok(Event::default().event("resync").data(data)),
                            (rx, sig, lvl, txt, pid, skip_until),
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );

    let mut prelude: Vec<Result<Event, Infallible>> = Vec::new();
    prelude.push(Ok(Event::default().event("connected").data("ok")));
    if gap {
        let data = serde_json::json!({ "since_seq": since_seq, "first_buf_seq": first_buf_seq })
            .to_string();
        prelude.push(Ok(Event::default().event("gap").data(data)));
    }

    use futures::StreamExt;
    let stream = futures::stream::iter(prelude)
        .chain(futures::stream::iter(backfill))
        .chain(live_stream);

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(5))
            .text("ping"),
    )
}

fn log_event_to_sse(e: &LiveLogEvent) -> Event {
    let data = serde_json::to_string(e).unwrap_or_default();
    Event::default()
        .event("log")
        .id(e.seq.to_string())
        .data(data)
}

// ─── artifacts/list (DB backfill) ───────────────────────────────────────────

fn parse_csv(s: Option<&str>) -> Vec<String> {
    s.map(|v| {
        v.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ArtifactsListQuery {
    /// Comma-separated category whitelist. Empty = all.
    pub categories: Option<String>,
    /// Comma-separated render_hint whitelist (matched against
    /// `user_metadata->>'render_hint'`).
    pub render_hints: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    #[serde(default = "default_artifact_limit")]
    pub limit: i64,
}

fn default_artifact_limit() -> i64 {
    200
}

#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/artifacts/list",
    params(
        ("process_id" = String, Path, description = "Process id"),
        ArtifactsListQuery,
    ),
    responses(
        (status = 200, description = "Recent artifacts", body = ArtifactsListResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "processes-live",
)]
pub async fn artifacts_list(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
    Query(qp): Query<ArtifactsListQuery>,
) -> Result<Json<ArtifactsListResponse>, ApiError> {
    let categories = parse_csv(qp.categories.as_deref());
    let hints = parse_csv(qp.render_hints.as_deref());
    let entries = crate::catalogue::queries::lineage_filtered(
        &state.db,
        &process_id,
        &categories,
        &hints,
        qp.since,
        qp.until,
        qp.limit,
    )
    .await
    .map_err(|e| {
        tracing::warn!(process_id = %process_id, "artifacts_list: {e}");
        ApiError::internal(e.to_string())
    })?;
    Ok(Json(ArtifactsListResponse { entries }))
}

// ─── artifacts/stream (SSE) ─────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct ArtifactsStreamQuery {
    #[serde(default)]
    pub since_seq: Option<u64>,
    pub categories: Option<String>,
    pub render_hints: Option<String>,
}

fn artifact_matches(
    e: &LiveArtifactEvent,
    pid: &str,
    categories: &Option<HashSet<String>>,
    hints: &Option<HashSet<String>>,
) -> bool {
    if e.process_id != pid {
        return false;
    }
    if let Some(set) = categories {
        if !set.contains(&e.category) {
            return false;
        }
    }
    if let Some(set) = hints {
        let hint = e
            .user_metadata
            .get("render_hint")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !set.contains(hint) {
            return false;
        }
    }
    true
}

/// SSE: emits `artifact` events whose `data` field is a JSON-stringified `LiveArtifactEvent`.
#[utoipa::path(
    get,
    path = "/api/v1/processes/{process_id}/artifacts/stream",
    params(
        ("process_id" = String, Path, description = "Process id"),
        ArtifactsStreamQuery,
    ),
    responses(
        (status = 200, description = "SSE stream of artifact events", content_type = "text/event-stream"),
    ),
    tag = "processes-live",
)]
pub async fn artifacts_stream(
    State(state): State<AppState>,
    Path(process_id): Path<String>,
    Query(qp): Query<ArtifactsStreamQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let since_seq = qp.since_seq.unwrap_or(0);
    let categories: Option<HashSet<String>> = qp.categories.as_deref().map(|s| {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    });
    let hints: Option<HashSet<String>> = qp.render_hints.as_deref().map(|s| {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    });

    let rx = state.live.subscribe_artifacts();
    let (snapshot, first_buf_seq) = state.live.artifacts_snapshot();
    let gap = since_seq > 0 && first_buf_seq > since_seq + 1;

    let pid_bf = process_id.clone();
    let cat_bf = categories.clone();
    let hints_bf = hints.clone();
    let backfill: Vec<Result<Event, Infallible>> = snapshot
        .into_iter()
        .filter(|e| e.seq > since_seq)
        .filter(|e| artifact_matches(e, &pid_bf, &cat_bf, &hints_bf))
        .map(|e| Ok(artifact_event_to_sse(&e)))
        .collect();

    let max_backfill_seq = state.live.artifacts_snapshot().1;
    let pid2 = process_id.clone();
    let live_stream = futures::stream::unfold(
        (rx, categories, hints, pid2, since_seq.max(max_backfill_seq)),
        |(mut rx, cats, hs, pid, skip_until)| async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if event.seq <= skip_until {
                            continue;
                        }
                        if !artifact_matches(&event, &pid, &cats, &hs) {
                            continue;
                        }
                        let seq = event.seq;
                        return Some((Ok(artifact_event_to_sse(&event)), (rx, cats, hs, pid, seq)));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        let data = serde_json::json!({ "missed": n }).to_string();
                        return Some((
                            Ok(Event::default().event("resync").data(data)),
                            (rx, cats, hs, pid, skip_until),
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );

    let mut prelude: Vec<Result<Event, Infallible>> = Vec::new();
    prelude.push(Ok(Event::default().event("connected").data("ok")));
    if gap {
        let data = serde_json::json!({ "since_seq": since_seq, "first_buf_seq": first_buf_seq })
            .to_string();
        prelude.push(Ok(Event::default().event("gap").data(data)));
    }

    use futures::StreamExt;
    let stream = futures::stream::iter(prelude)
        .chain(futures::stream::iter(backfill))
        .chain(live_stream);

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(5))
            .text("ping"),
    )
}

fn artifact_event_to_sse(e: &LiveArtifactEvent) -> Event {
    let data = serde_json::to_string(e).unwrap_or_default();
    Event::default()
        .event("artifact")
        .id(e.seq.to_string())
        .data(data)
}
