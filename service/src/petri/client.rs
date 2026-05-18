use petri_api_types::{RunMode, SetRunModeRequest, StateResponse, TopologyResponse};
use reqwest::Client;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum PetriError {
    #[error("petri-lab request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("petri-lab returned {status}: {body}")]
    Response { status: u16, body: String },
}

#[derive(Clone)]
pub struct PetriClient {
    client: Client,
    base_url: String,
}

impl PetriClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Deploy a scenario (AIR JSON) to a net.
    ///
    /// Wire shape: `LoadScenarioRequest` envelope `{ "scenario": <air_json> }`
    /// (sub-phase 2.5e-γ.mekhan-S3 cutover; the bare-scenario request shape was
    /// retired with the scaffold envelope cutover on the engine side per
    /// `feedback_no_backward_compat_hedging_in_migration_waves` +
    /// `feedback_delete_superseded_code`). Mekhan-service itself does not drive
    /// ablation, so `skip_mask` + `stage_overrides` are always empty here; the
    /// envelope's serde-skip-if-empty defaults render the wire body as
    /// `{"scenario": <air_json>}` — still the envelope shape, just with no
    /// additive keys.
    pub async fn deploy_scenario(
        &self,
        net_id: &str,
        air_json: &Value,
    ) -> Result<(), PetriError> {
        let url = format!("{}/api/nets/{}/scenario", self.base_url, net_id);
        let envelope = serde_json::json!({ "scenario": air_json });
        let resp = self.client.post(&url).json(&envelope).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(PetriError::Response { status, body });
        }
        Ok(())
    }

    /// Set the run mode of a net.
    pub async fn set_run_mode(
        &self,
        net_id: &str,
        mode: RunMode,
    ) -> Result<(), PetriError> {
        let url = format!("{}/api/nets/{}/run-mode", self.base_url, net_id);
        let resp = self
            .client
            .put(&url)
            .json(&SetRunModeRequest { mode })
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(PetriError::Response { status, body });
        }
        Ok(())
    }

    /// Get the current state (marking + enabled transitions) of a net.
    pub async fn get_state(&self, net_id: &str) -> Result<StateResponse, PetriError> {
        let url = format!("{}/api/nets/{}/state", self.base_url, net_id);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(PetriError::Response { status, body });
        }
        Ok(resp.json().await?)
    }

    /// Get the topology of a net.
    pub async fn get_topology(&self, net_id: &str) -> Result<TopologyResponse, PetriError> {
        let url = format!("{}/api/nets/{}/topology", self.base_url, net_id);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(PetriError::Response { status, body });
        }
        Ok(resp.json().await?)
    }

    /// Remove net from in-memory registry. Idempotent (404 is OK).
    pub async fn delete_net(&self, net_id: &str) -> Result<(), PetriError> {
        let url = format!("{}/api/nets/{}", self.base_url, net_id);
        let resp = self.client.delete(&url).send().await?;
        let status = resp.status().as_u16();
        // 404 is expected if already hibernated — treat as success
        if !resp.status().is_success() && status != 404 {
            let body = resp.text().await.unwrap_or_default();
            return Err(PetriError::Response { status, body });
        }
        Ok(())
    }

    /// Terminate a running net: stop then delete.
    pub async fn terminate_net(&self, net_id: &str) -> Result<(), PetriError> {
        // Best-effort stop; if the net is already completed/hibernated, this may 404
        let _ = self.set_run_mode(net_id, RunMode::Stopped).await;
        self.delete_net(net_id).await
    }

    /// Try to get engine state. Returns None if the engine doesn't have the net
    /// loaded (404, timeout, connection refused, etc.)
    pub async fn try_get_state(&self, net_id: &str) -> Option<StateResponse> {
        self.get_state(net_id).await.ok()
    }

    /// Try to get run mode. Returns None on any error.
    pub async fn try_get_run_mode(&self, net_id: &str) -> Option<RunMode> {
        let state = self.try_get_state(net_id).await?;
        Some(state.run_mode)
    }

    /// Get the SSE event stream URL for a net (caller connects directly).
    pub fn events_stream_url(&self, net_id: &str) -> String {
        format!("{}/api/nets/{}/events/stream", self.base_url, net_id)
    }
}
