use std::time::Duration;

use aithericon_executor_backend::SandboxConfig;
use aithericon_executor_logs::LogsConfig;
use aithericon_executor_metrics::MetricsConfig;
use aithericon_executor_storage::StorageConfig;
use serde::Deserialize;
use sysinfo::System;

use crate::nix::NixConfig;

/// Where the executor gets its jobs from.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobSource {
    /// Pull jobs from NATS JetStream via apalis queue.
    #[default]
    NatsQueue,
    /// Read jobs from a manifest file.
    Manifest,
}

/// How long the executor process lives.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Lifetime {
    /// Run indefinitely until shutdown signal.
    #[default]
    Daemon,
    /// Process available work then exit.
    RunToCompletion,
}

/// Policy for cleaning up run directories after execution.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicy {
    /// Remove immediately after execution completes.
    #[default]
    Immediate,
    /// Remove only on successful execution; retain on failure for debugging.
    OnSuccess,
    /// Never clean up automatically.
    Retain,
}

/// Configuration for the executor service.
///
/// Loaded via config-rs: defaults → optional config file → environment variables.
/// Env vars use `EXECUTOR_` prefix (e.g., `EXECUTOR_NATS_URL`, `EXECUTOR_CONCURRENCY`).
#[derive(Debug, Clone, Deserialize)]
pub struct ExecutorConfig {
    /// Base directory for run directories and artifact storage.
    #[serde(default = "default_base_dir")]
    pub base_dir: String,

    /// NATS server URL.
    #[serde(default = "default_nats_url")]
    pub nats_url: String,

    /// Path to NATS credentials file (.creds) for authenticated connections.
    /// When set, the executor uses `connect_with_credentials` instead of anonymous connect.
    #[serde(default)]
    pub nats_creds: Option<String>,

    /// Name of this executor instance (used as `source` in StatusUpdate).
    #[serde(default = "default_name")]
    pub name: String,

    /// Apalis namespace for job streams.
    #[serde(default = "default_namespace")]
    pub namespace: String,

    /// Optional subject/stream isolation prefix for status + event publishing.
    ///
    /// `None` (default) → the global `EXECUTOR_STATUS`/`EXECUTOR_EVENTS` streams
    /// and bare `executor.status.>` subjects (production). `Some(pfx)` →
    /// `STATUS_{pfx}`/`EVENTS_{pfx}` streams and `{pfx}.executor.status.>`
    /// subjects, matching the per-test isolation `ExecutorTestContext` uses.
    /// Set via `EXECUTOR_SUBJECT_PREFIX`; used by the sandbox e2e to point a
    /// containerized executor at a test's UUID-prefixed streams.
    #[serde(default)]
    pub subject_prefix: Option<String>,

    /// Number of concurrent jobs this executor can handle.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,

    /// Default timeout for executions (seconds) when job doesn't specify one.
    #[serde(default = "default_timeout_secs")]
    pub default_timeout_secs: u64,

    /// Maximum bytes to capture per output stream (stdout/stderr).
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,

    /// Maximum serialized byte size of a single **inline** output value
    /// (`set_output`/`path`-output) before the producer hard-errors the step.
    /// Distinct from `max_output_bytes` (the stdout/stderr tail cap). See
    /// [`crate::executor::DEFAULT_MAX_OUTPUT_INLINE_BYTES`].
    #[serde(default = "default_max_output_inline_bytes")]
    pub max_output_inline_bytes: usize,

    /// NATS client ping interval in seconds.
    ///
    /// Default 15s — short enough to keep WAN connections alive through idle-terminating
    /// load balancers (Hetzner LB and similar drop TCP connections at ~30-60s of silence),
    /// which would otherwise leave the executor's `messages()` pull stream silently dead
    /// and queued jobs undelivered. Set to 60+ for purely-local NATS where the default
    /// async-nats interval is fine.
    #[serde(default = "default_nats_ping_interval_secs")]
    pub nats_ping_interval_secs: u64,

    /// Apalis ack_wait in seconds — how long before unacked jobs are redelivered.
    #[serde(default = "default_ack_wait_secs")]
    pub ack_wait_secs: u64,

    /// Interval in seconds for progress heartbeats (extends ack_wait during execution).
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,

    /// Maximum delivery attempts before DLQ.
    #[serde(default = "default_max_deliver")]
    pub max_deliver: i64,

    /// Maximum un-acked messages JetStream will deliver to this consumer at once.
    ///
    /// A serial worker (`concurrency = 1`) that holds a large prefetch buffer can
    /// have queued-but-unstarted messages redelivered while it is blocked on a
    /// slow job (e.g. a cold Python venv build) — they exceed their ack window
    /// before the worker even reaches them. Bounding this to the worker's
    /// parallelism prevents that over-delivery. The lease drain executor sets it
    /// to `1` (`EXECUTOR_MAX_ACK_PENDING`); the daemon keeps the larger default
    /// for pull-pipelining throughput.
    #[serde(default = "default_max_ack_pending")]
    pub max_ack_pending: i64,

    /// Number of replicas for the status stream.
    #[serde(default = "default_status_replicas")]
    pub status_replicas: usize,

    /// Policy for cleaning up run directories after execution.
    #[serde(default)]
    pub cleanup_policy: CleanupPolicy,

    /// Storage backend configuration.
    ///
    /// When `None`, defaults to `LocalArtifactStore` at `{base_dir}`.
    /// When set, builds an OpenDAL-backed store (requires `opendal` feature).
    ///
    /// Environment variables: `EXECUTOR_STORAGE_BACKEND`, `EXECUTOR_STORAGE_ENDPOINT`, etc.
    /// Config file: `[storage]` section in `executor.toml`.
    #[serde(default)]
    pub storage: Option<StorageConfig>,

    /// LiveKit egress configuration (data channels tagged `transport: "livekit"`).
    ///
    /// When present, the executor mints a publish token and publishes JPEG frames
    /// as a WebRTC VP8 video track into the room `lk_{execution_id}__{channel}`.
    /// `None` (the default) leaves the `livekit` transport absent from the
    /// registry — a `transport: "livekit"` channel then fails loudly at dispatch
    /// rather than silently mis-routing. This struct ALWAYS parses (config is not
    /// feature-gated); the transport itself is gated behind the `livekit` feature.
    /// Config file: `[livekit]` section / `EXECUTOR_LIVEKIT__*` env vars.
    #[serde(default)]
    pub livekit: Option<LiveKitConfig>,

    /// Metrics collection and forwarding configuration.
    ///
    /// Controls which metric sinks are active and buffer limits.
    /// Config file: `[metrics]` section in `executor.toml`.
    #[serde(default)]
    pub metrics: Option<MetricsConfig>,

    /// Log forwarding configuration.
    ///
    /// Controls which log sinks are active and level filtering.
    /// Config file: `[logs]` section in `executor.toml`.
    #[serde(default)]
    pub logs: Option<LogsConfig>,

    /// Nix environment resolution configuration.
    ///
    /// When enabled, jobs can declare Nix packages in `spec.config.nix.packages`
    /// to get a cached, content-addressed environment.
    /// Config file: `[nix]` section in `executor.toml`.
    #[serde(default)]
    pub nix: Option<NixConfig>,

    /// Python venv cache configuration.
    ///
    /// When enabled, Python jobs that request a virtualenv share cache-resident
    /// venvs keyed by `(python_version, sorted(requirements), sdk_marker)`,
    /// symlinked into each run directory. Skips the venv + pip install cold-start
    /// for repeat invocations.
    /// Config file: `[python]` section in `executor.toml`.
    #[serde(default)]
    pub python: Option<PythonCacheConfig>,

    /// Process sandbox (nsjail) configuration.
    ///
    /// When enabled, the `process` and `python` backends run each job inside an
    /// nsjail namespace+cgroup jail (clean env, isolated netns, resource caps).
    /// Default OFF — `None` or `enabled = false` leaves both backends running
    /// exactly as today. Enabling on a non-Linux host (or without `nsjail` on
    /// PATH) is a fail-closed startup error.
    /// Config file: `[sandbox]` section in `executor.toml`.
    #[serde(default)]
    pub sandbox: Option<SandboxSettings>,

    /// Cancellation listener configuration.
    ///
    /// Controls which cancel triggers are active (NATS listener, HTTP endpoint).
    /// Config file: `[cancel]` section in `executor.toml`.
    #[serde(default)]
    pub cancel: CancelConfig,

    /// Where jobs come from: `nats_queue` (default) or `manifest`.
    ///
    /// Environment variable: `EXECUTOR_SOURCE=manifest`.
    #[serde(default)]
    pub source: JobSource,

    /// Process lifetime: `daemon` (default) or `run_to_completion`.
    ///
    /// Environment variable: `EXECUTOR_LIFETIME=run_to_completion`.
    #[serde(default)]
    pub lifetime: Lifetime,

    /// Path to manifest JSON file. Required when `source = manifest`.
    ///
    /// Environment variable: `EXECUTOR_MANIFEST_PATH=./manifest.json`.
    #[serde(default)]
    pub manifest_path: Option<String>,

    /// Stop on first job failure (only meaningful with `run_to_completion`).
    ///
    /// Environment variable: `EXECUTOR_FAIL_FAST=true`.
    #[serde(default)]
    pub fail_fast: bool,

    /// Hard cap on jobs to process before exiting (drain mode).
    ///
    /// Setting this auto-promotes `lifetime` to `RunToCompletion`.
    /// Environment variable: `EXECUTOR_MAX_JOBS=10`.
    #[serde(default)]
    pub max_jobs: Option<u64>,

    /// Minimum jobs before idle shutdown becomes eligible (drain mode).
    ///
    /// Setting this auto-promotes `lifetime` to `RunToCompletion`.
    /// Environment variable: `EXECUTOR_MIN_JOBS=5`.
    #[serde(default)]
    pub min_jobs: Option<u64>,

    /// Idle timeout in seconds for drain mode (how long to wait with no completions).
    ///
    /// Only used when `lifetime = RunToCompletion` and `source = NatsQueue`.
    /// Environment variable: `EXECUTOR_IDLE_TIMEOUT_SECS=30`.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,

    /// When set, the executor runs in **PerJob mode**: it creates an ephemeral
    /// NATS consumer with an exact subject filter `{namespace}.{priority}.{exec_id}`,
    /// pulls its single dispatched message, and exits. No shared-consumer state
    /// is involved, so concurrent one-shot dispatchers (Slurm sbatch, k8s Jobs)
    /// don't race each other. When `None`, the executor joins the shared
    /// **Pool** (durable consumer per priority, wildcard filter) — the legacy
    /// daemon-mode behavior.
    ///
    /// Set by the dispatcher (Slurm sbatch passes
    /// `--export=ALL,EXECUTOR_TARGET_EXEC_ID=<id>` from the engine's
    /// `SubmitRequest.execution_id`). The same id was used by the engine when
    /// publishing the job, so the consumer exact-matches its assigned message.
    ///
    /// Environment variable: `EXECUTOR_TARGET_EXEC_ID=<id>`.
    #[serde(default)]
    pub target_exec_id: Option<String>,

    /// Lab-fleet runner identity (Phase 1 — Lab Runner Fleet).
    ///
    /// When this executor was enrolled into a mekhan fleet via
    /// `aithericon-executor register`, the enrollment persists an
    /// `identity.json` under `{base_dir}/runner/`. `runner_id` is the
    /// control-plane UUID for this runner; `runner_token_path` points at the
    /// `rnr_` bearer-credential file used to authenticate heartbeat/control
    /// calls. Both are **optional** and have **no effect on job draining in
    /// Phase 1** — the worker still pulls from NATS with the existing
    /// shared/anonymous credentials. They are populated either from
    /// `EXECUTOR_RUNNER_ID` / config, or (when unset) auto-discovered from
    /// `{base_dir}/runner/identity.json` in `normalize()`.
    ///
    /// Environment variable: `EXECUTOR_RUNNER_ID=<uuid>`.
    #[serde(default)]
    pub runner_id: Option<String>,

    /// Path to the `rnr_` runner control-plane token (Phase 1 — Lab Runner
    /// Fleet). Defaults to `{base_dir}/runner/runner.token` when a runner
    /// identity is present. Optional; unused by job draining in Phase 1.
    #[serde(default)]
    pub runner_token_path: Option<std::path::PathBuf>,

    /// Interval in seconds between runner presence heartbeats (Phase 3 —
    /// presence-lease pool capacity). When a runner identity is present the
    /// daemon spawns a background task that publishes to
    /// `runner.{runner_id}.presence` on this interval so mekhan can keep the
    /// presence-pool unit alive (and expire it when heartbeats stop). Only
    /// meaningful when `runner_id` is set; ignored otherwise.
    ///
    /// Environment variable: `EXECUTOR_PRESENCE_INTERVAL_SECS=10`.
    #[serde(default = "default_presence_interval_secs")]
    pub presence_interval_secs: u64,

    /// ROS backend configuration (rosbridge connection — runner-local).
    ///
    /// The ROS backend reaches a rosbridge over a WebSocket. Unlike the
    /// resource-bound query backends, the endpoint is configured on the daemon
    /// (the runner advertises a reachable rosbridge) rather than bound per-step
    /// as a workspace resource.
    /// Config file: `[ros]` section in `executor.toml`.
    #[serde(default)]
    pub ros: Option<RosSettings>,

    /// Model-pool node-agent configuration (P2 — model-pool control plane).
    ///
    /// When set (alongside `runner_id` + `mekhan_url`), this daemon runs the
    /// vLLM node agent: it probes a local vLLM engine's served models, publishes
    /// them to mekhan as a runner interface catalog, subscribes to
    /// `runner.{id}.load`/`unload` control commands, and presence-reports the
    /// per-engine concurrency C + loaded model ids. The agent NEVER serves
    /// inference (that is conventional OpenAI HTTP straight to vLLM).
    /// Config file: `[model_agent]` section in `executor.toml`.
    #[serde(default)]
    pub model_agent: Option<ModelAgentSettings>,

    /// Mekhan control-plane base URL (e.g. `https://mekhan.example.com`). Used
    /// by the worker self-enroll path (`worker_reg_token`) to POST
    /// `/api/v1/workers/enroll` on boot, AND by the runner-side ROS interface-
    /// catalog publish (Phase 3) to POST `/api/v1/runners/{id}/interfaces` (the
    /// `rnr_` bearer is read from `runner_token_path`). The CLI runner-enroll
    /// subcommands take `--url` directly; this field is the daemon-boot analog.
    ///
    /// Environment variable: `EXECUTOR_MEKHAN_URL=https://mekhan.example.com`.
    #[serde(default)]
    pub mekhan_url: Option<String>,

    /// Enrolled-worker identity (Phase B — grouped + enrolled workers).
    ///
    /// When this executor was enrolled into a mekhan worker fleet (either by a
    /// boot-time `worker_reg_token` self-enroll, or pre-provisioned config),
    /// `worker_id` is the control-plane UUID for this worker. Unlike `runner_id`
    /// it does **not** become a routing partition — grouped workers COMPETE
    /// within their `worker_group`; the id is identity/presence only. Populated
    /// from `EXECUTOR_WORKER_ID` / config, or auto-discovered from
    /// `{base_dir}/worker/identity.json` in `normalize()`, or set by the
    /// boot-time self-enroll.
    ///
    /// Environment variable: `EXECUTOR_WORKER_ID=<uuid>`.
    #[serde(default)]
    pub worker_id: Option<String>,

    /// Routing group for an enrolled worker (Phase B). When set, the worker-pool
    /// fan-out binds a `PartitionedPool { partition: worker_group }` consumer on
    /// the PARALLEL `executor-<wire>-grp` namespace (filter
    /// `executor-<wire>-grp.{prio}.{group}.>`) instead of the anonymous `Pool`
    /// on `executor-<wire>`. Many workers of the same group share that durable
    /// and COMPETE for the group's jobs. When `worker_reg_token` self-enroll
    /// runs, the group is inherited from the registration token (the enroll
    /// response's `group`) and overrides any configured value.
    ///
    /// Environment variable: `EXECUTOR_WORKER_GROUP=<group>`.
    #[serde(default)]
    pub worker_group: Option<String>,

    /// One-time worker registration token (`wt_<uuid>.<secret>`) for boot-time
    /// self-enrollment (Phase B). When set and the worker is not already
    /// enrolled, the daemon generates a NATS user nkey, POSTs to mekhan
    /// `POST /api/v1/workers/enroll`, and persists the returned `wkr_` token +
    /// scoped NATS `.creds` under `{base_dir}/worker/`. The returned `group`
    /// becomes the routing group (no separate `EXECUTOR_WORKER_GROUP` required
    /// when self-enrolling). Requires `mekhan_url`.
    ///
    /// Environment variable: `EXECUTOR_WORKER_REG_TOKEN=wt_...`.
    #[serde(default)]
    pub worker_reg_token: Option<String>,

    /// The dispatch routing partition for an enrolled worker — the capacity-
    /// resource UUID of the group this worker competes in (unified worker model).
    /// Every worker-pool consumer binds `PartitionedPool { partition:
    /// worker_routing_partition }` on the `executor-<wire>-grp` stream family. It
    /// is workspace-safe by construction (two workspaces' "default" groups never
    /// collide) and is a valid JetStream/NATS subject token (`[0-9a-f-]`, no
    /// dots). Populated by the boot-time self-enroll (the enroll response's
    /// `routing_partition`) or auto-discovered from
    /// `{base_dir}/worker/identity.json` in `normalize()`. There is no anonymous
    /// worker path — a worker-pool daemon without this is a hard config error.
    ///
    /// Environment variable: `EXECUTOR_WORKER_ROUTING_PARTITION=<uuid>`.
    #[serde(default)]
    pub worker_routing_partition: Option<String>,
}

/// LiveKit egress connection settings (`transport: "livekit"`).
///
/// A nested struct (not flat fields) so the documented `EXECUTOR_LIVEKIT__*`
/// env vars bind — config-rs uses `__` as the nesting separator. Holds the
/// LiveKit server WS URL plus the API key/secret the executor uses to mint a
/// publish-scoped room token. Always parses (not feature-gated); the
/// `LiveKitTransport` that consumes it is gated behind the `livekit` feature.
///
/// Environment variables:
/// - `EXECUTOR_LIVEKIT__URL=ws://localhost:7880`
/// - `EXECUTOR_LIVEKIT__API_KEY=devkey`
/// - `EXECUTOR_LIVEKIT__API_SECRET=secret`
///
/// Config file: `[livekit]` section in `executor.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct LiveKitConfig {
    /// LiveKit server WebSocket URL (e.g. `ws://localhost:7880`).
    pub url: String,
    /// LiveKit API key used to mint room tokens.
    pub api_key: String,
    /// LiveKit API secret used to sign room tokens.
    pub api_secret: String,
}

/// ROS backend connection settings.
///
/// A nested struct (not a flat field) so the documented
/// `EXECUTOR_ROS__WS_URL` env var binds — config-rs uses `__` as the nesting
/// separator (see the builder's `.separator("__")`), so a flat `ros_ws_url`
/// field would only catch `EXECUTOR_ROS_WS_URL` and the documented form would
/// silently no-op. Mirrors [`PythonCacheConfig`] / [`SandboxSettings`].
///
/// Environment variable: `EXECUTOR_ROS__WS_URL=ws://host:9090`.
/// Config file: `[ros]` section in `executor.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct RosSettings {
    /// rosbridge WebSocket URL. When unset the [`ExecutorConfig::ros_ws_url`]
    /// helper defaults to `ws://localhost:9090` (rosbridge's default port).
    #[serde(default)]
    pub ws_url: Option<String>,
}

/// Model-pool node-agent settings (P2 — model-pool control plane).
///
/// A nested struct (not flat fields) so the documented `EXECUTOR_MODEL_AGENT__*`
/// env vars bind — config-rs uses `__` as the nesting separator (see the
/// builder's `.separator("__")`), so a flat `model_agent_vllm_url` field would
/// only catch `EXECUTOR_MODEL_AGENT_VLLM_URL` and the documented form would
/// silently no-op. Mirrors [`RosSettings`] / [`SandboxSettings`].
///
/// Environment variables:
/// - `EXECUTOR_MODEL_AGENT__VLLM_URL=http://localhost:8000`
/// - `EXECUTOR_MODEL_AGENT__SERVED_BASE_MODEL=meta-llama/Llama-3-8B`
/// - `EXECUTOR_MODEL_AGENT__MAX_NUM_SEQS=256`
///
/// Config file: `[model_agent]` section in `executor.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelAgentSettings {
    /// Which control-plane backend this node agent drives: `"vllm"` (default) or
    /// `"ollama"`. Selects how load/unload/probe map onto the server's API —
    /// vLLM admin surface (LoRA + sleep/wake) vs Ollama runtime (base
    /// warm/evict via `keep_alive`, the Metal-native path on Apple Silicon).
    #[serde(default)]
    pub backend: Option<String>,

    /// The model server's base URL. For `backend = "vllm"` this is the vLLM
    /// OpenAI server (e.g. `http://localhost:8000`); for `backend = "ollama"`
    /// the Ollama server (e.g. `http://localhost:11434`). The agent drives this
    /// server's ADMIN surface only (load/unload/probe) — never inference.
    /// Required for the agent to run. (Name kept as `vllm_url` for config
    /// stability; it is the endpoint for whichever backend is selected.)
    pub vllm_url: String,

    /// The served base model id, for labelling/override when the `/v1/models`
    /// probe is ambiguous or unavailable. Advisory.
    #[serde(default)]
    pub served_base_model: Option<String>,

    /// The GDPR residency zone this node serves inference from (e.g.
    /// `"eu-dev"`). Advertised verbatim in the runner's interface catalog so the
    /// inference router can fail-closed on a residency mismatch. `None` when the
    /// node is zone-agnostic. Advisory; opaque to the engine.
    #[serde(default)]
    pub residency_zone: Option<String>,

    /// The per-engine concurrency budget C (`=--max-num-seqs`). C is NOT in
    /// `/v1/models` (it is a vLLM launch arg), so the agent sources it from here
    /// and attributes it to the served base only (LoRA adapters share the
    /// base's budget). Reported on the presence heartbeat + the base catalog
    /// entry.
    #[serde(default)]
    pub max_num_seqs: Option<u32>,
}

/// On-disk runner identity persisted by `aithericon-executor register`.
///
/// Written to `{base_dir}/runner/identity.json` at enroll time. Read back in
/// `ExecutorConfig::normalize()` to self-identify the daemon (Phase 1). The
/// field names match the `register` subcommand's writer exactly.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct RunnerIdentity {
    pub runner_id: String,
    #[serde(default)]
    pub pool: Option<String>,
    pub workspace_id: String,
    /// Public NATS connect URL brokered by mekhan at enroll (the Traefik
    /// WebSocket front door, e.g. `wss://nats.aithericon.eu`). Persisted so a
    /// bare daemon defaults its `nats_url` here and needs no `EXECUTOR_NATS_URL`.
    /// `None` on identities written before brokering, or when mekhan had no
    /// public URL configured.
    #[serde(default)]
    pub nats_url: Option<String>,
}

/// On-disk enrolled-worker identity persisted by the boot-time self-enroll
/// (Phase B — grouped + enrolled workers).
///
/// Written to `{base_dir}/worker/identity.json` when a `worker_reg_token`
/// enrollment succeeds. Read back in `ExecutorConfig::normalize()` to
/// self-identify the daemon (so a restart re-uses the same `wkr_` identity +
/// group without re-enrolling). Mirrors mekhan's `EnrolledWorker` fields that
/// the daemon needs locally; `worker_id` is identity/presence only (NOT a
/// routing partition — grouped workers compete within `group`).
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct WorkerIdentity {
    pub worker_id: String,
    /// Display-only group alias (the human-facing group name).
    #[serde(default)]
    pub group: Option<String>,
    /// The capacity-resource UUID this worker's grouped consumer binds as its
    /// partition token (`executor-<wire>-grp.<prio>.<routing_partition>.>`) — the
    /// unified dispatch routing key. Optional on disk only for forward-read of
    /// pre-unification identities; the daemon hard-errors when it resolves to
    /// nothing (no anonymous worker path).
    #[serde(default)]
    pub routing_partition: Option<String>,
    pub workspace_id: String,
}

/// Configuration for the shared Python venv cache.
///
/// Environment variables: `EXECUTOR_PYTHON__ENABLED`, `EXECUTOR_PYTHON__CACHE_DIR`,
/// `EXECUTOR_PYTHON__PREFER_UV`. Config file: `[python]` section in `executor.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct PythonCacheConfig {
    /// Whether the venv cache is active. Default: false (preserves existing
    /// per-execution venv-build behavior until opted in).
    #[serde(default)]
    pub enabled: bool,

    /// Directory holding cached venvs. Defaults to `{base_dir}/python-venvs/`.
    #[serde(default)]
    pub cache_dir: Option<String>,

    /// Whether to use `uv` for venv creation and pip install when available.
    /// When `uv` is missing from PATH, the cache transparently falls back to
    /// `python -m venv` + `pip install`. Default: true.
    #[serde(default = "default_prefer_uv")]
    pub prefer_uv: bool,
}

impl Default for PythonCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cache_dir: None,
            prefer_uv: default_prefer_uv(),
        }
    }
}

fn default_prefer_uv() -> bool {
    true
}

/// Executor-wide sandbox (nsjail) settings.
///
/// Mirrors the `[nix]` / `[python]` config blocks: an `Option<SandboxSettings>`
/// on [`ExecutorConfig`], deserialized from the `[sandbox]` section /
/// `EXECUTOR_SANDBOX__*` env vars. Default OFF (`enabled = false`); when
/// enabled it is converted to a backend-side [`SandboxConfig`] at startup via
/// [`SandboxSettings::to_sandbox_config`].
///
/// Memory / fsize limits are expressed here in **MiB** (operator-friendly) and
/// converted to bytes where nsjail wants bytes.
#[derive(Debug, Clone, Deserialize)]
pub struct SandboxSettings {
    /// Whether the sandbox is active. Default: false (preserves existing
    /// unsandboxed process/python execution until opted in).
    #[serde(default)]
    pub enabled: bool,

    /// Memory cap in MiB → nsjail `--cgroup_mem_max` (converted to bytes).
    #[serde(default)]
    pub memory_limit_mb: Option<u64>,

    /// CPU quota in ms per wall-second → `--cgroup_cpu_ms_per_sec`.
    #[serde(default)]
    pub cpu_ms_per_sec: Option<u64>,

    /// Max number of pids → `--cgroup_pids_max`.
    #[serde(default)]
    pub pids_max: Option<u64>,

    /// Max file size the child may create, in MiB → `--rlimit_fsize`.
    #[serde(default)]
    pub rlimit_fsize_mb: Option<u64>,

    /// Max open file descriptors → `--rlimit_nofile`.
    #[serde(default)]
    pub rlimit_nofile: Option<u64>,

    /// When `false` (default), the child runs in an isolated netns. When
    /// `true`, the host netns is shared and `/etc/resolv.conf` is bound RO.
    #[serde(default)]
    pub allow_network: bool,

    /// Size of the private `/tmp` tmpfs, in MiB. Default: 64.
    #[serde(default = "default_tmpfs_size_mb")]
    pub tmpfs_size_mb: u64,

    /// Unprivileged uid (and gid) the child is dropped to. Default: 99999.
    #[serde(default = "default_sandbox_uid")]
    pub sandbox_uid: u32,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            memory_limit_mb: None,
            cpu_ms_per_sec: None,
            pids_max: None,
            rlimit_fsize_mb: None,
            rlimit_nofile: None,
            allow_network: false,
            tmpfs_size_mb: default_tmpfs_size_mb(),
            sandbox_uid: default_sandbox_uid(),
        }
    }
}

impl SandboxSettings {
    /// Convert these operator-facing settings into the backend-side
    /// [`SandboxConfig`] that `run_process` consumes. `memory_limit_mb` is
    /// converted MiB → bytes; `nsjail_bin` defaults to `"nsjail"`; extra
    /// mounts are empty in v1 (see docs/sandbox.md decision #6).
    pub fn to_sandbox_config(&self) -> SandboxConfig {
        SandboxConfig {
            nsjail_bin: "nsjail".into(),
            memory_limit: self.memory_limit_mb.map(|mb| mb * 1024 * 1024),
            cpu_ms_per_sec: self.cpu_ms_per_sec,
            pids_max: self.pids_max,
            rlimit_fsize_mb: self.rlimit_fsize_mb,
            rlimit_nofile: self.rlimit_nofile,
            allow_network: self.allow_network,
            tmpfs_size_mb: self.tmpfs_size_mb,
            sandbox_uid: self.sandbox_uid,
            readonly_mounts: Vec::new(),
            writable_mounts: Vec::new(),
        }
    }
}

fn default_tmpfs_size_mb() -> u64 {
    64
}

fn default_sandbox_uid() -> u32 {
    99999
}

/// Configuration for execution cancellation listeners.
///
/// Environment variables: `EXECUTOR_CANCEL_NATS`, `EXECUTOR_CANCEL_HTTP_PORT`, etc.
/// Config file: `[cancel]` section in `executor.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct CancelConfig {
    /// Enable the NATS cancel listener (subscribes to `executor.cancel.*`).
    /// Default: true — NATS is already required, so this is nearly free.
    #[serde(default = "default_cancel_nats")]
    pub nats: bool,

    /// Enable the HTTP cancel endpoint.
    /// Default: false — requires binding a port, opt-in.
    #[serde(default)]
    pub http: bool,

    /// Port for the HTTP cancel API. Only used when `http` is true.
    #[serde(default = "default_cancel_http_port")]
    pub http_port: u16,

    /// Bind address for the HTTP cancel API.
    #[serde(default = "default_cancel_http_bind")]
    pub http_bind: String,
}

impl Default for CancelConfig {
    fn default() -> Self {
        Self {
            nats: default_cancel_nats(),
            http: false,
            http_port: default_cancel_http_port(),
            http_bind: default_cancel_http_bind(),
        }
    }
}

impl ExecutorConfig {
    /// Load configuration from defaults → optional config file → environment.
    pub fn load() -> Result<Self, config::ConfigError> {
        let config = config::Config::builder()
            // Optional config file (executor.toml in current directory)
            .add_source(config::File::with_name("executor").required(false))
            // Environment variables with EXECUTOR_ prefix.
            // Double underscore separates nesting levels (e.g. EXECUTOR_CANCEL__NATS → cancel.nats).
            // Single underscores are literal (e.g. EXECUTOR_NATS_URL → nats_url).
            .add_source(
                config::Environment::with_prefix("EXECUTOR")
                    .prefix_separator("_")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        config.try_deserialize()
    }

    /// Normalize config after loading: auto-promote lifetime when drain fields are set,
    /// validate constraints.
    pub fn normalize(&mut self) {
        // PerJob mode forces one-shot semantics: the ephemeral consumer has
        // exactly one message, so anything other than RunToCompletion + max_jobs=1
        // would either leave the consumer alive past its purpose or pull more
        // messages than intended. This keeps a single env var (`EXECUTOR_TARGET_EXEC_ID`)
        // sufficient to flip the dispatcher into the right shape.
        if self.target_exec_id.is_some() {
            self.lifetime = Lifetime::RunToCompletion;
            if self.max_jobs.is_none() {
                self.max_jobs = Some(1);
            }
        }

        // Auto-promote lifetime to RunToCompletion when drain fields are set
        if (self.max_jobs.is_some() || self.min_jobs.is_some()) && self.lifetime == Lifetime::Daemon
        {
            self.lifetime = Lifetime::RunToCompletion;
        }

        // Validate max_jobs > 0
        if let Some(max) = self.max_jobs {
            assert!(max > 0, "max_jobs must be > 0, got {max}");
        }

        // Validate max >= min when both are set
        if let (Some(max), Some(min)) = (self.max_jobs, self.min_jobs) {
            assert!(max >= min, "max_jobs ({max}) must be >= min_jobs ({min})");
        }

        // Lab-fleet self-identify (Phase 1). If no explicit runner_id was given
        // (env/config), try to read one from the enrollment file the `register`
        // subcommand wrote at `{base_dir}/runner/identity.json`. This is purely
        // informational in Phase 1 — it does not change job draining. Failure to
        // read/parse is silently ignored (a non-enrolled executor is normal).
        let runner_dir = std::path::Path::new(&self.base_dir).join("runner");
        if self.runner_id.is_none() {
            let identity_path = runner_dir.join("identity.json");
            if let Ok(bytes) = std::fs::read(&identity_path) {
                if let Ok(identity) = serde_json::from_slice::<RunnerIdentity>(&bytes) {
                    self.runner_id = Some(identity.runner_id);
                }
            }
        }
        // Default the token path alongside the identity when we have a runner.
        if self.runner_id.is_some() && self.runner_token_path.is_none() {
            self.runner_token_path = Some(runner_dir.join("runner.token"));
        }

        // Default the NATS URL to the enroll-brokered public URL (the Traefik
        // WebSocket front door) persisted in identity.json, unless the operator
        // set one explicitly. This is what lets a bare `aithericon-executor`
        // connect with zero NATS config after `register` — the runner never has
        // to learn `EXECUTOR_NATS_URL`. Only fills the still-default sentinel;
        // an explicit env/config URL always wins. Read independently of the
        // runner_id discovery above so it applies even when runner_id came from
        // the environment.
        if self.nats_url == default_nats_url() {
            let identity_path = runner_dir.join("identity.json");
            if let Ok(bytes) = std::fs::read(&identity_path) {
                if let Ok(identity) = serde_json::from_slice::<RunnerIdentity>(&bytes) {
                    if let Some(url) = identity
                        .nats_url
                        .map(|u| u.trim().to_owned())
                        .filter(|u| !u.is_empty())
                    {
                        self.nats_url = url;
                    }
                }
            }
        }

        // Phase 3 (presence-lease pool capacity): a registered runner drains a
        // SHARED `runner-jobs` stream, PARTITIONED to its own runner id, so
        // presence-pool grants route to exactly this daemon WITHOUT a JetStream
        // stream per runner (the fleet is unbounded — a stream-per-runner would
        // explode the stream count). The drain namespace is the shared
        // [`RUNNER_JOBS_NAMESPACE`] (= the apalis stream key); the per-runner
        // partition is `runner_id`, applied as the `PartitionedPool` consumer
        // filter `runner-jobs.{prio}.{runner_id}.>` in `build_apalis_nats_config`.
        // The engine producer publishes to `runner-jobs.{prio}.{runner_id}.{exec}`
        // (it parses the grant's `executor_namespace = "runner-jobs/{runner_id}"`
        // into stream + partition). The presence *subject* stays dotted
        // (`runner.{id}.presence`) — that's a core NATS subject, not a stream
        // name. Non-breaking: an explicit `EXECUTOR_NAMESPACE` / config value
        // still wins (we only fill the field when it's still at its
        // `default_namespace()` sentinel), and a non-enrolled daemon keeps the
        // historical `executor_jobs` namespace.
        if self.runner_id.is_some() && self.namespace == default_namespace() {
            self.namespace = RUNNER_JOBS_NAMESPACE.to_string();
        }

        // Phase 2 (lab-runner NATS scoped creds): if the `register` /
        // `refresh-creds` flow wrote a `.creds` file and the operator hasn't
        // explicitly configured `nats_creds`, default to it so a registered
        // runner automatically connects to NATS with its scoped credentials.
        // Non-breaking: only fills an unset field, and a plain local dev NATS
        // (no auth) ignores creds anyway. Independent of `runner_id` discovery
        // so an explicitly-configured runner still gets its creds defaulted.
        if self.nats_creds.is_none() {
            let creds_path = runner_dir.join("runner.creds");
            if creds_path.exists() {
                self.nats_creds = Some(creds_path.to_string_lossy().into_owned());
            }
        }

        // Phase B (grouped + enrolled workers): self-identify from a prior
        // boot-time enrollment. If no explicit `worker_id` was given, read one
        // (plus its inherited `group`) from `{base_dir}/worker/identity.json`
        // (written by the self-enroll path). Failure to read/parse is silently
        // ignored — a non-enrolled worker is the normal back-compat default. The
        // explicit-wins rule mirrors the runner path: an env/config
        // `worker_group` is only filled from disk when it was unset.
        let worker_dir = std::path::Path::new(&self.base_dir).join("worker");
        if self.worker_id.is_none() {
            let identity_path = worker_dir.join("identity.json");
            if let Ok(bytes) = std::fs::read(&identity_path) {
                if let Ok(identity) = serde_json::from_slice::<WorkerIdentity>(&bytes) {
                    self.worker_id = Some(identity.worker_id);
                    if self.worker_group.is_none() {
                        self.worker_group = identity.group;
                    }
                    // The unified routing key (capacity-resource UUID). Filled
                    // from disk only when an explicit env/config value was unset.
                    if self.worker_routing_partition.is_none() {
                        self.worker_routing_partition = identity.routing_partition;
                    }
                }
            }
        }

        // Default `nats_creds` to the enrolled worker's scoped `.creds` when one
        // was persisted (and the operator hasn't set creds explicitly, and the
        // runner path didn't already claim it). Same non-breaking shape as the
        // runner block above.
        if self.nats_creds.is_none() {
            let creds_path = worker_dir.join("worker.creds");
            if creds_path.exists() {
                self.nats_creds = Some(creds_path.to_string_lossy().into_owned());
            }
        }
    }

    pub fn default_timeout(&self) -> Duration {
        Duration::from_secs(self.default_timeout_secs)
    }

    pub fn ack_wait(&self) -> Duration {
        Duration::from_secs(self.ack_wait_secs)
    }

    pub fn nats_ping_interval(&self) -> Duration {
        Duration::from_secs(self.nats_ping_interval_secs)
    }

    pub fn heartbeat_interval(&self) -> Duration {
        Duration::from_secs(self.heartbeat_interval_secs)
    }

    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_secs)
    }

    pub fn presence_interval(&self) -> Duration {
        Duration::from_secs(self.presence_interval_secs)
    }

    /// The rosbridge WebSocket URL for the ROS backend, defaulting to
    /// `ws://localhost:9090` (rosbridge's default port) when unset.
    pub fn ros_ws_url(&self) -> String {
        self.ros
            .as_ref()
            .and_then(|r| r.ws_url.clone())
            .unwrap_or_else(|| "ws://localhost:9090".to_string())
    }

    /// The mekhan base URL for the runner-side catalog publish, or `None` when
    /// unconfigured (the publish is then skipped, never fatal). Reads the flat
    /// `mekhan_url` field (shared with the worker self-enroll path) and trims a
    /// trailing slash so callers can append `/api/v1/...` without doubling it.
    pub fn mekhan_url(&self) -> Option<String> {
        self.mekhan_url
            .clone()
            .map(|u| u.trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty())
    }

    /// The model-pool node-agent settings, or `None` when no `[model_agent]`
    /// block is configured (the agent is then a no-op, never fatal). Gating
    /// mirrors `ros_ws_url`/`mekhan_url`: the agent only runs when this AND
    /// `runner_id` AND `mekhan_url()` all resolve.
    pub fn model_agent(&self) -> Option<&ModelAgentSettings> {
        self.model_agent.as_ref()
    }
}

fn default_base_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    format!("{home}/.aithericon/executor")
}

fn default_nats_url() -> String {
    "nats://localhost:4222".into()
}

fn default_name() -> String {
    let hostname = System::host_name().unwrap_or_else(|| "unknown".into());
    format!("executor-{hostname}")
}

fn default_namespace() -> String {
    "executor_jobs".into()
}

/// Shared apalis stream key for lab-runner-fleet job delivery. A registered
/// runner drains this ONE stream-set (`runner-jobs_{priority}`), partitioned to
/// its own runner id via a `PartitionedPool` consumer filter — so an unbounded
/// fleet shares a single stream-set instead of one stream-set per runner. Must
/// byte-match the engine producer's parse of `executor_namespace =
/// "runner-jobs/{runner_id}"` and mekhan's presence-controller stamp.
pub const RUNNER_JOBS_NAMESPACE: &str = "runner-jobs";

fn default_concurrency() -> usize {
    4
}

fn default_timeout_secs() -> u64 {
    3600
}

fn default_max_output_bytes() -> usize {
    aithericon_executor_backend::DEFAULT_MAX_OUTPUT_BYTES
}

fn default_max_output_inline_bytes() -> usize {
    crate::executor::DEFAULT_MAX_OUTPUT_INLINE_BYTES
}

fn default_ack_wait_secs() -> u64 {
    120
}

fn default_nats_ping_interval_secs() -> u64 {
    15
}

fn default_heartbeat_interval_secs() -> u64 {
    30
}

fn default_max_deliver() -> i64 {
    3
}

fn default_max_ack_pending() -> i64 {
    // Matches the apalis-nats default; preserves the daemon's pull-pipelining.
    // The lease drain executor overrides to 1 via EXECUTOR_MAX_ACK_PENDING.
    100
}

fn default_status_replicas() -> usize {
    1
}

fn default_idle_timeout_secs() -> u64 {
    30
}

fn default_presence_interval_secs() -> u64 {
    10
}

fn default_cancel_nats() -> bool {
    true
}

fn default_cancel_http_port() -> u16 {
    9090
}

fn default_cancel_http_bind() -> String {
    "0.0.0.0".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ExecutorConfig {
        ExecutorConfig {
            base_dir: default_base_dir(),
            nats_url: default_nats_url(),
            nats_creds: None,
            name: default_name(),
            namespace: default_namespace(),
            subject_prefix: None,
            concurrency: default_concurrency(),
            default_timeout_secs: default_timeout_secs(),
            max_output_bytes: default_max_output_bytes(),
            max_output_inline_bytes: default_max_output_inline_bytes(),
            ack_wait_secs: default_ack_wait_secs(),
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            max_deliver: default_max_deliver(),
            max_ack_pending: default_max_ack_pending(),
            status_replicas: default_status_replicas(),
            cleanup_policy: CleanupPolicy::default(),
            storage: None,
            livekit: None,
            metrics: None,
            logs: None,
            nix: None,
            python: None,
            sandbox: None,
            cancel: CancelConfig::default(),
            source: JobSource::default(),
            lifetime: Lifetime::default(),
            manifest_path: None,
            fail_fast: false,
            max_jobs: None,
            min_jobs: None,
            idle_timeout_secs: default_idle_timeout_secs(),
            nats_ping_interval_secs: default_nats_ping_interval_secs(),
            target_exec_id: None,
            runner_id: None,
            runner_token_path: None,
            presence_interval_secs: default_presence_interval_secs(),
            ros: None,
            model_agent: None,
            mekhan_url: None,
            worker_id: None,
            worker_group: None,
            worker_reg_token: None,
            worker_routing_partition: None,
        }
    }

    #[test]
    fn defaults_are_sane() {
        let config = test_config();

        assert_eq!(config.nats_url, "nats://localhost:4222");
        assert_eq!(config.source, JobSource::NatsQueue);
        assert_eq!(config.lifetime, Lifetime::Daemon);
        assert_eq!(config.namespace, "executor_jobs");
        assert_eq!(config.concurrency, 4);
        assert_eq!(config.default_timeout(), Duration::from_secs(3600));
        assert_eq!(config.ack_wait(), Duration::from_secs(120));
        assert_eq!(config.heartbeat_interval(), Duration::from_secs(30));
        assert_eq!(config.idle_timeout(), Duration::from_secs(30));
        assert!(config.name.starts_with("executor-"));
        assert!(config.max_jobs.is_none());
        assert!(config.min_jobs.is_none());
    }

    #[test]
    fn normalize_promotes_lifetime_with_max_jobs() {
        let mut config = test_config();
        config.max_jobs = Some(5);
        assert_eq!(config.lifetime, Lifetime::Daemon);
        config.normalize();
        assert_eq!(config.lifetime, Lifetime::RunToCompletion);
    }

    #[test]
    fn normalize_promotes_lifetime_with_min_jobs() {
        let mut config = test_config();
        config.min_jobs = Some(3);
        assert_eq!(config.lifetime, Lifetime::Daemon);
        config.normalize();
        assert_eq!(config.lifetime, Lifetime::RunToCompletion);
    }

    #[test]
    fn normalize_noop_when_already_run_to_completion() {
        let mut config = test_config();
        config.lifetime = Lifetime::RunToCompletion;
        config.max_jobs = Some(5);
        config.normalize();
        assert_eq!(config.lifetime, Lifetime::RunToCompletion);
    }

    #[test]
    fn normalize_noop_when_no_drain_fields() {
        let mut config = test_config();
        config.normalize();
        assert_eq!(config.lifetime, Lifetime::Daemon);
    }

    #[test]
    #[should_panic(expected = "max_jobs must be > 0")]
    fn normalize_rejects_zero_max_jobs() {
        let mut config = test_config();
        config.max_jobs = Some(0);
        config.normalize();
    }

    #[test]
    #[should_panic(expected = "max_jobs (2) must be >= min_jobs (5)")]
    fn normalize_rejects_max_less_than_min() {
        let mut config = test_config();
        config.max_jobs = Some(2);
        config.min_jobs = Some(5);
        config.normalize();
    }

    #[test]
    fn normalize_target_exec_id_forces_one_shot() {
        let mut config = test_config();
        assert_eq!(config.lifetime, Lifetime::Daemon);
        assert!(config.max_jobs.is_none());

        config.target_exec_id = Some("exec-abc".into());
        config.normalize();

        assert_eq!(config.lifetime, Lifetime::RunToCompletion);
        assert_eq!(config.max_jobs, Some(1));
    }

    #[test]
    fn normalize_defaults_namespace_to_runner_when_identity_present() {
        let mut config = test_config();
        config.runner_id = Some("rnr-123".into());
        assert_eq!(config.namespace, default_namespace());
        config.normalize();
        // Shared stream key (partition = runner_id is applied at consumer build,
        // not baked into the namespace) — see RUNNER_JOBS_NAMESPACE.
        assert_eq!(config.namespace, RUNNER_JOBS_NAMESPACE);
    }

    #[test]
    fn normalize_defaults_nats_url_from_brokered_identity() {
        // A runner enrolled via mekhan persists the brokered ws front-door URL
        // in identity.json; a bare daemon must default `nats_url` to it (zero
        // NATS config post-enroll).
        let tmp = std::env::temp_dir().join(format!("exec-broker-url-{}", std::process::id()));
        let runner_dir = tmp.join("runner");
        std::fs::create_dir_all(&runner_dir).unwrap();
        std::fs::write(
            runner_dir.join("identity.json"),
            br#"{"runner_id":"rnr-1","workspace_id":"ws-1","nats_url":"wss://nats.aithericon.eu"}"#,
        )
        .unwrap();

        let mut config = test_config();
        config.base_dir = tmp.to_string_lossy().into_owned();
        assert_eq!(config.nats_url, default_nats_url());
        config.normalize();

        assert_eq!(config.nats_url, "wss://nats.aithericon.eu");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn normalize_explicit_nats_url_wins_over_brokered() {
        // An operator-set EXECUTOR_NATS_URL must not be clobbered by the brokered
        // value on disk — only the still-default sentinel is filled.
        let tmp =
            std::env::temp_dir().join(format!("exec-broker-url-explicit-{}", std::process::id()));
        let runner_dir = tmp.join("runner");
        std::fs::create_dir_all(&runner_dir).unwrap();
        std::fs::write(
            runner_dir.join("identity.json"),
            br#"{"runner_id":"rnr-1","workspace_id":"ws-1","nats_url":"wss://nats.aithericon.eu"}"#,
        )
        .unwrap();

        let mut config = test_config();
        config.base_dir = tmp.to_string_lossy().into_owned();
        config.nats_url = "nats://my-own-nats:4222".into();
        config.normalize();

        assert_eq!(config.nats_url, "nats://my-own-nats:4222");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn normalize_keeps_explicit_namespace_over_runner_default() {
        let mut config = test_config();
        config.runner_id = Some("rnr-123".into());
        config.namespace = "custom_ns".into();
        config.normalize();
        assert_eq!(
            config.namespace, "custom_ns",
            "an explicitly-set namespace wins over the runner.{{id}} default"
        );
    }

    #[test]
    fn normalize_namespace_unchanged_without_runner_identity() {
        let mut config = test_config();
        // Isolate from the developer machine: the default base_dir is
        // $HOME/.aithericon/executor, where a real `register` enrollment may
        // have left a runner/identity.json that normalize() would discover.
        let tmp = std::env::temp_dir().join(format!(
            "exec-worker-no-identity-test-{}",
            std::process::id()
        ));
        config.base_dir = tmp.to_string_lossy().into_owned();
        config.normalize();
        assert_eq!(config.namespace, default_namespace());
    }

    #[test]
    fn normalize_discovers_worker_identity_and_group_from_disk() {
        // Write a {base_dir}/worker/identity.json and confirm normalize() picks
        // up worker_id + the inherited group + the routing partition, and
        // defaults nats_creds to the worker.creds file when present.
        let tmp = std::env::temp_dir().join(format!("exec-worker-test-{}", std::process::id()));
        let worker_dir = tmp.join("worker");
        std::fs::create_dir_all(&worker_dir).unwrap();
        std::fs::write(
            worker_dir.join("identity.json"),
            br#"{"worker_id":"wkr-123","group":"xrd_bench","routing_partition":"11111111-1111-1111-1111-111111111111","workspace_id":"ws-1"}"#,
        )
        .unwrap();
        std::fs::write(worker_dir.join("worker.creds"), b"creds").unwrap();

        let mut config = test_config();
        config.base_dir = tmp.to_string_lossy().into_owned();
        assert!(config.worker_id.is_none());
        config.normalize();

        assert_eq!(config.worker_id.as_deref(), Some("wkr-123"));
        assert_eq!(config.worker_group.as_deref(), Some("xrd_bench"));
        assert_eq!(
            config.worker_routing_partition.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(
            config.nats_creds.as_deref(),
            Some(worker_dir.join("worker.creds").to_string_lossy().as_ref())
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn normalize_explicit_worker_group_wins_over_disk() {
        let tmp = std::env::temp_dir().join(format!("exec-worker-test-grp-{}", std::process::id()));
        let worker_dir = tmp.join("worker");
        std::fs::create_dir_all(&worker_dir).unwrap();
        std::fs::write(
            worker_dir.join("identity.json"),
            br#"{"worker_id":"wkr-123","group":"from_disk","workspace_id":"ws-1"}"#,
        )
        .unwrap();

        let mut config = test_config();
        config.base_dir = tmp.to_string_lossy().into_owned();
        config.worker_group = Some("explicit".into());
        config.normalize();

        // worker_id is still discovered, but an explicitly-set group is kept.
        assert_eq!(config.worker_id.as_deref(), Some("wkr-123"));
        assert_eq!(config.worker_group.as_deref(), Some("explicit"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn normalize_no_worker_identity_leaves_routing_unset() {
        // No identity on disk → no worker_id / group / routing_partition. This
        // is a valid CONFIG state (e.g. a runner or PerJob daemon), but a
        // worker-pool daemon in this state is a hard startup error (the daemon
        // enforces mandatory enrollment — see the executor-service binary).
        let tmp =
            std::env::temp_dir().join(format!("exec-worker-test-anon-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut config = test_config();
        config.base_dir = tmp.to_string_lossy().into_owned();
        config.normalize();
        assert!(config.worker_id.is_none());
        assert!(config.worker_group.is_none());
        assert!(config.worker_routing_partition.is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn normalize_target_exec_id_respects_explicit_max_jobs() {
        let mut config = test_config();
        config.target_exec_id = Some("exec-abc".into());
        config.max_jobs = Some(3);
        config.normalize();

        assert_eq!(config.lifetime, Lifetime::RunToCompletion);
        assert_eq!(
            config.max_jobs,
            Some(3),
            "an explicitly-set max_jobs is honoured even with target_exec_id"
        );
    }
}
