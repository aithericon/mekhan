//! `aithericon-executor-surya-pool` — dedicated register-as-pool binary
//! for the Surya OCR executor.
//!
//! Parallel to `aithericon-executor-pool` (executor-llm's bin). Spawns
//! the managed Surya Python subprocess, binds the pool_listener axum
//! task, registers as a `compute_pool` row with capability-routing,
//! runs the 5s heartbeat loop, and exposes `/v1/healthz` + `/v1/ocr/extract`
//! at `pool_url`.
//!
//! Lifecycle:
//!
//! 1. Load config from env via `PoolBootConfig::from_env`.
//! 2. Spawn `SuryaSubprocess` (Python + Surya wrapper from the venv
//!    populated by `just surya-venv-setup`).
//! 3. Construct `SuryaAdapter` pointing at the subprocess.
//! 4. Call `register_as_pool` — kicks off pool_listener + plugin
//!    registration + register-with-cap-routing + heartbeat loop.
//!    Fail-closed.
//! 5. Wait on Ctrl+C; cancel heartbeat + listener; let the Surya
//!    subprocess outlive the bin per the `kill_on_drop(false)` contract
//!    (operator/systemd decides Surya shutdown — same pattern as
//!    executor-llm's executor-pool bin for Ollama).

use std::sync::Arc;

use aithericon_executor_surya::adapters::surya::SuryaAdapter;
use aithericon_executor_surya::pool_boot::{register_as_pool, PoolBootConfig};
use aithericon_executor_surya::surya_subprocess::{SuryaSubprocess, SuryaSubprocessConfig};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 1. Config from env.
    let Some(config) = PoolBootConfig::from_env()? else {
        anyhow::bail!(
            "AITHERICON_EXECUTOR_SURYA_REGISTER_AS_POOL=false — executor-surya-pool bin requires register-as-pool enabled"
        );
    };

    // 2. Spawn Surya subprocess.
    let surya_port: u16 = std::env::var("AITHERICON_EXECUTOR_SURYA_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7160);
    let venv_path = std::env::var("AITHERICON_EXECUTOR_SURYA_VENV_PATH")
        .ok()
        .map(std::path::PathBuf::from);
    let python_binary = std::env::var("AITHERICON_EXECUTOR_SURYA_BINARY_PATH")
        .ok()
        .map(std::path::PathBuf::from);
    let device = std::env::var("AITHERICON_EXECUTOR_SURYA_DEVICE").ok();
    let readiness_timeout_secs = std::env::var("AITHERICON_EXECUTOR_SURYA_READINESS_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let surya_config = SuryaSubprocessConfig {
        port: surya_port,
        venv_path,
        python_binary,
        device,
        readiness_timeout_secs,
    };
    let surya = SuryaSubprocess::start(&surya_config)
        .await
        .map_err(|e| anyhow::anyhow!("Surya subprocess start failed: {e}"))?;
    let surya = Arc::new(surya);
    tracing::info!(
        base_url = %surya.base_url(),
        "Managed Surya subprocess up and serving"
    );

    // 3. Construct adapter sharing the subprocess base_url.
    let adapter = Arc::new(SuryaAdapter::new(surya.base_url()));

    // 4. Register + heartbeat (fail-closed).
    let boot_handle = register_as_pool(&config, Arc::clone(&surya), Arc::clone(&adapter)).await?;
    tracing::info!(
        pool_id = %boot_handle.pool_id,
        ?boot_handle.hardware,
        "Surya executor pool registered + heartbeat loop running"
    );

    // 5. Wait on Ctrl+C; clean up.
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| anyhow::anyhow!("failed to install Ctrl+C handler: {e}"))?;
    tracing::info!("Shutdown signal received — cancelling heartbeat + pool listener");
    boot_handle.cancel.cancel();
    boot_handle.listener_cancel.cancel();
    let _ = boot_handle.heartbeat_task.await;
    // Best-effort plugin unregister (idempotent; cleans up in case
    // process is re-launched in the same shell after a clean exit).
    let _ = aithericon_executor_surya::plugin::unregister();
    tracing::info!("aithericon-executor-surya-pool exiting cleanly");
    Ok(())
}
