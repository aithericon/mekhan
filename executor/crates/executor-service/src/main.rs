use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use apalis::prelude::*;
use apalis_nats::{NatsStorage, ProgressHeartbeatLayer};
use tracing::{error, info, warn};

use aithericon_executor_backend::BatchSink;
#[cfg(feature = "docker")]
use aithericon_executor_docker::DockerBackend;
use aithericon_executor_domain::ExecutionJob;
#[cfg(feature = "http")]
use aithericon_executor_http::HttpBackend;
#[cfg(feature = "kreuzberg")]
use aithericon_executor_kreuzberg::KreuzbergBackend;
#[cfg(feature = "llm")]
use aithericon_executor_llm::LlmBackend;
use aithericon_executor_logs::{
    CompositeLogSink, FileLogSink, LevelFilterSink, LogSink, LogSinkConfig, NatsLogSink,
};
#[cfg(feature = "loki")]
use aithericon_executor_loki::LokiBackend;
use aithericon_executor_metrics::{
    CompositeMetricSink, InMemoryMetricSink, LokiMetricSink, MetricSink, MetricSinkConfig,
    MetricsConfig, NatsMetricSink,
};
#[cfg(feature = "postgres")]
use aithericon_executor_postgres::PostgresBackend;
use aithericon_executor_process::ProcessBackend;
#[cfg(feature = "prometheus")]
use aithericon_executor_prometheus::PrometheusBackend;
#[cfg(feature = "python")]
use aithericon_executor_python::cache::{BuildRequest, VenvCache};
#[cfg(feature = "python")]
use aithericon_executor_python::PythonBackend;
#[cfg(feature = "ros")]
use aithericon_executor_ros::RosBackend;
#[cfg(feature = "smtp")]
use aithericon_executor_smtp::SmtpBackend;
#[cfg(feature = "opendal")]
use aithericon_executor_storage::OpenDalArtifactStore;
#[cfg(not(feature = "opendal"))]
use aithericon_executor_storage::StorageBackend;
use aithericon_executor_storage::{ArtifactStore, BrokeredArtifactStore, LocalArtifactStore};
#[cfg(feature = "surya")]
use aithericon_executor_surya::SuryaBackend;
use aithericon_executor_worker::{
    drain_signal, handle_execution, spawn_fileserve_handler, spawn_presence_task,
    spawn_worker_presence_task, BackendRegistry, BatchRunner, CancellationRegistry,
    CompletionTracker, DrainConfig, ExecutorConfig, JobExecutor, JobSource, Lifetime,
    BrokeredBatchSink, LiveModelState, NatsBatchSink, NatsCancelListener, NixEnvironmentHook,
    SidecarLogConfig, StatusReporter, TransportRegistry,
};
use tokio_util::sync::CancellationToken;

mod register;
// `publish_catalog` shared by the ROS catalog publisher and the model-pool node
// agent — compiled when either consumer is built (a `vllm`-only GPU host pulls
// no ROS deps).
#[cfg(any(feature = "ros", feature = "vllm"))]
mod catalog_publish;
#[cfg(feature = "vllm")]
mod model_agent;
#[cfg(feature = "ros")]
mod ros_catalog;

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
            allow_network = config
                .sandbox
                .as_ref()
                .map(|s| s.allow_network)
                .unwrap_or(false),
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
/// - else → `Pool`: durable shared consumer on `config.namespace`. This is the
///   single-namespace drain/manifest path (Slurm/Nomad sbatch with no
///   target_exec_id, or local manifest dispatch) — NOT the competing-consumer
///   worker-pool path, which fans out per-backend grouped consumers in
///   `run_nats_daemon` (and requires enrollment).
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
    mut config: ExecutorConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Unified worker model: self-enroll on boot when a `worker_reg_token` is set
    // and we're not already enrolled. Runs BEFORE the NATS connect so the
    // freshly-minted scoped `.creds` and the resolved routing partition (the
    // group's capacity-resource UUID) are in effect for this daemon's connection
    // + consumer bind. Every worker MUST enroll — a worker-pool daemon without a
    // routing partition is rejected below (no anonymous path).
    maybe_enroll_worker(&mut config).await?;

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

    // The worker-pool path (an enrolled, competing-consumer worker — neither a
    // runner nor PerJob) fans the daemon out across one grouped
    // `executor-<wire>-grp` consumer per backend it serves (built below, after
    // the executor's backend set is known), partitioned by its routing group's
    // capacity-resource UUID. The runner-presence (`runner.{id}`) and PerJob/
    // lease (`target_exec_id`) paths keep the single-namespace storage built
    // from `config.namespace`.
    let worker_pool_mode = config.runner_id.is_none() && config.target_exec_id.is_none();

    let nats_client_for_cancel = nats_client.clone();

    // Set up cancellation registry and listeners
    let cancel_registry = CancellationRegistry::new();
    let cancel_shutdown = CancellationToken::new();

    if config.cancel.nats {
        // FAIL-CLOSED: cancellation is a safety control, not a nicety. If the
        // cancel listener can't bind, this runner must NOT drain jobs — an
        // unhonored cancel lets runaway work (a crawl hammering a filesystem, an
        // expensive compute, a destructive file_op) keep running while the UI
        // reports it stopped. Abort startup with an ACTIONABLE error rather than
        // the opaque transport timeout the bind would otherwise surface.
        NatsCancelListener::start(
            jetstream.clone(),
            cancel_registry.clone(),
            config.subject_prefix.as_deref(),
            config.status_replicas,
            cancel_shutdown.clone(),
        )
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
            format!(
                "cancel listener failed to start ({e}); refusing to drain uncancellable jobs. \
                 Likely cause: scoped NATS creds lacking EXECUTOR_CANCEL JetStream perms — \
                 refresh this runner's creds against an up-to-date mekhan, or set \
                 EXECUTOR_CANCEL__NATS=false to KNOWINGLY run without cancellation."
            )
            .into()
        })?;
        info!("JetStream cancel listener started");
    }

    #[cfg(feature = "http-cancel")]
    if config.cancel.http {
        start_http_cancel(
            &config.cancel,
            cancel_registry.clone(),
            cancel_shutdown.clone(),
        );
    }

    // Data-plane byte transport REGISTRY (docs/25 §6). Ensure the durable
    // `EXECUTOR_DATASTREAM` stream exists once, then hand each job's IPC sidecar
    // the registry so producer-write/consumer-read dispatch the adapter off the
    // channel's declared transport (`jetstream` durable | `nats-latest` lossy).
    TransportRegistry::ensure_streams(&jetstream, config.status_replicas).await?;
    let transports: Option<TransportRegistry> = Some(attach_livekit(
        attach_object_store(
            TransportRegistry::new(jetstream.clone(), nats_client_for_cancel.clone()),
            &config,
        ),
        &config,
    ));

    // Build the JobExecutor. `registered_wires` is the set of backend
    // wire-names that actually registered (feature-gated arms may skip) —
    // exactly the set the worker pool can serve.
    let batch_sink = build_batch_sink(&jetstream, &config).await?;
    let (executor, registered_wires) = build_executor(
        &config,
        reporter,
        &nats_client_for_cancel,
        cancel_registry,
        transports,
        batch_sink,
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
    // P2 (model-pool): the live `{models, C}` state the model agent mutates on
    // load/unload and the presence task re-reads each heartbeat. Created BEFORE
    // the presence spawn and handed to BOTH so a load/unload reflects on the
    // wire without a re-enroll. `None` for a non-model runner (presence then
    // omits the `{concurrency, models}` fields — legacy shape).
    let model_state: Option<LiveModelState> = if cfg!(feature = "vllm") {
        config.model_agent().map(|_| LiveModelState::new())
    } else {
        None
    };

    if let Some(runner_id) = config.runner_id.clone() {
        let mut backends: Vec<String> = registered_wires.iter().map(|w| w.to_string()).collect();
        // A model-serving node (the `[model_agent]` is active → `model_state` set)
        // advertises the first-class `llm-server` CAPABILITY alongside its executor
        // job wires. This is the data-plane SERVING role (hosts an inference engine
        // the router routes to), distinct from the `llm` job-executor backend; it
        // surfaces on the Fleet Live board and documents why this runner enrols into
        // the `model-serving` group (the authoritative pool-membership gate).
        if model_state.is_some() {
            backends.push(aithericon_backends::LLM_SERVER_WIRE.to_string());
        }
        // Probe the host/hardware fingerprint ONCE at startup (subprocess calls
        // to nvidia-smi/sysctl/etc. are too costly per-heartbeat, and the host is
        // static for the daemon's lifetime). Attached to every heartbeat for
        // fleet visibility; best-effort — absent fields simply don't appear.
        let host = Some(aithericon_executor_worker::probe_host());
        spawn_presence_task(
            nats_client_for_cancel.clone(),
            runner_id.clone(),
            backends.clone(),
            model_state.clone(),
            host,
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

    // Phase 3a (multi-endpoint file-servers): a co-located runner serves bytes
    // from an endpoint's LOCAL mount root on demand. mekhan publishes serve
    // requests to `fileserve.<group>.read` where <group> is the capacity-group
    // UUID the file-server endpoint binds to — the SAME partition this daemon
    // consumes jobs on. We queue-subscribe (queue group = <group>) so exactly
    // one co-grouped worker handles each request; the serve handler is cred-free
    // (mekhan sends the authoritative `root` per request) and path-jails every
    // read. It reuses the daemon's NATS client + shared shutdown token and runs
    // alongside the job consumers without interfering with job consumption.
    //
    // Serve groups = the partition(s) this daemon binds dispatch consumers on:
    // a runner serves its `runner_id`; an enrolled worker-pool daemon serves its
    // `worker_routing_partition` (the group's capacity-resource UUID). A daemon
    // with neither (drain/PerJob) serves no endpoint, so the handler is skipped.
    {
        let mut serve_groups: Vec<String> = Vec::new();
        if let Some(rid) = config.runner_id.clone() {
            serve_groups.push(rid);
        }
        if let Some(part) = config.worker_routing_partition.clone() {
            if !serve_groups.contains(&part) {
                serve_groups.push(part);
            }
        }
        if !serve_groups.is_empty() {
            info!(?serve_groups, "fileserve handler binding");
            spawn_fileserve_handler(
                nats_client_for_cancel.clone(),
                serve_groups,
                cancel_shutdown.clone(),
            );
        }
    }

    // Phase 3 (runner-side ROS catalog publish): when this daemon is a runner
    // with a mekhan URL + a reachable rosbridge, introspect the ROS interface
    // catalog and POST it to mekhan at startup. Fire-and-forget + best-effort —
    // a missing token / unreachable mekhan / no rosapi node never crashes the
    // daemon. No-op unless `runner_id` + `[mekhan].url` are configured.
    #[cfg(feature = "ros")]
    ros_catalog::spawn_catalog_publish(&config);

    // P2 (model-pool node agent): when this daemon is a runner with a mekhan URL
    // + a `[model_agent].vllm_url`, probe the local vLLM engine's served models,
    // publish them as a runner interface catalog, and subscribe to
    // `runner.{id}.load`/`unload` control commands — mapping each onto vLLM's
    // admin surface, re-pushing the catalog, and updating the live presence
    // state. Inference NEVER crosses this path (control-plane only). Reuses the
    // runner-scoped NATS client + the shared shutdown token. No-op unless the
    // `[model_agent]` block + `runner_id` + `[mekhan].url` all resolve.
    #[cfg(feature = "vllm")]
    if let Some(state) = model_state.clone() {
        model_agent::spawn_model_agent(
            &config,
            nats_client_for_cancel.clone(),
            state,
            cancel_shutdown.clone(),
        );
    }

    let mut monitor = Monitor::new();

    if worker_pool_mode {
        // Worker-pool fan-out (unified model): bind ONE grouped consumer per
        // backend this binary actually registered. Each gets its own NatsStorage
        // (auto-creates the namespace's streams) + worker (unique name
        // `{config.name}-{group}-{wire}`) sharing the SAME Arc<JobExecutor>. All
        // register on the single Monitor.
        if registered_wires.is_empty() {
            return Err(
                "worker-pool daemon has no ExecutorJob backends compiled in — nothing to drain; \
                 check the executor-service feature set"
                    .into(),
            );
        }

        // MANDATORY enrollment: every worker routes through a GROUP. The routing
        // partition is the group's capacity-resource UUID, resolved at enroll
        // (the implicit "default" group resolves to its own UUID server-side).
        // There is no anonymous worker path — a worker-pool daemon without a
        // routing partition cannot bind any dispatch consumer, so fail fast with
        // a clear remediation rather than silently draining nothing.
        let routing_partition = config.worker_routing_partition.clone().ok_or(
            "workers must enroll: no routing partition resolved. Set \
             EXECUTOR_WORKER_REG_TOKEN (+ EXECUTOR_MEKHAN_URL) so this worker \
             enrolls into a group on boot, or pre-provision \
             {base_dir}/worker/identity.json with a routing_partition.",
        )?;

        for wire in &registered_wires {
            // Unified single-stream grouped routing. An enrolled worker binds
            // `ConsumerMode::PartitionedPool { partition: <group_uuid> }` on the
            // `executor-<wire>-grp` namespace family, where <group_uuid> is the
            // routing group's capacity-resource UUID. mekhan's compiler stamps
            // every executor job's `executor_namespace =
            // "executor-<wire>-grp/<group_uuid>"` (a step naming no group is
            // stamped with the workspace's "default" group); the engine's
            // `split_namespace` splits on the first `/` → stream_ns
            // `executor-<wire>-grp`, partition `<group_uuid>`, publishing to
            // `executor-<wire>-grp.<prio>.<group_uuid>.<exec>`. The
            // `PartitionedPool` filter `executor-<wire>-grp.<prio>.<group_uuid>.>`
            // matches that, and its durable is partition-keyed
            // (`executor-<wire>-grp_<prio>_<group_uuid>_consumer`, NOT
            // worker-keyed) so MANY workers in the same group share one durable
            // and COMPETE — `worker_id` is identity only, never a partition.
            let namespace = aithericon_backends::executor_pool_namespace(wire) + "-grp";
            let consumer_mode = apalis_nats::ConsumerMode::PartitionedPool {
                partition: routing_partition.clone(),
            };

            let nats_config = apalis_nats::Config {
                namespace: namespace.clone(),
                max_deliver: config.max_deliver,
                ack_wait: config.ack_wait(),
                max_ack_pending: config.max_ack_pending,
                num_replicas: 1,
                enable_dlq: true,
                consumer_mode,
                ..Default::default()
            };

            let storage =
                NatsStorage::<ExecutionJob>::new_with_config(nats_client.clone(), nats_config)
                    .await?;

            // Worker name must stay unique across the served backends; include
            // the routing partition so logs disambiguate co-located groups, but
            // note the apalis worker NAME is NOT the consumer durable — the
            // durable is partition-keyed (shared), so co-grouped workers compete.
            let worker_name = format!("{}-{routing_partition}-{wire}", config.name);
            let worker = build_worker!(
                &worker_name,
                config.concurrency,
                config.heartbeat_interval(),
                executor.clone(),
                storage
            );
            monitor = monitor.register(worker);
            info!(%namespace, partition = %routing_partition, worker = %worker_name, "worker-pool backend consumer bound");
        }

        // Worker presence: advertise the backend set this pool worker drains.
        // Presence subject is `worker.<worker_id>.presence` — the `wkr_`
        // control-plane UUID (`config.worker_id`), which an enrolled worker
        // always has, so mekhan's FleetLiveness can correlate the heartbeat to
        // the worker DB row. The `unwrap_or` on `config.name` is defensive only
        // (the daemon already hard-errors above without a routing partition).
        // The backend set is wire-truth here; the `group` display alias is
        // carried so the fleet view can render which group each worker competes
        // in (the actual queue partition is the group's UUID, bound above).
        let backends: Vec<String> = registered_wires.iter().map(|w| w.to_string()).collect();
        let presence_id = config
            .worker_id
            .clone()
            .unwrap_or_else(|| config.name.clone());
        spawn_worker_presence_task(
            nats_client_for_cancel.clone(),
            presence_id.clone(),
            backends.clone(),
            config.worker_group.clone(),
            config.presence_interval(),
            cancel_shutdown.clone(),
        );
        info!(
            worker_id = %presence_id,
            group = ?config.worker_group,
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
        // FAIL-CLOSED (see the equivalent block in `run_nats_daemon` for the
        // full rationale): a runner that can't bind its cancel listener must not
        // drain uncancellable jobs.
        NatsCancelListener::start(
            jetstream.clone(),
            cancel_registry.clone(),
            config.subject_prefix.as_deref(),
            config.status_replicas,
            cancel_shutdown.clone(),
        )
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
            format!(
                "cancel listener failed to start ({e}); refusing to drain uncancellable jobs. \
                 Likely cause: scoped NATS creds lacking EXECUTOR_CANCEL JetStream perms — \
                 refresh this runner's creds against an up-to-date mekhan, or set \
                 EXECUTOR_CANCEL__NATS=false to KNOWINGLY run without cancellation."
            )
            .into()
        })?;
        info!("JetStream cancel listener started");
    }

    // Data-plane byte transport REGISTRY (see `run_nats_daemon` for the rationale).
    TransportRegistry::ensure_streams(&jetstream, config.status_replicas).await?;
    let transports: Option<TransportRegistry> = Some(attach_livekit(
        attach_object_store(
            TransportRegistry::new(jetstream.clone(), nats_client_for_cancel.clone()),
            &config,
        ),
        &config,
    ));

    // Build the executor with a completion tracker
    let tracker = Arc::new(CompletionTracker::new());
    let drain_rx = tracker.subscribe();
    let tracker_for_exit = tracker.clone();

    // Drain mode is PerJob (Slurm/Nomad-dispatched, exact-filter) or a
    // single-namespace pool drain — it does not fan out per backend, so the
    // registered wire set is unused here.
    let batch_sink = build_batch_sink(&jetstream, &config).await?;
    let (mut executor, _registered_wires) = build_executor(
        &config,
        reporter,
        &nats_client_for_cancel,
        cancel_registry,
        transports,
        batch_sink,
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
        jetstream.clone(),
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

    // Build executor — same pipeline as daemon mode.
    let cancel_registry = CancellationRegistry::new();
    // Data-plane transport REGISTRY: a manifest job MAY be a producer
    // (`open_output`) or a consumer (`stream`), so wire it here too.
    TransportRegistry::ensure_streams(&jetstream, config.status_replicas).await?;
    let transports: Option<TransportRegistry> = Some(attach_livekit(
        attach_object_store(
            TransportRegistry::new(jetstream.clone(), nats_client.clone()),
            &config,
        ),
        &config,
    ));
    // Manifest mode is its own single-namespace dispatcher — no per-backend
    // fan-out, so the registered wire set is unused here.
    let batch_sink = build_batch_sink(&jetstream, &config).await?;
    let (executor, _registered_wires) = build_executor(
        &config,
        reporter.clone(),
        &nats_client,
        cancel_registry,
        transports,
        batch_sink,
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
    #[allow(unused_variables)] batch_sink: &Option<Arc<dyn BatchSink>>,
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
            let mut process = ProcessBackend::new().with_max_output_bytes(config.max_output_bytes);
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
                    .with_default_storage(config.storage.clone())
                    // Durable fold sink for sink-mode crawls (docs/32). NATS
                    // stays behind the trait — the backend never sees it.
                    .with_batch_sink(batch_sink.clone()),
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
        #[cfg(feature = "ros")]
        "ros" => {
            let ws_url = config.ros_ws_url();
            info!(%ws_url, "ros backend registered");
            registry.register(RosBackend::new(ws_url))
        }
        other => {
            info!(
                "backend '{other}' declared in aithericon-backends but not built into this executor binary — skipping"
            );
            registry
        }
    }
}

/// Build the durable NATS fold sink injected into the file-ops backend
/// (sink-mode crawl batches, docs/32). Shared by all three operating modes.
/// Ensures the `INVENTORY_FOLD` stream; the stamped serve identity uses the
/// SAME precedence as `JobExecutor.serve_group` / the fileserve binding
/// (runner_id first, then the worker routing partition).
async fn build_batch_sink(
    jetstream: &async_nats::jetstream::Context,
    config: &ExecutorConfig,
) -> Result<Option<Arc<dyn BatchSink>>, Box<dyn std::error::Error + Send + Sync>> {
    let serve_group = config
        .runner_id
        .clone()
        .or_else(|| config.worker_routing_partition.clone());

    // Brokered runner: no reliable JetStream publish-ack over the WS front door,
    // so fold batches POST through mekhan (`/api/storage/fold`) instead of
    // JS-publishing to INVENTORY_FOLD. Same selection as the brokered artifact +
    // secret stores (a `runner_broker_base()` + a readable `runner.token`). An
    // in-cluster worker (static storage / direct NATS) keeps the NATS sink.
    if let Some(broker_base) = config.runner_broker_base() {
        let base_dir = std::path::Path::new(&config.base_dir);
        if let Some(token) = read_runner_token(config, base_dir) {
            info!(base = %broker_base, "building brokered fold sink (runner fold proxy)");
            return Ok(Some(Arc::new(BrokeredBatchSink::new(
                broker_base,
                token,
                serve_group,
            ))));
        }
    }

    let sink = NatsBatchSink::new(
        jetstream.clone(),
        config.status_replicas,
        config.subject_prefix.clone(),
        serve_group,
    )
    .await?;
    Ok(Some(Arc::new(sink)))
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
    transports: Option<TransportRegistry>,
    batch_sink: Option<Arc<dyn BatchSink>>,
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
        registry = register_executor_backend(registry, meta, config, &base_dir, &batch_sink);
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
    // Zero-secret runner: when there's no direct VAULT_ADDR but the runner
    // enrolled against a broker (base + runner_id + token all resolve), route
    // `wrapped_secrets` unwrapping through mekhan's self-scoped HTTP endpoint.
    // In-cluster workers (VAULT_ADDR set) keep the direct Vault path; the broker
    // config is harmless to build (PlanSecretsHook only uses it when
    // vault_addr is None).
    let broker_secrets = build_broker_secrets(config, &base_dir);
    let pipeline = Arc::new(aithericon_executor_worker::staging::default_pipeline(
        base_dir.clone(),
        artifact_store.clone(),
        Some(secret_store),
        vault_addr,
        broker_secrets,
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
            log_config,
            completion_tracker: None,
            transports,
            // Same precedence as the fileserve `serve_groups` binding below:
            // a runner serves its runner_id, an enrolled pool daemon its
            // routing partition.
            serve_group: config
                .runner_id
                .clone()
                .or_else(|| config.worker_routing_partition.clone()),
            max_output_inline_bytes: config.max_output_inline_bytes,
        },
        registered_wires,
    ))
}

/// Enumerate the executor-job backend wire-names this binary serves, by
/// compiled feature — WITHOUT building any backend.
///
/// [`build_executor`] is the authoritative `registered_wires` source but it (a)
/// requires a live NATS client (for metric/log sinks) and (b) has real
/// side-effects (Docker connect, venv-cache build), so it can't run as a
/// pre-connect dry-run. This is a cheap, side-effect-free mirror of the same
/// `#[cfg(feature = ...)]` gating used in [`register_executor_backend`], used
/// only to report `backends` to mekhan at boot-time self-enroll (before the
/// NATS connection is established with the freshly-minted creds).
///
/// `process` is always-on; every other wire is feature-gated. Keep this list in
/// sync with `register_executor_backend`'s match arms. A backend that is
/// compiled-in but unavailable at runtime (e.g. Docker daemon down) is still
/// advertised here — the scoped JWT mekhan mints is a superset; an unavailable
/// backend only fails its own granted jobs (visible self-harm), never escalates.
fn served_wires() -> Vec<&'static str> {
    // `mut` is conditionally used — every `push` below is feature-gated, so a
    // build with only `process` compiled never mutates after the initializer.
    #[allow(unused_mut)]
    let mut wires: Vec<&'static str> = vec!["process"];
    #[cfg(feature = "python")]
    wires.push("python");
    #[cfg(feature = "docker")]
    wires.push("docker");
    #[cfg(feature = "http")]
    wires.push("http");
    #[cfg(feature = "llm")]
    wires.push("llm");
    #[cfg(feature = "file-ops")]
    wires.push("file_ops");
    #[cfg(feature = "kreuzberg")]
    wires.push("kreuzberg");
    #[cfg(feature = "surya")]
    wires.push("surya");
    #[cfg(feature = "smtp")]
    wires.push("smtp");
    #[cfg(feature = "postgres")]
    wires.push("postgres");
    #[cfg(feature = "loki")]
    wires.push("loki");
    #[cfg(feature = "prometheus")]
    wires.push("prometheus");
    wires
}

/// Boot-time worker self-enroll (Phase B — grouped + enrolled workers).
///
/// When `worker_reg_token` is set and the worker is not already enrolled
/// (`{base_dir}/worker/identity.json` absent), POST to mekhan
/// `/api/v1/workers/enroll`, persist the `wkr_` token + scoped `.creds` under
/// `{base_dir}/worker/`, then re-`normalize()` so the daemon picks up the
/// discovered `worker_id`, the inherited `worker_group`, and `nats_creds`. This
/// mirrors the runner enroll-then-pick-up-from-disk flow, but runs inline on
/// boot instead of as a separate CLI subcommand. Idempotent: a restart with the
/// identity already on disk is a no-op (the token is single-use server-side
/// anyway). `mekhan_url` is required when a token is present.
async fn maybe_enroll_worker(
    config: &mut ExecutorConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(token) = config.worker_reg_token.clone() else {
        return Ok(());
    };

    // Already enrolled (identity on disk → normalize() set worker_id): skip.
    if config.worker_id.is_some() {
        info!("worker already enrolled — skipping self-enroll");
        return Ok(());
    }

    let mekhan_url = config.mekhan_url.clone().ok_or(
        "EXECUTOR_WORKER_REG_TOKEN is set but EXECUTOR_MEKHAN_URL is not — \
         cannot self-enroll without the mekhan control-plane URL",
    )?;

    let backends: Vec<String> = served_wires().iter().map(|w| w.to_string()).collect();

    let enrolled = register::enroll_worker(
        &mekhan_url,
        &token,
        &config.name,
        backends,
        &config.base_dir,
    )
    .await?;

    // Re-normalize so the just-written {base_dir}/worker/identity.json + creds
    // are picked up: worker_id, the display group alias, the routing partition
    // (the capacity-resource UUID the grouped consumer binds), and nats_creds.
    config.normalize();

    info!(
        worker_id = ?enrolled.worker_id,
        group = ?enrolled.group,
        routing_partition = %enrolled.routing_partition,
        nats_creds = enrolled.creds_path.is_some(),
        "worker self-enroll complete"
    );

    Ok(())
}

/// Attach the durable object-store data-plane transport (`transport: "s3"`) to a
/// freshly-built [`TransportRegistry`], when the `opendal` feature is on and a
/// `[storage]` section is configured. Reuses the SAME `StorageConfig` that backs
/// the artifact store, so the datastream objects land in the configured bucket
/// (under the store prefix). A worker with no `[storage]` simply has no `"s3"`
/// transport — a `transport: "s3"` channel then fails loudly at dispatch.
#[cfg(feature = "opendal")]
fn attach_object_store(registry: TransportRegistry, config: &ExecutorConfig) -> TransportRegistry {
    let Some(storage) = &config.storage else {
        return registry;
    };
    match aithericon_executor_storage::build_operator(storage) {
        Ok(operator) => {
            info!(
                backend = ?storage.backend,
                "data-plane object-store transport enabled (transport=s3)"
            );
            registry.with_object_store(operator, storage.prefix.clone())
        }
        Err(e) => {
            warn!(error = %e, "object-store data-plane transport unavailable — build_operator failed");
            registry
        }
    }
}

/// No-op without the `opendal` feature — the `"s3"` transport is then simply
/// absent from the registry (and the `get("s3")` match arm compiles to `None`).
#[cfg(not(feature = "opendal"))]
fn attach_object_store(registry: TransportRegistry, _config: &ExecutorConfig) -> TransportRegistry {
    registry
}

/// Attach the presentation-only LiveKit egress data-plane transport
/// (`transport: "livekit"`) to a [`TransportRegistry`], when the `livekit`
/// feature is on and a `[livekit]` section is configured. A worker with no
/// `[livekit]` simply has no `"livekit"` transport — a `transport: "livekit"`
/// channel then fails loudly at dispatch (clear error, never a panic).
#[cfg(feature = "livekit")]
fn attach_livekit(registry: TransportRegistry, config: &ExecutorConfig) -> TransportRegistry {
    let Some(livekit) = &config.livekit else {
        return registry;
    };
    info!(url = %livekit.url, "data-plane livekit egress transport enabled (transport=livekit)");
    registry.with_livekit(livekit.clone())
}

/// No-op without the `livekit` feature — the `"livekit"` transport is then simply
/// absent from the registry (and the `get("livekit")` match arm compiles to `None`).
#[cfg(not(feature = "livekit"))]
fn attach_livekit(registry: TransportRegistry, _config: &ExecutorConfig) -> TransportRegistry {
    registry
}

/// Build the artifact store based on config.
///
/// Selection order (additive — a static-storage daemon is unchanged):
/// 1. `config.storage` present → `OpenDalArtifactStore` (in-cluster worker, the
///    `opendal` feature) or a local store. Static storage ALWAYS wins.
/// 2. No `config.storage`, but the runner enrolled against a zero-secret broker
///    (a `runner_broker_base()` + a readable `runner.token`) →
///    `BrokeredArtifactStore`: all blob I/O proxies through
///    `{base}/api/storage/blob` authenticated with the runner's bearer token, no
///    cloud credentials on the box.
/// 3. Otherwise → `LocalArtifactStore` at `base_dir` (the historical default).
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

    // Zero-secret runner: no static storage, but enrolled against a broker.
    // Prefer the brokered blob proxy over a local store so artifacts land in the
    // platform object store reachable by in-cluster readers.
    if let Some(broker_base) = config.runner_broker_base() {
        if let Some(token) = read_runner_token(config, base_dir) {
            info!(
                base = %broker_base,
                "building brokered artifact store (zero-secret runner blob proxy)"
            );
            let store = BrokeredArtifactStore::new(broker_base, token, reqwest::Client::new());
            return Ok(Some(Arc::new(store)));
        }
        warn!(
            base = %broker_base,
            "runner broker base configured but no readable runner token; \
             falling back to local artifact store"
        );
    }

    Ok(Some(Arc::new(LocalArtifactStore::new(
        base_dir.to_path_buf(),
    ))))
}

/// Read the runner bearer token (`rnr_<uuid>.<secret>`) for the brokered store.
/// Prefers the resolved `config.runner_token_path` (set in `normalize()`), else
/// the conventional `{base_dir}/runner/runner.token`. Returns the trimmed token
/// on success, `None` when the file is absent/unreadable.
fn read_runner_token(config: &ExecutorConfig, base_dir: &Path) -> Option<String> {
    let path = config
        .runner_token_path
        .clone()
        .unwrap_or_else(|| base_dir.join("runner").join("runner.token"));
    std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Assemble the brokered secret-unwrap config for the staging pipeline, when
/// this daemon is a zero-secret runner: a `runner_broker_base()`, a `runner_id`,
/// and a readable `runner.token` must all resolve. `None` otherwise (an
/// in-cluster worker; `PlanSecretsHook` then never reroutes through mekhan).
fn build_broker_secrets(
    config: &ExecutorConfig,
    base_dir: &Path,
) -> Option<aithericon_executor_worker::staging::BrokerSecretsConfig> {
    let base_url = config.runner_broker_base()?;
    let runner_id = config.runner_id.clone()?;
    let runner_token = read_runner_token(config, base_dir)?;
    Some(aithericon_executor_worker::staging::BrokerSecretsConfig {
        base_url,
        runner_token,
        runner_id,
    })
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
    // Metrics are ON by default: the executor always holds a NATS client, so the
    // NATS metric sink is wired unless explicitly disabled. An absent `[metrics]`
    // block (`config.metrics = None`) means "use defaults" (enabled + NATS sink),
    // NOT "off" — only an explicit `enabled = false` turns metrics off.
    let metrics_config = match &config.metrics {
        Some(cfg) if !cfg.enabled => return Ok(None),
        Some(cfg) => cfg.clone(),
        None => MetricsConfig::default(),
    };

    // No sinks listed → default to the NATS sink (the only transport that reaches
    // mekhan; Memory stays on the runner, Loki needs a URL). This is what makes a
    // bare runner's metrics — e.g. the file-ops crawl's files/sec — show up in the
    // run's Metrics tab with zero config.
    let sink_configs: Vec<MetricSinkConfig> = if metrics_config.sinks.is_empty() {
        vec![MetricSinkConfig::Nats]
    } else {
        metrics_config.sinks.clone()
    };

    let mut sinks: Vec<Arc<dyn MetricSink>> = Vec::new();

    for sink_config in &sink_configs {
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
