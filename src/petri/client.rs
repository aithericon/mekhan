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
    pub async fn deploy_scenario(
        &self,
        net_id: &str,
        air_json: &Value,
    ) -> Result<(), PetriError> {
        let url = format!("{}/api/nets/{}/scenario", self.base_url, net_id);
        let resp = self.client.post(&url).json(air_json).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(PetriError::Response { status, body });
        }
        Ok(())
    }

    /// Set the run mode of a net (e.g., "running", "paused").
    pub async fn set_run_mode(&self, net_id: &str, mode: &str) -> Result<(), PetriError> {
        let url = format!("{}/api/nets/{}/run-mode", self.base_url, net_id);
        let resp = self
            .client
            .put(&url)
            .json(&serde_json::json!({"mode": mode}))
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
    pub async fn get_state(&self, net_id: &str) -> Result<Value, PetriError> {
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
    pub async fn get_topology(&self, net_id: &str) -> Result<Value, PetriError> {
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

    /// Terminate a running net: pause then delete.
    /// Per Section 11.8: no direct terminate endpoint yet,
    /// so we set run-mode to paused, then delete.
    pub async fn terminate_net(&self, net_id: &str) -> Result<(), PetriError> {
        // Best-effort pause; if the net is already completed/hibernated, this may 404
        let _ = self.set_run_mode(net_id, "paused").await;
        self.delete_net(net_id).await
    }

    /// Try to get engine state. Returns None if the engine doesn't have the net
    /// loaded (404, timeout, connection refused, etc.)
    pub async fn try_get_state(&self, net_id: &str) -> Option<Value> {
        self.get_state(net_id).await.ok()
    }

    /// Try to get run mode. Returns None on any error.
    pub async fn try_get_run_mode(&self, net_id: &str) -> Option<String> {
        let url = format!("{}/api/nets/{}/run-mode", self.base_url, net_id);
        let resp = self.client.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().await.ok()?;
        body.get("current_mode")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Get the SSE event stream URL for a net (caller connects directly).
    pub fn events_stream_url(&self, net_id: &str) -> String {
        format!("{}/api/nets/{}/events/stream", self.base_url, net_id)
    }
}
