//! nsjail sandbox isolation contract — e2e gate (phase 5b).
//!
//! Gated behind `TEST_SANDBOX=1` (like `TEST_S3_BUCKET` / `MEKHAN_E2E_ZITADEL`).
//! When the flag is unset every test in this file early-returns *before*
//! touching Docker, so the default `cargo test` run stays green on any host
//! (including non-Linux dev machines that cannot run nsjail at all).
//!
//! ## What this asserts (docs/sandbox.md "Isolation contract asserted")
//!
//! | Property                | Probe                                            | Assertion                                                     |
//! |-------------------------|--------------------------------------------------|--------------------------------------------------------------|
//! | Network denied          | task opens outbound socket / DNS                 | fails by default; **succeeds** with `allow_network=true`     |
//! | FS confinement          | task reads planted `/host-secret` & `/etc/shadow`| not visible; own run_dir read/write **works**                |
//! | Memory cgroup           | task allocates past `memory_limit`               | OOM-killed (Signal / non-zero outcome)                       |
//! | Env scrubbed            | executor has `VAULT_TOKEN=…`; task reads it       | **not** present in child env                                 |
//! | uid separation          | task runs `id -u`                                | returns `sandbox_uid`, not the executor uid                  |
//! | Happy path              | normal echo + python job                         | completes; outputs + status lifecycle intact                 |
//! | IPC survives            | python job calls `set_output` over `ipc.sock`    | output received by sidecar                                   |
//! | No orphan after cancel  | cancel a long sandboxed job                      | nsjail + grandchild both gone (PID-ns torn down)             |
//! | PID cap (optional)      | fork-bomb-ish task                               | hits `pids_max`, host unaffected                             |
//!
//! ## Runtime model (decisions #2, #3, #12)
//!
//! Unlike the in-process [`ExecutorTestContext`] worker used by the other
//! integration suites, the sandbox can only be exercised against a **real
//! Linux + nsjail** executor. So each probe:
//!
//!   1. reuses the shared NATS testcontainer + a per-test UUID-prefixed stream
//!      (via [`ExecutorTestContext`]) for job-push + terminal-status collection;
//!   2. boots the cross-compiled `Dockerfile.sandbox-test` executor image (built
//!      by `just sandbox-test-image`) via testcontainers, in **drain mode**
//!      against that NATS with **host networking**, privileged enough to nest
//!      namespaces, wired to the test's per-prefix NATS isolation
//!      (`EXECUTOR_NATS_URL`/`EXECUTOR_NAMESPACE`/`EXECUTOR_SUBJECT_PREFIX`)
//!      plus the probe's `EXECUTOR_SANDBOX__*` env;
//!   3. asserts the contract row on the terminal `StatusUpdate`.
//!
//! ## What's wired vs. what needs a Linux host
//!
//! The image, the `just sandbox-test-image` recipe, and the container↔NATS
//! plumbing are all in place. To actually run the gate you need: (a) a **Linux
//! host** (host networking + nested namespaces — Docker Desktop/macOS can't do
//! the host-networking hop), (b) the image built locally, and (c) the two opt-in
//! flags `TEST_SANDBOX=1` + `SANDBOX_IMAGE_AVAILABLE=1`. Without those the suite
//! no-ops (every probe early-returns before touching Docker), so the default
//! `cargo test` run stays green on any host. The only remaining runtime TODO is
//! the in-container process-table check in `sandbox_no_orphan_after_cancel`
//! (needs `ContainerAsync::exec`); see that test.

use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/// `true` when the suite is enabled (`TEST_SANDBOX=1`).
fn sandbox_enabled() -> bool {
    std::env::var("TEST_SANDBOX").as_deref() == Ok("1")
}

/// Skip-guard used at the top of every probe. Returns `true` (→ early return)
/// when the suite is disabled, after logging why — mirrors how the S3 / Zitadel
/// gated suites no-op when their env flag is unset.
fn skip_if_disabled(probe: &str) -> bool {
    if !sandbox_enabled() {
        eprintln!(
            "[sandbox] SKIP {probe}: set TEST_SANDBOX=1 (+ build the \
             Dockerfile.sandbox-test image, phase 4) to run the isolation gate"
        );
        return true;
    }
    true_only_on_linux(probe)
}

/// nsjail is Linux-only; even with `TEST_SANDBOX=1` there is nothing to run on
/// a non-Linux host (the container itself is Linux, but the probes that inspect
/// the planted host path / executor uid assume a Linux Docker host).
fn true_only_on_linux(probe: &str) -> bool {
    if !cfg!(target_os = "linux") {
        eprintln!("[sandbox] SKIP {probe}: TEST_SANDBOX=1 but host is not Linux");
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Sandbox test environment (image boot + probe-job plumbing)
// ---------------------------------------------------------------------------

/// The Docker image built by `just sandbox-test-image` → `Dockerfile.sandbox-test`.
/// A thin Linux image carrying a pinned nsjail, python3, nix, and the
/// cross-compiled `aithericon-executor-service`, run in drain mode.
///
/// MUST agree with the justfile `sandbox_test_image` default; both read the
/// `SANDBOX_TEST_IMAGE` env override so the recipe and the test never drift.
fn sandbox_image_ref() -> String {
    std::env::var("SANDBOX_TEST_IMAGE")
        .unwrap_or_else(|_| "aithericon-executor-sandbox-test:latest".into())
}

/// Split the image ref into `(name, tag)` for `GenericImage::new`.
fn sandbox_image_name_tag() -> (String, String) {
    let r = sandbox_image_ref();
    match r.rsplit_once(':') {
        Some((n, t)) => (n.to_string(), t.to_string()),
        None => (r, "latest".to_string()),
    }
}

/// Host path planted inside the test image that the sandboxed task must NOT be
/// able to read (the FS-confinement probe). Baked by the Dockerfile.
const PLANTED_HOST_SECRET: &str = "/host-secret";

/// The unprivileged uid the sandbox drops the child to. Matches both
/// `SandboxSettings::default_sandbox_uid` and the Dockerfile comment so the
/// uid-separation probe, the config default, and the image all agree.
const SANDBOX_UID: u32 = 99999;

/// A planted executor-process env var that must be scrubbed from the child.
/// We give it a vault-token-shaped name+value so the assertion is meaningful.
const PLANTED_VAULT_TOKEN: &str = "s.SANDBOX-TEST-VAULT-TOKEN-must-not-leak";

/// Per-probe sandbox tuning passed to the executor container as
/// `EXECUTOR_SANDBOX__*` env. Defaults are sandbox-ON, network-OFF.
#[derive(Clone)]
struct SandboxEnv {
    enabled: bool,
    allow_network: bool,
    memory_limit_mb: Option<u64>,
    pids_max: Option<u64>,
    sandbox_uid: u32,
    /// Drain cap (`EXECUTOR_MAX_JOBS`): how many jobs the container processes
    /// before exiting. 1 for the contract probes (one job per container); the
    /// startup-overhead benchmark raises it to run N jobs through one boot.
    max_jobs: u64,
}

impl Default for SandboxEnv {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_network: false,
            memory_limit_mb: None,
            pids_max: None,
            sandbox_uid: SANDBOX_UID,
            max_jobs: 1,
        }
    }
}

impl SandboxEnv {
    /// Render to the `(KEY, VALUE)` pairs the executor reads from its config
    /// layer (`EXECUTOR_SANDBOX__*`, nested via `__`). Also plants the
    /// would-be-leaked `VAULT_TOKEN` in the executor's own environment so the
    /// env-scrub probe has something to detect.
    fn to_container_env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            ("EXECUTOR_SANDBOX__ENABLED".into(), self.enabled.to_string()),
            (
                "EXECUTOR_SANDBOX__ALLOW_NETWORK".into(),
                self.allow_network.to_string(),
            ),
            (
                "EXECUTOR_SANDBOX__SANDBOX_UID".into(),
                self.sandbox_uid.to_string(),
            ),
            // Planted secret in the executor env — the sandbox must scrub it.
            ("VAULT_TOKEN".into(), PLANTED_VAULT_TOKEN.into()),
        ];
        if let Some(mb) = self.memory_limit_mb {
            env.push(("EXECUTOR_SANDBOX__MEMORY_LIMIT_MB".into(), mb.to_string()));
        }
        if let Some(p) = self.pids_max {
            env.push(("EXECUTOR_SANDBOX__PIDS_MAX".into(), p.to_string()));
        }
        env
    }
}

/// Boots + owns one sandboxed executor container for a single probe.
///
/// The container is held in `_container` for the lifetime of the probe; the
/// `Drop` impl of `ContainerAsync` tears it down. The `ctx` (NATS streams +
/// job-push + status collection) is the same per-test isolation the rest of the
/// suite uses.
struct SandboxTestEnv {
    ctx: ExecutorTestContext,
    // Held to keep the container alive for the probe's duration.
    #[allow(dead_code)]
    container: Option<testcontainers::ContainerAsync<testcontainers::GenericImage>>,
}

impl SandboxTestEnv {
    /// Boot the sandbox-test executor image, wired to the per-test NATS stream,
    /// with the probe's `EXECUTOR_SANDBOX__*` env.
    ///
    /// When the image isn't available locally (`SANDBOX_IMAGE_AVAILABLE` unset),
    /// [`Self::boot_container`] returns `None` and the probe runs against the
    /// in-process worker spawned by `ctx` — which on a Linux host with nsjail +
    /// `EXECUTOR_SANDBOX__ENABLED` *also* exercises the sandbox, but the
    /// privileged-container path is the canonical gate. See
    /// [`Self::manual_run_recipe`].
    async fn new(sandbox: SandboxEnv) -> Self {
        let ctx = ExecutorTestContext::new().await;
        let container = Self::boot_container(&ctx, &sandbox).await;
        Self { ctx, container }
    }

    /// Build + start the privileged sandbox-test container against `ctx`'s NATS.
    ///
    /// Returns `None` (with a logged reason) when the image isn't available
    /// (`SANDBOX_IMAGE_AVAILABLE` unset), so the gate degrades to "run on the
    /// host's nsjail" rather than panicking on a missing image.
    async fn boot_container(
        ctx: &ExecutorTestContext,
        sandbox: &SandboxEnv,
    ) -> Option<testcontainers::ContainerAsync<testcontainers::GenericImage>> {
        use testcontainers::core::{CgroupnsMode, WaitFor};
        use testcontainers::runners::AsyncRunner;
        use testcontainers::{GenericImage, ImageExt};

        // The executor container must reach the shared NATS testcontainer AND
        // publish into this test's UUID-prefixed streams. We point it at:
        //   - EXECUTOR_NATS_URL    = the shared container's host URL. With host
        //     networking the container shares the host net namespace, so the
        //     testcontainers-mapped host port resolves from inside.
        //   - EXECUTOR_NAMESPACE   = `{prefix}_jobs` — the apalis namespace the
        //     harness pushes jobs into (ExecutorTestContext).
        //   - EXECUTOR_SUBJECT_PREFIX = `{prefix}` — so the executor's
        //     StatusReporter writes to `STATUS_{prefix}` / `{prefix}.executor.
        //     status.>`, exactly where this test's status consumer listens.
        let nats_url =
            aithericon_executor_test_harness::nats::shared_nats_url().await.to_string();
        let job_namespace = format!("{}_jobs", ctx.prefix);
        let subject_prefix = ctx.prefix.clone();

        // Build the container request: privileged + host-cgroups so nsjail can
        // nest user/pid/mount/net/cgroup namespaces; host networking so it can
        // dial the NATS host port; drain-mode executor env; probe-specific
        // EXECUTOR_SANDBOX__* + the planted VAULT_TOKEN.
        let (image_name, image_tag) = sandbox_image_name_tag();
        let mut image = GenericImage::new(&image_name, &image_tag)
            // The executor logs this on stdout (tracing fmt → stdout) once the
            // apalis NATS storage is connected, just before the worker starts
            // consuming — a deterministic readiness signal.
            .with_wait_for(WaitFor::message_on_stdout("apalis NATS storage ready"))
            .with_privileged(true)
            .with_cgroupns_mode(CgroupnsMode::Host)
            .with_cap_add("SYS_ADMIN")
            // Host networking (Linux only) so the testcontainers-mapped NATS
            // host port is reachable as the same URL the harness connected to.
            .with_network("host")
            // Drain mode: pull a bounded number of jobs, then exit. Wire the
            // per-test NATS isolation so the container consumes the harness's
            // jobs and publishes status where the test's consumer reads.
            .with_env_var("EXECUTOR_SOURCE", "nats_queue")
            .with_env_var("EXECUTOR_LIFETIME", "run_to_completion")
            .with_env_var("EXECUTOR_IDLE_TIMEOUT_SECS", "30")
            .with_env_var("EXECUTOR_MAX_JOBS", sandbox.max_jobs.to_string())
            .with_env_var("EXECUTOR_NATS_URL", nats_url)
            .with_env_var("EXECUTOR_NAMESPACE", job_namespace)
            .with_env_var("EXECUTOR_SUBJECT_PREFIX", subject_prefix);

        for (k, v) in sandbox.to_container_env() {
            image = image.with_env_var(k, v);
        }

        // The image must be built locally (`just sandbox-test-image`) AND the
        // host must be Linux (host networking + nested namespaces). Otherwise
        // fall back to the in-process worker path.
        if !sandbox_image_available() {
            eprintln!(
                "[sandbox] image {} not available — \
                 falling back to in-process worker. {}",
                sandbox_image_ref(),
                Self::manual_run_recipe()
            );
            return None;
        }

        match image.start().await {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("[sandbox] failed to start sandbox-test container: {e}");
                None
            }
        }
    }

    /// Documents how to build + run the gate by hand once phase 4 lands.
    fn manual_run_recipe() -> &'static str {
        "Manual run:\n  \
         1. `just executor-sandbox-image`  # zigbuild Linux executor + assemble Dockerfile.sandbox-test\n  \
         2. `docker run --privileged --cgroupns=host --network=host \\\n        \
              -e NATS_URL=nats://127.0.0.1:<port> \\\n        \
              -e EXECUTOR_SOURCE=nats_queue -e EXECUTOR_LIFETIME=run_to_completion \\\n        \
              -e EXECUTOR_SANDBOX__ENABLED=true \\\n        \
              aithericon-executor-sandbox-test:latest`\n  \
         3. `TEST_SANDBOX=1 cargo test -p aithericon-executor-service --test sandbox`"
    }

    /// Spawn the fallback in-process worker (when no container was booted).
    /// On a Linux host with nsjail installed + `EXECUTOR_SANDBOX__ENABLED=true`
    /// in the process env, this still drives the real sandbox; otherwise it runs
    /// unsandboxed (the contract assertions then degrade — see each probe).
    fn spawn_fallback_worker(&self) -> tokio::task::JoinHandle<()> {
        self.ctx.spawn_worker()
    }
}

/// Whether the sandbox-test image exists locally. Gated by
/// `SANDBOX_IMAGE_AVAILABLE=1` so CI can opt-in once phase 4 builds it.
fn sandbox_image_available() -> bool {
    std::env::var("SANDBOX_IMAGE_AVAILABLE").as_deref() == Ok("1")
}

// ---------------------------------------------------------------------------
// Probe job builders
// ---------------------------------------------------------------------------

mod probes {
    use std::collections::HashMap;

    use aithericon_executor_domain::{ExecutionJob, JobPriority};
    #[cfg(feature = "python")]
    use aithericon_executor_domain::OutputDeclaration;
    use aithericon_executor_process::ProcessConfig;

    fn bash_job(eid: &str, script: &str, timeout: Option<std::time::Duration>) -> ExecutionJob {
        ExecutionJob {
            execution_id: eid.to_string(),
            spec: ProcessConfig {
                command: "bash".into(),
                args: vec!["-c".into(), script.into()],
                env: HashMap::new(),
                working_dir: None,
                // Clean env is the sandbox default; never inherit for probes
                // that assert env-scrubbing.
                inherit_env: false,
            }
            .into_spec(),
            metadata: HashMap::new(),
            timeout,
            priority: JobPriority::Medium,
            stream_events: None,
            feed_chunks: false,
            channels: Vec::new(),
            wrapped_secrets: None,
        }
    }

    /// Network probe: attempt an outbound TCP connection (and DNS). Exits non-zero
    /// when the connection fails (sandbox default), zero when it succeeds.
    pub fn network_probe(eid: &str) -> ExecutionJob {
        // `getent hosts` forces DNS; `bash /dev/tcp` forces an outbound socket.
        bash_job(
            eid,
            "getent hosts example.com && \
             (exec 3<>/dev/tcp/1.1.1.1/53) && echo NET_OK",
            Some(std::time::Duration::from_secs(15)),
        )
    }

    /// FS-confinement probe: the planted host secret and `/etc/shadow` must NOT
    /// be readable, while the own run_dir (cwd) must be read+write. Exits non-zero
    /// if either forbidden path is readable; zero only when confinement holds.
    pub fn fs_confinement_probe(eid: &str, planted_host_secret: &str) -> ExecutionJob {
        let script = format!(
            "set -e; \
             if cat {planted} 2>/dev/null; then echo LEAK_HOST_SECRET; exit 21; fi; \
             if cat /etc/shadow 2>/dev/null; then echo LEAK_SHADOW; exit 22; fi; \
             echo own-rundir-write > ./confinement_probe.txt; \
             cat ./confinement_probe.txt; \
             echo FS_CONFINED",
            planted = planted_host_secret,
        );
        bash_job(eid, &script, Some(std::time::Duration::from_secs(15)))
    }

    /// Memory-cgroup probe: allocate well past `memory_limit`. Under the cgroup
    /// cap the OOM killer terminates the child → Signal/non-zero terminal
    /// outcome. `head -c` from /dev/zero into a shell var forces resident pages.
    pub fn memory_probe(eid: &str, alloc_mb: u64) -> ExecutionJob {
        // python is the most portable way to actually touch the pages.
        let script = format!(
            "python3 -c 'a = bytearray({bytes}); \
             [a.__setitem__(i, 1) for i in range(0, len(a), 4096)]; print(\"ALLOC_OK\")'",
            bytes = alloc_mb * 1024 * 1024,
        );
        bash_job(eid, &script, Some(std::time::Duration::from_secs(20)))
    }

    /// Env-scrub probe: the executor has `VAULT_TOKEN` set; the sandboxed child
    /// must NOT see it. Prints the value (empty when scrubbed). We assert on
    /// stdout downstream.
    pub fn env_scrub_probe(eid: &str) -> ExecutionJob {
        bash_job(
            eid,
            r#"echo "VAULT_TOKEN=[${VAULT_TOKEN:-<scrubbed>}]""#,
            Some(std::time::Duration::from_secs(10)),
        )
    }

    /// uid-separation probe: `id -u` must report the configured `sandbox_uid`,
    /// not the (root/0 or executor) uid.
    pub fn uid_probe(eid: &str) -> ExecutionJob {
        bash_job(
            eid,
            r#"echo "UID=$(id -u)""#,
            Some(std::time::Duration::from_secs(10)),
        )
    }

    /// Minimal no-op job for the startup-overhead benchmark — the cheapest
    /// possible workload (`true`), so the measured per-job wall time is
    /// dominated by fixed costs (staging + the nsjail spawn when sandboxed),
    /// not the workload itself.
    pub fn bench_noop_probe(eid: &str) -> ExecutionJob {
        bash_job(eid, "exit 0", Some(std::time::Duration::from_secs(15)))
    }

    /// Happy-path process probe: a plain echo that must complete cleanly with
    /// its stdout intact.
    pub fn happy_echo_probe(eid: &str) -> ExecutionJob {
        bash_job(
            eid,
            "echo sandbox-happy-path",
            Some(std::time::Duration::from_secs(10)),
        )
    }

    /// Long-running probe for the no-orphan-after-cancel assertion. No timeout
    /// (cancellation is the only exit). Spawns a grandchild `sleep` so the test
    /// can assert the whole PID-ns is torn down, not just nsjail.
    pub fn long_running_probe(eid: &str) -> ExecutionJob {
        bash_job(
            eid,
            "sleep 600 & echo \"GRANDCHILD=$!\"; wait",
            None,
        )
    }

    /// Optional PID-cap probe: a bounded fork loop that must hit `pids_max`
    /// without taking down the host. Bounded (no true fork bomb) so a missing
    /// cap can't wedge CI.
    pub fn pid_cap_probe(eid: &str) -> ExecutionJob {
        bash_job(
            eid,
            "for i in $(seq 1 64); do sleep 5 & done; wait; echo FORKED_ALL",
            Some(std::time::Duration::from_secs(15)),
        )
    }

    /// Output declaration for the python IPC `set_output` probe.
    #[cfg(feature = "python")]
    pub fn ipc_output_decl() -> Vec<OutputDeclaration> {
        vec![OutputDeclaration {
            name: "result".into(),
            path: Some("result.json".into()),
            required: true,
            kind: None,
            upload_to: None,
        }]
    }

    /// Python IPC probe: calls `set_output` over the run_dir `ipc.sock`. The
    /// sidecar (outside the sandbox, listening on the RW-bound run_dir socket)
    /// must receive it. Requires the `python` feature.
    #[cfg(feature = "python")]
    pub fn python_ipc_probe(eid: &str) -> ExecutionJob {
        use aithericon_executor_python::PythonConfig;
        ExecutionJob {
            execution_id: eid.to_string(),
            spec: PythonConfig::inline_spec_with_io(
                r#"set_output("result", {"answer": 42, "via": "ipc.sock"})"#,
                vec![],
                ipc_output_decl(),
            ),
            metadata: HashMap::new(),
            timeout: Some(std::time::Duration::from_secs(60)),
            priority: JobPriority::Medium,
            stream_events: None,
            feed_chunks: false,
            channels: Vec::new(),
            wrapped_secrets: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Probe → terminal-status helper
// ---------------------------------------------------------------------------

/// Push a probe job, collect its terminal status (or whatever statuses arrived
/// before timeout). Spawns the fallback in-process worker when no container was
/// booted. Returns the collected `StatusUpdate`s.
async fn run_probe(
    env: &SandboxTestEnv,
    consumer_name: &str,
    eid: &str,
    job: aithericon_executor_domain::ExecutionJob,
    timeout: Duration,
) -> Vec<aithericon_executor_domain::StatusUpdate> {
    let consumer = env.ctx.status_consumer(consumer_name, eid).await;

    // When a real sandbox container is running it is the queue consumer; the
    // in-process fallback only spawns when we couldn't boot the image.
    let worker = if env.container.is_none() {
        Some(env.spawn_fallback_worker())
    } else {
        None
    };

    env.ctx.push_job(job).await;
    let statuses = env.ctx.collect_statuses(&consumer, timeout).await;

    if let Some(w) = worker {
        w.abort();
    }
    statuses
}

// ---------------------------------------------------------------------------
// Probes (one #[tokio::test] per isolation-contract row)
// ---------------------------------------------------------------------------

/// Network denied by default; allowed with `EXECUTOR_SANDBOX__ALLOW_NETWORK=true`.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_network_denied_by_default_and_toggle() {
    if skip_if_disabled("network_denied") {
        return;
    }

    // 1) Default (network DENIED): the outbound socket/DNS must fail → non-terminal-success.
    let denied = SandboxTestEnv::new(SandboxEnv::default()).await;
    let eid = format!("net-deny-{}", Uuid::new_v4().simple());
    let statuses = run_probe(
        &denied,
        "net-deny",
        &eid,
        probes::network_probe(&eid),
        Duration::from_secs(40),
    )
    .await;
    let terminal = statuses
        .last()
        .expect("network-denied probe produced no terminal status");
    // Gate symmetrically with the allow-side positive below: only a real
    // sandboxed container guarantees the netns isolation. On the in-process
    // fallback (image unavailable) the worker may not be sandboxed, so the
    // probe would complete and this assertion would misfire.
    if sandbox_image_available() {
        assert_ne!(
            terminal.status,
            ExecutionStatus::Completed,
            "network must be DENIED by default — probe should NOT complete cleanly; \
             got {:?}",
            terminal.status
        );
    }
    denied.ctx.cleanup().await;

    // 2) allow_network=true: the same probe must now complete cleanly.
    let allowed = SandboxTestEnv::new(SandboxEnv {
        allow_network: true,
        ..SandboxEnv::default()
    })
    .await;
    let eid2 = format!("net-allow-{}", Uuid::new_v4().simple());
    let statuses2 = run_probe(
        &allowed,
        "net-allow",
        &eid2,
        probes::network_probe(&eid2),
        Duration::from_secs(40),
    )
    .await;
    let terminal2 = statuses2
        .last()
        .expect("network-allowed probe produced no terminal status");
    // TODO(sandbox-runtime): with a live image + network egress this asserts
    // Completed. Without egress (offline CI) it may still fail — gate the strict
    // form on SANDBOX_IMAGE_AVAILABLE.
    if sandbox_image_available() {
        assert_eq!(
            terminal2.status,
            ExecutionStatus::Completed,
            "allow_network=true must let the outbound probe complete; got {:?}",
            terminal2.status
        );
    }
    allowed.ctx.cleanup().await;
}

/// FS confinement: planted host secret + `/etc/shadow` unreadable; run_dir RW.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_fs_confinement() {
    if skip_if_disabled("fs_confinement") {
        return;
    }

    let env = SandboxTestEnv::new(SandboxEnv::default()).await;
    let eid = format!("fs-{}", Uuid::new_v4().simple());
    let statuses = run_probe(
        &env,
        "fs-confine",
        &eid,
        probes::fs_confinement_probe(&eid, PLANTED_HOST_SECRET),
        Duration::from_secs(40),
    )
    .await;
    let terminal = statuses
        .last()
        .expect("fs-confinement probe produced no terminal status");

    // The probe exits 0 (Completed) ONLY when both forbidden paths were
    // unreadable AND the run_dir write succeeded. Any leak → non-zero exit.
    if sandbox_image_available() {
        assert_eq!(
            terminal.status,
            ExecutionStatus::Completed,
            "fs confinement broken: planted host secret or /etc/shadow was \
             readable, or run_dir was not writable; got {:?}",
            terminal.status
        );
        let stdout = terminal.detail["stdout_tail"].as_str().unwrap_or("");
        assert!(
            stdout.contains("FS_CONFINED") && stdout.contains("own-rundir-write"),
            "expected confinement marker + run_dir write echo, got: {stdout:?}"
        );
        assert!(
            !stdout.contains("LEAK_HOST_SECRET") && !stdout.contains("LEAK_SHADOW"),
            "host secret / shadow leaked into the sandbox: {stdout:?}"
        );
    }
    env.ctx.cleanup().await;
}

/// Memory cgroup: allocate past `memory_limit` → OOM (Signal / non-zero outcome).
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_memory_cgroup_oom() {
    if skip_if_disabled("memory_cgroup") {
        return;
    }

    // 64 MiB cap, allocate 256 MiB → must OOM.
    let env = SandboxTestEnv::new(SandboxEnv {
        memory_limit_mb: Some(64),
        ..SandboxEnv::default()
    })
    .await;
    let eid = format!("oom-{}", Uuid::new_v4().simple());
    let statuses = run_probe(
        &env,
        "oom",
        &eid,
        probes::memory_probe(&eid, 256),
        Duration::from_secs(40),
    )
    .await;
    let terminal = statuses
        .last()
        .expect("memory probe produced no terminal status");

    if sandbox_image_available() {
        // OOM surfaces as Failed (Signal{signal:9} / non-zero exit) — never a
        // clean Completed.
        assert_ne!(
            terminal.status,
            ExecutionStatus::Completed,
            "allocating past the cgroup memory cap must NOT complete cleanly \
             (expected OOM); got {:?}",
            terminal.status
        );
        // When it's a signal kill the outcome should reflect a Signal or a
        // non-zero exit code (python killed mid-alloc).
        let outcome = &terminal.detail["outcome"];
        let is_oom = outcome["type"] == "signal"
            || (outcome["type"] == "exit_failure"
                && outcome["exit_code"].as_i64().unwrap_or(0) != 0);
        assert!(
            is_oom,
            "expected Signal or non-zero exit from OOM, got outcome: {outcome}"
        );
    }
    env.ctx.cleanup().await;
}

/// Env scrubbed: executor's `VAULT_TOKEN` must not reach the child.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_env_scrubbed() {
    if skip_if_disabled("env_scrubbed") {
        return;
    }

    let env = SandboxTestEnv::new(SandboxEnv::default()).await;
    let eid = format!("env-{}", Uuid::new_v4().simple());
    let statuses = run_probe(
        &env,
        "env-scrub",
        &eid,
        probes::env_scrub_probe(&eid),
        Duration::from_secs(30),
    )
    .await;
    let terminal = statuses
        .last()
        .expect("env-scrub probe produced no terminal status");

    if sandbox_image_available() {
        assert_eq!(terminal.status, ExecutionStatus::Completed);
        let stdout = terminal.detail["stdout_tail"].as_str().unwrap_or("");
        assert!(
            !stdout.contains(PLANTED_VAULT_TOKEN),
            "executor VAULT_TOKEN leaked into the sandboxed child: {stdout:?}"
        );
        assert!(
            stdout.contains("VAULT_TOKEN=[<scrubbed>]"),
            "expected VAULT_TOKEN to be scrubbed (empty) in the child, got: {stdout:?}"
        );
    }
    env.ctx.cleanup().await;
}

/// uid separation: `id -u` returns the configured `sandbox_uid`.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_uid_separation() {
    if skip_if_disabled("uid_separation") {
        return;
    }

    let env = SandboxTestEnv::new(SandboxEnv::default()).await;
    let eid = format!("uid-{}", Uuid::new_v4().simple());
    let statuses = run_probe(
        &env,
        "uid",
        &eid,
        probes::uid_probe(&eid),
        Duration::from_secs(30),
    )
    .await;
    let terminal = statuses
        .last()
        .expect("uid probe produced no terminal status");

    if sandbox_image_available() {
        assert_eq!(terminal.status, ExecutionStatus::Completed);
        let stdout = terminal.detail["stdout_tail"].as_str().unwrap_or("");
        assert!(
            stdout.contains(&format!("UID={SANDBOX_UID}")),
            "expected sandbox uid {SANDBOX_UID}, got: {stdout:?}"
        );
        assert!(
            !stdout.contains("UID=0"),
            "child must NOT run as root (uid 0): {stdout:?}"
        );
    }
    env.ctx.cleanup().await;
}

/// Happy path: a normal echo + a python job complete with intact lifecycle.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_happy_path_process_and_python() {
    if skip_if_disabled("happy_path") {
        return;
    }

    // Process echo.
    let env = SandboxTestEnv::new(SandboxEnv::default()).await;
    let eid = format!("happy-echo-{}", Uuid::new_v4().simple());
    let statuses = run_probe(
        &env,
        "happy-echo",
        &eid,
        probes::happy_echo_probe(&eid),
        Duration::from_secs(30),
    )
    .await;
    let terminal = statuses.last().expect("happy echo produced no status");
    if sandbox_image_available() {
        assert_eq!(
            terminal.status,
            ExecutionStatus::Completed,
            "happy-path echo must complete under the sandbox; got {:?}",
            terminal.status
        );
        let stdout = terminal.detail["stdout_tail"].as_str().unwrap_or("");
        assert!(
            stdout.contains("sandbox-happy-path"),
            "echo stdout missing under sandbox: {stdout:?}"
        );
    }
    env.ctx.cleanup().await;

    // Python happy path (only meaningful with the python feature compiled in).
    #[cfg(feature = "python")]
    {
        let penv = SandboxTestEnv::new(SandboxEnv::default()).await;
        let peid = format!("happy-py-{}", Uuid::new_v4().simple());
        let pstatuses = run_probe(
            &penv,
            "happy-py",
            &peid,
            probes::python_ipc_probe(&peid),
            Duration::from_secs(90),
        )
        .await;
        let pterminal = pstatuses.last().expect("happy python produced no status");
        if sandbox_image_available() {
            assert_eq!(
                pterminal.status,
                ExecutionStatus::Completed,
                "happy-path python must complete under the sandbox; got {:?}",
                pterminal.status
            );
        }
        penv.ctx.cleanup().await;
    }
}

/// IPC survives: a python job's `set_output` over the run_dir `ipc.sock` is
/// received by the sidecar and surfaces in the terminal status outputs.
#[cfg(feature = "python")]
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_ipc_survives() {
    if skip_if_disabled("ipc_survives") {
        return;
    }

    let env = SandboxTestEnv::new(SandboxEnv::default()).await;
    let eid = format!("ipc-{}", Uuid::new_v4().simple());
    let statuses = run_probe(
        &env,
        "ipc",
        &eid,
        probes::python_ipc_probe(&eid),
        Duration::from_secs(90),
    )
    .await;
    let terminal = statuses.last().expect("ipc probe produced no status");

    if sandbox_image_available() {
        assert_eq!(
            terminal.status,
            ExecutionStatus::Completed,
            "python set_output job must complete under the sandbox; got {:?}",
            terminal.status
        );
        let output = &terminal.detail["outputs"]["result"];
        assert_eq!(
            *output,
            serde_json::json!({"answer": 42, "via": "ipc.sock"}),
            "set_output over the sandboxed ipc.sock did not reach the sidecar; \
             got: {output}"
        );
    }
    env.ctx.cleanup().await;
}

/// No orphan after cancel: cancelling a long sandboxed job tears down the whole
/// PID-ns — nsjail + the grandchild `sleep` are both gone.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_no_orphan_after_cancel() {
    use tokio_util::sync::CancellationToken;

    if skip_if_disabled("no_orphan_after_cancel") {
        return;
    }

    let env = SandboxTestEnv::new(SandboxEnv::default()).await;
    let eid = format!("orphan-{}", Uuid::new_v4().simple());
    let consumer = env.ctx.status_consumer("orphan", &eid).await;

    // Cancel listener + (fallback) worker.
    let shutdown = CancellationToken::new();
    let listener = env.ctx.start_cancel_listener(shutdown.clone()).await;
    let worker = if env.container.is_none() {
        Some(env.spawn_fallback_worker())
    } else {
        None
    };

    env.ctx.push_job(probes::long_running_probe(&eid)).await;

    // Wait for the job to register (Running) before cancelling.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut running = false;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if env.ctx.cancel_registry.active_count() > 0 {
            running = true;
            break;
        }
    }
    // With a live container the registry lives in the container, not here; only
    // assert the local-registry precondition on the fallback path.
    if env.container.is_none() {
        assert!(running, "long sandboxed job never registered as running");
    }

    env.ctx.publish_cancel(&eid).await;

    let statuses = env
        .ctx
        .collect_statuses(&consumer, Duration::from_secs(30))
        .await;
    let terminal = statuses.last().expect("cancel probe produced no status");

    if sandbox_image_available() {
        assert_eq!(
            terminal.status,
            ExecutionStatus::Cancelled,
            "cancelled sandboxed job must report Cancelled; got {:?}",
            terminal.status
        );

        // PID-ns teardown assertion: after cancel, no nsjail process and no
        // orphaned `sleep 600` grandchild may remain on the executor host.
        //
        // TODO(sandbox-runtime): inspect the container's process table (via
        // `ContainerAsync::exec` running `pgrep -fa 'nsjail|sleep 600'`) and
        // assert it is empty. Requires the booted container handle; wired once
        // the image lands. The fallback (in-process) path asserts the host's
        // process table directly below.
        assert_no_orphans_on_host();
    } else {
        // Fallback path on a Linux host with nsjail: assert the host has no
        // surviving nsjail / probe grandchild after cancel.
        assert_no_orphans_on_host();
    }

    shutdown.cancel();
    let _ = listener.await;
    if let Some(w) = worker {
        w.abort();
    }
    env.ctx.cleanup().await;
}

/// Optional PID-cap probe: a bounded fork loop hits `pids_max` without harming
/// the host. Marked optional in the contract — only asserted with a live image.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_pid_cap_optional() {
    if skip_if_disabled("pid_cap") {
        return;
    }

    let env = SandboxTestEnv::new(SandboxEnv {
        pids_max: Some(16),
        ..SandboxEnv::default()
    })
    .await;
    let eid = format!("pidcap-{}", Uuid::new_v4().simple());
    let statuses = run_probe(
        &env,
        "pidcap",
        &eid,
        probes::pid_cap_probe(&eid),
        Duration::from_secs(40),
    )
    .await;
    let terminal = statuses.last().expect("pid-cap probe produced no status");

    if sandbox_image_available() {
        // The 64-fork loop must NOT fully succeed under a 16-pid cap.
        assert_ne!(
            terminal.status,
            ExecutionStatus::Completed,
            "fork loop must hit the pids_max cap (not complete all forks); got {:?}",
            terminal.status
        );
    }
    env.ctx.cleanup().await;
}

/// Assert no orphaned nsjail / sandbox grandchild survives on the executor host.
///
/// On the fallback (in-process) path this inspects the *test process host*; on
/// the container path the equivalent check runs inside the container (see the
/// TODO in `sandbox_no_orphan_after_cancel`). Best-effort: a missing `pgrep`
/// (non-Linux) is treated as "nothing to assert".
fn assert_no_orphans_on_host() {
    let out = std::process::Command::new("pgrep")
        .args(["-fa", "sleep 600"])
        .output();
    if let Ok(o) = out {
        let listing = String::from_utf8_lossy(&o.stdout);
        // The probe's grandchild is `sleep 600`; nothing matching it should
        // survive the cancel.
        assert!(
            !listing.contains("sleep 600"),
            "orphaned sandbox grandchild survived cancel: {listing}"
        );
    }
}

// ---------------------------------------------------------------------------
// Startup-overhead benchmark (the per-job nsjail cost question)
// ---------------------------------------------------------------------------

/// (mean, median, p90) of a sample set, in the input unit. Empty → all zero.
fn stats(mut samples: Vec<f64>) -> (f64, f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let median = samples[samples.len() / 2];
    let p90_idx = (((samples.len() as f64) * 0.9) as usize).min(samples.len() - 1);
    (mean, median, samples[p90_idx])
}

/// Push `n` trivial no-op jobs one at a time through `env`'s container,
/// recording each job's submit→terminal wall time in milliseconds. Only
/// `Completed` jobs are sampled. The container's NATS/staging latency is the
/// same sandboxed vs. not, so the *difference* between two runs isolates the
/// nsjail per-job startup cost.
async fn bench_per_job_ms(env: &SandboxTestEnv, n: usize) -> Vec<f64> {
    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        let eid = format!("bench-{i}-{}", Uuid::new_v4().simple());
        let consumer = env.ctx.status_consumer(&format!("bench{i}"), &eid).await;
        let t0 = std::time::Instant::now();
        env.ctx.push_job(probes::bench_noop_probe(&eid)).await;
        let statuses = env
            .ctx
            .collect_statuses(&consumer, Duration::from_secs(30))
            .await;
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if statuses
            .last()
            .map(|s| s.status == ExecutionStatus::Completed)
            .unwrap_or(false)
        {
            samples.push(elapsed_ms);
        }
    }
    samples
}

/// Quantify the sandbox's per-job startup overhead: run N no-op jobs through a
/// sandboxed container and N through a sandbox-disabled one, and report the
/// per-job wall-time delta. Report-only (no hard assert on the delta — absolute
/// timings are host/CI-dependent), so it never flakes the gate.
///
/// Gated behind `SANDBOX_BENCH=1` (on top of the usual `TEST_SANDBOX=1` +
/// `SANDBOX_IMAGE_AVAILABLE=1`) since it boots 2 containers and runs 2·N jobs.
/// Override the sample count with `SANDBOX_BENCH_N` (default 20).
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_startup_overhead_benchmark() {
    if skip_if_disabled("startup_overhead") {
        return;
    }
    if std::env::var("SANDBOX_BENCH").as_deref() != Ok("1") {
        eprintln!("[sandbox] SKIP startup_overhead: set SANDBOX_BENCH=1 to run the benchmark");
        return;
    }
    let n: usize = std::env::var("SANDBOX_BENCH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    // Sandboxed run.
    let on = SandboxTestEnv::new(SandboxEnv {
        max_jobs: n as u64,
        ..SandboxEnv::default()
    })
    .await;
    let on_ms = bench_per_job_ms(&on, n).await;
    on.ctx.cleanup().await;

    // Sandbox-disabled baseline.
    let off = SandboxTestEnv::new(SandboxEnv {
        enabled: false,
        max_jobs: n as u64,
        ..SandboxEnv::default()
    })
    .await;
    let off_ms = bench_per_job_ms(&off, n).await;
    off.ctx.cleanup().await;

    let (m_on, med_on, p90_on) = stats(on_ms.clone());
    let (m_off, med_off, p90_off) = stats(off_ms.clone());
    eprintln!(
        "[sandbox][bench] n={n}  (sandboxed {}/{n} completed, unsandboxed {}/{n} completed)",
        on_ms.len(),
        off_ms.len()
    );
    eprintln!("[sandbox][bench] sandboxed    ms/job: mean={m_on:.1} median={med_on:.1} p90={p90_on:.1}");
    eprintln!("[sandbox][bench] unsandboxed  ms/job: mean={m_off:.1} median={med_off:.1} p90={p90_off:.1}");
    eprintln!(
        "[sandbox][bench] ⇒ nsjail per-job overhead ≈ mean {:.1} ms / median {:.1} ms",
        m_on - m_off,
        med_on - med_off
    );

    // Only assert we actually collected data when a real image ran; never
    // assert on the magnitude (CI timing varies).
    if sandbox_image_available() {
        assert!(
            !on_ms.is_empty() && !off_ms.is_empty(),
            "benchmark collected no completed samples (sandboxed={}, unsandboxed={})",
            on_ms.len(),
            off_ms.len()
        );
    }
}
