//! Boot-time orchestrator: probe → register → spawn heartbeat.
//!
//! Sub-phase 2.2 C7: replaces the orchestration body that used to live in
//! `cloud-layer-pool-ollama/src/main.rs:127`'s `main()` with a reusable
//! library entry point the executor service `main` can call.
//!
//! Behavior — strict-fail-closed:
//!
//! - Read configuration from env (or via [`PoolBootConfig`] explicit
//!   construction in tests).
//! - Probe hardware via [`crate::hardware_probe::probe_hardware`].
//! - Mint platform_admin JWT via [`crate::register::mint_register_jwt`].
//! - POST `/v1/pools/register` and capture `(pool_id, heartbeat_token)`.
//! - Spawn the long-running heartbeat task; return the
//!   [`PoolBootHandle`] with the cancellation token so the caller can
//!   drop the heartbeat on shutdown.
//!
//! Any error in steps 1-4 returns `Err`; the caller (`executor-service`
//! `main`) should propagate to process exit. The fail-closed posture
//! matches the legacy pool-ollama contract: a pool that registered
//! without a bearer token would 401-storm capability-routing's heartbeat
//! handler; better to refuse to start than to silently fail.

use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::hardware_probe::{probe_hardware, HardwareAdvertisement};
use crate::heartbeat::{heartbeat_loop, probe_loaded_models, HeartbeatConfig};
use crate::ollama_subprocess::OllamaSubprocess;
use crate::register::{
    build_register_request, default_kreuzberg_ocr_enabled, default_pool_name,
    default_pool_tenant_id, default_requester_role, engine_caps_for_hardware, mint_register_jwt,
    register_on_boot,
};

/// Explicit configuration for register-on-boot. Constructors:
///
/// - [`PoolBootConfig::from_env`] — production path; reads env vars per
///   A4 § 2.4 (renamed at C7 from the legacy `CLOUD_LAYER_POOL_*` set to
///   `AITHERICON_EXECUTOR_POOL_*`).
/// - direct struct literal — tests + alternative deployment runners.
pub struct PoolBootConfig {
    /// Capability-routing base URL, e.g. `http://127.0.0.1:3101`.
    pub capability_routing_url: String,
    /// HS256 shared secret used to mint the platform_admin JWT. Sourced
    /// from `CLOUD_LAYER_JWT_SECRET` in env — same name as legacy
    /// pool-ollama; the secret is shared across cloud-layer services and
    /// the executor.
    pub jwt_secret: String,
    /// Tenant identity for the platform_admin JWT claim. Default canonical
    /// dev tenant when unset.
    pub tenant_id: Uuid,
    /// Requester role for the JWT. `platform_admin` for register access.
    pub requester_role: String,
    /// Stable per-host pool identity; defaults to `${hostname}-executor`.
    pub pool_name: String,
    /// URL where the executor exposes its `pool_url` surface (e.g. a tiny
    /// healthz listener). Capability-routing records this verbatim for
    /// operator inspection. The executor's per-job dispatch flows via
    /// NATS, NOT via this URL.
    pub pool_url: String,
    /// Override for [`probe_hardware`]'s `force` parameter. Read from
    /// `AITHERICON_FORCE_HARDWARE` at env-load time.
    pub force_hardware: Option<String>,
    /// Phase-1a OCR-framing gate (env-driven via
    /// `AITHERICON_EXECUTOR_KREUZBERG_ENABLED`). When `true`, the
    /// executor's register payload AND every heartbeat tick advertise a
    /// `services.kreuzberg = { healthy: true }` block — cap-routing's
    /// resolver then grants `Capability::Ocr` on this pool's row. When
    /// `false` (the default), the wire shape stays byte-identical to
    /// the pre-OCR baseline.
    ///
    /// Single source of truth: both `register_as_pool`'s call to
    /// [`build_register_request`] AND the spawned [`HeartbeatConfig`]
    /// derive their kreuzberg flag from this one field — they MUST
    /// stay aligned (see [`crate::heartbeat::HeartbeatConfig::kreuzberg_enabled`]
    /// doc on why heartbeat parity matters).
    pub kreuzberg_enabled: bool,
}

impl PoolBootConfig {
    /// Read configuration from env. Returns `Ok(Some(_))` when the
    /// registration is enabled (default); `Ok(None)` when explicitly
    /// disabled via `AITHERICON_EXECUTOR_REGISTER_AS_POOL=false`; `Err`
    /// when required env is missing.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let enabled = std::env::var("AITHERICON_EXECUTOR_REGISTER_AS_POOL")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);
        if !enabled {
            return Ok(None);
        }

        let capability_routing_url = std::env::var("CLOUD_LAYER_CAPABILITY_ROUTING_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:3101".to_string());
        let jwt_secret = std::env::var("CLOUD_LAYER_JWT_SECRET").map_err(|_| {
            anyhow::anyhow!(
                "CLOUD_LAYER_JWT_SECRET not set — required to mint platform_admin JWT for /v1/pools/register"
            )
        })?;
        let tenant_id_str = default_pool_tenant_id();
        let tenant_id = Uuid::parse_str(&tenant_id_str).map_err(|e| {
            anyhow::anyhow!("AITHERICON_EXECUTOR_POOL_TENANT_ID is not a valid UUID: {e}")
        })?;
        let requester_role = default_requester_role();
        let pool_name = default_pool_name();
        let pool_port: u16 = std::env::var("AITHERICON_EXECUTOR_POOL_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3301);
        let pool_url = std::env::var("AITHERICON_EXECUTOR_POOL_URL")
            .unwrap_or_else(|_| format!("http://127.0.0.1:{pool_port}"));
        let force_hardware = std::env::var("AITHERICON_FORCE_HARDWARE").ok();
        let kreuzberg_enabled = default_kreuzberg_ocr_enabled();

        Ok(Some(Self {
            capability_routing_url,
            jwt_secret,
            tenant_id,
            requester_role,
            pool_name,
            pool_url,
            force_hardware,
            kreuzberg_enabled,
        }))
    }
}

/// Returned by [`register_as_pool`]: holds the heartbeat task's join
/// handle and the cancellation token. The caller cancels + awaits at
/// shutdown.
pub struct PoolBootHandle {
    pub pool_id: Uuid,
    pub cancel: CancellationToken,
    pub heartbeat_task: JoinHandle<()>,
    pub hardware: HardwareAdvertisement,
}

/// Probe + register + spawn heartbeat. Returns the boot handle on success.
///
/// Fail-closed: any error short-circuits the boot and propagates to the
/// caller; the executor service should exit non-zero. The legacy pool-
/// ollama bin took the same posture.
pub async fn register_as_pool(
    config: &PoolBootConfig,
    ollama: Arc<OllamaSubprocess>,
) -> anyhow::Result<PoolBootHandle> {
    // 1. Probe hardware.
    let hardware = probe_hardware(config.force_hardware.as_deref());
    tracing::info!(?hardware, pool_name = %config.pool_name, "Hardware probed for register-as-pool");

    // 2. Derive engine capabilities + initial loaded models.
    let engine_capabilities = engine_caps_for_hardware(&hardware);
    let initial_loaded_models = probe_loaded_models(ollama.as_ref()).await;
    let ollama_url = ollama.base_url();

    // 3. Mint platform_admin JWT.
    let jwt = mint_register_jwt(&config.jwt_secret, config.tenant_id, &config.requester_role)?;

    // 4. POST /v1/pools/register. Fail-closed on any error.
    let request = build_register_request(
        config.pool_name.clone(),
        config.pool_url.clone(),
        &hardware,
        &engine_capabilities,
        ollama_url,
        initial_loaded_models,
        config.kreuzberg_enabled,
    );
    tracing::info!(
        pool_name = %config.pool_name,
        tenant_id = %config.tenant_id,
        capability_routing = %config.capability_routing_url,
        "Registering executor pool with capability-routing"
    );
    let registered = register_on_boot(&config.capability_routing_url, &jwt, &request)
        .await
        .map_err(|e| anyhow::anyhow!("executor pool registration failed (fail-closed): {e:#}"))?;
    let pool_id = registered.pool_id;
    let heartbeat_token = registered.heartbeat_token;
    tracing::info!(
        pool_id = %pool_id,
        pool_name = %config.pool_name,
        "Executor pool registered; received bearer token (length={})",
        heartbeat_token.len()
    );

    // 5. Spawn heartbeat task with a cancellation token under the caller's
    // control.
    let cancel = CancellationToken::new();
    let hb_config = HeartbeatConfig {
        capability_routing_url: config.capability_routing_url.clone(),
        pool_id,
        pool_url: config.pool_url.clone(),
        hardware: hardware.clone(),
        engine_capabilities,
        heartbeat_token,
        kreuzberg_enabled: config.kreuzberg_enabled,
    };
    let cancel_hb = cancel.clone();
    let ollama_hb = Arc::clone(&ollama);
    let heartbeat_task =
        tokio::spawn(async move { heartbeat_loop(cancel_hb, hb_config, ollama_hb).await });

    Ok(PoolBootHandle {
        pool_id,
        cancel,
        heartbeat_task,
        hardware,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Module-local serialisation for tests that mutate process env. Both
    /// `from_env_short_circuits_when_disabled` and
    /// `from_env_round_trips_kreuzberg_enabled_flag` touch the same env
    /// vars (`AITHERICON_EXECUTOR_REGISTER_AS_POOL`, `CLOUD_LAYER_JWT_SECRET`,
    /// `AITHERICON_EXECUTOR_KREUZBERG_ENABLED`). cargo test runs tests in
    /// parallel by default, so without a lock the set/restore windows of
    /// one test land inside the other and both flake. Holding this lock
    /// for the entire test body makes them effectively sequential — no
    /// new dep needed.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// `AITHERICON_EXECUTOR_REGISTER_AS_POOL=false` short-circuits boot —
    /// the executor service can opt out (useful for compute-agent-style
    /// deployments that should NOT register as an inference pool).
    #[test]
    fn from_env_short_circuits_when_disabled() {
        // Poison-safe: if another env-mutating test panicked while
        // holding the lock, recover the inner () and continue.
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let prior = std::env::var("AITHERICON_EXECUTOR_REGISTER_AS_POOL").ok();
        std::env::set_var("AITHERICON_EXECUTOR_REGISTER_AS_POOL", "false");
        let cfg = PoolBootConfig::from_env().expect("env read succeeds");
        assert!(
            cfg.is_none(),
            "register-as-pool disabled when env=false → Ok(None)"
        );
        match prior {
            Some(p) => std::env::set_var("AITHERICON_EXECUTOR_REGISTER_AS_POOL", p),
            None => std::env::remove_var("AITHERICON_EXECUTOR_REGISTER_AS_POOL"),
        }
    }

    /// `AITHERICON_EXECUTOR_KREUZBERG_ENABLED` round-trips through
    /// `PoolBootConfig::from_env` to the `kreuzberg_enabled` field, so
    /// `register_as_pool` then propagates that value into BOTH the
    /// `build_register_request` call AND the spawned `HeartbeatConfig`.
    /// Both env-paths (set + unset) asserted in one body to keep the
    /// `ENV_LOCK` window contiguous.
    #[test]
    fn from_env_round_trips_kreuzberg_enabled_flag() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Capture + restore the full env touched by this test so it stays
        // hermetic against the rest of the suite.
        let prior_kreuzberg = std::env::var("AITHERICON_EXECUTOR_KREUZBERG_ENABLED").ok();
        let prior_register = std::env::var("AITHERICON_EXECUTOR_REGISTER_AS_POOL").ok();
        let prior_secret = std::env::var("CLOUD_LAYER_JWT_SECRET").ok();

        // Ensure required env is satisfied so from_env can return Ok(Some).
        std::env::set_var("CLOUD_LAYER_JWT_SECRET", "test-secret-kreuzberg-round-trip");
        std::env::remove_var("AITHERICON_EXECUTOR_REGISTER_AS_POOL");

        // 1. enabled path.
        std::env::set_var("AITHERICON_EXECUTOR_KREUZBERG_ENABLED", "true");
        let cfg_on = PoolBootConfig::from_env()
            .expect("env read succeeds")
            .expect("register-as-pool not disabled");
        assert!(
            cfg_on.kreuzberg_enabled,
            "kreuzberg_enabled=true round-trips from env=true"
        );

        // 2. disabled (unset) path — the production default.
        std::env::remove_var("AITHERICON_EXECUTOR_KREUZBERG_ENABLED");
        let cfg_off = PoolBootConfig::from_env()
            .expect("env read succeeds")
            .expect("register-as-pool not disabled");
        assert!(
            !cfg_off.kreuzberg_enabled,
            "kreuzberg_enabled defaults false when env unset"
        );

        // Restore prior env (cross-test isolation).
        match prior_kreuzberg {
            Some(p) => std::env::set_var("AITHERICON_EXECUTOR_KREUZBERG_ENABLED", p),
            None => std::env::remove_var("AITHERICON_EXECUTOR_KREUZBERG_ENABLED"),
        }
        match prior_register {
            Some(p) => std::env::set_var("AITHERICON_EXECUTOR_REGISTER_AS_POOL", p),
            None => std::env::remove_var("AITHERICON_EXECUTOR_REGISTER_AS_POOL"),
        }
        match prior_secret {
            Some(p) => std::env::set_var("CLOUD_LAYER_JWT_SECRET", p),
            None => std::env::remove_var("CLOUD_LAYER_JWT_SECRET"),
        }
    }
}
