//! Process-level sandboxing via [nsjail](https://github.com/google/nsjail).
//!
//! This module is the **pure config + argv-building** layer (phase 1 of the
//! sandbox feature). It owns [`SandboxConfig`] — the executor-wide sandbox
//! settings — plus the two operations the rest of the executor consumes:
//!
//! - [`SandboxConfig::validate`] — a fail-closed startup check (we must be on
//!   Linux and the `nsjail` binary must resolve on `PATH`).
//! - [`SandboxConfig::build_nsjail_args`] — turns a [`ProcessConfig`] +
//!   [`RunContext`] into the full nsjail argv (`nsjail … -- command args…`),
//!   ready for `Command::new(nsjail_bin).args(argv)`.
//!
//! No nsjail process is ever spawned here (beyond the `--help` probe in
//! `validate`); the argv is built as plain strings so it is fully unit-testable
//! off-Linux. Wiring into `run_process` + the backends is phase 2.
//!
//! See `executor/docs/sandbox.md` for the locked design + the 12 resolved
//! decisions. Defaults follow that doc: ONCE mode (`-Mo`), drop to an
//! unprivileged uid, RW bind of the run_dir, private `/tmp` tmpfs, `/nix/store`
//! RO when a nix closure exists (coarse `/usr,/lib,/lib64,/bin` otherwise),
//! clean env by default, isolated netns unless `allow_network`.

use std::path::PathBuf;
use std::process::Command;

use aithericon_executor_backend_configs::process::ProcessConfig;
use aithericon_executor_domain::{ExecutorError, RunContext};

/// Executor-wide sandbox settings.
///
/// One instance is built at startup from the `[sandbox]` config block and
/// threaded into the process/python backends. There are no per-job overrides in
/// v1 (see docs/sandbox.md decision #4).
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// nsjail binary name or path (default `"nsjail"`); resolved on `PATH`.
    pub nsjail_bin: String,

    /// Memory cap in **bytes** → `--cgroup_mem_max`.
    pub memory_limit: Option<u64>,

    /// CPU quota in ms per wall-second → `--cgroup_cpu_ms_per_sec`.
    pub cpu_ms_per_sec: Option<u64>,

    /// Max number of pids → `--cgroup_pids_max`.
    pub pids_max: Option<u64>,

    /// Max file size the child may create, in MiB → `--rlimit_fsize`.
    pub rlimit_fsize_mb: Option<u64>,

    /// Max open file descriptors → `--rlimit_nofile`.
    pub rlimit_nofile: Option<u64>,

    /// When `false` (default), the child runs in an isolated netns. When
    /// `true`, the host netns is shared (`--disable_clone_newnet`) and
    /// `/etc/resolv.conf` is bind-mounted RO.
    pub allow_network: bool,

    /// Size of the private `/tmp` tmpfs, in MiB.
    pub tmpfs_size_mb: u64,

    /// Unprivileged uid (and gid) the child is dropped to inside the sandbox.
    pub sandbox_uid: u32,

    /// Extra read-only bind mounts (host paths, mounted at the same path).
    pub readonly_mounts: Vec<PathBuf>,

    /// Extra read-write bind mounts (host paths, mounted at the same path).
    pub writable_mounts: Vec<PathBuf>,
}

impl SandboxConfig {
    /// Fail-closed startup check.
    ///
    /// Errors when:
    /// - we are not on Linux (nsjail is Linux-only — enabling the sandbox
    ///   anywhere else is a configuration mistake, not a silent fall-through to
    ///   unsandboxed execution), or
    /// - `nsjail_bin` does not resolve to a runnable binary on `PATH` (probed
    ///   by spawning it with `--help`).
    ///
    /// Returns [`ExecutorError::Config`] — the closest fit for a startup /
    /// configuration failure.
    pub fn validate(&self) -> Result<(), ExecutorError> {
        // `cfg!` is a compile-target check (intentional): the executor is built
        // natively for its deploy target, so "compiled for Linux" == "runs on
        // Linux" for our binaries. A cross-built artifact run elsewhere is out
        // of scope.
        if !cfg!(target_os = "linux") {
            return Err(ExecutorError::Config(format!(
                "sandbox enabled but the executor is not running on Linux; \
                 nsjail requires Linux namespaces (nsjail_bin = {})",
                self.nsjail_bin
            )));
        }

        // Probe that the binary is resolvable AND behaves like nsjail. `--help`
        // exits fast and touches nothing. We reject three fail-closed cases: a
        // spawn failure (not found / not executable), and a present-but-wrong
        // binary whose `--help` neither succeeds nor mentions nsjail in its
        // usage output (a stub named `nsjail` must not pass).
        match Command::new(&self.nsjail_bin).arg("--help").output() {
            Ok(out) => {
                let looks_like_nsjail = out.status.success()
                    || String::from_utf8_lossy(&out.stderr)
                        .to_lowercase()
                        .contains("nsjail")
                    || String::from_utf8_lossy(&out.stdout)
                        .to_lowercase()
                        .contains("nsjail");
                if looks_like_nsjail {
                    Ok(())
                } else {
                    Err(ExecutorError::Config(format!(
                        "sandbox enabled but {:?} on PATH does not behave like nsjail \
                         (--help produced no nsjail usage output)",
                        self.nsjail_bin
                    )))
                }
            }
            Err(e) => Err(ExecutorError::Config(format!(
                "sandbox enabled but the nsjail binary {:?} is not runnable on PATH: {e}",
                self.nsjail_bin
            ))),
        }
    }

    /// Build the full nsjail argv for one job.
    ///
    /// Returns `(nsjail_bin, argv)` where `argv` is everything after the binary
    /// name, ending in `-- {command} {args…}`. The caller does
    /// `Command::new(nsjail_bin).args(argv)` — env is carried *inside* the argv
    /// via `--env` flags (clean env by default), NOT via `Command::env`.
    ///
    /// See the "Default invocation shape" block in docs/sandbox.md.
    // The argv is built as a long, conditional, sequential push sequence — far
    // clearer than a giant `vec![]` literal spanning the same conditionals.
    #[allow(clippy::vec_init_then_push)]
    pub fn build_nsjail_args(
        &self,
        spec: &ProcessConfig,
        run_context: &RunContext,
    ) -> (String, Vec<String>) {
        let mut argv: Vec<String> = Vec::new();

        // ONCE mode: one command per nsjail invocation, maps to our
        // one-job-per-exec model. Quiet + log to stderr (fd 2).
        //
        // TODO(sandbox-hardening): v1 relies on nsjail's `-Mo` namespace/uid/
        // mount/cgroup defaults; it does NOT ship a custom seccomp-bpf policy
        // (nsjail enables none without an explicit `--seccomp_string`) nor an
        // explicit RO-`/proc` tightening. The network/fs/memory/env/uid
        // isolation properties hold without these; see docs/sandbox.md #11.
        argv.push("-Mo".into());
        argv.push("--really_quiet".into());
        argv.push("--log_fd".into());
        argv.push("2".into());

        // Drop to the unprivileged uid/gid inside the sandbox.
        argv.push("--user".into());
        argv.push(self.sandbox_uid.to_string());
        argv.push("--group".into());
        argv.push(self.sandbox_uid.to_string());

        // Work inside the run directory (replaces Command::current_dir).
        let run_dir = run_context.run_dir.root.display().to_string();
        argv.push("--cwd".into());
        argv.push(run_dir.clone());

        // RW bind of the run_dir — holds ipc.sock, staged inputs, outputs.
        argv.push("--bindmount".into());
        argv.push(format!("{run_dir}:{run_dir}"));

        // Extra writable binds (host path mounted at the same path).
        for m in &self.writable_mounts {
            let p = m.display().to_string();
            argv.push("--bindmount".into());
            argv.push(format!("{p}:{p}"));
        }

        // Private /tmp tmpfs.
        argv.push("--tmpfsmount".into());
        argv.push("/tmp".into());
        argv.push("--tmpfs_size".into());
        argv.push((self.tmpfs_size_mb * 1024 * 1024).to_string());

        // IPC socket rescue. When the execution_id is long (compound UUIDs from
        // the engine), `RunDirectory` relocates `ipc.sock` out of the run_dir to
        // a short `/tmp/.aex/{hash}/` path so it fits the Unix `sun_path` limit.
        // That path lives under `/tmp`, which the private tmpfs above would
        // shadow — the child would never see the socket and IPC (set_output,
        // artifacts, progress) would silently break. Bind the socket's parent
        // dir RW; emitted AFTER `--tmpfsmount /tmp` so nsjail layers this bind
        // over the tmpfs (mounts apply in argv order). When the socket is the
        // normal `{root}/ipc.sock`, it is already covered by the run_dir bind.
        let ipc_sock = &run_context.run_dir.ipc_socket;
        if !ipc_sock.starts_with(&run_context.run_dir.root) {
            if let Some(parent) = ipc_sock.parent() {
                let p = parent.display().to_string();
                argv.push("--bindmount".into());
                argv.push(format!("{p}:{p}"));
            }
        }

        // --- libs: nix-closure path OR coarse system fallback ---
        // TODO(sandbox): minimal closure mounts via nix-store -qR
        if let Some(store_path) = nix_store_path(run_context) {
            // The nix closure is self-contained (its own glibc/ld), so a single
            // RO bind of the whole store is enough — no /usr etc.
            argv.push("--bindmount_ro".into());
            argv.push(format!("{store_path}:{store_path}"));
        } else {
            // No nix closure — fall back to coarse system library mounts.
            // Only bind dirs that actually exist: nsjail fails the whole
            // invocation if a --bindmount_ro source is missing, and the set
            // varies by distro/arch (e.g. /lib64 exists on x86_64 but not on
            // arm64 Debian, where the loader lives under /lib).
            argv.extend(coarse_ro_binds(|p| std::path::Path::new(p).exists()));
        }

        // Extra read-only binds.
        for m in &self.readonly_mounts {
            let p = m.display().to_string();
            argv.push("--bindmount_ro".into());
            argv.push(format!("{p}:{p}"));
        }

        // Resolver config only when the network is allowed.
        if self.allow_network {
            argv.push("--bindmount_ro".into());
            argv.push("/etc/resolv.conf:/etc/resolv.conf".into());
        }

        // Clean env: emit explicit --env for spec.env, then run_context.env,
        // then run_context.resolved_env (plaintext secrets win). The executor's
        // own environment (vault token, NATS creds) is NOT inherited unless
        // the job opted into inherit_env (→ --keep_env).
        for (k, v) in &spec.env {
            argv.push("--env".into());
            argv.push(format!("{k}={v}"));
        }
        for (k, v) in &run_context.env {
            argv.push("--env".into());
            argv.push(format!("{k}={v}"));
        }
        for (k, v) in &run_context.resolved_env {
            argv.push("--env".into());
            argv.push(format!("{k}={v}"));
        }
        if spec.inherit_env {
            argv.push("--keep_env".into());
        }

        // cgroup resource caps — only emitted when configured. On a cgroup v2
        // host (the modern default, incl. recent Docker/k8s) nsjail must be told
        // to use the v2 hierarchy with `--use_cgroupv2`; without it `--cgroup_*`
        // targets the legacy v1 `/sys/fs/cgroup/<controller>/` tree, which does
        // not exist under unified v2 and aborts nsjail at init. Detect v2 by the
        // unified-hierarchy marker file.
        let any_cgroup_cap =
            self.memory_limit.is_some() || self.pids_max.is_some() || self.cpu_ms_per_sec.is_some();
        if needs_cgroupv2_flag(any_cgroup_cap, |p| std::path::Path::new(p).exists()) {
            argv.push("--use_cgroupv2".into());
        }
        if let Some(mem) = self.memory_limit {
            argv.push("--cgroup_mem_max".into());
            argv.push(mem.to_string());
        }
        if let Some(pids) = self.pids_max {
            argv.push("--cgroup_pids_max".into());
            argv.push(pids.to_string());
        }
        if let Some(cpu) = self.cpu_ms_per_sec {
            argv.push("--cgroup_cpu_ms_per_sec".into());
            argv.push(cpu.to_string());
        }

        // rlimits — only when configured.
        if let Some(fsize) = self.rlimit_fsize_mb {
            argv.push("--rlimit_fsize".into());
            argv.push(fsize.to_string());
        }
        if let Some(nofile) = self.rlimit_nofile {
            argv.push("--rlimit_nofile".into());
            argv.push(nofile.to_string());
        }

        // Network: isolated netns by default; share the host netns only when
        // the network is explicitly allowed.
        if self.allow_network {
            argv.push("--disable_clone_newnet".into());
        }

        // Command + args after the `--` separator.
        argv.push("--".into());
        argv.push(spec.command.clone());
        for a in &spec.args {
            argv.push(a.clone());
        }

        (self.nsjail_bin.clone(), argv)
    }
}

/// Candidate coarse system dirs bound read-only when no nix closure is present.
/// Which ones exist is distro/arch-dependent (`/lib64` is x86_64-only), so the
/// builder filters by existence — see [`coarse_ro_binds`].
const COARSE_DIRS: &[&str] = &["/usr", "/lib", "/lib64", "/bin"];

/// Build the `--bindmount_ro {d}:{d}` argv pairs for the coarse system dirs that
/// exist (per the `exists` predicate). Binding a missing source path makes
/// nsjail abort, so non-existent dirs are skipped. Pure + predicate-injected so
/// the filtering is unit-testable off the host filesystem.
fn coarse_ro_binds(exists: impl Fn(&str) -> bool) -> Vec<String> {
    COARSE_DIRS
        .iter()
        .filter(|d| exists(d))
        .flat_map(|d| ["--bindmount_ro".to_string(), format!("{d}:{d}")])
        .collect()
}

/// Whether nsjail needs the `--use_cgroupv2` flag: true when at least one
/// cgroup cap is configured AND the host runs the unified cgroup v2 hierarchy
/// (detected by the `cgroup.controllers` marker at the cgroup mount root).
/// Pure + predicate-injected for off-host unit testing.
fn needs_cgroupv2_flag(any_cgroup_cap: bool, exists: impl Fn(&str) -> bool) -> bool {
    any_cgroup_cap && exists("/sys/fs/cgroup/cgroup.controllers")
}

/// Pull `backend_state["nix"]["store_path"]` as a string, if present.
///
/// When set, the job ran inside a nix closure whose store path is
/// self-contained — we bind that store RO instead of the coarse system dirs.
fn nix_store_path(run_context: &RunContext) -> Option<String> {
    run_context
        .backend_state
        .get("nix")
        .and_then(|n| n.get("store_path"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aithericon_executor_domain::{ExecutionSpec, RunDirectory};
    use std::collections::HashMap;
    use std::time::Duration;

    fn base_config() -> SandboxConfig {
        SandboxConfig {
            nsjail_bin: "nsjail".into(),
            memory_limit: None,
            cpu_ms_per_sec: None,
            pids_max: None,
            rlimit_fsize_mb: None,
            rlimit_nofile: None,
            allow_network: false,
            tmpfs_size_mb: 64,
            sandbox_uid: 65534,
            readonly_mounts: vec![],
            writable_mounts: vec![],
        }
    }

    fn proc_spec() -> ProcessConfig {
        ProcessConfig {
            command: "echo".into(),
            args: vec!["hello".into(), "world".into()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: false,
        }
    }

    fn run_ctx(backend_state: serde_json::Value) -> RunContext {
        let rd = RunDirectory::new(&PathBuf::from("/data/exec"), "exec-1");
        let mut rc = RunContext::for_test(
            "exec-1",
            ExecutionSpec {
                backend: "process".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({}),
                config_ref: None,
            },
            rd,
            Duration::from_secs(60),
        );
        rc.backend_state = backend_state;
        rc
    }

    /// Index of the first occurrence of `flag` in argv, if any.
    fn pos(argv: &[String], flag: &str) -> Option<usize> {
        argv.iter().position(|a| a == flag)
    }

    fn contains(argv: &[String], flag: &str) -> bool {
        pos(argv, flag).is_some()
    }

    #[test]
    fn once_mode_and_command_after_separator() {
        let cfg = base_config();
        let (bin, argv) = cfg.build_nsjail_args(&proc_spec(), &run_ctx(serde_json::Value::Null));

        assert_eq!(bin, "nsjail");
        assert!(contains(&argv, "-Mo"), "must run in ONCE mode");

        let sep = pos(&argv, "--").expect("must have a -- separator");
        // command + args follow the separator, in order.
        assert_eq!(argv[sep + 1], "echo");
        assert_eq!(argv[sep + 2], "hello");
        assert_eq!(argv[sep + 3], "world");
    }

    #[test]
    fn nix_store_path_mounts_store_and_no_coarse_dirs() {
        let cfg = base_config();
        let state = serde_json::json!({ "nix": { "store_path": "/nix/store" } });
        let (_bin, argv) = cfg.build_nsjail_args(&proc_spec(), &run_ctx(state));

        let joined = argv.join(" ");
        assert!(
            joined.contains("/nix/store:/nix/store"),
            "expected nix store RO bind, argv: {joined}"
        );
        assert!(
            !joined.contains("/usr:/usr"),
            "must NOT mount coarse /usr when a nix closure is present"
        );
    }

    #[test]
    fn coarse_binds_filter_by_existence() {
        // All present → every candidate bound, as RO pairs in order.
        let all = coarse_ro_binds(|_| true);
        assert_eq!(
            all,
            vec![
                "--bindmount_ro", "/usr:/usr",
                "--bindmount_ro", "/lib:/lib",
                "--bindmount_ro", "/lib64:/lib64",
                "--bindmount_ro", "/bin:/bin",
            ]
        );
        // None present → no binds (nsjail would abort on a missing source).
        assert!(coarse_ro_binds(|_| false).is_empty());
        // Subset (e.g. arm64 Debian: no /lib64) → only existing dirs bound.
        let arm = coarse_ro_binds(|p| p != "/lib64");
        assert!(arm.iter().any(|a| a == "/usr:/usr"));
        assert!(arm.iter().any(|a| a == "/lib:/lib"));
        assert!(arm.iter().any(|a| a == "/bin:/bin"));
        assert!(
            !arm.iter().any(|a| a == "/lib64:/lib64"),
            "missing /lib64 must be skipped"
        );
    }

    #[test]
    fn cgroupv2_flag_gated_on_caps_and_host() {
        let v2_host = |p: &str| p == "/sys/fs/cgroup/cgroup.controllers";
        let v1_host = |_: &str| false;
        // v2 host + a cap → flag needed
        assert!(needs_cgroupv2_flag(true, v2_host));
        // v2 host but no cap → no flag (nothing to put in a cgroup)
        assert!(!needs_cgroupv2_flag(false, v2_host));
        // v1 host (no unified marker) + a cap → no v2 flag (legacy hierarchy)
        assert!(!needs_cgroupv2_flag(true, v1_host));
    }

    #[test]
    fn no_nix_store_uses_coarse_not_nix() {
        let cfg = base_config();
        let (_bin, argv) = cfg.build_nsjail_args(&proc_spec(), &run_ctx(serde_json::Value::Null));
        let joined = argv.join(" ");
        // Without a closure we never mount /nix/store; at least /usr (which
        // exists on every host this runs on) is bound from the coarse set.
        assert!(
            !joined.contains("/nix/store"),
            "must NOT mount /nix/store without a closure"
        );
        assert!(joined.contains("/usr:/usr"), "expected coarse /usr mount");
    }

    #[test]
    fn network_denied_by_default() {
        let cfg = base_config();
        let (_bin, argv) = cfg.build_nsjail_args(&proc_spec(), &run_ctx(serde_json::Value::Null));

        assert!(
            !contains(&argv, "--disable_clone_newnet"),
            "isolated netns by default: --disable_clone_newnet must be absent"
        );
        assert!(
            !argv.join(" ").contains("resolv.conf"),
            "no resolv.conf mount when network denied"
        );
    }

    #[test]
    fn network_allowed_shares_netns_and_mounts_resolv() {
        let mut cfg = base_config();
        cfg.allow_network = true;
        let (_bin, argv) = cfg.build_nsjail_args(&proc_spec(), &run_ctx(serde_json::Value::Null));

        assert!(
            contains(&argv, "--disable_clone_newnet"),
            "allow_network must share the host netns"
        );
        assert!(
            argv.join(" ").contains("/etc/resolv.conf:/etc/resolv.conf"),
            "allow_network must bind resolv.conf RO"
        );
    }

    #[test]
    fn clean_env_excludes_executor_env_but_keeps_spec_and_context() {
        let cfg = base_config();
        // a planted executor-only env var that must NOT leak into argv
        std::env::set_var("AITHERICON_SANDBOX_TEST_LEAK", "VAULT-TOKEN-XYZ");

        let mut spec = proc_spec();
        spec.env.insert("SPEC_VAR".into(), "spec-val".into());

        let mut rc = run_ctx(serde_json::Value::Null);
        rc.env.insert("CTX_VAR".into(), "ctx-val".into());
        rc.resolved_env
            .insert("SECRET_VAR".into(), "resolved-val".into());

        let (_bin, argv) = cfg.build_nsjail_args(&spec, &rc);
        let joined = argv.join(" ");

        assert!(
            !joined.contains("AITHERICON_SANDBOX_TEST_LEAK"),
            "executor-only env must NOT appear in argv: {joined}"
        );
        assert!(
            !joined.contains("VAULT-TOKEN-XYZ"),
            "executor env value leaked into argv: {joined}"
        );
        assert!(
            joined.contains("SPEC_VAR=spec-val"),
            "spec.env must be emitted via --env"
        );
        assert!(
            joined.contains("CTX_VAR=ctx-val"),
            "run_context.env must be emitted via --env"
        );
        assert!(
            joined.contains("SECRET_VAR=resolved-val"),
            "run_context.resolved_env must be emitted via --env"
        );

        std::env::remove_var("AITHERICON_SANDBOX_TEST_LEAK");
    }

    #[test]
    fn inherit_env_emits_keep_env() {
        let cfg = base_config();
        let mut spec = proc_spec();
        spec.inherit_env = true;
        let (_bin, argv) = cfg.build_nsjail_args(&spec, &run_ctx(serde_json::Value::Null));
        assert!(
            contains(&argv, "--keep_env"),
            "inherit_env=true must emit --keep_env"
        );

        // and absent when false
        let (_bin2, argv2) =
            cfg.build_nsjail_args(&proc_spec(), &run_ctx(serde_json::Value::Null));
        assert!(
            !contains(&argv2, "--keep_env"),
            "inherit_env=false must NOT emit --keep_env"
        );
    }

    #[test]
    fn cgroup_and_rlimit_flags_only_when_some() {
        // None → flags absent
        let cfg = base_config();
        let (_b, argv) = cfg.build_nsjail_args(&proc_spec(), &run_ctx(serde_json::Value::Null));
        for flag in [
            "--cgroup_mem_max",
            "--cgroup_pids_max",
            "--cgroup_cpu_ms_per_sec",
            "--rlimit_fsize",
            "--rlimit_nofile",
        ] {
            assert!(!contains(&argv, flag), "{flag} must be absent when None");
        }

        // Some → flags present with the right values
        let mut cfg2 = base_config();
        cfg2.memory_limit = Some(512 * 1024 * 1024);
        cfg2.pids_max = Some(128);
        cfg2.cpu_ms_per_sec = Some(500);
        cfg2.rlimit_fsize_mb = Some(100);
        cfg2.rlimit_nofile = Some(256);
        let (_b2, argv2) = cfg2.build_nsjail_args(&proc_spec(), &run_ctx(serde_json::Value::Null));

        let val_after = |flag: &str| -> Option<String> {
            pos(&argv2, flag).and_then(|i| argv2.get(i + 1).cloned())
        };
        assert_eq!(
            val_after("--cgroup_mem_max"),
            Some((512u64 * 1024 * 1024).to_string())
        );
        assert_eq!(val_after("--cgroup_pids_max"), Some("128".into()));
        assert_eq!(val_after("--cgroup_cpu_ms_per_sec"), Some("500".into()));
        assert_eq!(val_after("--rlimit_fsize"), Some("100".into()));
        assert_eq!(val_after("--rlimit_nofile"), Some("256".into()));
    }

    #[test]
    fn relocated_ipc_socket_parent_is_bound_after_tmpfs() {
        // A long execution_id forces RunDirectory to relocate ipc.sock out of
        // the run_dir to /tmp/.aex/{hash}/ipc.sock (Unix sun_path limit). The
        // private /tmp tmpfs would otherwise shadow it. Assert the parent is
        // bound RW and that the bind comes AFTER the tmpfs mount so it wins.
        let long_id = "a".repeat(120);
        let rd = RunDirectory::new(&PathBuf::from("/data/exec"), &long_id);
        assert!(
            rd.ipc_socket.starts_with("/tmp/.aex/"),
            "precondition: long id should relocate the socket under /tmp/.aex, got {:?}",
            rd.ipc_socket
        );
        let ipc_parent = rd.ipc_socket.parent().unwrap().display().to_string();

        let mut rc = RunContext::for_test(
            long_id.clone(),
            ExecutionSpec {
                backend: "process".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({}),
                config_ref: None,
            },
            rd,
            Duration::from_secs(60),
        );
        rc.backend_state = serde_json::Value::Null;

        let cfg = base_config();
        let (_b, argv) = cfg.build_nsjail_args(&proc_spec(), &rc);

        let bind_spec = format!("{ipc_parent}:{ipc_parent}");
        let bind_pos = argv
            .iter()
            .position(|a| a == &bind_spec)
            .expect("ipc socket parent must be bound RW");
        let tmpfs_pos = pos(&argv, "--tmpfsmount").expect("tmpfs must be mounted");
        assert!(
            bind_pos > tmpfs_pos,
            "ipc parent bind ({bind_pos}) must come AFTER the /tmp tmpfs ({tmpfs_pos}) so it is not shadowed"
        );
    }

    #[test]
    fn normal_ipc_socket_needs_no_extra_bind() {
        // Short id → socket stays at {root}/ipc.sock, already covered by the
        // run_dir bind; no extra /tmp bind should be emitted.
        let cfg = base_config();
        let rc = run_ctx(serde_json::Value::Null);
        assert!(
            rc.run_dir.ipc_socket.starts_with(&rc.run_dir.root),
            "precondition: short id keeps the socket inside the run_dir"
        );
        let (_b, argv) = cfg.build_nsjail_args(&proc_spec(), &rc);
        assert!(
            !argv.join(" ").contains("/tmp/.aex/"),
            "no /tmp/.aex bind should be emitted for an in-run_dir socket"
        );
    }

    #[test]
    fn run_dir_bound_rw_and_cwd() {
        let cfg = base_config();
        let (_b, argv) = cfg.build_nsjail_args(&proc_spec(), &run_ctx(serde_json::Value::Null));
        let joined = argv.join(" ");
        // run_dir root for exec-1 under /data/exec
        assert!(
            joined.contains("--cwd /data/exec/runs/exec-1"),
            "cwd must be the run_dir root: {joined}"
        );
        assert!(
            joined.contains("--bindmount /data/exec/runs/exec-1:/data/exec/runs/exec-1"),
            "run_dir must be bound RW: {joined}"
        );
    }
}
