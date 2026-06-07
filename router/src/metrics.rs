//! Minimal Prometheus exposition (doc 11 §5.8 autoscale signal source).
//!
//! Hand-rolled (no `prometheus` crate) — a handful of atomic counters plus
//! per-replica in-flight derived live from the replica table's admission
//! semaphores. The P4-L2 autoscaler scrapes `GET /metrics`; the router only
//! EMITS, never actuates (doc 11 data-plane/control-plane separation).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use inference_core::InferenceRequestLog;

use crate::routing::ReplicaStat;

/// Upper bounds (seconds) for the per-model request-duration histogram. Coarse
/// by design — operators want "p50 ≈ 2s, tail < 30s", not microsecond fidelity.
/// Each observation lands in the first bucket whose bound it does not exceed;
/// anything slower than the last bound counts only in `+Inf` (= `_count`).
const LATENCY_BUCKETS_SECS: [f64; 10] = [0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0];

/// Per-model rollup of terminal requests — the durable demand/throughput signal
/// Prometheus scrapes (Grafana owns the over-time view; the router only EMITS).
/// Token + request counters are monotonic; the latency histogram is `le`-bucketed
/// counts plus a sum for the average.
#[derive(Debug, Default, Clone)]
struct ModelMeter {
    // Requests by terminal disposition (mirrors `MeterContext::finish`'s status).
    completed: u64,
    unmetered: u64,
    cancelled: u64,
    upstream_error: u64,
    // Token throughput (input vs output) — the "tokens in/out per model" series.
    prompt_tokens: u64,
    completion_tokens: u64,
    // Request-duration histogram: per-bucket (non-cumulative) counts aligned to
    // `LATENCY_BUCKETS_SECS`, rendered cumulatively. `latency_sum`/`latency_count`
    // back the `_sum`/`_count` series (and the implicit `+Inf` bucket).
    bucket_counts: [u64; LATENCY_BUCKETS_SECS.len()],
    latency_sum: f64,
    latency_count: u64,
}

#[derive(Debug, Default)]
pub struct Metrics {
    pub requests_total: AtomicU64,
    pub completed_total: AtomicU64,
    pub rejected_429_total: AtomicU64,
    pub cancelled_total: AtomicU64,
    pub upstream_error_total: AtomicU64,
    /// Per-model STARVED counter (P4-L2 demand signal): a request arrived for a
    /// model that had no live replica (`NoReplica`) or whose replicas were all
    /// saturated (429). This is the scale-FROM-zero signal — `model_inflight`
    /// alone is 0 when a `scale_to_zero` policy has scaled down, so the autoscaler
    /// reads the *delta* of this counter between scrapes to detect fresh demand.
    model_starved: Mutex<HashMap<String, u64>>,
    /// Per-model terminal rollup — tokens in/out, requests by status, and a
    /// request-duration histogram. Recorded from every metering record (the same
    /// terminal that publishes to NATS), so the Prometheus view and the durable
    /// audit ledger are fed from one point.
    models: Mutex<HashMap<String, ModelMeter>>,
}

impl Metrics {
    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a starved request for `model` (no live replica, or all saturated).
    /// The P4-L2 autoscaler scrapes the per-model counter delta to scale from
    /// zero / under saturation.
    pub fn inc_starved(&self, model: &str) {
        if let Ok(mut m) = self.model_starved.lock() {
            *m.entry(model.to_string()).or_insert(0) += 1;
        }
    }

    /// Fold one terminal metering record into the per-model Prometheus rollup.
    /// Called at every terminal alongside `publish_meter`, so the scrape-able
    /// series and the durable ledger never diverge. Latency is the wall-clock
    /// `finished_at − started_at` (floored at 0 — clock skew never produces a
    /// negative duration).
    pub fn observe_record(&self, rec: &InferenceRequestLog) {
        let latency_secs =
            (rec.finished_at - rec.started_at).num_milliseconds().max(0) as f64 / 1000.0;
        self.observe(
            &rec.model,
            &rec.status,
            rec.prompt_tokens,
            rec.completion_tokens,
            latency_secs,
        );
    }

    /// The primitive behind [`observe_record`] (split out so it is testable
    /// without constructing a full record). `status` is the metering status
    /// string (`completed` / `unmetered` / `cancelled` / `upstream_error`); an
    /// unknown status still counts toward tokens + latency but no request bucket.
    pub fn observe(
        &self,
        model: &str,
        status: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        latency_secs: f64,
    ) {
        let Ok(mut models) = self.models.lock() else {
            return;
        };
        let m = models.entry(model.to_string()).or_default();
        match status {
            "completed" => m.completed += 1,
            "unmetered" => m.unmetered += 1,
            "cancelled" => m.cancelled += 1,
            "upstream_error" => m.upstream_error += 1,
            _ => {}
        }
        m.prompt_tokens += prompt_tokens;
        m.completion_tokens += completion_tokens;
        // Histogram: first bucket whose bound the latency does not exceed.
        if let Some(i) = LATENCY_BUCKETS_SECS.iter().position(|&b| latency_secs <= b) {
            m.bucket_counts[i] += 1;
        }
        m.latency_sum += latency_secs;
        m.latency_count += 1;
    }

    /// Render Prometheus text exposition for the counters + per-replica
    /// in-flight / capacity gauges.
    pub fn render(&self, replicas: &[ReplicaStat]) -> String {
        let mut out = String::with_capacity(1024);

        let counters: [(&str, &AtomicU64, &str); 5] = [
            (
                "inference_router_requests_total",
                &self.requests_total,
                "Total chat-completions requests received.",
            ),
            (
                "inference_router_completed_total",
                &self.completed_total,
                "Requests that completed and were metered.",
            ),
            (
                "inference_router_rejected_429_total",
                &self.rejected_429_total,
                "Requests rejected with 429 due to replica saturation.",
            ),
            (
                "inference_router_cancelled_total",
                &self.cancelled_total,
                "Requests cancelled (NATS or disconnect).",
            ),
            (
                "inference_router_upstream_error_total",
                &self.upstream_error_total,
                "Requests that failed talking to an upstream replica.",
            ),
        ];
        for (name, counter, help) in counters {
            let _ = writeln!(out, "# HELP {name} {help}");
            let _ = writeln!(out, "# TYPE {name} counter");
            let _ = writeln!(out, "{name} {}", counter.load(Ordering::Relaxed));
        }

        let _ = writeln!(
            out,
            "# HELP inference_router_replica_inflight In-flight requests per replica."
        );
        let _ = writeln!(out, "# TYPE inference_router_replica_inflight gauge");
        let _ = writeln!(
            out,
            "# HELP inference_router_replica_capacity Admission capacity (--max-num-seqs) per replica."
        );
        let _ = writeln!(out, "# TYPE inference_router_replica_capacity gauge");
        for r in replicas {
            let zone = r.residency_zone.as_deref().unwrap_or("");
            let _ = writeln!(
                out,
                "inference_router_replica_inflight{{replica=\"{}\",zone=\"{}\",live=\"{}\"}} {}",
                r.id, zone, r.live, r.in_flight
            );
            let _ = writeln!(
                out,
                "inference_router_replica_capacity{{replica=\"{}\",zone=\"{}\"}} {}",
                r.id, zone, r.capacity
            );
        }

        // Per-model demand series (P4-L2). `model_inflight` = sum of in-flight
        // across a model's LIVE replicas (the scale-DOWN signal: 0 ⇒ idle ⇒ a
        // `scale_to_zero` policy may drop to 0). `model_starved_total` = the
        // scale-FROM-zero signal (a request found no live/un-saturated replica).
        // The autoscaler reads inflight as a level + the starved counter's delta.
        let mut inflight_by_model: HashMap<&str, usize> = HashMap::new();
        for r in replicas {
            if !r.live {
                continue;
            }
            for m in &r.model_ids {
                *inflight_by_model.entry(m.as_str()).or_insert(0) += r.in_flight;
            }
        }
        // Ensure every starved model also appears with an inflight series (0 when
        // scaled to zero) so the scraper sees a stable model set.
        let starved = self
            .model_starved
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        for model in starved.keys() {
            inflight_by_model.entry(model.as_str()).or_insert(0);
        }

        let _ = writeln!(
            out,
            "# HELP inference_router_model_inflight In-flight requests summed across a model's live replicas."
        );
        let _ = writeln!(out, "# TYPE inference_router_model_inflight gauge");
        for (model, n) in &inflight_by_model {
            let _ = writeln!(
                out,
                "inference_router_model_inflight{{model=\"{model}\"}} {n}"
            );
        }

        let _ = writeln!(
            out,
            "# HELP inference_router_model_starved_total Requests that found no live/un-saturated replica for a model (scale-from-zero signal)."
        );
        let _ = writeln!(out, "# TYPE inference_router_model_starved_total counter");
        for (model, n) in &starved {
            let _ = writeln!(
                out,
                "inference_router_model_starved_total{{model=\"{model}\"}} {n}"
            );
        }

        // Per-model throughput + latency (the durable over-time series; Grafana
        // dashboards these, the autoscaler ignores them). Sorted for a stable,
        // diffable exposition.
        let models = self.models.lock().map(|m| m.clone()).unwrap_or_default();
        let mut ordered: Vec<(&String, &ModelMeter)> = models.iter().collect();
        ordered.sort_by(|a, b| a.0.cmp(b.0));

        let _ = writeln!(
            out,
            "# HELP inference_router_model_requests_total Terminal requests per model by status."
        );
        let _ = writeln!(out, "# TYPE inference_router_model_requests_total counter");
        for (model, m) in &ordered {
            for (status, n) in [
                ("completed", m.completed),
                ("unmetered", m.unmetered),
                ("cancelled", m.cancelled),
                ("upstream_error", m.upstream_error),
            ] {
                let _ = writeln!(
                    out,
                    "inference_router_model_requests_total{{model=\"{model}\",status=\"{status}\"}} {n}"
                );
            }
        }

        let _ = writeln!(
            out,
            "# HELP inference_router_model_prompt_tokens_total Prompt (input) tokens per model."
        );
        let _ = writeln!(
            out,
            "# TYPE inference_router_model_prompt_tokens_total counter"
        );
        for (model, m) in &ordered {
            let _ = writeln!(
                out,
                "inference_router_model_prompt_tokens_total{{model=\"{model}\"}} {}",
                m.prompt_tokens
            );
        }

        let _ = writeln!(
            out,
            "# HELP inference_router_model_completion_tokens_total Completion (output) tokens per model."
        );
        let _ = writeln!(
            out,
            "# TYPE inference_router_model_completion_tokens_total counter"
        );
        for (model, m) in &ordered {
            let _ = writeln!(
                out,
                "inference_router_model_completion_tokens_total{{model=\"{model}\"}} {}",
                m.completion_tokens
            );
        }

        let _ = writeln!(
            out,
            "# HELP inference_router_request_duration_seconds Request wall-clock duration per model (all terminals)."
        );
        let _ = writeln!(
            out,
            "# TYPE inference_router_request_duration_seconds histogram"
        );
        for (model, m) in &ordered {
            let mut cumulative = 0u64;
            for (i, bound) in LATENCY_BUCKETS_SECS.iter().enumerate() {
                cumulative += m.bucket_counts[i];
                let _ = writeln!(
                    out,
                    "inference_router_request_duration_seconds_bucket{{model=\"{model}\",le=\"{bound}\"}} {cumulative}"
                );
            }
            let _ = writeln!(
                out,
                "inference_router_request_duration_seconds_bucket{{model=\"{model}\",le=\"+Inf\"}} {}",
                m.latency_count
            );
            let _ = writeln!(
                out,
                "inference_router_request_duration_seconds_sum{{model=\"{model}\"}} {}",
                m.latency_sum
            );
            let _ = writeln!(
                out,
                "inference_router_request_duration_seconds_count{{model=\"{model}\"}} {}",
                m.latency_count
            );
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counters_and_replica_gauges() {
        let m = Metrics::default();
        Metrics::inc(&m.requests_total);
        Metrics::inc(&m.rejected_429_total);
        let replicas = vec![ReplicaStat {
            id: "replica-0".into(),
            residency_zone: Some("eu-west".into()),
            model_ids: vec!["m1".into()],
            capacity: 4,
            in_flight: 1,
            live: true,
        }];
        let text = m.render(&replicas);
        assert!(text.contains("inference_router_requests_total 1"));
        assert!(text.contains("inference_router_rejected_429_total 1"));
        assert!(text.contains("inference_router_replica_inflight{replica=\"replica-0\",zone=\"eu-west\",live=\"true\"} 1"));
        assert!(text.contains(
            "inference_router_replica_capacity{replica=\"replica-0\",zone=\"eu-west\"} 4"
        ));
    }

    #[test]
    fn renders_per_model_demand_series() {
        let m = Metrics::default();
        // Two starved requests for a scaled-to-zero model (no replicas).
        m.inc_starved("cold-model");
        m.inc_starved("cold-model");
        // A live replica serving a different model with one in-flight.
        let replicas = vec![ReplicaStat {
            id: "replica-0".into(),
            residency_zone: None,
            model_ids: vec!["warm-model".into()],
            capacity: 4,
            in_flight: 2,
            live: true,
        }];
        let text = m.render(&replicas);
        assert!(text.contains("inference_router_model_inflight{model=\"warm-model\"} 2"));
        // The starved (scaled-to-zero) model surfaces an inflight=0 series too.
        assert!(text.contains("inference_router_model_inflight{model=\"cold-model\"} 0"));
        assert!(text.contains("inference_router_model_starved_total{model=\"cold-model\"} 2"));
    }

    #[test]
    fn renders_per_model_throughput_and_latency() {
        let m = Metrics::default();
        // Two completed requests for `chat`: tokens accumulate; latencies land in
        // distinct histogram buckets (0.2s ≤ 0.25 bucket, 3.0s ≤ 5.0 bucket).
        m.observe("chat", "completed", 10, 20, 0.2);
        m.observe("chat", "completed", 5, 15, 3.0);
        // One upstream error (no tokens) and one unmetered completion.
        m.observe("chat", "upstream_error", 0, 0, 0.01);
        m.observe("chat", "unmetered", 7, 0, 0.3);

        let text = m.render(&[]);

        // Requests by status.
        assert!(text.contains(
            "inference_router_model_requests_total{model=\"chat\",status=\"completed\"} 2"
        ));
        assert!(text.contains(
            "inference_router_model_requests_total{model=\"chat\",status=\"upstream_error\"} 1"
        ));
        assert!(text.contains(
            "inference_router_model_requests_total{model=\"chat\",status=\"unmetered\"} 1"
        ));
        assert!(text.contains(
            "inference_router_model_requests_total{model=\"chat\",status=\"cancelled\"} 0"
        ));

        // Tokens in/out.
        assert!(text.contains("inference_router_model_prompt_tokens_total{model=\"chat\"} 22"));
        assert!(text.contains("inference_router_model_completion_tokens_total{model=\"chat\"} 35"));

        // Histogram is CUMULATIVE: the 0.01s + 0.2s observations are ≤ 0.25,
        // the 0.3s adds at 0.5, the 3.0s adds at 5.0; +Inf == total count (4).
        assert!(text.contains(
            "inference_router_request_duration_seconds_bucket{model=\"chat\",le=\"0.25\"} 2"
        ));
        assert!(text.contains(
            "inference_router_request_duration_seconds_bucket{model=\"chat\",le=\"0.5\"} 3"
        ));
        assert!(text.contains(
            "inference_router_request_duration_seconds_bucket{model=\"chat\",le=\"5\"} 4"
        ));
        assert!(text.contains(
            "inference_router_request_duration_seconds_bucket{model=\"chat\",le=\"+Inf\"} 4"
        ));
        assert!(text.contains("inference_router_request_duration_seconds_count{model=\"chat\"} 4"));
    }

    #[test]
    fn latency_above_last_bucket_only_counts_in_inf() {
        let m = Metrics::default();
        // 120s exceeds the 60s top bound → no finite bucket, only +Inf/count.
        m.observe("slow", "completed", 1, 1, 120.0);
        let text = m.render(&[]);
        assert!(text.contains(
            "inference_router_request_duration_seconds_bucket{model=\"slow\",le=\"60\"} 0"
        ));
        assert!(text.contains(
            "inference_router_request_duration_seconds_bucket{model=\"slow\",le=\"+Inf\"} 1"
        ));
        assert!(text.contains("inference_router_request_duration_seconds_count{model=\"slow\"} 1"));
    }
}
