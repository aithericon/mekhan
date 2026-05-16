//! Heartbeat loop — sends pool state to capability-routing every 5 seconds.
//!
//! Sub-phase 2.2 C7: ports the heartbeat flow from the deleted
//! `cloud-layer/cloud-layer-pool-ollama/src/heartbeat.rs` into the executor.
//! Wire shape is byte-identical with the legacy pool; only the registrant
//! changed (per A4 § 6).
//!
//! Cadence: 5s.
//! Cancellation: via `tokio_util::sync::CancellationToken` — dropped on
//! shutdown.
//! Failure handling: exponential backoff (1→2→4→8→16→30s cap) when
//! capability-routing is unreachable. The executor does NOT refuse new
//! inference requests when the heartbeat target is unreachable — in-flight
//! requests dispatched before the outage must still complete.
//!
//! **queue_depth semantics:** legacy pool-ollama hardcoded `queue_depth: 0`
//! (Ollama-internal queue not exposed). Executor reports its own task-
//! dispatch queue count when wired in a follow-on slice; today it carries
//! `0` so the existing capability-routing load-scoring algorithm sees a
//! valid value (lower queue_depth preferred — see
//! `cloud-layer-capability-routing/src/load_scoring.rs`). Honest-absence
//! flagged in `crate::pool_boot` for the future slice.

use std::sync::Arc;
use std::time::Duration;

use tokio::time;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::hardware_probe::HardwareAdvertisement;
use crate::ollama_subprocess::OllamaSubprocess;

const HEARTBEAT_INTERVAL_SECS: u64 = 5;
const MAX_BACKOFF_SECS: u64 = 30;

/// Configuration captured at boot for the long-running heartbeat task.
/// All fields are read once at register time and never mutated; the loop
/// re-uses the same payload skeleton each tick with only the runtime
/// probes (loaded_models, ollama_version, health) refreshed.
#[derive(Clone)]
pub struct HeartbeatConfig {
    pub capability_routing_url: String,
    pub pool_id: Uuid,
    pub pool_url: String,
    pub hardware: HardwareAdvertisement,
    pub engine_capabilities: Vec<String>,
    pub heartbeat_token: String,
}

/// Long-running heartbeat task. Spawned by `executor-service::main` after
/// [`crate::register::register_on_boot`] returns.
pub async fn heartbeat_loop(
    cancel: CancellationToken,
    config: HeartbeatConfig,
    ollama: Arc<OllamaSubprocess>,
) {
    let client = reqwest::Client::new();
    let heartbeat_url = format!("{}/v1/compute/heartbeat", config.capability_routing_url);
    let mut backoff_secs: u64 = 1;

    let mut interval = time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(pool_id = %config.pool_id, "Heartbeat loop cancelled");
                break;
            }
            _ = interval.tick() => {
                let payload = build_payload(&config, &ollama).await;

                match client
                    .post(&heartbeat_url)
                    .bearer_auth(&config.heartbeat_token)
                    .json(&payload)
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 204 => {
                        if backoff_secs > 1 {
                            tracing::info!(
                                pool_id = %config.pool_id,
                                "Heartbeat reconnected to capability-routing"
                            );
                        }
                        backoff_secs = 1;
                    }
                    Ok(resp) => {
                        tracing::warn!(
                            pool_id = %config.pool_id,
                            status = %resp.status(),
                            backoff_secs,
                            "Heartbeat received non-success from capability-routing"
                        );
                        apply_backoff(&mut backoff_secs, &cancel).await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            pool_id = %config.pool_id,
                            error = %e,
                            backoff_secs,
                            "Heartbeat failed — capability-routing unreachable (executor continues serving)"
                        );
                        apply_backoff(&mut backoff_secs, &cancel).await;
                    }
                }
            }
        }
    }
}

async fn apply_backoff(backoff: &mut u64, cancel: &CancellationToken) {
    let sleep = Duration::from_secs(*backoff);
    tokio::select! {
        _ = cancel.cancelled() => {}
        _ = time::sleep(sleep) => {}
    }
    *backoff = (*backoff * 2).min(MAX_BACKOFF_SECS);
}

async fn build_payload(
    config: &HeartbeatConfig,
    ollama: &OllamaSubprocess,
) -> serde_json::Value {
    let health = if ollama.health_check().await {
        "Ready"
    } else {
        "Degraded"
    };

    // Ask Ollama which models are currently loaded.
    let loaded_models = probe_loaded_models(ollama).await;
    let ollama_version = probe_ollama_version(ollama).await;

    serde_json::json!({
        "pool_id": config.pool_id,
        "pool_url": config.pool_url,
        "hardware": config.hardware,
        "engines": [{
            "kind": "Ollama",
            "version": ollama_version,
            "capabilities": config.engine_capabilities,
        }],
        "loaded_models": loaded_models,
        "queue_depth": 0,
        "health": health,
    })
}

/// Probe Ollama for currently-loaded models. Exposed so the executor's
/// register-on-boot can populate the first cluster-status snapshot with
/// the actual model set (rather than waiting 5s for the first heartbeat).
pub async fn probe_loaded_models(ollama: &OllamaSubprocess) -> Vec<String> {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let url = format!("{}/api/ps", ollama.base_url());
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                json["models"]
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .filter_map(|m| m["name"].as_str().map(String::from))
                    .collect()
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

async fn probe_ollama_version(ollama: &OllamaSubprocess) -> String {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return "unknown".to_string(),
    };
    let url = format!("{}/api/version", ollama.base_url());
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                json["version"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string()
            } else {
                "unknown".to_string()
            }
        }
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply-backoff doubles the value up to the 30s cap. Validates the
    /// backoff contract from A4 § 6 (1→2→4→8→16→30s) — the heartbeat
    /// loop's full HTTP/Ollama integration is covered by the cert script
    /// `bash scripts/e2e_2_1a_close_cert.sh` literally invoked at C7 close
    /// (recipe-as-named binding) rather than by an in-tree mock harness.
    #[tokio::test]
    async fn apply_backoff_doubles_with_30s_cap() {
        let cancel = CancellationToken::new();
        cancel.cancel(); // skip the actual sleep
        let mut b: u64 = 1;
        apply_backoff(&mut b, &cancel).await;
        assert_eq!(b, 2);
        apply_backoff(&mut b, &cancel).await;
        assert_eq!(b, 4);
        apply_backoff(&mut b, &cancel).await;
        assert_eq!(b, 8);
        apply_backoff(&mut b, &cancel).await;
        assert_eq!(b, 16);
        apply_backoff(&mut b, &cancel).await;
        assert_eq!(b, 30, "16 doubles to 32, capped at 30");
        apply_backoff(&mut b, &cancel).await;
        assert_eq!(b, 30, "cap holds");
    }
}
