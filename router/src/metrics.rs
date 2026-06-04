//! Minimal Prometheus exposition (doc 11 §5.8 autoscale signal source).
//!
//! Hand-rolled (no `prometheus` crate) — a handful of atomic counters plus
//! per-replica in-flight derived live from the replica table's admission
//! semaphores. The P4-L2 autoscaler scrapes `GET /metrics`; the router only
//! EMITS, never actuates (doc 11 data-plane/control-plane separation).

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::routing::ReplicaStat;

#[derive(Debug, Default)]
pub struct Metrics {
    pub requests_total: AtomicU64,
    pub completed_total: AtomicU64,
    pub rejected_429_total: AtomicU64,
    pub cancelled_total: AtomicU64,
    pub upstream_error_total: AtomicU64,
}

impl Metrics {
    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
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
}
