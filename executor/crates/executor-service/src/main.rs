use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use apalis::prelude::*;
use apalis_nats::{NatsStorage, ProgressHeartbeatLayer};
use tracing::{error, info, warn};

#[cfg(feature = "docker")]
use aithericon_executor_docker::DockerBackend;
use aithericon_executor_domain::ExecutionJob;
#[cfg(feature = "http")]
use aithericon_executor_http::HttpBackend;
#[cfg(feature = "kreuzberg")]
use aithericon_executor_kreuzberg::KreuzbergBackend;
#[cfg(feature = "surya")]
use aithericon_executor_surya::SuryaBackend;
#[cfg(feature = "llm")]
use aithericon_executor_llm::LlmBackend;
use aithericon_executor_logs::{
    CompositeLogSink, FileLogSink, LevelFilterSink, LogSink, LogSinkConfig, NatsLogSink,
};
use aithericon_executor_metrics::{
    CompositeMetricSink, InMemoryMetricSink, LokiMetricSink, MetricSink, MetricSinkConfig,
    NatsMetricSink,
};
use aithericon_executor_process::ProcessBackend;
#[cfg(feature = "python")]
use aithericon_executor_python::cache::{BuildRequest, VenvCache};
#[cfg(feature = "python")]
use aithericon_executor_python::PythonBackend;
#[cfg(feature = "smtp")]
use aithericon_executor_smtp::SmtpBackend;
#[cfg(feature = "postgres")]
use aithericon_executor_postgres::PostgresBackend;
#[cfg(feature = "loki")]
use aithericon_executor_loki::LokiBackend;
#[cfg(feature = "prometheus")]
use aithericon_executor_prometheus::PrometheusBackend;
#[cfg(feature = "opendal")]
use aithericon_executor_storage::OpenDalArtifactStore;
#[cfg(not(feature = "opendal"))]
use aithericon_executor_storage::StorageBackend;
use aithericon_executor_storage::{ArtifactStore, LocalArtifactStore};
use aithericon_executor_worker::{
    drain_signal, handle_execution, spawn_presence_task, spawn_worker_presence_task,
    BackendRegistry, BatchRunner, CancellationRegistry, ChunkRegistry, CompletionTracker,
    DrainConfig, ExecutorConfig, JobExecutor, JobSource, Lifetime, NatsCancelListener,
    NatsChunkListener, NixEnvironmentHook, SidecarLogConfig, StatusReporter,
};
use tokio_util::sync::CancellationToken;

mod register;

/// Connect to NATS, optionally using a credentials file.
///
/// Uses `ConnectOptions` directly (rather than the apalis-nats helpers) so we can
/// tune `ping_interval` and `max_outstanding_pings`. Default async-nats settings
/// (60s ping) are too lax for WAN deployments behind idle-terminating load
/// balancers (Hetzner LB and friends drop TCP connections at ~30-60s of silence),
/// which leaves the executor's `messages()` pull stream silently dead and queued
/// jobs undelivered. See `nats_ping_interval_secs` in `ExecutorConfig`.
async fn connect_nats(
    config: &ExecutorConfig,
) -> Result<async_nats::Client, Box<dyn std::error::Error + Send + Sync>> {
    let ping_interval = config.nats_ping_interval();

    let options = match &config.nats_creds {
        Some(creds_path) => {
            let expanded = shellexpand::tilde(creds_path);
            info!(
                creds = %expanded,
                ping_interval_secs = ping_interval.as_secs(),
                "connecting to NATS with credentials"
            );
            async_nats::ConnectOptions::with_credentials_file(&*expanded).await?
        }
        None => {
            info!(
                ping_interval_secs = ping_interval.as_secs(),
                "connecting to NATS (anonymous)"
            );
            async_nats::ConnectOptions::new()
        }
    };

    Ok(options
        .ping_interval(ping_interval)
        .max_reconnects(None)
        .retry_on_initial_connect()
        .connect(&config.nats_url)
        .await?)
}

/// Build an apalis worker — shared by daemon and manifest modes.
///
/// Macro because the WorkerBuilder chain produces an unnameable tower service
/// type that can't be expressed as a function return type without boxing.
macro_rules! build_worker {
    ($name:expr, $concurrency:expr, $heartbeat_interval:expr, $executor:expr, $storage:expr) => {
        WorkerBuilder::new($name)
            .concurrency($concurrency)
            .data($executor)
            .option_layer(Some(ProgressHeartbeatLayer::new($heartbeat_interval)))
            .backend($storage)
            .build_fn(handle_execution)
    };
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Subcommand dispatch. Kept minimal — the daemon path is the main mode and
    // is selected by absence of any subcommand. Add new modes by name here.
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        #[cfg(feature = "python")]
        Some("warm-venv") => return warm_venv().await,
        Some("register") => return register::register().await,
        Some("refresh-creds") => return register::refresh_creds().await,
        Some("--help") | Some("-h") => {
            println!("usage: aithericon-executor [warm-venv | register | refresh-creds]");
            println!();
            println!("Without arguments, runs as a worker. With `warm-venv`,");
            println!("populates the venv cache from $EXECUTOR_WARM_REQUIREMENTS");
            println!("and exits. See [python] config / EXECUTOR_PYTHON__* env vars");
            println!("for cache_dir and prefer_uv knobs.");
            println!();
            println!("With `register --url <mekhan> --token rt_... --name <n>`,");
            println!("enrolls this executor into a mekhan lab-runner fleet and");
            println!("persists the credential under {{base_dir}}/runner/.");
            println!();
            println!("With `refresh-creds --url <mekhan>`, mints/rotates this");
            println!("runner's scoped NATS creds and writes {{base_dir}}/runner/runner.creds.");
            return Ok(());
        }
        Some(other) if other.starts_with("--") => {
            // Tolerate `--option=value` invocations from harness wrappers.
        }
        Some(other) => {
            return Err(format!("unknown subcommand: {other}").into());
        }
        None => {}
    }

    // Load configuration (defaults → executor.toml → EXECUTOR_* env vars)
    let mut config = ExecutorConfig::load().map_err(|e| {
        error!("configuration error: {e}");
        e
    })?;
    config.normalize();

    // Fail-closed sandbox validation. When the sandbox is enabled we must be on
    // Linux with a runnable `nsjail` on PATH — otherwise we exit non-zero now
    // rather than silently running user code unsandboxed (or failing every job
    // later). Disabled/None → no-op, behavior is exactly as today.
    if config.sandbox.as_ref().map(|s| s.enabled).unwrap_or(false) {
        let sandbox_cfg = config
            .sandbox
            .as_ref()
            .expect("checked enabled above")
            .to_sandbox_config();
        if let Err(e) = sandbox_cfg.validate() {
            error!("sandbox startup validation failed: {e}");
            return Err(e.into());
        }
        info!(
            allow_network = config.sandbox.as_ref().map(|s| s.allow_network).unwrap_or(false),
            "sandbox enabled — process/python jobs run under nsjail"
        );
    }

    info!(
        name = %config.name,
        nats_url = %config.nats_url,
        namespace = %config.namespace,
        base_dir = %config.base_dir,
        source = ?config.source,
        lifetime = ?config.lifetime,
        concurrency = config.concurrency,
        default_timeout_secs = config.default_timeout_secs,
        max_output_bytes = config.max_output_bytes,
        ack_wait_secs = config.ack_wait_secs,
        nats_ping_interval_secs = config.nats_ping_interval_secs,
        heartbeat_interval_secs = config.heartbeat_interval_secs,
        max_deliver = config.max_deliver,
        cleanup_policy = ?config.cleanup_policy,
        max_jobs = ?config.max_jobs,
        min_jobs = ?config.min_jobs,
        idle_timeout_secs = config.idle_timeout_secs,
        fail_fast = config.fail_fast,
        cancel_nats = config.cancel.nats,
        cancel_http = config.cancel.http,
        target_exec_id = ?config.target_exec_id,
        consumer_mode = if config.target_exec_id.is_some() { "PerJob" } else { "Pool" },
        runner_id = ?config.runner_id,
        presence_interval_secs = config.presence_interval_secs,
        "configuration loaded"
    );

    match (&config.source, &config.lifetime) {
        (JobSource::NatsQueue, Lifetime::Daemon) => run_nats_daemon(config).await,
        (JobSource::NatsQueue, Lifetime::RunToCompletion) => run_nats_drain(config).await,
        (JobSource::Manifest, Lifetime::RunToCompletion) => run_manifest(config).await,
        (source, lifetime) => {
            error!(
                ?source,
                ?lifetime,
                "unsupported source/lifetime combination"
            );
            Err(
                format!("unsupported source/lifetime combination: {source:?} + {lifetime:?}")
                    .into(),
            )
        }
    }
}

/// Build the apalis-nats `Config` shared by NATS-queue paths.
///
/// The `consumer_mode` is derived from `target_exec_id` then `runner_id`:
/// - `target_exec_id = Some(id)` → `PerJob { exec_id: id }`: ephemeral consumer
///   with exact filter `{namespace}.{priority}.{id}`. The dispatcher (Slurm
///   sbatch) sets `EXECUTOR_TARGET_EXEC_ID` to the same id the engine published
///   with, so this consumer pulls exactly its dispatched message and exits.
/// - else `runner_id = Some(rid)` → `PartitionedPool { partition: rid }`: a
///   registered lab runner drains the SHARED `runner-jobs` stream filtered to
///   its own partition `runner-jobs.{priority}.{rid}.>` (exclusive routing,
///   one shared stream-set for the whole fleet). `config.namespace` is already
///   `runner-jobs` (set in `ExecutorConfig::normalize`).
/// - else → `Pool`: durable shared consumer (legacy daemon-mode behavior).
fn build_apalis_nats_config(config: &ExecutorConfig) -> apalis_nats::Config {
    let consumer_mode = match (&config.target_exec_id, &config.runner_id) {
        (Some(id), _) => apalis_nats::ConsumerMode::PerJob {
            exec_id: id.clone(),
        },
        (None, Some(runner_id)) => apalis_nats::ConsumerMode::PartitionedPool {
            partition: runner_id.clone(),
        },
        (None, None) => apalis_nats::ConsumerMode::Pool,
    };

    apalis_nats::Config {
        namespace: config.namespace.clone(),
        max_deliver: config.max_deliver,
        ack_wait: config.ack_wait(),
        max_ack_pending: config.max_ack_pending,
        num_replicas: 1,
        enable_dlq: true,
        consumer_mode,
        ..Default::default()
    }
}

/// Run the executor as a long-running daemon pulling jobs from NATS.
async fn run_nats_daemon(
    config: ExecutorConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Connect to NATS
    let nats_client = connect_nats(&config).await?;
    let jetstream = async_nats::jetstream::new(nats_client.clone());

    info!("connected to NATS");

    let reporter = StatusReporter::new_with_prefix(
        jetstream.clone(),
        config.name.clone(),
        config.status_replicas,
        config.subject_prefix.clone(),
    )
    .await?;

    // The worker-pool default (non-enrolled, non-PerJob) fans the daemon out
    // across one `executor.{wire}` namespace per backend it serves (built
    // below, after the executor's backend set is known). The runner-presence
    // (`runner.{id}`) and PerJob/lease (`target_exec_id`) paths keep the
    // historical single-namespace storage built from `config.namespace`.
    let worker_pool_mode = config.runner_id.is_none() && config.target_exec_id.is_none();

    let nats_client_for_cancel = nats_client.clone();

    // Set up cancellation registry and listeners
    let cancel_registry = CancellationRegistry::new();
    let cancel_shutdown = CancellationToken::new();

    if config.cancel.nats {
        NatsCancelListener::start(
            nats_client_for_cancel.clone(),
            cancel_registry.clone(),
            None,
            cancel_shutdown.clone(),
        )
        .await?;
        info!("NATS cancel listener started");
    }

    #[cfg(feature = "http-cancel")]
    if config.cancel.http {
        start_http_cancel(
            &config.cancel,
            cancel_registry.clone(),
            cancel_shutdown.clone(),
        );
    }

    // Set up the inbound live chunk feed (the "live IPC reducer"). Always-on:
    // the per-job opt-in lives on `job.feed_chunks`, not here. The listener
    // drains the ordered+lossless `EXECUTOR_CHUNKS` JetStream into per-job
    // channels; reuses the cancel-listener shutdown token.
    let chunk_registry = ChunkRegistry::new();
    NatsChunkListener::ensure_stream(&jetstream, config.status_replicas).await?;
    NatsChunkListener::start(
        jetstream.clone(),
        chunk_registry.clone(),
        None,
        cancel_shutdown.clone(),
    )
    .await?;
    info!("NATS chunk listener started");

    // Build the JobExecutor. `registered_wires` is the set of backend
    // wire-names that actually registered (feature-gated arms may skip) —
    // exactly the set the worker pool can serve.
    let (executor, registered_wires) = build_executor(
        &config,
        reporter,
        &nats_client_for_cancel,
        cancel_registry,
        chunk_registry,
    )?;
    let executor = Arc::new(executor);

    // Phase 3 (presence-lease pool capacity): a registered runner advertises
    // liveness so mekhan keeps its presence-pool unit alive. Reuses the daemon's
    // already-connected, runner-scoped NATS client (no second connection) and
    // the cancel/chunk-listener shutdown token. No-op for a non-enrolled daemon
    // (no runner identity → no presence task), so behavior is unchanged there.
    //
    // The payload advertises `registered_wires` — the runner's `backends`
    // dimension (set-membership, docs/23 §4), the SAME set the worker-pool path
    // advertises below. This is spawned AFTER `build_executor` so the registered
    // wire set is known. mekhan uses it for fleet visibility + a best-effort
    // publish-time coverage warning on presence-pool steps (it never hard-gates
    // placement — caps remain the authoritative grant guard).
    if let Some(runner_id) = config.runner_id.clone() {
        let backends: Vec<String> = registered_wires.iter().map(|w| w.to_string()).collect();
        spawn_presence_task(
            nats_client_for_cancel.clone(),
            runner_id.clone(),
            backends.clone(),
            config.presence_interval(),
            cancel_shutdown.clone(),
        );
        info!(
            %runner_id,
            ?backends,
            interval_secs = config.presence_interval_secs,
            "runner presence heartbeat started"
        );
    }

    let mut monitor = Monitor::new();

    if worker_pool_mode {
        // Worker-pool fan-out: bind ONE Pool consumer per `executor.{wire}`
        // namespace for every backend this binary actually registered. Each
        // gets its own NatsStorage (auto-creates the namespace's streams) +
        // worker (unique name `{config.name}-{wire}`) sharing the SAME
        // Arc<JobExecutor>. All register on the single Monitor.
        if registered_wires.is_empty() {
            return Err(
                "worker-pool daemon has no ExecutorJob backends compiled in — nothing to drain; \
                 check the executor-service feature set"
                    .into(),
            );
        }

        for wire in &registered_wires {
            let namespace = aithericon_backends::executor_pool_namespace(wire);
            // Pool (competing-consumers) config for this backend's namespace.
            // No target_exec_id here by construction (worker_pool_mode), so the
            // mode is always Pool.
            let nats_config = apalis_nats::Config {
                namespace: namespace.clone(),
                max_deliver: config.max_deliver,
                ack_wait: config.ack_wait(),
                max_ack_pending: config.max_ack_pending,
                num_replicas: 1,
                enable_dlq: true,
                consumer_mode: apalis_nats::ConsumerMode::Pool,
                ..Default::default()
            };

            let storage = NatsStorage::<ExecutionJob>::new_with_config(
                nats_client.clone(),
                nats_config,
            )
            .await?;

            let worker_name = format!("{}-{wire}", config.name);
            let worker = build_worker!(
                &worker_name,
                config.concurrency,
                config.heartbeat_interval(),
                executor.clone(),
                storage
            );
            monitor = monitor.register(worker);
            info!(%namespace, worker = %worker_name, "worker-pool backend consumer bound");
        }

        // Worker presence: advertise the backend set this pool worker drains.
        // `config.name` is the process-stable worker id — it is the operator-
        // facing label for this daemon and is already unique per deployment
        // (k8s pod name / systemd unit / dev slot); reusing it keeps the
        // presence subject stable across restarts of the same worker without
        // minting a fresh uuid each boot (which would leak stale presence
        // rows on the watcher side). The backend set is wire-truth here.
        let backends: Vec<String> = registered_wires.iter().map(|w| w.to_string()).collect();
        spawn_worker_presence_task(
            nats_client_for_cancel.clone(),
            config.name.clone(),
            backends.clone(),
            config.presence_interval(),
            cancel_shutdown.clone(),
        );
        info!(
            worker_id = %config.name,
            ?backends,
            interval_secs = config.presence_interval_secs,
            "worker presence heartbeat started"
        );
    } else {
        // Runner-presence (shared `runner-jobs` stream, partitioned to this
        // runner) or PerJob/lease (`target_exec_id`) path — single-storage +
        // worker. `build_apalis_nats_config` picks the consumer mode/filter.
        let nats_config = build_apalis_nats_config(&config);
        let storage =
            NatsStorage::<ExecutionJob>::new_with_config(nats_client, nats_config).await?;
        info!("apalis NATS storage ready");

        let worker = build_worker!(
            &config.name,
            config.concurrency,
            config.heartbeat_interval(),
            executor,
            storage
        );
        monitor = monitor.register(worker);
    }

    info!(
        concurrency = config.concurrency,
        worker_pool_mode,
        backends = registered_wires.len(),
        "worker(s) built, starting monitor"
    );

    // Run with graceful shutdown on Ctrl+C
    monitor
        .shutdown_timeout(Duration::from_secs(30))
        .run_with_signal(async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for ctrl+c");
            info!("shutdown signal received");
            cancel_shutdown.cancel();
            Ok(())
        })
        .await?;

    info!("executor service stopped");
    Ok(())
}

/// Run the executor in drain mode: pull jobs from NATS, process up to a bounded number, then exit.
///
/// Shutdown triggers:
/// - `completed >= max_jobs` → immediate exit
/// - `completed >= min_jobs` + idle timeout → exit
/// - Ctrl+C → always works as escape hatch
async fn run_nats_drain(
    config: ExecutorConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Connect to NATS
    let nats_client = connect_nats(&config).await?;
    let jetstream = async_nats::jetstream::new(nats_client.clone());

    info!("connected to NATS");

    let reporter = StatusReporter::new_with_prefix(
        jetstream.clone(),
        config.name.clone(),
        config.status_replicas,
        config.subject_prefix.clone(),
    )
    .await?;

    let nats_config = build_apalis_nats_config(&config);

    let nats_client_for_cancel = nats_client.clone();
    let storage = NatsStorage::<ExecutionJob>::new_with_config(nats_client, nats_config).await?;

    // Set up cancellation
    let cancel_registry = CancellationRegistry::new();
    let cancel_shutdown = CancellationToken::new();

    if config.cancel.nats {
        NatsCancelListener::start(
            nats_client_for_cancel.clone(),
            cancel_registry.clone(),
            None,
            cancel_shutdown.clone(),
        )
        .await?;
        info!("NATS cancel listener started");
    }

    // Inbound live chunk feed (see `run_nats_daemon` for the rationale).
    let chunk_registry = ChunkRegistry::new();
    NatsChunkListener::ensure_stream(&jetstream, config.status_replicas).await?;
    NatsChunkListener::start(
        jetstream.clone(),
        chunk_registry.clone(),
        None,
        cancel_shutdown.clone(),
    )
    .await?;
    info!("NATS chunk listener started");

    // Build the executor with a completion tracker
    let tracker = Arc::new(CompletionTracker::new());
    let drain_rx = tracker.subscribe();
    let tracker_for_exit = tracker.clone();

    // Drain mode is PerJob (Slurm/Nomad-dispatched, exact-filter) or a
    // single-namespace pool drain — it does not fan out per backend, so the
    // registered wire set is unused here.
    let (mut executor, _registered_wires) = build_executor(
        &config,
        reporter,
        &nats_client_for_cancel,
        cancel_registry,
        chunk_registry,
    )?;
    executor.completion_tracker = Some(tracker);
    let executor = Arc::new(executor);

    let worker = build_worker!(
        &config.name,
        config.concurrency,
        config.heartbeat_interval(),
        executor,
        storage
    );

    info!(
        concurrency = config.concurrency,
        "drain worker built, starting monitor"
    );

    let drain_config = DrainConfig {
        min_jobs: config.min_jobs,
        max_jobs: config.max_jobs,
        idle_timeout: config.idle_timeout(),
    };

    let target_exec_id = config.target_exec_id.clone();

    Monitor::new()
        .register(worker)
        .shutdown_timeout(Duration::from_secs(30))
        .run_with_signal(async move {
            tokio::select! {
                _ = drain_signal(drain_rx, &drain_config) => {
                    info!("drain condition met, shutting down");
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("shutdown signal received");
                }
            }
            cancel_shutdown.cancel();
            Ok(())
        })
        .await?;

    let completed = tracker_for_exit.completed();
    info!(completed, "executor drain mode stopped");

    // PerJob orphan detection — the executor was launched targeting a specific
    // exec_id (e.g. by Slurm sbatch with EXECUTOR_TARGET_EXEC_ID) but exited
    // without ever processing a job. Exiting 0 here would lie to the scheduler:
    // sacct/squeue would surface a "completed cleanly" result, no failure
    // signal would reach the engine, and the engine's pending_execution token
    // would orphan. Surface the orphan via a non-zero exit code (75 = EX_TEMPFAIL
    // from sysexits.h, indicating an infra-level retry-worthy failure) so the
    // SchedulerWatcher emits a sig_failed that the engine's t_pending_slurm_failed
    // can consume.
    if target_exec_id.is_some() && completed == 0 {
        error!(
            target_exec_id = ?target_exec_id,
            "executor exited without processing its targeted job (PerJob orphan); \
             reporting failure via exit code 75 so the scheduler sees a non-zero exit"
        );
        std::process::exit(75);
    }

    Ok(())
}

/// Run the executor against a manifest file — push jobs through apalis, collect results, exit.
///
/// Jobs go through the same apalis worker pipeline as NATS-sourced jobs, giving them
/// ack timeout protection, progress heartbeats, and the same handler lifecycle.
async fn run_manifest(
    config: ExecutorConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let manifest_path = config
        .manifest_path
        .as_deref()
        .ok_or("manifest source requires EXECUTOR_MANIFEST_PATH to be set")?;

    let manifest = BatchRunner::load_manifest(Path::new(manifest_path))?;

    info!(
        jobs = manifest.jobs.len(),
        manifest = manifest_path,
        "running manifest"
    );

    // Connect to NATS
    let nats_client = connect_nats(&config).await?;
    let jetstream = async_nats::jetstream::new(nats_client.clone());
    let reporter = StatusReporter::new_with_prefix(
        jetstream,
        config.name.clone(),
        config.status_replicas,
        config.subject_prefix.clone(),
    )
    .await?;

    // Manifest-specific storage: no retries, no DLQ — failures go to BatchResult.
    // Manifest mode is its own dispatcher (BatchRunner pushes the jobs locally),
    // so consumer mode is always Pool — there's no remote sbatch to coordinate
    // a PerJob exec_id with.
    let nats_config = apalis_nats::Config {
        namespace: config.namespace.clone(),
        max_deliver: 1,
        ack_wait: config.ack_wait(),
        num_replicas: 1,
        enable_dlq: false,
        consumer_mode: apalis_nats::ConsumerMode::Pool,
        ..Default::default()
    };
    let storage =
        NatsStorage::<ExecutionJob>::new_with_config(nats_client.clone(), nats_config).await?;

    // Build executor — same pipeline as daemon mode. Manifest mode runs local
    // jobs with no live inbound feed, so the chunk registry is present but never
    // populated (no `NatsChunkListener`); reducer jobs aren't a manifest path.
    let cancel_registry = CancellationRegistry::new();
    let chunk_registry = ChunkRegistry::new();
    // Manifest mode is its own single-namespace dispatcher — no per-backend
    // fan-out, so the registered wire set is unused here.
    let (executor, _registered_wires) = build_executor(
        &config,
        reporter.clone(),
        &nats_client,
        cancel_registry,
        chunk_registry,
    )?;
    let executor = Arc::new(executor);

    // Start the apalis worker in the background (same pipeline as daemon)
    let shutdown = CancellationToken::new();
    let shutdown_for_worker = shutdown.clone();

    let worker = build_worker!(
        &config.name,
        1,
        config.heartbeat_interval(),
        executor,
        storage.clone()
    );

    let monitor_handle = tokio::spawn(async move {
        Monitor::new()
            .register(worker)
            .shutdown_timeout(Duration::from_secs(30))
            .run_with_signal(async move {
                shutdown_for_worker.cancelled().await;
                Ok(())
            })
            .await
    });

    info!("manifest worker started, pushing jobs through apalis");

    // Push jobs through the queue and collect results via status stream
    let runner = BatchRunner::new(storage, reporter, config.fail_fast);
    let result = runner.run(&manifest).await;

    // Shutdown the worker
    shutdown.cancel();
    let _ = monitor_handle.await;

    // Print JSON summary to stdout
    println!("{}", serde_json::to_string_pretty(&result)?);

    info!(
        total = result.total,
        succeeded = result.succeeded,
        failed = result.failed,
        "manifest complete"
    );

    if result.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Register one `ExecutorJob` backend on the registry. Match arms are
/// feature-gated so backends whose feature isn't compiled simply log a
/// skip line — keeps `build_executor` working under any feature subset
/// (`docker`-less CI builds, `--no-default-features` reproducer builds,
/// etc.).
///
/// Unknown wire-names are also a warn-and-skip rather than a hard error
/// because `aithericon-backends::BACKENDS` is the source of truth — if a
/// new entry lands there before the executor side gets a match arm, the
/// service test fails first, not this binary's startup.
fn register_executor_backend(
    registry: BackendRegistry,
    meta: &aithericon_backends::BackendMeta,
    config: &ExecutorConfig,
    #[allow(unused_variables)] base_dir: &Path,
) -> BackendRegistry {
    // Enabled-sandbox config, shared by the process + python backends. The
    // fail-closed `validate()` already ran once at startup in `main`, so an
    // enabled sandbox here is known-good (Linux + nsjail on PATH).
    let sandbox_cfg = config
        .sandbox
        .as_ref()
        .filter(|s| s.enabled)
        .map(|s| s.to_sandbox_config());

    match meta.wire_name {
        "process" => {
            info!("process backend registered");
            let mut process =
                ProcessBackend::new().with_max_output_bytes(config.max_output_bytes);
            if let Some(cfg) = &sandbox_cfg {
                process = process.with_sandbox(cfg.clone());
            }
            registry.register(process)
        }
        #[cfg(feature = "python")]
        "python" => {
            let mut python = PythonBackend::new().with_max_output_bytes(config.max_output_bytes);
            if let Some(venv_cache) = build_venv_cache(config, base_dir) {
                python = python.with_venv_cache(venv_cache);
                info!("python backend registered with venv cache");
            } else {
                info!("python backend registered (no venv cache)");
            }
            if let Some(cfg) = &sandbox_cfg {
                python = python.with_sandbox(cfg.clone());
            }
            registry.register(python)
        }
        #[cfg(feature = "docker")]
        "docker" => match DockerBackend::new() {
            Ok(docker) => {
                info!("docker backend registered");
                let mut docker = docker.with_max_output_bytes(config.max_output_bytes);
                // Docker is its own isolator — map the same sandbox intent onto
                // the container's native HostConfig (network/caps/readonly/
                // user/limits) rather than nsjail.
                if let Some(cfg) = &sandbox_cfg {
                    docker = docker.with_sandbox(cfg.clone());
                }
                registry.register(docker)
            }
            Err(e) => {
                warn!("docker backend unavailable: {e}");
                registry
            }
        },
        #[cfg(feature = "http")]
        "http" => {
            info!("http backend registered");
            registry.register(HttpBackend::new())
        }
        #[cfg(feature = "llm")]
        "llm" => {
            info!("llm backend registered");
            registry.register(LlmBackend::new())
        }
        #[cfg(feature = "file-ops")]
        "file_ops" => {
            info!("file_ops backend registered");
            registry.register(
                aithericon_executor_file_ops::FileOpsBackend::new()
                    .with_default_storage(config.storage.clone()),
            )
        }
        #[cfg(feature = "kreuzberg")]
        "kreuzberg" => {
            info!("kreuzberg backend registered");
            registry.register(KreuzbergBackend::new())
        }
        #[cfg(feature = "surya")]
        "surya" => {
            info!("surya backend registered");
            registry.register(SuryaBackend::new())
        }
        #[cfg(feature = "smtp")]
        "smtp" => {
            info!("smtp backend registered");
            registry.register(SmtpBackend::new())
        }
        #[cfg(feature = "postgres")]
        "postgres" => {
            info!("postgres backend registered");
            registry.register(PostgresBackend::new())
        }
        #[cfg(feature = "loki")]
        "loki" => {
            info!("loki backend registered");
            registry.register(LokiBackend::new())
        }
        #[cfg(feature = "prometheus")]
        "prometheus" => {
            info!("prometheus backend registered");
            registry.register(PrometheusBackend::new())
        }
        other => {
            info!(
                "backend '{other}' declared in aithericon-backends but not built into this executor binary — skipping"
            );
            registry
        }
    }
}

/// Build a `JobExecutor` from config — shared by both service and batch modes.
///
/// Backend registration is driven by `aithericon_backends::BACKENDS`. Every
/// entry with `dispatch_mode == ExecutorJob` is dispatched to
/// `register_executor_backend`, whose feature-gated match arms own the
/// `Backend::new(...)` calls. Backends with `dispatch_mode == EngineEffect`
/// (CatalogueQuery today) are skipped — the engine runs them itself.
///
/// Returns the built executor alongside the set of wire-names that *actually*
/// registered. A feature-gated arm may warn-and-skip (its cargo feature isn't
/// compiled, or the backend is unavailable at runtime — e.g. docker), so the
/// "registered" set is a strict subset of the `ExecutorJob` `BACKENDS`. We
/// detect a successful registration by comparing the registry's backend count
/// before vs. after each `register_executor_backend` call. The worker-pool
/// daemon uses this set to bind one `executor.{wire}` Pool consumer per backend
/// it can actually serve, and to advertise its coverage via worker presence.
fn build_executor(
    config: &ExecutorConfig,
    reporter: StatusReporter,
    nats_client: &async_nats::Client,
    cancel_registry: CancellationRegistry,
    chunk_registry: ChunkRegistry,
) -> Result<(JobExecutor, Vec<&'static str>), Box<dyn std::error::Error + Send + Sync>> {
    let base_dir = PathBuf::from(&config.base_dir);

    #[allow(unused_mut)]
    let mut registry = BackendRegistry::new(config.default_timeout());
    let mut registered_wires: Vec<&'static str> = Vec::new();

    for meta in aithericon_backends::BACKENDS {
        if !matches!(
            meta.dispatch_mode,
            aithericon_backends::DispatchMode::ExecutorJob
        ) {
            continue;
        }
        let before = registry.len();
        registry = register_executor_backend(registry, meta, config, &base_dir);
        // A feature-gated arm that skipped (unbuilt feature / unavailable
        // backend) leaves the count unchanged; only count the wire-name when
        // the backend truly landed in the registry.
        if registry.len() > before {
            registered_wires.push(meta.wire_name);
        }
    }

    let registry = Arc::new(registry);

    // Create artifact store from config (or fall back to local)
    let artifact_store = build_artifact_store(config, &base_dir)?;

    // Build metric sink(s) from config
    let metric_sink = build_metric_sink(config, nats_client)?;

    // Build log sink(s) from config
    let log_sink = build_log_sink(config, nats_client, &base_dir)?;

    // Build optional Nix environment hook
    let nix_hook = config.nix.as_ref().filter(|n| n.enabled).map(|n| {
        let cache = n
            .cache_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dir.join("nix-envs"));
        let mut hook = NixEnvironmentHook::new(cache.clone());

        // Discover aithericon SDK path for inclusion in Nix environments
        if let Some(sdk_path) = find_sdk_path() {
            info!(sdk_path = %sdk_path.display(), "nix hook: SDK path discovered");
            hook = hook.with_sdk_path(sdk_path);
        }

        info!(cache_dir = %cache.display(), "nix environment hook enabled");
        hook
    });

    // Build the staging pipeline with default hooks
    let secret_store: Arc<dyn aithericon_secrets::SecretStore> = build_secret_store();
    let vault_addr = std::env::var("VAULT_ADDR").ok().filter(|s| !s.is_empty());
    let pipeline = Arc::new(aithericon_executor_worker::staging::default_pipeline(
        base_dir.clone(),
        artifact_store.clone(),
        Some(secret_store),
        vault_addr,
        nix_hook,
    ));

    // Build sidecar log config from logs config
    let log_config = {
        let logs = config.logs.as_ref();
        SidecarLogConfig {
            max_recent_errors: logs.map_or(50, |l| l.max_recent_errors),
            rate_limit: logs.map_or(100_000, |l| l.rate_limit_max_entries),
            batch_size: logs.map_or(50, |l| l.batch_size),
            batch_flush_interval_ms: logs.map_or(500, |l| l.batch_flush_interval_ms),
        }
    };

    Ok((
        JobExecutor {
            reporter,
            registry,
            pipeline,
            base_dir,
            artifact_store,
            cleanup_policy: config.cleanup_policy.clone(),
            metric_sink,
            log_sink,
            cancel_registry,
            chunk_registry,
            log_config,
            completion_tracker: None,
        },
        registered_wires,
    ))
}

/// Build the artifact store based on config.
///
/// When the `opendal` feature is enabled and a `[storage]` config section
/// is present, uses `OpenDalArtifactStore` for the configured backend.
/// Otherwise falls back to `LocalArtifactStore` at `base_dir`.
fn build_artifact_store(
    config: &ExecutorConfig,
    base_dir: &Path,
) -> Result<Option<Arc<dyn ArtifactStore>>, Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(feature = "opendal")]
    {
        if let Some(storage_config) = &config.storage {
            info!(backend = ?storage_config.backend, "building artifact store from storage config");
            let store = OpenDalArtifactStore::from_config(storage_config)
                .map_err(|e| format!("storage init failed: {e}"))?;
            return Ok(Some(Arc::new(store)));
        }
    }

    #[cfg(not(feature = "opendal"))]
    {
        if let Some(storage_config) = &config.storage {
            if !matches!(storage_config.backend, StorageBackend::Local) {
                return Err(format!(
                    "storage backend {:?} requires the 'opendal' feature flag",
                    storage_config.backend
                )
                .into());
            }
            info!("using local artifact store from storage config");
            return Ok(Some(Arc::new(LocalArtifactStore::new(PathBuf::from(
                &storage_config.endpoint,
            )))));
        }
    }

    Ok(Some(Arc::new(LocalArtifactStore::new(
        base_dir.to_path_buf(),
    ))))
}

/// Build the metric sink from config.
///
/// When metrics are enabled and sinks are configured, builds a composite
/// sink that fans out to all specified backends. When no metrics config
/// is present, returns `None` (metrics pass through without sinking).
fn build_metric_sink(
    config: &ExecutorConfig,
    nats_client: &async_nats::Client,
) -> Result<Option<Arc<dyn MetricSink>>, Box<dyn std::error::Error + Send + Sync>> {
    let metrics_config = match &config.metrics {
        Some(cfg) if cfg.enabled => cfg,
        _ => return Ok(None),
    };

    if metrics_config.sinks.is_empty() {
        return Ok(None);
    }

    let mut sinks: Vec<Arc<dyn MetricSink>> = Vec::new();

    for sink_config in &metrics_config.sinks {
        match sink_config {
            MetricSinkConfig::Memory => {
                info!(
                    max_per_execution = metrics_config.max_buffer_per_execution,
                    "activating in-memory metric sink"
                );
                sinks.push(Arc::new(InMemoryMetricSink::new(
                    metrics_config.max_buffer_per_execution,
                )));
            }
            MetricSinkConfig::Nats => {
                info!("activating NATS metric sink");
                sinks.push(Arc::new(NatsMetricSink::new(nats_client.clone())));
            }
            MetricSinkConfig::Loki { url, static_labels } => {
                info!(url, "activating Loki metric sink");
                sinks.push(Arc::new(LokiMetricSink::new(
                    url.clone(),
                    static_labels.clone(),
                )));
            }
        }
    }

    match sinks.len() {
        0 => Ok(None),
        1 => Ok(Some(sinks.remove(0))),
        _ => Ok(Some(Arc::new(CompositeMetricSink::new(sinks)))),
    }
}

/// Build the log sink from config.
///
/// When logs are enabled and sinks are configured, builds a composite
/// sink that fans out to all specified backends with per-sink level filtering.
/// When no logs config is present, returns `None`.
fn build_log_sink(
    config: &ExecutorConfig,
    nats_client: &async_nats::Client,
    base_dir: &Path,
) -> Result<Option<Arc<dyn LogSink>>, Box<dyn std::error::Error + Send + Sync>> {
    let logs_config = match &config.logs {
        Some(cfg) if cfg.enabled => cfg,
        _ => return Ok(None),
    };

    if logs_config.sinks.is_empty() {
        return Ok(None);
    }

    let mut sinks: Vec<Arc<dyn LogSink>> = Vec::new();

    for sink_config in &logs_config.sinks {
        match sink_config {
            LogSinkConfig::File { min_level } => {
                info!(?min_level, "activating file log sink");
                let file_sink: Arc<dyn LogSink> = Arc::new(FileLogSink::new(
                    base_dir.to_path_buf(),
                    logs_config.filename.clone(),
                ));
                match min_level {
                    Some(level) => {
                        sinks.push(Arc::new(LevelFilterSink::new(file_sink, *level)));
                    }
                    None => sinks.push(file_sink),
                }
            }
            LogSinkConfig::Nats {
                min_level,
                batch_size: _,
            } => {
                info!(?min_level, "activating NATS log sink");
                let nats_sink: Arc<dyn LogSink> = Arc::new(NatsLogSink::new(nats_client.clone()));
                sinks.push(Arc::new(LevelFilterSink::new(nats_sink, *min_level)));
            }
            LogSinkConfig::Loki { .. } => {
                // Loki sink requires the `loki` feature on executor-logs.
                // When enabled, build here; otherwise log a warning.
                warn!("Loki log sink configured but `loki` feature not enabled, skipping");
            }
        }
    }

    match sinks.len() {
        0 => Ok(None),
        1 => Ok(Some(sinks.remove(0))),
        _ => Ok(Some(Arc::new(CompositeLogSink::new(sinks)))),
    }
}

/// Discover the aithericon Python SDK package path.
///
/// Checks `AITHERICON_SDK_PATH` env var, then falls back to the workspace-relative path.
fn find_sdk_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("AITHERICON_SDK_PATH") {
        let p = PathBuf::from(path);
        if p.join("pyproject.toml").exists() {
            return Some(p);
        }
    }
    // Development fallback: relative to the workspace root
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir).parent()?.parent()?;
    let sdk_path = workspace_root.join("packages").join("aithericon-sdk");
    if sdk_path.join("pyproject.toml").exists() {
        return Some(sdk_path);
    }
    None
}

/// Compute a stable marker for the SDK at `sdk_path`. Used as a hash input
/// for the venv cache so SDK updates invalidate cached venvs that bundle it.
///
/// Strategy: parse `pyproject.toml`'s `[project].version` (or `[tool.poetry].version`),
/// then suffix with the mtime of the most recently modified file. This catches
/// both formal version bumps and dev-loop edits without forcing a full source
/// tree hash on every cache lookup.
#[cfg(feature = "python")]
fn compute_sdk_marker(sdk_path: &Path) -> Option<String> {
    let pyproject = sdk_path.join("pyproject.toml");
    let text = std::fs::read_to_string(&pyproject).ok()?;
    let version = parse_pyproject_version(&text).unwrap_or_else(|| "0.0.0".to_string());

    // Walk the source tree (shallow: skip build/dist/__pycache__/.egg-info) and
    // record the latest mtime. Best-effort — fall back to version-only if walk fails.
    let mtime = newest_mtime_unix(sdk_path).unwrap_or(0);
    Some(format!("v={version};mtime={mtime}"))
}

#[cfg(feature = "python")]
fn parse_pyproject_version(text: &str) -> Option<String> {
    // Minimal parser: look for `version = "X"` under either `[project]` or
    // `[tool.poetry]`. The full TOML parser is overkill for one field; if this
    // ever needs to handle dynamic-version configs, we can pull in `toml`.
    let mut current_section = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current_section = rest.to_string();
            continue;
        }
        if current_section == "project" || current_section == "tool.poetry" {
            if let Some(rest) = trimmed.strip_prefix("version") {
                let rest = rest.trim_start_matches([' ', '=', '\t']);
                let rest = rest.trim_matches(['"', '\'', ' ']);
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
        }
    }
    None
}

#[cfg(feature = "python")]
fn newest_mtime_unix(root: &Path) -> Option<u64> {
    use std::time::UNIX_EPOCH;
    let mut newest = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_lossy = name.to_string_lossy();
            if matches!(
                name_lossy.as_ref(),
                "build" | "dist" | "__pycache__" | ".git" | "target" | ".venv"
            ) || name_lossy.ends_with(".egg-info")
            {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(epoch) = modified.duration_since(UNIX_EPOCH) {
                            let secs = epoch.as_secs();
                            if secs > newest {
                                newest = secs;
                            }
                        }
                    }
                }
            }
        }
    }
    if newest == 0 {
        None
    } else {
        Some(newest)
    }
}

/// Pre-warm the venv cache from a requirements file. Invoked via
/// `aithericon-executor warm-venv`. Reuses the daemon's `[python]` config
/// for cache_dir / prefer_uv so the warmed entry is observable to workers.
///
/// Required env: `EXECUTOR_WARM_REQUIREMENTS=path/to/requirements.txt`.
/// Optional env: `EXECUTOR_WARM_PYTHON` (default `python3`),
/// `EXECUTOR_WARM_SDK=1` to bundle the local aithericon SDK.
///
/// Prints the resolved cache-resident venv path on success, exits non-zero
/// on failure. Idempotent: a second invocation with the same inputs is a
/// cache hit and returns immediately.
#[cfg(feature = "python")]
async fn warm_venv() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let reqs_path = std::env::var("EXECUTOR_WARM_REQUIREMENTS")
        .map_err(|_| "EXECUTOR_WARM_REQUIREMENTS must be set (path to a requirements.txt file)")?;
    let python = std::env::var("EXECUTOR_WARM_PYTHON").unwrap_or_else(|_| "python3".into());
    let include_sdk = std::env::var("EXECUTOR_WARM_SDK")
        .ok()
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"));

    let requirements = parse_requirements_file(&reqs_path)
        .map_err(|e| format!("failed to read {reqs_path}: {e}"))?;
    if requirements.is_empty() {
        warn!(path = %reqs_path, "requirements file is empty; cache entry will hold only the interpreter");
    } else {
        info!(
            count = requirements.len(),
            path = %reqs_path,
            "parsed requirements"
        );
    }

    let mut config = ExecutorConfig::load().map_err(|e| {
        error!("configuration error: {e}");
        e
    })?;
    config.normalize();
    let base_dir = PathBuf::from(&config.base_dir);

    // For warm-venv we build the cache regardless of [python].enabled — the
    // act of warming is itself an opt-in. Use the cache_dir/prefer_uv knobs
    // when set, defaults otherwise.
    let py_cfg = config.python.clone().unwrap_or_default();
    let cache_dir = py_cfg
        .cache_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir.join("python-venvs"));
    let sdk_path = if include_sdk { find_sdk_path() } else { None };
    let sdk_marker = sdk_path.as_deref().and_then(compute_sdk_marker);

    let cache = VenvCache::new(cache_dir.clone(), py_cfg.prefer_uv, sdk_marker)?;
    info!(
        cache_dir = %cache_dir.display(),
        prefer_uv = py_cfg.prefer_uv,
        include_sdk,
        "warming venv cache"
    );

    let req = BuildRequest {
        python: &python,
        requirements: &requirements,
        sdk_path: sdk_path.as_deref(),
    };
    let resolved = cache.resolve(req).await?;
    let s = cache.stats();
    info!(
        path = %resolved.display(),
        hits = s.hits,
        misses = s.misses,
        build_duration_ms_total = s.build_duration_ms_total,
        "warm-venv complete"
    );
    println!("{}", resolved.display());
    Ok(())
}

/// Parse a pip-style requirements.txt: one requirement per line, `#` starts
/// a comment, blank lines ignored. Does NOT expand `-r recursive.txt` or
/// `-e editable/path` directives — those are out of scope for the cache key.
#[cfg(feature = "python")]
fn parse_requirements_file(path: &str) -> std::io::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Build the shared Python venv cache from config, or return None when disabled.
#[cfg(feature = "python")]
fn build_venv_cache(config: &ExecutorConfig, base_dir: &Path) -> Option<Arc<VenvCache>> {
    let py_cfg = config.python.as_ref()?;
    if !py_cfg.enabled {
        return None;
    }

    let cache_dir = py_cfg
        .cache_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| base_dir.join("python-venvs"));

    let sdk_marker = find_sdk_path().and_then(|p| compute_sdk_marker(&p));

    match VenvCache::new(cache_dir.clone(), py_cfg.prefer_uv, sdk_marker) {
        Ok(cache) => {
            info!(
                cache_dir = %cache_dir.display(),
                prefer_uv = py_cfg.prefer_uv,
                "venv cache initialized"
            );
            let arc = Arc::new(cache);
            spawn_venv_stats_logger(Arc::downgrade(&arc));
            Some(arc)
        }
        Err(e) => {
            warn!(
                cache_dir = %cache_dir.display(),
                error = %e,
                "venv cache initialization failed; falling back to per-execution builds"
            );
            None
        }
    }
}

/// Periodically log cache counters at INFO level so Loki/Grafana can derive
/// hit-rate and build-cost dashboards from log fields. Holds a Weak ref so
/// the task exits when the cache is dropped (process shutdown).
#[cfg(feature = "python")]
fn spawn_venv_stats_logger(weak: std::sync::Weak<VenvCache>) {
    use tokio::time::{interval, Duration};
    tokio::spawn(async move {
        // 60s gives Grafana enough resolution without spamming logs.
        let mut ticker = interval(Duration::from_secs(60));
        ticker.tick().await; // burn the immediate first tick
        loop {
            ticker.tick().await;
            let Some(cache) = weak.upgrade() else {
                break;
            };
            let s = cache.stats();
            // Skip emitting until we've seen at least one resolve — keeps the
            // log clean on idle workers.
            if s.hits == 0 && s.misses == 0 {
                continue;
            }
            info!(
                metric = "venv_cache_stats",
                hits = s.hits,
                misses = s.misses,
                hit_ratio = s.hit_ratio(),
                builds_in_flight = s.builds_in_flight,
                build_duration_ms_total = s.build_duration_ms_total,
                "venv cache stats snapshot"
            );
        }
    });
}

/// Build the secret store for the staging pipeline.
///
/// When `VAULT_ADDR` and `VAULT_TOKEN` are set (and the `vault` feature is enabled),
/// returns a chained store that tries env vars first, then falls back to Vault.
/// Otherwise returns an env-var-only store.
fn build_secret_store() -> Arc<dyn aithericon_secrets::SecretStore> {
    #[cfg(feature = "vault")]
    {
        if let Some(vault) = aithericon_secrets::VaultSecretStore::from_env() {
            let prefix = std::env::var("VAULT_SECRET_PREFIX").unwrap_or_default();
            let mount =
                std::env::var("VAULT_SECRET_MOUNT").unwrap_or_else(|_| "secret".to_string());
            let vault = vault.mount(mount).key_prefix(prefix);
            info!("secret store: env_var -> vault");
            return Arc::new(aithericon_secrets::ChainedSecretStore::new(vec![
                Box::new(aithericon_secrets::EnvVarSecretStore),
                Box::new(vault),
            ]));
        }
    }
    info!("secret store: env_var");
    Arc::new(aithericon_secrets::EnvVarSecretStore)
}

/// Start the HTTP cancel endpoint as a background task.
///
/// `POST /cancel/{execution_id}` returns JSON `{ "execution_id": "...", "cancelled": true/false }`.
/// Graceful shutdown via the provided `CancellationToken`.
#[cfg(feature = "http-cancel")]
fn start_http_cancel(
    cancel_config: &aithericon_executor_worker::CancelConfig,
    registry: aithericon_executor_worker::CancellationRegistry,
    shutdown: CancellationToken,
) {
    use axum::{extract::Path, routing::post, Json, Router};

    let bind = format!("{}:{}", cancel_config.http_bind, cancel_config.http_port);
    info!(%bind, "starting HTTP cancel endpoint");

    tokio::spawn(async move {
        let app = Router::new().route(
            "/cancel/{execution_id}",
            post(move |Path(execution_id): Path<String>| {
                let registry = registry.clone();
                async move {
                    let cancelled = registry.cancel(&execution_id);
                    if cancelled {
                        info!(%execution_id, "cancellation triggered via HTTP");
                    }
                    Json(serde_json::json!({
                        "execution_id": execution_id,
                        "cancelled": cancelled,
                    }))
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .expect("failed to bind HTTP cancel endpoint");

        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown.cancelled().await;
            })
            .await
            .expect("HTTP cancel server error");
    });
}

#[cfg(all(test, feature = "python"))]
mod tests {
    use super::*;

    #[test]
    fn parse_requirements_skips_comments_and_blanks() {
        let tmp = tempfile::Builder::new().prefix("reqs-").tempdir().unwrap();
        let path = tmp.path().join("requirements.txt");
        std::fs::write(
            &path,
            "# top comment\n\
             numpy>=1.20\n\
             \n\
             pandas  # inline comment\n\
             # another comment\n\
             requests\n\
             \n",
        )
        .unwrap();

        let reqs = parse_requirements_file(path.to_str().unwrap()).unwrap();
        assert_eq!(reqs, vec!["numpy>=1.20", "pandas", "requests"]);
    }

    #[test]
    fn parse_requirements_empty_file_yields_empty_vec() {
        let tmp = tempfile::Builder::new().prefix("reqs-").tempdir().unwrap();
        let path = tmp.path().join("requirements.txt");
        std::fs::write(&path, "# only comments\n\n").unwrap();
        let reqs = parse_requirements_file(path.to_str().unwrap()).unwrap();
        assert!(reqs.is_empty());
    }

    #[test]
    fn parse_requirements_preserves_version_specifiers() {
        let tmp = tempfile::Builder::new().prefix("reqs-").tempdir().unwrap();
        let path = tmp.path().join("requirements.txt");
        std::fs::write(&path, "numpy>=1.20,<2.0\nsix==1.16.0\n").unwrap();
        let reqs = parse_requirements_file(path.to_str().unwrap()).unwrap();
        assert_eq!(reqs, vec!["numpy>=1.20,<2.0", "six==1.16.0"]);
    }
}
