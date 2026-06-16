//! Inference telemetry for the Control-Plane "Router" surface (docs/29 + docs/11).
//!
//! Two reads that turn the self-hosted-inference page from a static "go scrape
//! Prometheus" pointer into REAL data — without making the product UI depend on
//! Prometheus being up:
//!
//!   * `GET /api/v1/inference/router-live` — a point-in-time proxy of the
//!     router's `/metrics` exposition, parsed into JSON. These are the live
//!     operational gauges the durable ledger can't carry: per-replica in-flight
//!     vs admission capacity, per-model in-flight + starvation (the
//!     scale-from-zero signal), and the global request counters. Fail-soft:
//!     `available=false` when the router is unconfigured/unreachable (same
//!     `AUTOSCALER_DEMAND_URL` knob the autoscaler + engine-headroom poll use).
//!
//!   * `GET /api/v1/inference/timeseries` — historical per-model throughput,
//!     latency percentiles, and error rate, time-bucketed over the durable
//!     `inference_request_log` ledger via TimescaleDB `time_bucket` (the
//!     extension is created by `migrations/20240105000000_create_hpi_tables.sql`
//!     and the dev image is `timescale/timescaledb`). This is the "over time"
//!     view that used to live only in Prometheus/Grafana — the data was already
//!     in the ledger, mekhan just never aggregated it.
//!
//! A real Prometheus scraping the router is still worth running as an ops layer
//! (`just dev up-prometheus`), but it is NOT what this page reads.

use std::collections::{BTreeMap, HashMap};

use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::auth::AuthUser;
use crate::models::error::ApiError;
use crate::AppState;

// ── Live router /metrics proxy ───────────────────────────────────────────────

/// The router endpoint to scrape, derived from the same env knob the autoscaler
/// and engine-headroom poll already use (`AUTOSCALER_DEMAND_URL`), falling back
/// to `MEKHAN_ROUTER_URL`. `None` ⇒ no router configured ⇒ the live view degrades
/// to `available=false` (fail-soft, never an error).
fn router_metrics_url() -> Option<String> {
    std::env::var("AUTOSCALER_DEMAND_URL")
        .ok()
        .or_else(|| std::env::var("MEKHAN_ROUTER_URL").ok())
        .filter(|u| !u.is_empty())
        .map(|u| format!("{}/metrics", u.trim_end_matches('/')))
}

/// Global router counters (monotonic since the router started).
#[derive(Debug, Default, Clone, Serialize, ToSchema)]
pub struct RouterGlobalCounters {
    pub requests_total: u64,
    pub completed_total: u64,
    pub rejected_429_total: u64,
    pub cancelled_total: u64,
    pub upstream_error_total: u64,
}

/// One upstream replica's live admission state.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RouterReplicaLive {
    pub replica: String,
    pub zone: Option<String>,
    pub live: bool,
    /// In-flight requests currently admitted on this replica.
    pub in_flight: u64,
    /// Admission capacity (`--max-num-seqs`) for this replica.
    pub capacity: u64,
}

/// One model's live demand signals (summed across its live replicas).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RouterModelLive {
    pub model: String,
    /// In-flight requests summed across this model's LIVE replicas (scale-DOWN
    /// signal — 0 ⇒ idle).
    pub inflight: u64,
    /// Cumulative requests that found no live/un-saturated replica (the
    /// scale-FROM-zero demand signal).
    pub starved: u64,
    pub completed: u64,
    pub unmetered: u64,
    pub cancelled: u64,
    pub upstream_error: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Mean request latency (ms) derived from the histogram `_sum`/`_count`.
    /// `None` when no terminal request has been observed for this model.
    pub avg_latency_ms: Option<f64>,
}

/// Parsed snapshot of the router's `/metrics` exposition.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct RouterLiveMetrics {
    /// `false` ⇒ no router configured or the scrape failed; all other fields are
    /// empty/zero. The UI shows a "router unreachable" hint rather than an error.
    pub available: bool,
    pub global: RouterGlobalCounters,
    pub replicas: Vec<RouterReplicaLive>,
    pub models: Vec<RouterModelLive>,
}

/// `GET /api/v1/inference/router-live` — point-in-time router operational gauges.
///
/// Proxies + parses the router's `/metrics` exposition into JSON so the product
/// UI can render live per-replica/per-model state without a Prometheus
/// dependency. Fail-soft: a missing/unreachable router yields
/// `{ available: false, … }` with a 200, never a 5xx.
#[utoipa::path(
    get,
    path = "/api/v1/inference/router-live",
    responses(
        (status = 200, description = "Live router operational gauges (point-in-time); available=false when the router is unreachable", body = RouterLiveMetrics),
    ),
    tag = "models",
)]
pub async fn router_live_metrics(
    State(_state): State<AppState>,
    _user: AuthUser,
) -> Json<RouterLiveMetrics> {
    let Some(url) = router_metrics_url() else {
        return Json(RouterLiveMetrics::default());
    };
    let body = match reqwest::Client::new().get(&url).send().await {
        Ok(resp) if resp.status().is_success() => resp.text().await.unwrap_or_default(),
        Ok(resp) => {
            tracing::debug!(%url, status = %resp.status(), "router /metrics non-200");
            return Json(RouterLiveMetrics::default());
        }
        Err(e) => {
            tracing::debug!(%url, "router /metrics scrape failed: {e}");
            return Json(RouterLiveMetrics::default());
        }
    };
    Json(parse_router_metrics(&body))
}

/// One parsed exposition sample: `name{labels} value`.
struct Sample {
    name: String,
    labels: HashMap<String, String>,
    value: f64,
}

/// Parse a Prometheus text exposition into samples, skipping `#` comment lines.
/// Tolerant of our own router output (no quoted commas in label values); a line
/// that doesn't parse is silently dropped rather than failing the whole scrape.
fn parse_exposition(body: &str) -> Vec<Sample> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((series, value)) = line.rsplit_once(char::is_whitespace) else {
            continue;
        };
        let Ok(value) = value.trim().parse::<f64>() else {
            continue;
        };
        let (name, labels) = match series.find('{') {
            Some(i) => {
                let name = series[..i].to_string();
                let end = series.rfind('}').unwrap_or(series.len());
                (name, parse_labels(&series[i + 1..end]))
            }
            None => (series.to_string(), HashMap::new()),
        };
        out.push(Sample {
            name,
            labels,
            value,
        });
    }
    out
}

/// Parse `key="value",key2="value2"` label sets. Values are unquoted; empty
/// strings (`zone=""`) become absent rather than `Some("")`.
fn parse_labels(s: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for pair in s.split(',') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"');
        if !v.is_empty() {
            m.insert(k.trim().to_string(), v.to_string());
        }
    }
    m
}

/// Fold parsed samples into the structured live snapshot.
fn parse_router_metrics(body: &str) -> RouterLiveMetrics {
    let samples = parse_exposition(body);

    let mut global = RouterGlobalCounters::default();
    // Replicas keyed by id so the two gauges (inflight, capacity) merge.
    let mut replicas: BTreeMap<String, RouterReplicaLive> = BTreeMap::new();
    // Per-model accumulator; assembled into RouterModelLive at the end.
    #[derive(Default)]
    struct ModelAcc {
        inflight: u64,
        starved: u64,
        completed: u64,
        unmetered: u64,
        cancelled: u64,
        upstream_error: u64,
        prompt_tokens: u64,
        completion_tokens: u64,
        latency_sum: f64,
        latency_count: u64,
    }
    let mut models: BTreeMap<String, ModelAcc> = BTreeMap::new();
    let u = |v: f64| -> u64 { v.max(0.0).round() as u64 };

    for s in &samples {
        match s.name.as_str() {
            "inference_router_requests_total" => global.requests_total = u(s.value),
            "inference_router_completed_total" => global.completed_total = u(s.value),
            "inference_router_rejected_429_total" => global.rejected_429_total = u(s.value),
            "inference_router_cancelled_total" => global.cancelled_total = u(s.value),
            "inference_router_upstream_error_total" => global.upstream_error_total = u(s.value),
            "inference_router_replica_inflight" => {
                if let Some(id) = s.labels.get("replica") {
                    let r = replicas
                        .entry(id.clone())
                        .or_insert_with(|| RouterReplicaLive {
                            replica: id.clone(),
                            zone: None,
                            live: false,
                            in_flight: 0,
                            capacity: 0,
                        });
                    r.in_flight = u(s.value);
                    r.zone = s.labels.get("zone").cloned().or(r.zone.take());
                    r.live = s.labels.get("live").map(|v| v == "true").unwrap_or(r.live);
                }
            }
            "inference_router_replica_capacity" => {
                if let Some(id) = s.labels.get("replica") {
                    let r = replicas
                        .entry(id.clone())
                        .or_insert_with(|| RouterReplicaLive {
                            replica: id.clone(),
                            zone: None,
                            live: false,
                            in_flight: 0,
                            capacity: 0,
                        });
                    r.capacity = u(s.value);
                    r.zone = s.labels.get("zone").cloned().or(r.zone.take());
                }
            }
            "inference_router_model_inflight" => {
                if let Some(m) = s.labels.get("model") {
                    models.entry(m.clone()).or_default().inflight = u(s.value);
                }
            }
            "inference_router_model_starved_total" => {
                if let Some(m) = s.labels.get("model") {
                    models.entry(m.clone()).or_default().starved = u(s.value);
                }
            }
            "inference_router_model_requests_total" => {
                if let (Some(m), Some(status)) = (s.labels.get("model"), s.labels.get("status")) {
                    let acc = models.entry(m.clone()).or_default();
                    match status.as_str() {
                        "completed" => acc.completed = u(s.value),
                        "unmetered" => acc.unmetered = u(s.value),
                        "cancelled" => acc.cancelled = u(s.value),
                        "upstream_error" => acc.upstream_error = u(s.value),
                        _ => {}
                    }
                }
            }
            "inference_router_model_prompt_tokens_total" => {
                if let Some(m) = s.labels.get("model") {
                    models.entry(m.clone()).or_default().prompt_tokens = u(s.value);
                }
            }
            "inference_router_model_completion_tokens_total" => {
                if let Some(m) = s.labels.get("model") {
                    models.entry(m.clone()).or_default().completion_tokens = u(s.value);
                }
            }
            "inference_router_request_duration_seconds_sum" => {
                if let Some(m) = s.labels.get("model") {
                    models.entry(m.clone()).or_default().latency_sum = s.value;
                }
            }
            "inference_router_request_duration_seconds_count" => {
                if let Some(m) = s.labels.get("model") {
                    models.entry(m.clone()).or_default().latency_count = u(s.value);
                }
            }
            _ => {}
        }
    }

    let models = models
        .into_iter()
        .map(|(model, a)| RouterModelLive {
            model,
            inflight: a.inflight,
            starved: a.starved,
            completed: a.completed,
            unmetered: a.unmetered,
            cancelled: a.cancelled,
            upstream_error: a.upstream_error,
            prompt_tokens: a.prompt_tokens,
            completion_tokens: a.completion_tokens,
            avg_latency_ms: (a.latency_count > 0)
                .then(|| (a.latency_sum / a.latency_count as f64) * 1000.0),
        })
        .collect();

    RouterLiveMetrics {
        available: true,
        global,
        replicas: replicas.into_values().collect(),
        models,
    }
}

// ── Historical timeseries over the durable ledger ────────────────────────────

const DEFAULT_BUCKET_SECS: i64 = 60;
const MIN_BUCKET_SECS: i64 = 5;
const MAX_BUCKET_SECS: i64 = 3600;
const DEFAULT_WINDOW_SECS: i64 = 3600;
const MAX_WINDOW_SECS: i64 = 7 * 24 * 3600;

/// Query params for the per-model timeseries aggregation.
#[derive(Debug, Deserialize, IntoParams)]
pub struct InferenceTimeseriesQuery {
    /// Bucket width in seconds (default 60, clamped 5..3600).
    pub bucket_secs: Option<i64>,
    /// Look-back window in seconds (default 3600, capped at 7 days).
    pub window_secs: Option<i64>,
    /// Restrict to a single model.
    pub model: Option<String>,
    /// Restrict to one workflow instance's requests.
    pub instance_id: Option<String>,
}

/// One `(bucket, model)` rollup of the inference ledger.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct InferenceTimeseriesPoint {
    pub bucket: DateTime<Utc>,
    pub model_id: String,
    pub requests: i64,
    pub completed: i64,
    /// `cancelled` + `upstream_error` (the failure dispositions).
    pub errors: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// Median request latency (ms) within the bucket; `None` if no rows.
    pub p50_ms: Option<f64>,
    /// p95 request latency (ms) within the bucket.
    pub p95_ms: Option<f64>,
}

/// `GET /api/v1/inference/timeseries` — per-model throughput / latency / errors
/// over time, bucketed over the durable `inference_request_log` ledger.
///
/// NOT tenant-scoped yet, for the same reason as the audit-ledger read
/// (`list_inference_requests`): the ledger's `tenant_id` is the router's Bearer
/// tenant, not yet mapped to the workspace UUID. Auth is still required.
#[utoipa::path(
    get,
    path = "/api/v1/inference/timeseries",
    params(InferenceTimeseriesQuery),
    responses(
        (status = 200, description = "Per-model inference throughput/latency/error timeseries, oldest bucket first", body = Vec<InferenceTimeseriesPoint>),
    ),
    tag = "models",
)]
pub async fn inference_timeseries(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(q): Query<InferenceTimeseriesQuery>,
) -> Result<Json<Vec<InferenceTimeseriesPoint>>, ApiError> {
    let bucket_secs = q
        .bucket_secs
        .unwrap_or(DEFAULT_BUCKET_SECS)
        .clamp(MIN_BUCKET_SECS, MAX_BUCKET_SECS);
    let window_secs = q
        .window_secs
        .unwrap_or(DEFAULT_WINDOW_SECS)
        .clamp(MIN_BUCKET_SECS, MAX_WINDOW_SECS);

    // `time_bucket` is the TimescaleDB extension (created by the hpi migration;
    // dev image is timescale/timescaledb-pg16). Latency is finished−started in ms.
    let rows: Vec<InferenceTimeseriesPoint> = sqlx::query_as(
        "SELECT \
           time_bucket(make_interval(secs => $1), started_at) AS bucket, \
           model_id, \
           count(*)::bigint AS requests, \
           count(*) FILTER (WHERE status = 'completed')::bigint AS completed, \
           count(*) FILTER (WHERE status IN ('upstream_error','cancelled'))::bigint AS errors, \
           coalesce(sum(prompt_tokens), 0)::bigint AS prompt_tokens, \
           coalesce(sum(completion_tokens), 0)::bigint AS completion_tokens, \
           percentile_cont(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (finished_at - started_at)) * 1000.0) AS p50_ms, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (finished_at - started_at)) * 1000.0) AS p95_ms \
         FROM inference_request_log \
         WHERE started_at >= now() - make_interval(secs => $2) \
           AND ($3::text IS NULL OR model_id = $3) \
           AND ($4::text IS NULL OR instance_id = $4) \
         GROUP BY bucket, model_id \
         ORDER BY bucket ASC, model_id ASC",
    )
    .bind(bucket_secs as f64)
    .bind(window_secs as f64)
    .bind(q.model.as_deref())
    .bind(q.instance_id.as_deref())
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("inference timeseries: {e}")))?;

    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A representative router /metrics body (the shapes router/src/metrics.rs emits).
    const SAMPLE: &str = "\
# HELP inference_router_requests_total Total chat-completions requests received.
# TYPE inference_router_requests_total counter
inference_router_requests_total 42
inference_router_completed_total 40
inference_router_rejected_429_total 1
inference_router_cancelled_total 1
inference_router_upstream_error_total 0
# TYPE inference_router_replica_inflight gauge
inference_router_replica_inflight{replica=\"replica-0\",zone=\"eu-dev\",live=\"true\"} 2
inference_router_replica_capacity{replica=\"replica-0\",zone=\"eu-dev\"} 4
inference_router_replica_inflight{replica=\"replica-1\",zone=\"\",live=\"false\"} 0
inference_router_replica_capacity{replica=\"replica-1\",zone=\"\"} 4
inference_router_model_inflight{model=\"warm\"} 2
inference_router_model_inflight{model=\"cold\"} 0
inference_router_model_starved_total{model=\"cold\"} 5
inference_router_model_requests_total{model=\"warm\",status=\"completed\"} 38
inference_router_model_requests_total{model=\"warm\",status=\"upstream_error\"} 0
inference_router_model_requests_total{model=\"warm\",status=\"cancelled\"} 1
inference_router_model_requests_total{model=\"warm\",status=\"unmetered\"} 1
inference_router_model_prompt_tokens_total{model=\"warm\"} 1200
inference_router_model_completion_tokens_total{model=\"warm\"} 800
inference_router_request_duration_seconds_bucket{model=\"warm\",le=\"0.5\"} 30
inference_router_request_duration_seconds_sum{model=\"warm\"} 76
inference_router_request_duration_seconds_count{model=\"warm\"} 40
";

    #[test]
    fn parses_global_counters() {
        let m = parse_router_metrics(SAMPLE);
        assert!(m.available);
        assert_eq!(m.global.requests_total, 42);
        assert_eq!(m.global.completed_total, 40);
        assert_eq!(m.global.rejected_429_total, 1);
        assert_eq!(m.global.cancelled_total, 1);
        assert_eq!(m.global.upstream_error_total, 0);
    }

    #[test]
    fn merges_replica_inflight_and_capacity() {
        let m = parse_router_metrics(SAMPLE);
        let r0 = m
            .replicas
            .iter()
            .find(|r| r.replica == "replica-0")
            .unwrap();
        assert_eq!(r0.in_flight, 2);
        assert_eq!(r0.capacity, 4);
        assert_eq!(r0.zone.as_deref(), Some("eu-dev"));
        assert!(r0.live);
        // Empty zone label → None, live=false carried through.
        let r1 = m
            .replicas
            .iter()
            .find(|r| r.replica == "replica-1")
            .unwrap();
        assert_eq!(r1.zone, None);
        assert!(!r1.live);
        assert_eq!(r1.capacity, 4);
    }

    #[test]
    fn folds_per_model_demand_throughput_and_latency() {
        let m = parse_router_metrics(SAMPLE);
        let warm = m.models.iter().find(|x| x.model == "warm").unwrap();
        assert_eq!(warm.inflight, 2);
        assert_eq!(warm.completed, 38);
        assert_eq!(warm.cancelled, 1);
        assert_eq!(warm.unmetered, 1);
        assert_eq!(warm.prompt_tokens, 1200);
        assert_eq!(warm.completion_tokens, 800);
        // avg = sum/count * 1000 = 76/40*1000 = 1900ms.
        assert_eq!(warm.avg_latency_ms, Some(1900.0));

        let cold = m.models.iter().find(|x| x.model == "cold").unwrap();
        assert_eq!(cold.inflight, 0);
        assert_eq!(cold.starved, 5);
        // No terminal request observed → no average.
        assert_eq!(cold.avg_latency_ms, None);
    }

    #[test]
    fn empty_body_is_available_but_blank() {
        let m = parse_router_metrics("");
        assert!(m.available);
        assert!(m.replicas.is_empty());
        assert!(m.models.is_empty());
        assert_eq!(m.global.requests_total, 0);
    }
}
