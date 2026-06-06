//! Heartbeat loop — sends pool state to capability-routing every 5 seconds.
//!
//! Mirrors `aithericon_executor_llm::heartbeat` shape. Re-probes Surya
//! subprocess health on each tick so `services.surya.healthy` accurately
//! reflects live state — cap-routing's heartbeat handler overwrites the
//! row's `services` column on every tick, so the heartbeat MUST always
//! emit a fresh `surya` block (omitting it would wipe the Capability::Ocr
//! advertisement post-register).
//!
//! Failure handling: exponential backoff (1→2→4→8→16→30s cap) when
//! cap-routing is unreachable. The pool does NOT refuse in-flight OCR
//! requests when the heartbeat target is unreachable.

use std::sync::Arc;
use std::time::Duration;

use tokio::time;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::hardware_probe::HardwareAdvertisement;
use crate::surya_subprocess::SuryaSubprocess;

const HEARTBEAT_INTERVAL_SECS: u64 = 5;
const MAX_BACKOFF_SECS: u64 = 30;

/// Captured-at-boot configuration for the long-running heartbeat task.
#[derive(Clone)]
pub struct HeartbeatConfig {
    pub capability_routing_url: String,
    pub pool_id: Uuid,
    pub pool_url: String,
    pub hardware: HardwareAdvertisement,
    pub engine_capabilities: Vec<String>,
    pub heartbeat_token: String,
}

/// Long-running heartbeat task. Spawned by [`crate::pool_boot::register_as_pool`].
pub async fn heartbeat_loop(
    cancel: CancellationToken,
    config: HeartbeatConfig,
    surya: Arc<SuryaSubprocess>,
) {
    let client = reqwest::Client::new();
    let heartbeat_url = format!("{}/v1/compute/heartbeat", config.capability_routing_url);
    let mut backoff_secs: u64 = 1;
    let mut interval = time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(pool_id = %config.pool_id, "Surya heartbeat loop cancelled");
                break;
            }
            _ = interval.tick() => {
                let surya_healthy = surya.health_check().await;
                let payload = build_payload_from_parts(&config, surya_healthy);

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
                                "Surya heartbeat reconnected to capability-routing"
                            );
                        }
                        backoff_secs = 1;
                    }
                    Ok(resp) => {
                        tracing::warn!(
                            pool_id = %config.pool_id,
                            status = %resp.status(),
                            backoff_secs,
                            "Surya heartbeat non-success from capability-routing"
                        );
                        apply_backoff(&mut backoff_secs, &cancel).await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            pool_id = %config.pool_id,
                            error = %e,
                            backoff_secs,
                            "Surya heartbeat failed — capability-routing unreachable"
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

/// Pure payload assembly. Split from the runtime loop so unit tests can
/// assert wire shape without standing up a live Surya subprocess.
///
/// Contract: always emits pool_id, pool_url, hardware, engines,
/// loaded_models (empty for Surya), queue_depth, health (mapped from
/// surya_healthy), services.surya.healthy.
fn build_payload_from_parts(config: &HeartbeatConfig, surya_healthy: bool) -> serde_json::Value {
    let health = if surya_healthy { "Ready" } else { "Degraded" };
    serde_json::json!({
        "pool_id": config.pool_id,
        "pool_url": config.pool_url,
        "hardware": config.hardware,
        "engines": [{
            "kind": "Surya",
            "version": "0.x",
            "capabilities": config.engine_capabilities,
        }],
        "loaded_models": [],
        "queue_depth": 0,
        "health": health,
        "services": {
            "surya": { "healthy": surya_healthy },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_config() -> HeartbeatConfig {
        HeartbeatConfig {
            capability_routing_url: "http://127.0.0.1:3101".to_string(),
            pool_id: Uuid::nil(),
            pool_url: "http://127.0.0.1:3302".to_string(),
            hardware: HardwareAdvertisement::Metal {
                unified_memory_gb: 128,
            },
            engine_capabilities: vec!["OcrLayout".to_string()],
            heartbeat_token: "fixture-token".to_string(),
        }
    }

    #[tokio::test]
    async fn apply_backoff_doubles_with_30s_cap() {
        let cancel = CancellationToken::new();
        cancel.cancel();
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

    #[test]
    fn build_payload_healthy_subprocess_emits_ready_and_surya_healthy_true() {
        let config = fixture_config();
        let payload = build_payload_from_parts(&config, true);
        assert_eq!(payload["health"], "Ready");
        assert_eq!(payload["services"]["surya"]["healthy"], true);
        assert_eq!(payload["engines"][0]["kind"], "Surya");
    }

    #[test]
    fn build_payload_unhealthy_subprocess_emits_degraded_and_surya_healthy_false() {
        let config = fixture_config();
        let payload = build_payload_from_parts(&config, false);
        assert_eq!(payload["health"], "Degraded");
        assert_eq!(payload["services"]["surya"]["healthy"], false);
    }

    #[test]
    fn build_payload_always_emits_surya_services_block() {
        // Honest-absence: unlike executor-llm's heartbeat which omits
        // `services` entirely when kreuzberg_enabled=false, Surya's
        // pool MUST always emit `services.surya` — that's the entire
        // reason this pool exists. The block is always present;
        // `healthy` reflects probed state.
        let config = fixture_config();
        let healthy = build_payload_from_parts(&config, true);
        let unhealthy = build_payload_from_parts(&config, false);
        assert!(healthy["services"]["surya"].is_object());
        assert!(unhealthy["services"]["surya"].is_object());
    }

    #[test]
    fn build_payload_never_advertises_other_service_blocks() {
        // Honest-absence: Surya executor doesn't have ollama / vllm /
        // kreuzberg / ocr_sidecar — those belong to other pool
        // flavours. Surya executor MUST NOT advertise them or
        // cap-routing's resolver could grant unrelated capabilities.
        let config = fixture_config();
        let payload = build_payload_from_parts(&config, true);
        let services = &payload["services"];
        assert!(
            services.get("ollama").is_none(),
            "MUST NOT advertise ollama"
        );
        assert!(services.get("vllm").is_none(), "MUST NOT advertise vllm");
        assert!(
            services.get("kreuzberg").is_none(),
            "MUST NOT advertise kreuzberg"
        );
        assert!(
            services.get("ocr_sidecar").is_none(),
            "MUST NOT advertise legacy ocr_sidecar"
        );
    }
}
