//! Live-engine HTTP client for L2 benchmarks.
//!
//! A thin async wrapper over the running `core-engine` HTTP API. It deploys a
//! generated [`ScenarioDefinition`], drives evaluation, wakes a hibernated net,
//! and reads the event count — enough to measure the write-path throughput,
//! concurrent-net contention, and cold-wake rehydration costs that only exist
//! once the real NATS/JetStream eventing stack is in the loop.
//!
//! HTTP-only by design: the engine routes every append through NATS internally,
//! so the wall-clock of a synchronous `evaluate` already includes the
//! append-and-wait round-trip (the "I/O tax"). A direct NATS subscription for
//! per-append latency percentiles is a future refinement.

use std::time::Duration;

use aithericon_sdk::ScenarioDefinition;
use serde::Deserialize;

/// Boxed-error result for the live driver (a bench tool; precise error types
/// are not worth the ceremony).
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Default engine HTTP base URL (overridable via `--engine-url` / `PETRI_ENGINE_URL`).
pub const DEFAULT_ENGINE_URL: &str = "http://localhost:3030";

/// Subset of the engine's `EvaluateResponse` we care about.
#[derive(Debug, Deserialize)]
pub struct EvaluateOutcome {
    #[serde(default)]
    pub steps_executed: usize,
    /// One entry per fired transition; we only need the count.
    #[serde(default)]
    pub transitions_fired: Vec<serde_json::Value>,
    #[serde(default)]
    pub final_state: Option<String>,
}

impl EvaluateOutcome {
    /// Number of transitions fired in this evaluation pass.
    pub fn fired(&self) -> usize {
        self.transitions_fired.len()
    }
}

/// Async HTTP client bound to one engine base URL.
#[derive(Clone)]
pub struct EngineClient {
    http: reqwest::Client,
    base: String,
}

impl EngineClient {
    /// Build a client. `base` is the engine HTTP root (e.g. `http://localhost:3030`).
    pub fn new(base: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()?;
        Ok(Self {
            http,
            base: base.into(),
        })
    }

    /// Poll the flat `/api/state` endpoint until the engine answers (any HTTP
    /// status counts as "up"; a transport error means "not yet"). Returns an
    /// error if it never comes up within `attempts` × 300ms.
    pub async fn wait_ready(&self, attempts: usize) -> Result<()> {
        for _ in 0..attempts {
            if self
                .http
                .get(format!("{}/api/state", self.base))
                .send()
                .await
                .is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Err(format!("engine at {} not reachable", self.base).into())
    }

    /// Deploy a generated net under `net_id` via POST `/api/nets/{id}/scenario`.
    /// The endpoint expects a `LoadScenarioRequest`, i.e. the `ScenarioDefinition`
    /// nested under a `scenario` field (the other fields — skip_mask,
    /// stage_overrides, net_parameters — default server-side). Seeds in the
    /// scenario are created server-side as part of the load.
    pub async fn deploy(&self, net_id: &str, def: &ScenarioDefinition) -> Result<()> {
        let body = serde_json::json!({ "scenario": serde_json::to_value(def)? });
        let resp = self
            .http
            .post(format!("{}/api/nets/{}/scenario", self.base, net_id))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("deploy {net_id} -> HTTP {status}: {text}").into());
        }
        Ok(())
    }

    /// Drive the net to quiescence (or `max_steps`). This is the synchronous
    /// firing path: every transition fired persists an event through NATS, so
    /// the call's wall-clock is the throughput signal.
    pub async fn evaluate(&self, net_id: &str, max_steps: usize) -> Result<EvaluateOutcome> {
        let resp = self
            .http
            .post(format!(
                "{}/api/nets/{}/command/evaluate",
                self.base, net_id
            ))
            .json(&serde_json::json!({ "max_steps": max_steps }))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("evaluate {net_id} -> HTTP {status}: {text}").into());
        }
        Ok(resp.json::<EvaluateOutcome>().await?)
    }

    /// Wake a (possibly hibernated) net, forcing rehydration from the event log.
    /// Returns once the engine has responded.
    pub async fn wake(&self, net_id: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/api/nets/{}/command/wake", self.base, net_id))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("wake {net_id} -> HTTP {status}: {text}").into());
        }
        Ok(())
    }

    /// Read the persisted event count for a net (GET `/api/nets/{id}/events`).
    pub async fn event_count(&self, net_id: &str) -> Result<usize> {
        let resp = self
            .http
            .get(format!("{}/api/nets/{}/events", self.base, net_id))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("events {net_id} -> HTTP {status}").into());
        }
        let v: serde_json::Value = resp.json().await?;
        Ok(v.get("events")
            .and_then(|e| e.as_array())
            .map(|a| a.len())
            .unwrap_or(0))
    }
}
