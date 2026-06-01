use petri_api_types::{
    DispatchOptions, LoadScenarioRequest, RunMode, ScenarioDefinition, SetRunModeRequest,
    StateResponse, TopologyResponse,
};
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
    /// Wire shape: `LoadScenarioRequest` envelope
    /// `{ "scenario": <air_json>, "skip_mask": [...], "stage_overrides": {...} }`
    /// — γ.mekhan cutover; the bare-scenario request shape was retired per
    /// `feedback_no_backward_compat_hedging_in_migration_waves` +
    /// `feedback_delete_superseded_code`.
    ///
    /// `dispatch_options` is the per-run ablation envelope: `skip_mask`
    /// (transition IDs to skip at evaluate-time) and `stage_overrides`
    /// (per-transition JSON merge-patch keyed by transition_id). #126.2
    /// extends this beyond the previous `Vec::new()/HashMap::new()` stub —
    /// trigger-fire callers thread caller-supplied dispatch options from the
    /// `FireTriggerRequest` body. Empty fields serialize-skip per the
    /// envelope's `skip_serializing_if`, so a fire without ablation renders
    /// as `{"scenario": <air_json>}` on the wire byte-identically to the
    /// prior shape.
    pub async fn deploy_scenario(
        &self,
        net_id: &str,
        air_json: &Value,
        dispatch_options: DispatchOptions,
        net_parameters: Option<Value>,
    ) -> Result<(), PetriError> {
        let url = format!("{}/api/nets/{}/scenario", self.base_url, net_id);
        // The engine consumes a typed `ScenarioDefinition`; the launcher's
        // `parameterize_*` step produces opaque JSON. Convert here so the
        // envelope's request body is one strongly-typed shape, not two
        // half-serialized halves.
        let scenario: ScenarioDefinition = serde_json::from_value(air_json.clone()).map_err(
            |e| PetriError::Response {
                status: 0,
                body: format!("parameterized AIR is not a valid ScenarioDefinition: {e}"),
            },
        )?;
        // Tenant propagation D1-A: `net_parameters` rides the same envelope and
        // is stored on the engine's net service via `set_net_parameters`, where
        // the firing path reads `net_parameters.tenant_id` into the pre-dispatch
        // metadata. Serialize-skips when `None`, so a fire without parameters
        // renders byte-identically to the prior wire shape.
        let envelope = LoadScenarioRequest {
            scenario,
            skip_mask: dispatch_options.skip_mask,
            stage_overrides: dispatch_options.stage_overrides,
            net_parameters,
        };
        let resp = self.client.post(&url).json(&envelope).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(PetriError::Response { status, body });
        }
        Ok(())
    }

    /// Set the run mode of a net.
    pub async fn set_run_mode(&self, net_id: &str, mode: RunMode) -> Result<(), PetriError> {
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

    /// List every live cluster client from the engine's multi-cluster
    /// `ClusterRegistry` (docs/16 — `GET /api/clusters`). Returns the raw engine
    /// payload so the mekhan handler can join human names + re-serialize.
    pub async fn list_clusters(&self) -> Result<Value, PetriError> {
        let url = format!("{}/api/clusters", self.base_url);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(PetriError::Response { status, body });
        }
        Ok(resp.json().await?)
    }

    /// Force-reconnect a cluster (`POST /api/clusters/{resource_id}/reconnect`).
    pub async fn reconnect_cluster(&self, resource_id: &str) -> Result<Value, PetriError> {
        let url = format!("{}/api/clusters/{}/reconnect", self.base_url, resource_id);
        let resp = self.client.post(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(PetriError::Response { status, body });
        }
        Ok(resp.json().await?)
    }

    /// Drain a cluster (`POST /api/clusters/{resource_id}/drain`).
    pub async fn drain_cluster(&self, resource_id: &str) -> Result<Value, PetriError> {
        let url = format!("{}/api/clusters/{}/drain", self.base_url, resource_id);
        let resp = self.client.post(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(PetriError::Response { status, body });
        }
        Ok(resp.json().await?)
    }
}
