//! L2 demand seam (model-pool P4-L2). **Stub only** — L1 constructs the
//! autoscaler with `demand = None` and never calls this.
//!
//! L2 (reactive autoscaling) is HARD-BLOCKED on the Router `/metrics`: the
//! autoscaler scrapes the doc-11 §5.8 demand signal
//! (`queue_depth × avg_tokens_remaining`) per model and feeds it to
//! [`crate::models::model_replicas::compute_target`] so `scale_to_zero` /
//! `keep_warm` policies react to load. This module pins the trait + the
//! Prometheus scraper SHAPE so L2 lands without restructuring the loop; the
//! actual scrape is intentionally unimplemented here.

use async_trait::async_trait;

/// A per-model demand signal source. The loop calls [`DemandSource::demand_for`]
/// each tick for each policy in a reactive mode; `None` means "no signal" (the
/// reactive modes then make no decision — see `compute_target`).
#[async_trait]
pub trait DemandSource: Send + Sync {
    /// The current demand for `model_id` (doc 11 §5.8
    /// `queue_depth × avg_tokens_remaining`), or `None` if unavailable.
    async fn demand_for(&self, model_id: &str) -> Option<f64>;
}

/// Scrapes the Router `/metrics` Prometheus endpoint for per-model demand.
/// Constructed from `AUTOSCALER_DEMAND_URL`. **L2 — not wired in L1.**
pub struct PrometheusDemandSource {
    /// Router `/metrics` base URL (e.g. `http://inference-router:8080`).
    #[allow(dead_code)]
    pub url: String,
}

#[async_trait]
impl DemandSource for PrometheusDemandSource {
    async fn demand_for(&self, _model_id: &str) -> Option<f64> {
        // L2: scrape `self.url`/metrics, parse the per-replica in-flight /
        // queue-depth gauges, fold to `queue_depth × avg_tokens_remaining`.
        // Deliberately unimplemented in the L1 increment.
        None
    }
}
