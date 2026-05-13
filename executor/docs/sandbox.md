# Sandbox (nsjail) — TODO

Process-level sandboxing for the process and Python backends via [nsjail](https://github.com/google/nsjail).

## Status: Not yet implemented

## Why nsjail

The process and Python backends run child processes with zero isolation — same UID, full filesystem access, full network. For FaaS workloads running user-submitted code, this is insufficient.

nsjail wraps a single command in Linux namespace isolation, cgroups, seccomp-bpf, and bind mount restrictions. It's a single external binary with no daemon — ONCE mode (`-Mo`) maps directly to our execution model.

Alternatives considered:
- **DIY** (nix + landlock + seccompiler + cgroups-rs) — maximum control, but reimplements what nsjail already handles (namespace setup ordering, uid mapping, mount propagation edge cases)
- **hakoniwa** — Rust library with the right primitives, but less battle-tested (56 stars vs nsjail's widespread use at Google)
- **bubblewrap** (`bwrap`) — lightweight sandboxer used by Flatpak, provides the same namespace primitives (mount, PID, network, user, seccomp-bpf). Slightly lighter than nsjail but **lacks built-in cgroup support** (memory/CPU/PID limits). Since our `SandboxConfig` relies on `--cgroup_mem_max`, `--cgroup_cpu_ms_per_sec`, and `--cgroup_pids_max`, nsjail is the better fit — switching to bwrap would mean reimplementing resource limiting via systemd slices or direct cgroup manipulation
- **Docker backend** — already provides container isolation, but startup overhead is too high for FaaS throughput

## Design

### New module: `executor-backend/src/sandbox.rs`

```rust
pub struct SandboxConfig {
    /// Path to nsjail binary (default: "nsjail")
    pub nsjail_bin: String,

    /// Memory limit in bytes (--cgroup_mem_max)
    pub memory_limit: Option<u64>,

    /// CPU milliseconds per second (--cgroup_cpu_ms_per_sec, e.g. 500 = 50%)
    pub cpu_ms_per_sec: Option<u64>,

    /// Max PIDs inside sandbox (--cgroup_pids_max)
    pub pids_max: Option<u64>,

    /// Max file size in MB (--rlimit_fsize)
    pub rlimit_fsize_mb: Option<u64>,

    /// Max open file descriptors (--rlimit_nofile)
    pub rlimit_nofile: Option<u64>,

    /// Disable network inside sandbox (default: true)
    pub disable_network: bool,

    /// Extra read-only bind mounts beyond defaults (host paths)
    pub readonly_mounts: Vec<PathBuf>,

    /// Extra read-write bind mounts
    pub writable_mounts: Vec<PathBuf>,
}
```

`SandboxConfig::build_nsjail_args()` takes a `ProcessConfig` + `RunContext` and returns `(nsjail_bin, Vec<String>)` — the full nsjail CLI invocation including `-- command args...`.

Default nsjail invocation:
```
nsjail -Mo --really_quiet --log_fd 2 \
  --bindmount_ro /usr:/usr \
  --bindmount_ro /lib:/lib \
  --bindmount_ro /lib64:/lib64 \
  --bindmount_ro /etc/resolv.conf:/etc/resolv.conf \
  --bindmount {run_dir}:{run_dir} \
  --cwd {working_dir} \
  --env KEY=VALUE \
  --cgroup_mem_max {bytes} \
  --cgroup_pids_max {count} \
  --disable_clone_newnet \
  -- {command} {args...}
```

Also: `validate()` method that checks nsjail binary exists on PATH.

### Modify: `process/child.rs` — `run_process()`

Add `sandbox: Option<&SandboxConfig>` parameter.

When `Some`:
- Build command via `SandboxConfig::build_nsjail_args()` instead of raw `Command::new(spec.command)`
- Environment vars passed as `--env` flags to nsjail (nsjail manages child env), NOT via `Command::env()`
- Working dir handled by nsjail `--cwd`, NOT via `Command::current_dir()`
- stdout/stderr piping, kill_on_drop, timeout, cancellation, TailBuffer — unchanged

When `None`:
- Existing behavior, no changes.

### Modify: `process/mod.rs` — `ProcessBackend`

Add `sandbox: Option<SandboxConfig>` field. Builder method `with_sandbox()`. Pass `self.sandbox.as_ref()` to `run_process()`.

### Modify: `python/mod.rs` — `PythonBackend`

Same pattern — optional `SandboxConfig`, passed through to `run_process()`.

### Modify: `lib.rs`

Export `pub mod sandbox` and `SandboxConfig`.

## Nix closure-aware mounts

When the `NixEnvironmentHook` resolves a Nix environment, it stores the store path in `run_context.backend_state["nix"]["store_path"]`. The sandbox should use this to compute **minimal bind mounts** from the Nix closure instead of mounting the entire `/nix/store`.

### Why this matters

Without Nix, the default mounts expose `/usr`, `/lib`, `/lib64` — the task sees every binary and library on the host. With Nix, we can mount only the exact dependency closure:

```bash
# Compute the closure (all transitive deps)
nix-store -qR /nix/store/...-aithericon-env
# Returns ~30 paths: glibc, python, numpy, scipy, etc.
```

The task literally cannot see binaries or libraries it didn't declare. Adding `scipy` to requirements automatically expands the closure and the sandbox mounts — no manual configuration.

### Implementation in `build_nsjail_args()`

When `run_context.backend_state["nix"]["store_path"]` is set:

1. Run `nix-store -qR {store_path}` to get the full closure
2. Generate `--bindmount_ro` for each path in the closure
3. **Do not** mount `/usr`, `/lib`, `/lib64` — the Nix closure is self-contained (includes its own glibc, ld-linux, etc.)

```
nsjail -Mo --really_quiet --log_fd 2 \
  --bindmount_ro /nix/store/abc-glibc:/nix/store/abc-glibc \
  --bindmount_ro /nix/store/def-python3:/nix/store/def-python3 \
  --bindmount_ro /nix/store/ghi-numpy:/nix/store/ghi-numpy \
  ... (one per closure path) \
  --bindmount {run_dir}:{run_dir} \
  --cwd {working_dir} \
  -- {command} {args...}
```

When `backend_state["nix"]` is absent (non-Nix job), fall back to the coarse `/usr`, `/lib`, `/lib64` mounts as before.

### Closure caching

`nix-store -qR` is fast (~10ms) but runs per job. The closure for a given store path is immutable, so it can be cached alongside the environment:

- Cache key: store path
- Cache value: list of closure paths
- Stored in `{nix_cache_dir}/{hash}.closure`

## Environment variable handling

nsjail starts the child with a clean environment by default. Two modes:

- `inherit_env: true` → pass `--keep_env` to nsjail, then `--env` for overrides
- `inherit_env: false` → only explicit `--env K=V` flags (spec env + RunContext env)

## IPC socket

The run_dir bind mount (read-write) includes `ipc.sock`, so gRPC IPC between executor and sandboxed child works without changes.

## Scope boundary

This covers executor-backend only. Wiring `SandboxConfig` into `ExecutorConfig` (executor-worker) and exposing via `executor.toml` / `EXECUTOR_SANDBOX_*` env vars is a follow-up task.

## Testing

- Existing tests pass unchanged (sandbox is `None` by default)
- Manual verification with nsjail installed: `ProcessBackend::new().with_sandbox(config)` running sandboxed `echo hello`
- CI: tests that need nsjail should be gated behind a feature flag or env var (`NSJAIL_BIN` set)
