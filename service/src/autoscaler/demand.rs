//! L2 demand source (model-pool P4-L2) — reactive autoscaling off the Router
//! `/metrics`.
//!
//! The autoscaler scrapes the inference router's Prometheus exposition for two
//! per-model series (emitted by `router/src/metrics.rs`):
//!
//! - `inference_router_model_inflight{model="X"}` — a GAUGE: in-flight requests
//!   summed across X's live replicas. The scale-DOWN level (0 ⇒ idle).
//! - `inference_router_model_starved_total{model="X"}` — a COUNTER: requests that
//!   found no live / un-saturated replica. The scale-FROM-zero signal — when a
//!   `scale_to_zero` policy has dropped X to 0 replicas, `inflight` is 0, so the
//!   counter's DELTA between scrapes is the only evidence of fresh demand.
//!
//! Per-tick demand = `inflight + (starved - starved_at_last_scrape)`. `> 0` lifts
//! a `scale_to_zero` policy off zero (or floors `keep_warm`); a sustained `0`
//! (no in-flight, no new starves) lets it scale back down after the policy
//! cooldown. The router only EMITS — this is the control-plane consumer (doc 11
//! data-plane / control-plane separation; the router never actuates).
//!
//! Constructed from `AUTOSCALER_DEMAND_URL` (the router base, e.g.
//! `http://inference-router:8080`); absent ⇒ the loop runs L1 manual mode with
//! `demand = None`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

/// A per-model demand signal source. The loop calls [`DemandSource::demand_for`]
/// each tick for each policy in a reactive mode; `None` means "no signal" (the
/// reactive modes then make no decision — see
/// [`crate::models::model_replicas::compute_target`]).
#[async_trait]
pub trait DemandSource: Send + Sync {
    /// The current demand for `model_id`, or `None` if unavailable (scrape failed).
    async fn demand_for(&self, model_id: &str) -> Option<f64>;
}

/// Scrapes the Router `/metrics` Prometheus endpoint for per-model demand.
pub struct PrometheusDemandSource {
    /// Router base URL (the loop GETs `{base}/metrics`).
    base_url: String,
    http: reqwest::Client,
    /// Previous scrape's `model_starved_total` per model, for the delta. Persists
    /// across ticks (the source is constructed once, shared via `Arc`).
    prev_starved: Mutex<HashMap<String, u64>>,
}

impl PrometheusDemandSource {
    /// Build a demand source over a router base URL (no trailing `/metrics`).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            prev_starved: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl DemandSource for PrometheusDemandSource {
    async fn demand_for(&self, model_id: &str) -> Option<f64> {
        let url = format!("{}/metrics", self.base_url);
        let body = match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => resp.text().await.ok()?,
            Ok(resp) => {
                tracing::debug!(%url, status = %resp.status(), "router /metrics non-200");
                return None;
            }
            Err(e) => {
                tracing::debug!(%url, "router /metrics scrape failed: {e}");
                return None;
            }
        };

        let inflight =
            parse_model_metric(&body, "inference_router_model_inflight", model_id).unwrap_or(0);
        let starved = parse_model_metric(&body, "inference_router_model_starved_total", model_id)
            .unwrap_or(0);

        // Counter delta since the last scrape (handles a router restart: a smaller
        // value than `prev` ⇒ treat as 0 new, not a negative).
        let new_starves = {
            let mut prev = self.prev_starved.lock().expect("prev_starved poisoned");
            let was = prev.insert(model_id.to_string(), starved).unwrap_or(0);
            starved.saturating_sub(was)
        };

        Some(inflight as f64 + new_starves as f64)
    }
}

/// Parse a per-model Prometheus sample line of the form
/// `metric{model="<id>",...} <value>`. Returns the value for the matching model,
/// or `None` if absent. Tolerant of extra labels; ignores `# HELP`/`# TYPE`.
fn parse_model_metric(body: &str, metric: &str, model_id: &str) -> Option<u64> {
    let model_label = format!("model=\"{model_id}\"");
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.starts_with(metric) {
            continue;
        }
        // The metric token must end at `{` or whitespace so `..._inflight` doesn't
        // match `..._inflight_total` etc.
        let after = &line[metric.len()..];
        if !after.starts_with('{') && !after.starts_with(' ') {
            continue;
        }
        if !line.contains(&model_label) {
            continue;
        }
        if let Some(value) = line.rsplit(' ').next() {
            if let Ok(n) = value.trim().parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# HELP inference_router_model_inflight In-flight per model.
# TYPE inference_router_model_inflight gauge
inference_router_model_inflight{model=\"warm\"} 3
inference_router_model_inflight{model=\"cold\"} 0
# TYPE inference_router_model_starved_total counter
inference_router_model_starved_total{model=\"cold\"} 5
";

    #[test]
    fn parses_per_model_values() {
        assert_eq!(
            parse_model_metric(SAMPLE, "inference_router_model_inflight", "warm"),
            Some(3)
        );
        assert_eq!(
            parse_model_metric(SAMPLE, "inference_router_model_inflight", "cold"),
            Some(0)
        );
        assert_eq!(
            parse_model_metric(SAMPLE, "inference_router_model_starved_total", "cold"),
            Some(5)
        );
        // Absent model / absent series → None.
        assert_eq!(
            parse_model_metric(SAMPLE, "inference_router_model_inflight", "ghost"),
            None
        );
        assert_eq!(
            parse_model_metric(SAMPLE, "inference_router_model_starved_total", "warm"),
            None
        );
    }

    #[test]
    fn metric_prefix_is_not_greedy() {
        // `..._inflight` must not match a hypothetical `..._inflight_total`.
        let body = "inference_router_model_inflight_total{model=\"x\"} 9\n";
        assert_eq!(
            parse_model_metric(body, "inference_router_model_inflight", "x"),
            None
        );
    }
}
