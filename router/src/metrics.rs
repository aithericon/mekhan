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

use crate::routing::ReplicaStat;

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
}
