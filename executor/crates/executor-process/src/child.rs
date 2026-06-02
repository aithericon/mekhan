use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use aithericon_executor_domain::{
    ExecutionOutcome, ExecutionResult, ExecutionStatus, ExecutorError, RunContext,
};

use aithericon_executor_backend::tail::TailBuffer;
use aithericon_executor_backend::traits::StatusCallback;
use aithericon_executor_backend::SandboxConfig;

use crate::ProcessConfig;

/// Grace period after SIGTERM before sending SIGKILL.
const TERM_GRACE_SECS: u64 = 5;

/// Execute a process config within a RunContext, returning the result.
///
/// When `sandbox` is `Some`, the command is wrapped in nsjail: the spawned
/// process is `nsjail … -- {command} {args…}` instead of the command itself.
/// nsjail owns the child's env (via `--env` flags) and cwd (via `--cwd`), so in
/// the sandboxed branch we do NOT call `Command::env`/`current_dir` — doing so
/// would re-inherit the executor's own environment into the nsjail process.
/// When `None`, behavior is the historical unsandboxed exec, byte-for-byte.
pub async fn run_process(
    spec: &ProcessConfig,
    run_context: &RunContext,
    max_output_bytes: usize,
    status_cb: &StatusCallback,
    cancel: CancellationToken,
    sandbox: Option<&SandboxConfig>,
) -> Result<ExecutionResult, ExecutorError> {
    let start = tokio::time::Instant::now();
    let timeout = run_context.timeout;

    let mut cmd = if let Some(sandbox) = sandbox {
        // Sandboxed: build the full nsjail argv (incl. `-- command args…`).
        // nsjail carries env via `--env` and cwd via `--cwd`, so we attach
        // neither here — the executor's own environment must not leak in.
        let (nsjail_bin, argv) = sandbox.build_nsjail_args(spec, run_context);
        let mut cmd = Command::new(nsjail_bin);
        cmd.args(argv);
        cmd
    } else {
        // Unsandboxed: spawn the command directly with env + cwd as before.
        let mut cmd = Command::new(&spec.command);
        cmd.args(&spec.args);

        if !spec.inherit_env {
            cmd.env_clear();
        }

        // Apply spec env vars first
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        // Then apply RunContext env vars (these take precedence, e.g. AITHERICON_* vars).
        // For any env name that had a `{{secret:KEY}}` template, `resolved_env`
        // carries the plaintext from the in-memory side-channel (never serialized
        // to context.json). Apply `env` first then overlay `resolved_env` so the
        // resolved values win without leaking through `env` to disk.
        for (k, v) in &run_context.env {
            cmd.env(k, v);
        }
        for (k, v) in &run_context.resolved_env {
            cmd.env(k, v);
        }

        if let Some(dir) = &spec.working_dir {
            cmd.current_dir(dir);
        }

        cmd
    };

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(ExecutorError::SpawnFailed)?;

    let pid = child.id().unwrap_or(0);
    debug!(pid, command = %spec.command, "process spawned");

    // Report Running with pid
    status_cb(ExecutionStatus::Running, json!({ "pid": pid })).await;

    // Take ownership of stdout/stderr handles
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    // Spawn output capture tasks
    let mut stdout_buf = TailBuffer::new(max_output_bytes);
    let mut stderr_buf = TailBuffer::new(max_output_bytes);

    let stdout_task = tokio::spawn(async move {
        let mut buf = TailBuffer::new(max_output_bytes);
        if let Some(reader) = stdout_handle {
            let _ = buf.capture(reader).await;
        }
        buf
    });

    let stderr_task = tokio::spawn(async move {
        let mut buf = TailBuffer::new(max_output_bytes);
        if let Some(reader) = stderr_handle {
            let _ = buf.capture(reader).await;
        }
        buf
    });

    // Wait for exit, timeout, or cancellation
    let outcome = tokio::select! {
        biased;

        _ = cancel.cancelled() => {
            debug!(pid, "cancellation requested, sending SIGTERM");
            terminate_child(&mut child).await;
            ExecutionOutcome::Cancelled
        }

        _ = tokio::time::sleep(timeout) => {
            warn!(pid, ?timeout, "execution timed out, sending SIGTERM");
            terminate_child(&mut child).await;
            ExecutionOutcome::TimedOut
        }

        status = child.wait() => {
            match status {
                Ok(exit) => {
                    if exit.success() {
                        ExecutionOutcome::Success
                    } else {
                        #[cfg(unix)]
                        {
                            use std::os::unix::process::ExitStatusExt;
                            if let Some(sig) = exit.signal() {
                                return Ok(ExecutionResult {
                                    outcome: ExecutionOutcome::Signal { signal: sig },
                                    duration: start.elapsed(),
                                    stdout_tail: stdout_task.await.ok().and_then(|b| b.into_string()),
                                    stderr_tail: stderr_task.await.ok().and_then(|b| b.into_string()),
                                    artifact_manifest: None,
                                    outputs: Default::default(),
                                    progress: None,
                                    run_dir: None,
                                    metrics: None,
                                    logs: None,
                                });
                            }
                        }
                        ExecutionOutcome::ExitFailure {
                            exit_code: exit.code().unwrap_or(-1),
                        }
                    }
                }
                Err(e) => ExecutionOutcome::BackendError {
                    message: e.to_string(),
                },
            }
        }
    };

    // Collect output tails
    stdout_buf = stdout_task.await.unwrap_or(stdout_buf);
    stderr_buf = stderr_task.await.unwrap_or(stderr_buf);

    Ok(ExecutionResult {
        outcome,
        duration: start.elapsed(),
        stdout_tail: stdout_buf.into_string(),
        stderr_tail: stderr_buf.into_string(),
        artifact_manifest: None,
        outputs: Default::default(),
        progress: None,
        run_dir: None,
        metrics: None,
        logs: None,
    })
}

/// Send SIGTERM, wait grace period, then SIGKILL if still alive.
///
/// When the job is sandboxed, `child` is the **nsjail** process, not the user
/// command. That is exactly what we want to signal: nsjail is the PID-namespace
/// init (pid 1 inside the sandbox), so SIGTERM is forwarded to the child and a
/// SIGKILL tears down the whole PID namespace — no orphaned grandchildren
/// survive the teardown.
async fn terminate_child(child: &mut tokio::process::Child) {
    // Try SIGTERM first (Unix) or kill (Windows)
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
        return;
    }

    // Wait for graceful exit
    let grace = tokio::time::sleep(Duration::from_secs(TERM_GRACE_SECS));
    tokio::select! {
        _ = child.wait() => {
            debug!("process exited after SIGTERM");
        }
        _ = grace => {
            warn!("grace period expired, sending SIGKILL");
            let _ = child.kill().await;
        }
    }
}
