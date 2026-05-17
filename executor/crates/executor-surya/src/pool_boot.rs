//! Boot-time orchestrator for the Surya executor pool.
//!
//! Mirrors `aithericon_executor_llm::pool_boot` shape. Wires together:
//!
//! 1. [`crate::surya_subprocess::SuryaSubprocess::start`] — spawn Python
//!    + Surya, wait for readiness.
//! 2. [`crate::plugin::register`] — register the kreuzberg `OcrBackend`
//!    plugin so kreuzberg-driven document-extraction in-process can
//!    route OCR through this pool's Surya subprocess.
//! 3. [`crate::pool_listener::spawn_pool_listener`] — bind axum on
//!    `pool_url` serving `/v1/healthz` + `/v1/ocr/extract`.
//! 4. [`crate::hardware_probe::probe_hardware`] — fingerprint the host.
//! 5. [`crate::register::register_on_boot`] — POST to cap-routing's
//!    `/v1/pools/register` with the captured state.
//! 6. [`crate::heartbeat::heartbeat_loop`] — spawn the 5s heartbeat
//!    task.
//!
//! Fail-closed: any error in steps 1-5 returns `Err`; the caller (bin's
//! `main`) propagates to process exit.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::adapters::surya::SuryaAdapter;
use crate::hardware_probe::{probe_hardware, HardwareAdvertisement};
use crate::heartbeat::{heartbeat_loop, HeartbeatConfig};
use crate::pool_listener::spawn_pool_listener;
use crate::register::{
    build_register_request, default_pool_name, default_pool_tenant_id, default_requester_role,
    engine_caps_for_hardware, mint_register_jwt, register_on_boot,
};
use crate::surya_subprocess::SuryaSubprocess;

/// Captured-at-boot configuration. Constructed via [`PoolBootConfig::from_env`]
/// in production; tests build directly.
pub struct PoolBootConfig {
    /// Capability-routing base URL, e.g. `http://127.0.0.1:3101`.
    pub capability_routing_url: String,
    /// HS256 shared secret used to mint the platform_admin JWT.
    pub jwt_secret: String,
    /// Tenant identity for the JWT claim.
    pub tenant_id: Uuid,
    /// Requester role for the JWT.
    pub requester_role: String,
    /// Stable per-host pool identity; defaults to `${hostname}-executor-surya`.
    pub pool_name: String,
    /// URL of the pool_listener (also the control_url per #74).
    pub pool_url: String,
    /// Bind address for the axum pool_listener.
    pub pool_bind: SocketAddr,
    /// Override for [`probe_hardware`].
    pub force_hardware: Option<String>,
}

impl PoolBootConfig {
    /// Read configuration from env. Returns `Ok(Some(_))` when
    /// registration is enabled; `Ok(None)` when explicitly disabled via
    /// `AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL=false`; `Err` when
    /// required env is missing.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let enabled = std::env::var("AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);
        if !enabled {
            return Ok(None);
        }

        let capability_routing_url = std::env::var("CLOUD_LAYER_CAPABILITY_ROUTING_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:3101".to_string());
        let jwt_secret = std::env::var("CLOUD_LAYER_JWT_SECRET").map_err(|_| {
            anyhow::anyhow!(
                "CLOUD_LAYER_JWT_SECRET not set — required to mint platform_admin JWT"
            )
        })?;
        let tenant_id_str = default_pool_tenant_id();
        let tenant_id = Uuid::parse_str(&tenant_id_str).map_err(|e| {
            anyhow::anyhow!(
                "AITHERICON_EXECUTOR_SURYA_POOL_TENANT_ID is not a valid UUID: {e}"
            )
        })?;
        let requester_role = default_requester_role();
        let pool_name = default_pool_name();
        let pool_port: u16 = std::env::var("AITHERICON_EXECUTOR_SURYA_POOL_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3302);
        let pool_url = std::env::var("AITHERICON_EXECUTOR_SURYA_POOL_URL")
            .unwrap_or_else(|_| format!("http://127.0.0.1:{pool_port}"));
        let pool_bind: SocketAddr = std::env::var("AITHERICON_EXECUTOR_SURYA_POOL_BIND")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], pool_port)));
        let force_hardware = std::env::var("AITHERICON_FORCE_HARDWARE").ok();

        Ok(Some(Self {
            capability_routing_url,
            jwt_secret,
            tenant_id,
            requester_role,
            pool_name,
            pool_url,
            pool_bind,
            force_hardware,
        }))
    }
}

/// Returned by [`register_as_pool`]; the bin's `main()` cancels + awaits
/// on shutdown.
pub struct PoolBootHandle {
    pub pool_id: Uuid,
    pub cancel: CancellationToken,
    pub heartbeat_task: JoinHandle<()>,
    pub listener_cancel: CancellationToken,
    pub hardware: HardwareAdvertisement,
}

/// Probe + listener + plugin-register + register-with-cap-routing +
/// spawn heartbeat. Returns the boot handle on success.
pub async fn register_as_pool(
    config: &PoolBootConfig,
    surya: Arc<SuryaSubprocess>,
    adapter: Arc<SuryaAdapter>,
) -> anyhow::Result<PoolBootHandle> {
    // 1. Hardware probe.
    let hardware = probe_hardware(config.force_hardware.as_deref());
    tracing::info!(?hardware, pool_name = %config.pool_name, "Hardware probed for Surya pool");

    // 2. Engine capabilities + initial health probe.
    let engine_capabilities = engine_caps_for_hardware(&hardware);
    let surya_healthy = surya.health_check().await;
    if !surya_healthy {
        // Fail-closed: a Surya subprocess that isn't healthy at register
        // time is the same shape as a missing pool — better to refuse
        // boot than register an Ocr-advertising pool that 422s every
        // request.
        anyhow::bail!(
            "Surya subprocess not healthy at register time (probe of {} failed); refusing to register pool",
            surya.base_url()
        );
    }

    // 3. Pool listener (axum healthz + /v1/ocr/extract).
    let listener_cancel = CancellationToken::new();
    let _actual = spawn_pool_listener(
        config.pool_bind,
        Arc::clone(&adapter),
        listener_cancel.clone(),
    )
    .await?;
    tracing::info!(bind = %config.pool_bind, "Surya pool_listener up");

    // 4. kreuzberg plugin registration — wires in the in-process
    //    document-extraction path so kreuzberg::extract_file calls with
    //    `OcrConfig::backend = "surya"` route through this pool's Surya
    //    subprocess.
    crate::plugin::register(Arc::clone(&adapter)).map_err(|e| {
        anyhow::anyhow!("kreuzberg plugin registration failed (fail-closed): {e}")
    })?;
    tracing::info!("kreuzberg OcrBackend plugin 'surya' registered");

    // 5. Mint JWT + POST /v1/pools/register. Fail-closed on any error.
    let jwt = mint_register_jwt(&config.jwt_secret, config.tenant_id, &config.requester_role)?;
    let request = build_register_request(
        config.pool_name.clone(),
        config.pool_url.clone(),
        &hardware,
        &engine_capabilities,
        surya_healthy,
    );
    tracing::info!(
        pool_name = %config.pool_name,
        tenant_id = %config.tenant_id,
        capability_routing = %config.capability_routing_url,
        "Registering Surya executor pool with capability-routing"
    );
    let registered = register_on_boot(&config.capability_routing_url, &jwt, &request)
        .await
        .map_err(|e| {
            anyhow::anyhow!("Surya executor pool registration failed (fail-closed): {e:#}")
        })?;
    let pool_id = registered.pool_id;
    let heartbeat_token = registered.heartbeat_token;
    tracing::info!(
        pool_id = %pool_id,
        pool_name = %config.pool_name,
        "Surya executor pool registered; received bearer token (length={})",
        heartbeat_token.len()
    );

    // 6. Spawn heartbeat task.
    let cancel = CancellationToken::new();
    let hb_config = HeartbeatConfig {
        capability_routing_url: config.capability_routing_url.clone(),
        pool_id,
        pool_url: config.pool_url.clone(),
        hardware: hardware.clone(),
        engine_capabilities,
        heartbeat_token,
    };
    let cancel_hb = cancel.clone();
    let surya_hb = Arc::clone(&surya);
    let heartbeat_task =
        tokio::spawn(async move { heartbeat_loop(cancel_hb, hb_config, surya_hb).await });

    Ok(PoolBootHandle {
        pool_id,
        cancel,
        heartbeat_task,
        listener_cancel,
        hardware,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialise env-mutation tests against a module-local lock —
    /// avoids races against parallel tests touching the same env keys.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn from_env_short_circuits_when_disabled() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prior = std::env::var("AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL").ok();
        std::env::set_var("AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL", "false");
        let cfg = PoolBootConfig::from_env().expect("env read succeeds");
        assert!(cfg.is_none(), "register-as-pool disabled when env=false");
        match prior {
            Some(p) => std::env::set_var("AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL", p),
            None => std::env::remove_var("AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL"),
        }
    }

    #[test]
    fn from_env_requires_jwt_secret() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prior_enabled = std::env::var("AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL").ok();
        let prior_secret = std::env::var("CLOUD_LAYER_JWT_SECRET").ok();
        std::env::remove_var("AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL");
        std::env::remove_var("CLOUD_LAYER_JWT_SECRET");

        // Note: PoolBootConfig holds jwt_secret + intentionally does NOT
        // derive Debug to prevent secret-leak in debug output, so
        // `.expect_err` (which requires Debug on the Ok variant) is
        // unavailable here — explicit match instead.
        let err = match PoolBootConfig::from_env() {
            Ok(_) => panic!("missing JWT secret must Err, got Ok"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("CLOUD_LAYER_JWT_SECRET"),
            "err must name the missing env var; got: {msg}"
        );

        match prior_enabled {
            Some(p) => std::env::set_var("AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL", p),
            None => std::env::remove_var("AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL"),
        }
        match prior_secret {
            Some(p) => std::env::set_var("CLOUD_LAYER_JWT_SECRET", p),
            None => std::env::remove_var("CLOUD_LAYER_JWT_SECRET"),
        }
    }

    #[test]
    fn from_env_default_pool_port_is_3302() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prior_secret = std::env::var("CLOUD_LAYER_JWT_SECRET").ok();
        let prior_port = std::env::var("AITHERICON_EXECUTOR_SURYA_POOL_PORT").ok();
        std::env::set_var("CLOUD_LAYER_JWT_SECRET", "test-secret");
        std::env::remove_var("AITHERICON_EXECUTOR_SURYA_POOL_PORT");

        let cfg = PoolBootConfig::from_env()
            .expect("env read")
            .expect("not disabled");
        // Honest-absence: default port MUST NOT collide with
        // executor-llm's 3301.
        assert!(
            cfg.pool_url.ends_with(":3302"),
            "default pool_url must use port 3302, got {}",
            cfg.pool_url
        );
        assert!(
            !cfg.pool_url.ends_with(":3301"),
            "Surya pool port MUST NOT collide with executor-llm's 3301"
        );

        match prior_secret {
            Some(p) => std::env::set_var("CLOUD_LAYER_JWT_SECRET", p),
            None => std::env::remove_var("CLOUD_LAYER_JWT_SECRET"),
        }
        match prior_port {
            Some(p) => std::env::set_var("AITHERICON_EXECUTOR_SURYA_POOL_PORT", p),
            None => std::env::remove_var("AITHERICON_EXECUTOR_SURYA_POOL_PORT"),
        }
    }
}
