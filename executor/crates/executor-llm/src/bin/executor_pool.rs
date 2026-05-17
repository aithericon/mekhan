//! `aithericon-executor-pool` — dedicated register-as-pool binary.
//!
//! Sub-phase 2.2 C7 canonical replacement for the deleted
//! `cloud-layer-pool-ollama` bin. Spawns a managed Ollama subprocess,
//! probes hardware, registers the executor as a `compute_pool` with
//! capability-routing, runs the 5s heartbeat loop, and exposes a minimal
//! HTTP listener at the configured `pool_url` serving:
//!
//!   - `GET /v1/healthz` — liveness probe
//!   - `POST /v1/inference` — synchronous HTTP inference bridge (sub-phase
//!     2.3b). Cap-routing's engine-side `HttpInferenceHandler` dispatches
//!     inference requests here; the handler wraps `OllamaAdapter` against the
//!     managed subprocess. Lease validation is deferred to a later slice;
//!     any non-empty Bearer is accepted.
//!
//! This binary is intentionally **lightweight** — it does NOT consume
//! NATS, apalis, gRPC, or any of the heavier `executor-service`
//! infrastructure. The split keeps the cloud-layer `just dev` stack
//! simple: capability-routing + model-registry + model-router + this
//! binary, all running natively. Per-job dispatch via NATS arrives in a
//! later wave that wires the full `executor-service`.
//!
//! Lifecycle:
//!
//! 1. Load config from env (see [`aithericon_executor_llm::PoolBootConfig
//!    ::from_env`]).
//! 2. Spawn managed Ollama subprocess on `AITHERICON_EXECUTOR_OLLAMA_PORT`
//!    (default 11436).
//! 3. Spawn the pool_listener axum task on `AITHERICON_EXECUTOR_POOL_PORT`
//!    (default 3301) serving `/v1/healthz` + `POST /v1/inference`.
//! 4. Call [`aithericon_executor_llm::register_as_pool`] — registers with
//!    capability-routing + spawns the heartbeat loop. Fail-closed.
//! 5. Wait on Ctrl+C; cancel the heartbeat + pool_listener; let the
//!    Ollama subprocess outlive the bin per the OllamaSubprocess
//!    `kill_on_drop(false)` contract (operator/systemd decides shutdown).

use std::net::SocketAddr;
use std::sync::Arc;

use aithericon_executor_llm::{
    register_as_pool, spawn_pool_listener, OllamaSubprocess, OllamaSubprocessConfig,
    PoolBootConfig,
};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Honor a local .env if present (parity with legacy pool-ollama).
    let _ = dotenvy_load();

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 1. Config.
    let Some(config) = PoolBootConfig::from_env()? else {
        anyhow::bail!(
            "AITHERICON_EXECUTOR_REGISTER_AS_POOL=false — executor-pool bin requires register-as-pool enabled"
        );
    };
    let ollama_port: u16 = std::env::var("AITHERICON_EXECUTOR_OLLAMA_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(11436);
    let pool_bind: SocketAddr = std::env::var("AITHERICON_EXECUTOR_POOL_BIND")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            let port: u16 = std::env::var("AITHERICON_EXECUTOR_POOL_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3301);
            SocketAddr::from(([0, 0, 0, 0], port))
        });

    // 2. Spawn Ollama subprocess.
    let ollama_config = OllamaSubprocessConfig {
        port: ollama_port,
        binary_path: None,
        readiness_timeout_secs: 30,
    };
    let ollama = OllamaSubprocess::start(&ollama_config)
        .await
        .map_err(|e| anyhow::anyhow!("ollama subprocess start failed: {e}"))?;
    let ollama = Arc::new(ollama);
    tracing::info!(
        port = ollama_port,
        "Managed Ollama subprocess up and serving"
    );

    // Workstream #30 (sub-phase 2.5a): always_hot pre-pull. Pull each
    // model in AITHERICON_EXECUTOR_ALWAYS_HOT_MODELS BEFORE registration
    // so cap-routing's first heartbeat snapshot already shows the model
    // warm. Fail-closed: any pull failure aborts boot.
    let always_hot: Vec<String> = std::env::var("AITHERICON_EXECUTOR_ALWAYS_HOT_MODELS")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if !always_hot.is_empty() {
        tracing::info!(
            count = always_hot.len(),
            models = ?always_hot,
            "Pre-pulling AITHERICON_EXECUTOR_ALWAYS_HOT_MODELS (workstream #30)"
        );
        for model in &always_hot {
            ollama.model_load(model).await.map_err(|e| {
                anyhow::anyhow!(
                    "AITHERICON_EXECUTOR_ALWAYS_HOT_MODELS pre-pull '{}' failed (fail-closed): {}",
                    model,
                    e
                )
            })?;
            tracing::info!(model = %model, "always_hot pull complete");
        }
    }

    // 3. Pool listener (healthz + inference + /v1/models/{load,evict}).
    //    OllamaAdapter is the sole CompletionPort in the pool binary —
    //    Anthropic/OpenAI adapters are not wired here since this executor
    //    manages a local Ollama subprocess.
    let llm_port: Arc<dyn aithericon_executor_llm::CompletionPort> =
        Arc::new(aithericon_executor_llm::adapters::ollama::OllamaAdapter);
    let listener_cancel = CancellationToken::new();
    let _actual_addr = spawn_pool_listener(
        pool_bind,
        listener_cancel.clone(),
        llm_port,
        Arc::clone(&ollama),
    )
    .await?;
    tracing::info!(
        bind = %pool_bind,
        "Pool listener up serving /v1/healthz + POST /v1/inference"
    );

    // 4. Register + heartbeat (fail-closed).
    let boot_handle = register_as_pool(&config, Arc::clone(&ollama))
        .await
        .map_err(|e| anyhow::anyhow!("executor pool register failed (fail-closed): {e:#}"))?;
    tracing::info!(
        pool_id = %boot_handle.pool_id,
        "Executor pool registered + heartbeat loop running"
    );

    // 5. Block on Ctrl+C, then cancel + clean up.
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| anyhow::anyhow!("failed to install Ctrl+C handler: {e}"))?;
    tracing::info!("Shutdown signal received — cancelling heartbeat + pool listener");
    boot_handle.cancel.cancel();
    listener_cancel.cancel();
    let _ = boot_handle.heartbeat_task.await;
    tracing::info!("executor-pool exiting cleanly");
    Ok(())
}

/// Best-effort `.env` load — no error if the file is absent.
fn dotenvy_load() -> Result<(), ()> {
    // We don't take a dotenvy dep here to keep the bin lean; instead we
    // honor the cloud-layer convention that the parent process (e.g.
    // `just dev`) has already exported the env. This stub is a hook for
    // future dev-only enrichment.
    Ok(())
}
