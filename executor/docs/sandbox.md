# Sandbox (nsjail) — Implementation Plan

Process-level isolation for the **process** and **python** backends via
[nsjail](https://github.com/google/nsjail). Executor-side only; no mekhan/engine/
compiler/openapi changes. Decisions below were resolved in a design interview
(2026-06-01) and are locked for v1.

## Status: planned, ready to implement

## Why nsjail

The process and Python backends run child processes with **zero isolation** —
same UID as the executor, full filesystem access, full network, the executor's
own environment (NATS creds, vault token) inherited. For FaaS workloads running
user-submitted code, this is insufficient.

nsjail wraps a single command in Linux namespace isolation, cgroups, seccomp-bpf,
and bind-mount restrictions. Single external binary, no daemon — ONCE mode
(`-Mo`) maps directly to our one-command-per-job execution model.

Alternatives (DIY landlock/seccompiler, hakoniwa, bubblewrap, Docker backend)
were considered and rejected; the deciding factor vs. bubblewrap is nsjail's
built-in cgroup support (`--cgroup_mem_max` / `--cgroup_pids_max`), which we rely
on for resource limiting.

## The key architectural fact

**`run_process` in `executor-process/src/child.rs` is the single execution
chokepoint.** Both backends funnel through it:

- `ProcessBackend::execute` → `run_process` (`executor-process/src/lib.rs`)
- `PythonBackend::execute` → `run_process` (`executor-python/src/lib.rs:238`)

So wrapping nsjail at `run_process` covers **both** backends in one edit. The
older design draft's "also modify python/mod.rs" step is unnecessary — Python
already reuses `run_process`. (The `Command::new("python3")` calls scattered
through `executor-python/src/runner.rs` are all unit-test/venv helpers, not the
live exec path.)

## Locked decisions (the interview)

| # | Decision | Choice |
|---|----------|--------|
| 1 | Scope of first cut | **Full vertical slice** (config → backend → `run_process` → real nsjail) + e2e, one branch |
| 2 | e2e runtime | **Docker testcontainer** (Linux + nsjail) is the automated gate; Nomad live-proof is a deferred follow-up |
| 3 | nsjail privilege model | **Root-in-container, nsjail drops child to an unprivileged uid**; test container runs privileged enough to nest namespaces |
| 4 | Config granularity | **Executor-wide only** (`[sandbox]` / `EXECUTOR_SANDBOX__*`). No per-job spec hints — avoids colliding with parallel wire-contract work |
| 5 | Default network | **Deny** (isolated netns); single `allow_network` escape hatch |
| 6 | Mount strategy | **Coarse mounts** + whole `/nix/store` RO when a nix store_path exists. Closure-minimization deferred (TODO) |
| 7 | Failure posture | **Fail closed** — validate at startup, fail jobs (not silently run unsandboxed) at runtime; enabling on non-Linux is a startup error |
| 8 | e2e isolation contract | Assert: network-denied (+allow toggle), fs-confinement, memory cgroup OOM, happy-path intact, IPC survives, uid separation, env-scrubbed. PID-cap optional |
| 9 | Signal/cancel propagation | **Signal the nsjail PID**; rely on PID-ns teardown (nsjail = ns init). Keep `terminate_child` as-is. Add "no orphan after cancel" assertion |
| 10 | Code location | `SandboxConfig` + arg-building in **`executor-backend/src/sandbox.rs`**; `run_process` takes `Option<&SandboxConfig>`; backends get `with_sandbox(...)` |
| 11 | Hardening defaults | new user/pid/mount/ipc/uts/cgroup ns (nsjail `-Mo` defaults), uid drop, isolated netns, private tmpfs `/tmp` (+ minimal `/dev`/`/proc` from `-Mo`), **clean env by default** (`inherit_env`→`--keep_env`), rlimits + cgroup caps. **Deviation from the interview:** a *custom seccomp-bpf policy* and an explicit RO-`/proc` tightening are NOT shipped in v1 — nsjail does not enable seccomp without an explicit policy string, and a python-safe policy is a rabbit hole. The headline isolation properties (network deny, fs confinement, memory cap, env scrub, uid separation) hold without it. Tracked as a hardening follow-up. |
| 12 | e2e binary delivery | **Cross-compile Linux executor on host** (reuse the slurm-up zigbuild path), COPY into a thin nsjail+python+nix image |

## Design

### `executor-backend/src/sandbox.rs` (new)

```rust
pub struct SandboxConfig {
    pub nsjail_bin: String,             // default "nsjail"
    pub memory_limit: Option<u64>,      // bytes  → --cgroup_mem_max
    pub cpu_ms_per_sec: Option<u64>,    // --cgroup_cpu_ms_per_sec
    pub pids_max: Option<u64>,          // --cgroup_pids_max
    pub rlimit_fsize_mb: Option<u64>,   // --rlimit_fsize
    pub rlimit_nofile: Option<u64>,     // --rlimit_nofile
    pub allow_network: bool,            // default false → isolated netns
    pub tmpfs_size_mb: u64,             // private /tmp size
    pub sandbox_uid: u32,               // unprivileged uid the child runs as
    pub readonly_mounts: Vec<PathBuf>,  // extra RO binds
    pub writable_mounts: Vec<PathBuf>,  // extra RW binds
}

impl SandboxConfig {
    /// Startup check: nsjail present + we're on Linux. Fail-closed.
    pub fn validate(&self) -> Result<(), ExecutorError>;

    /// Build the full nsjail argv (incl. `-- command args...`) from the
    /// ProcessConfig + RunContext. Reads run_context.backend_state["nix"]
    /// ["store_path"] to decide /nix/store vs coarse system mounts, and
    /// run_context.env / resolved_env for --env flags.
    pub fn build_nsjail_args(
        &self,
        spec: &ProcessConfig,
        run_context: &RunContext,
    ) -> (String, Vec<String>);
}
```

Default invocation shape (ONCE mode):

```
nsjail -Mo --really_quiet --log_fd 2 \
  --user {sandbox_uid} --group {sandbox_uid} \
  --cwd {run_dir} \
  --bindmount   {run_dir}:{run_dir}          # RW: holds ipc.sock, inputs, outputs
  --tmpfsmount /tmp  --tmpfs_size {bytes} \
  # --- libs: nix-closure path OR coarse fallback ---
  --bindmount_ro /nix/store:/nix/store        # when backend_state.nix.store_path set
  # else:  --bindmount_ro /usr /lib /lib64 /bin
  --bindmount_ro /etc/resolv.conf:/etc/resolv.conf \  # only if allow_network
  --env PATH=... --env PYTHONPATH=... --env AITHERICON_*=... \  # clean env, explicit only
  --cgroup_mem_max {bytes} --cgroup_pids_max {n} --cgroup_cpu_ms_per_sec {n} \
  --rlimit_fsize {mb} --rlimit_nofile {n} \
  # net: isolated by default; --disable_clone_newnet only when allow_network
  -- {command} {args...}
```

- **Clean env by default.** Only spec env + `RunContext.env`/`resolved_env` are
  passed via `--env`. The executor's own environment (vault token, NATS creds) is
  NOT inherited. `spec.inherit_env == true` maps to nsjail `--keep_env`.
- **`/nix/store` RO** when `backend_state["nix"]["store_path"]` is set (the
  closure is self-contained — its own glibc/ld); coarse `/usr,/lib,/lib64,/bin`
  otherwise. `// TODO(sandbox): minimal closure mounts via nix-store -qR` marker
  goes at this branch.
- **IPC works unchanged:** `ipc.sock` lives under the RW run_dir bind.

### `executor-process/src/child.rs` — `run_process`

Add `sandbox: Option<&SandboxConfig>`. When `Some`:

- Build argv via `SandboxConfig::build_nsjail_args()` → `Command::new(nsjail_bin)`
  with those args, instead of `Command::new(spec.command)`.
- Env handled by nsjail `--env` flags, NOT `Command::env()` (so the spawned
  nsjail process doesn't itself forward executor env into the child).
- `--cwd` replaces `Command::current_dir()`.
- **Unchanged:** stdout/stderr piping, `TailBuffer`, timeout, cancellation,
  `kill_on_drop(true)`, and `terminate_child` — these now target the **nsjail
  PID**, which is correct: nsjail is the PID-ns init, SIGTERM is forwarded and
  SIGKILL tears down the whole namespace (no orphaned grandchildren). The `pid`
  in the `Running` status becomes the nsjail PID (documented).

When `None`: existing behavior verbatim.

### `executor-process` / `executor-python` backends

`ProcessBackend` and `PythonBackend` each gain a `sandbox: Option<SandboxConfig>`
field + `with_sandbox(cfg)` builder; pass `self.sandbox.as_ref()` into
`run_process`.

### `executor-worker/src/config.rs` — `ExecutorConfig`

New `pub sandbox: Option<SandboxConfig-ish>` section deserialized from
`[sandbox]` / `EXECUTOR_SANDBOX__*` (mirrors the existing `nix` / `python`
config blocks). Fields: `enabled`, `memory_limit_mb`, `cpu_ms_per_sec`,
`pids_max`, `allow_network`, `tmpfs_size_mb`, `sandbox_uid`, rlimits.

### `executor-service/src/main.rs`

Where backends are constructed (`~:566-577`): if `config.sandbox.enabled`,
build a `SandboxConfig`, call `.validate()?` (fail-closed at startup — exits
non-zero if nsjail missing or non-Linux), and thread it via `.with_sandbox(cfg)`
into `ProcessBackend` and `PythonBackend`.

## Backend coverage — nsjail is not uniform

nsjail wraps the **spawn of a child process that runs untrusted code**, so it
only applies where that actually happens. The backends fall into four groups:

| Group | Backends | Treatment |
|-------|----------|-----------|
| Spawn untrusted child via `run_process` | `process`, `python` | **Covered** by the nsjail wrap (one chokepoint). Any future backend that runs user code through `run_process` inherits it for free. |
| Fully in-process I/O (no child) | `http`, `postgres`, `file_ops`, `smtp`, `llm` core | nsjail N/A — there is no child to wrap. The "untrusted" surface is config (URL/SQL/creds), isolated at other layers (validated query idents + RLS, egress policy, …). |
| Delegates isolation to an external isolator | `docker` | **Covered** — the same `SandboxConfig` *intent* is mapped onto the container's native `HostConfig` (see below), not nsjail. |
| In-process parser of untrusted input | `kreuzberg` (malicious docs), the doc fed to `surya` | Real isolation desire, but **not addressable by the child-wrap** — would need an out-of-process redesign (fork+nsjail the parse) or seccomp; tracked as a follow-up. `surya`/`llm` already run as persistent managed daemons (not ONCE-mode), so they'd need their own containment story. |

### Docker backend parity (`executor-docker::container::apply_sandbox_to_host_config`)

`DockerBackend` gains the same `with_sandbox(...)` builder; when set, the policy
is translated to Docker's native isolation on every container:

- **Security floors (enforced, override per-job `DockerConfig`):** `network_mode
  = "none"` unless `allow_network`; `cap_drop = ["ALL"]`; `security_opt =
  ["no-new-privileges:true"]`; `readonly_rootfs` + a private writable `/tmp`
  tmpfs (the run_dir bind stays RW for outputs/ipc); container runs as the
  unprivileged `sandbox_uid`.
- **Resource ceilings (stricter of job-vs-sandbox):** `memory = min(job,
  sandbox)`; `pids_limit` + `cpu_period`/`cpu_quota` from the sandbox.

Same `EXECUTOR_SANDBOX__*` knobs, same deny-by-default + clean posture; Docker
mechanism instead of nsjail.

## What v1 does NOT do (explicit non-goals / follow-ups)

- **Per-job sandbox overrides** (`spec.config.sandbox.*`). The chokepoint has
  `RunContext` in hand, so a later overlay merges cleanly. Out of scope to avoid
  colliding with parallel spec/wire-contract work.
- **Nix-closure-minimal mounts.** Whole `/nix/store` RO for v1; `nix-store -qR`
  closure + closure cache is the hardening follow-up (TODO marker in place).
- **Custom seccomp policy.** nsjail's default policy for v1.
- **Rootless / unprivileged-userns executor.** Root-in-container for v1.
- **Nomad live-proof.** The Nomad job template gets `EXECUTOR_SANDBOX__*`
  later; this branch ships the Docker-testcontainer gate only.

## e2e testing

Reuse the proven `slurm-up` cross-compile recipe. Deliverables:

1. **`Dockerfile.sandbox-test`** — a thin Linux image: pinned known-good
   `nsjail` (built from source or distro-packaged), `python3`, `nix`. The
   cross-compiled `aithericon-executor-service` is COPYed in. Runs in drain mode
   against the test NATS. Container runs privileged enough to nest namespaces
   (`--privileged` or `--cap-add=SYS_ADMIN --cgroupns=host`).
2. **`just` recipe** to zigbuild the Linux executor + assemble the image.
3. **`executor-service/tests/sandbox.rs`** — gated behind `TEST_SANDBOX=1`
   (like `TEST_S3_BUCKET` / `MEKHAN_E2E_ZITADEL`). Boots the container via
   testcontainers, enqueues isolation-probe jobs over a per-test (UUID-prefixed)
   NATS stream, asserts the contract.

### Isolation contract asserted

| Property | Probe | Assertion |
|----------|-------|-----------|
| Network denied | task opens outbound socket / DNS | fails when default; **succeeds** with `allow_network=true` |
| FS confinement | task reads planted host `/host-secret` & `/etc/shadow` | not visible; own run_dir read/write **works** |
| Memory cgroup | task allocates past `memory_limit` | OOM-killed (Signal/non-zero outcome) |
| Env scrubbed | executor has `VAULT_TOKEN=…`; task reads it | **not** present in child env |
| uid separation | task runs `id -u` | returns `sandbox_uid`, not executor uid |
| Happy path | normal echo + python job | completes; outputs + status lifecycle intact |
| IPC survives | python job calls `set_output` / emits artifact over `ipc.sock` | output received by sidecar |
| No orphan after cancel | cancel a long sandboxed job | nsjail + grandchild both gone (PID-ns torn down) |
| PID cap (optional) | fork-bomb-ish task | hits `pids_max`, host unaffected |

## Phased build order

1. **`SandboxConfig` + `build_nsjail_args` + `validate`** in `executor-backend`,
   with unit tests on argv construction (nix vs coarse, network on/off, env
   scrubbing, mounts). No runtime needed — pure string-building.
2. **Thread through `run_process` + backends** (`Option<&SandboxConfig>`,
   `with_sandbox`). Existing tests stay green (default `None`).
3. **`ExecutorConfig` `[sandbox]` section + `main.rs` wiring** incl. fail-closed
   `validate()` at startup and the non-Linux guard.
4. **Test image + just recipe** (`Dockerfile.sandbox-test`, cross-build).
5. **`executor-service/tests/sandbox.rs`** — the isolation contract, `TEST_SANDBOX=1`.
6. (Follow-up, not this branch) Nomad job-template `EXECUTOR_SANDBOX__*` +
   live-proof; closure-minimal mounts; per-job overrides.
