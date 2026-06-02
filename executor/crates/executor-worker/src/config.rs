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

fn default_concurrency() -> usize {
    4
}

fn default_timeout_secs() -> u64 {
    3600
}

fn default_max_output_bytes() -> usize {
    aithericon_executor_backend::DEFAULT_MAX_OUTPUT_BYTES
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
            ack_wait_secs: default_ack_wait_secs(),
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            max_deliver: default_max_deliver(),
            max_ack_pending: default_max_ack_pending(),
            status_replicas: default_status_replicas(),
            cleanup_policy: CleanupPolicy::default(),
            storage: None,
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
