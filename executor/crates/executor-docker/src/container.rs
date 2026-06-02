use bollard::container::LogOutput;
use bollard::models::{ContainerCreateBody, HostConfig, Mount, MountTypeEnum};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, StopContainerOptionsBuilder, WaitContainerOptionsBuilder,
};
use bollard::Docker;
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use aithericon_executor_domain::{
    ExecutionOutcome, ExecutionResult, ExecutionStatus, ExecutorError, RunContext,
};

use aithericon_executor_backend::tail::TailBuffer;
use aithericon_executor_backend::traits::StatusCallback;
use aithericon_executor_backend::SandboxConfig;

use crate::{DockerConfig, PullPolicy, CONTAINER_RUN_DIR};

/// Grace period after stop request before force-killing the container.
const STOP_GRACE_SECS: i32 = 5;

/// Ensure the image is available locally, pulling if needed per the pull policy.
pub async fn ensure_image(
    client: &Docker,
    image: &str,
    policy: PullPolicy,
) -> Result<(), ExecutorError> {
    let should_pull = match policy {
        PullPolicy::Always => true,
        PullPolicy::Never => false,
        PullPolicy::IfNotPresent => client.inspect_image(image).await.is_err(),
    };

    if should_pull {
        debug!(image, "pulling docker image");
        let opts = CreateImageOptionsBuilder::new().from_image(image).build();

        let mut stream = client.create_image(Some(opts), None, None);
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = &info.status {
                        debug!(image, status = %status, "pull progress");
                    }
                }
                Err(e) => {
                    return Err(ExecutorError::StagingFailed(format!(
                        "failed to pull image '{image}': {e}"
                    )));
                }
            }
        }
        debug!(image, "image pull complete");
    }

    Ok(())
}

/// Execute a Docker container within a RunContext, returning the result.
pub async fn run_container(
    client: &Docker,
    config: &DockerConfig,
    run_context: &RunContext,
    max_output_bytes: usize,
    status_cb: &StatusCallback,
    cancel: CancellationToken,
    sandbox: Option<&SandboxConfig>,
) -> Result<ExecutionResult, ExecutorError> {
    let start = tokio::time::Instant::now();
    let timeout = run_context.timeout;

    // Build container configuration
    let body = build_container_body(config, run_context, sandbox);

    let container_name = format!("aithericon-{}", run_context.execution_id);
    let create_opts = CreateContainerOptionsBuilder::new()
        .name(&container_name)
        .build();

    // Create container
    let create_response = client
        .create_container(Some(create_opts), body)
        .await
        .map_err(|e| ExecutorError::StagingFailed(format!("failed to create container: {e}")))?;

    let container_id = create_response.id;
    debug!(container_id = %container_id, image = %config.image, "container created");

    // Start container
    if let Err(e) = client
        .start_container(
            &container_id,
            None::<bollard::query_parameters::StartContainerOptions>,
        )
        .await
    {
        // Clean up the created container on start failure
        let _ = remove_container(client, &container_id).await;
        return Err(ExecutorError::SpawnFailed(std::io::Error::other(format!(
            "failed to start container: {e}"
        ))));
    }

    debug!(container_id = %container_id, "container started");
    status_cb(
        ExecutionStatus::Running,
        json!({ "container_id": &container_id }),
    )
    .await;

    // Wait for container exit, timeout, or cancellation
    let outcome = tokio::select! {
        biased;

        _ = cancel.cancelled() => {
            debug!(container_id = %container_id, "cancellation requested, stopping container");
            stop_container(client, &container_id).await;
            ExecutionOutcome::Cancelled
        }

        _ = tokio::time::sleep(timeout) => {
            warn!(container_id = %container_id, ?timeout, "execution timed out, stopping container");
            stop_container(client, &container_id).await;
            ExecutionOutcome::TimedOut
        }

        result = wait_container(client, &container_id) => {
            match result {
                Ok(exit_code) => {
                    if exit_code == 0 {
                        ExecutionOutcome::Success
                    } else {
                        ExecutionOutcome::ExitFailure { exit_code: exit_code as i32 }
                    }
                }
                Err(e) => ExecutionOutcome::BackendError {
                    message: format!("container wait failed: {e}"),
                },
            }
        }
    };

    // Capture container logs
    let (stdout_tail, stderr_tail) = capture_logs(client, &container_id, max_output_bytes).await;

    // Remove container if configured
    if config.remove_container {
        if let Err(e) = remove_container(client, &container_id).await {
            warn!(container_id = %container_id, error = %e, "failed to remove container");
        }
    }

    Ok(ExecutionResult {
        outcome,
        duration: start.elapsed(),
        stdout_tail,
        stderr_tail,
        artifact_manifest: None,
        outputs: Default::default(),
        progress: None,
        run_dir: None,
        metrics: None,
        logs: None,
    })
}

/// Build the Docker container body from DockerConfig and RunContext.
fn build_container_body(
    config: &DockerConfig,
    run_context: &RunContext,
    sandbox: Option<&SandboxConfig>,
) -> ContainerCreateBody {
    // Build environment variables
    let mut env_vars = build_env_vars(config, run_context);

    // Override AITHERICON_* paths to container-internal paths
    env_vars.push(format!("AITHERICON_RUN_DIR={CONTAINER_RUN_DIR}"));
    env_vars.push(format!("AITHERICON_INPUTS_DIR={CONTAINER_RUN_DIR}/inputs"));
    env_vars.push(format!(
        "AITHERICON_OUTPUTS_DIR={CONTAINER_RUN_DIR}/outputs"
    ));
    env_vars.push(format!(
        "AITHERICON_ARTIFACTS_DIR={CONTAINER_RUN_DIR}/artifacts"
    ));
    env_vars.push(format!(
        "AITHERICON_IPC_SOCKET={CONTAINER_RUN_DIR}/ipc.sock"
    ));
    env_vars.push(format!(
        "AITHERICON_EXECUTION_ID={}",
        run_context.execution_id
    ));

    // Build mounts
    let mut mounts = vec![Mount {
        target: Some(CONTAINER_RUN_DIR.to_string()),
        source: Some(run_context.run_dir.root.to_string_lossy().into_owned()),
        typ: Some(MountTypeEnum::BIND),
        read_only: Some(false),
        ..Default::default()
    }];

    // Add extra volume mounts
    for vol in &config.extra_volumes {
        if let Some((host, container)) = vol.split_once(':') {
            mounts.push(Mount {
                target: Some(container.to_string()),
                source: Some(host.to_string()),
                typ: Some(MountTypeEnum::BIND),
                read_only: Some(false),
                ..Default::default()
            });
        }
    }

    // Build host config
    let mut host_config = HostConfig {
        mounts: Some(mounts),
        ..Default::default()
    };

    if let Some(network) = &config.network_mode {
        host_config.network_mode = Some(network.clone());
    }

    // Apply resource limits
    if let Some(limits) = &config.resource_limits {
        host_config.memory = limits.memory_bytes;
        host_config.cpu_shares = limits.cpu_shares;
        host_config.cpu_quota = limits.cpu_quota;
    }

    // Map the executor-wide sandbox policy onto Docker's native isolation. This
    // is the Docker analogue of the nsjail wrap on the process/python backends
    // (see executor-backend::sandbox) — same intent, Docker mechanism. Returns
    // the non-root user string to stamp onto the ContainerCreateBody.
    let sandbox_user = sandbox.and_then(|sb| apply_sandbox_to_host_config(&mut host_config, sb));

    let cmd = if config.command.is_empty() {
        None
    } else {
        Some(config.command.clone())
    };

    ContainerCreateBody {
        image: Some(config.image.clone()),
        env: Some(env_vars),
        cmd,
        entrypoint: config.entrypoint.clone(),
        host_config: Some(host_config),
        user: sandbox_user,
        ..Default::default()
    }
}

/// Translate a [`SandboxConfig`] onto a container's [`HostConfig`], returning
/// the non-root `user` string for the `ContainerCreateBody`. The Docker
/// analogue of the nsjail wrap on the process/python backends — same intent,
/// Docker mechanism.
///
/// Security floors (enforced, override the per-job `DockerConfig`):
/// - network denied (`network_mode = "none"`) unless `allow_network`
/// - all Linux capabilities dropped (`cap_drop = ["ALL"]`)
/// - no privilege escalation (`security_opt = ["no-new-privileges:true"]`)
/// - read-only rootfs + a private writable `/tmp` tmpfs (mirrors the nsjail
///   private tmpfs; the run_dir bind stays writable for outputs/ipc)
/// - the container process runs as the unprivileged `sandbox_uid`
///
/// Resource ceilings (tightened to the stricter of job-vs-sandbox):
/// - `memory` = min(job, sandbox); `pids_limit` + `cpu_period`/`cpu_quota`
///   from the sandbox when set.
fn apply_sandbox_to_host_config(host_config: &mut HostConfig, sb: &SandboxConfig) -> Option<String> {
    // Network: deny by default (override any job network_mode); the workload
    // keeps egress only when the operator opted in via allow_network.
    if !sb.allow_network {
        host_config.network_mode = Some("none".into());
    }

    // Drop all caps + forbid privilege escalation.
    host_config.cap_drop = Some(vec!["ALL".into()]);
    let mut security_opt = host_config.security_opt.take().unwrap_or_default();
    if !security_opt.iter().any(|o| o == "no-new-privileges:true") {
        security_opt.push("no-new-privileges:true".into());
    }
    host_config.security_opt = Some(security_opt);

    // Read-only rootfs + private writable /tmp (parity with the nsjail tmpfs).
    // The run_dir is bind-mounted RW above, so outputs/ipc still work.
    host_config.readonly_rootfs = Some(true);
    let mut tmpfs = host_config.tmpfs.take().unwrap_or_default();
    tmpfs.insert("/tmp".into(), format!("rw,size={}m", sb.tmpfs_size_mb));
    host_config.tmpfs = Some(tmpfs);

    // Memory ceiling: the tighter of the per-job limit and the sandbox cap.
    if let Some(mem) = sb.memory_limit {
        let mem = mem as i64;
        host_config.memory = Some(match host_config.memory {
            Some(existing) if existing > 0 => existing.min(mem),
            _ => mem,
        });
    }

    // PID cap.
    if let Some(pids) = sb.pids_max {
        host_config.pids_limit = Some(pids as i64);
    }

    // CPU quota: cpu_ms_per_sec is ms of CPU per wall-second. With the standard
    // 100ms period, quota = cpu_ms_per_sec * 100 (e.g. 500 → 50_000 = 50%).
    if let Some(cpu_ms) = sb.cpu_ms_per_sec {
        host_config.cpu_period = Some(100_000);
        host_config.cpu_quota = Some((cpu_ms as i64) * 100);
    }

    // Non-root: run the container process as the unprivileged sandbox uid.
    Some(format!("{}:{}", sb.sandbox_uid, sb.sandbox_uid))
}

/// Build the list of environment variables for the container.
///
/// Resolved secrets in `run_context.resolved_env` overlay `run_context.env`
/// — `env` still contains the unresolved `{{secret:KEY}}` templates as a
/// defense-in-depth guarantee against accidental persistence, so we must
/// prefer the resolved value when present.
fn build_env_vars(config: &DockerConfig, run_context: &RunContext) -> Vec<String> {
    let mut vars: Vec<String> = Vec::new();

    // Config-level env vars first
    for (k, v) in &config.env {
        vars.push(format!("{k}={v}"));
    }

    // RunContext env vars (take precedence — AITHERICON_* and others from staging).
    // Skip host-path AITHERICON_* vars since we override them with container paths.
    // For any name present in `resolved_env`, use the resolved plaintext rather
    // than the `{{secret:KEY}}` template that lives in `env`.
    for (k, v) in &run_context.env {
        if k.starts_with("AITHERICON_") {
            continue;
        }
        let effective = run_context.resolved_env.get(k).unwrap_or(v);
        vars.push(format!("{k}={effective}"));
    }

    // Resolved-only entries that don't appear in `env` (in practice the
    // PlanSecretsHook only writes keys that exist in `env`, but be defensive).
    for (k, v) in &run_context.resolved_env {
        if k.starts_with("AITHERICON_") {
            continue;
        }
        if !run_context.env.contains_key(k) {
            vars.push(format!("{k}={v}"));
        }
    }

    vars
}

/// Wait for a container to exit and return the exit code.
///
/// Falls back to `inspect_container` if the wait stream errors or is empty,
/// which can happen for very short-lived containers.
async fn wait_container(
    client: &Docker,
    container_id: &str,
) -> Result<i64, bollard::errors::Error> {
    let opts = WaitContainerOptionsBuilder::new()
        .condition("not-running")
        .build();

    let mut stream = client.wait_container(container_id, Some(opts));
    match stream.next().await {
        Some(Ok(response)) => Ok(response.status_code),
        Some(Err(e)) => {
            // Wait stream errored — container may have already exited.
            // Fall back to inspect.
            debug!(
                container_id,
                error = %e,
                "wait stream errored, falling back to inspect"
            );
            inspect_exit_code(client, container_id).await
        }
        None => {
            // Stream ended without a response — inspect to get exit code
            inspect_exit_code(client, container_id).await
        }
    }
}

/// Inspect a container and extract its exit code.
async fn inspect_exit_code(
    client: &Docker,
    container_id: &str,
) -> Result<i64, bollard::errors::Error> {
    let info = client
        .inspect_container(
            container_id,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await?;
    let exit_code = info.state.and_then(|s| s.exit_code).unwrap_or(-1);
    Ok(exit_code)
}

/// Stop a container gracefully (SIGTERM + grace period), then force kill if needed.
async fn stop_container(client: &Docker, container_id: &str) {
    let opts = StopContainerOptionsBuilder::new()
        .t(STOP_GRACE_SECS)
        .build();

    match client.stop_container(container_id, Some(opts)).await {
        Ok(()) => {
            debug!(container_id, "container stopped");
        }
        Err(e) => {
            // Container may have already exited — that's fine
            debug!(
                container_id,
                error = %e,
                "stop_container returned error (may have already exited)"
            );
        }
    }
}

/// Remove a container, forcing removal if it's still running.
async fn remove_container(
    client: &Docker,
    container_id: &str,
) -> Result<(), bollard::errors::Error> {
    let opts = RemoveContainerOptionsBuilder::new().force(true).build();
    client.remove_container(container_id, Some(opts)).await
}

/// Capture stdout and stderr from container logs into tail buffers.
async fn capture_logs(
    client: &Docker,
    container_id: &str,
    max_bytes: usize,
) -> (Option<String>, Option<String>) {
    let opts = LogsOptionsBuilder::new()
        .stdout(true)
        .stderr(true)
        .follow(false)
        .build();

    let mut stdout_buf = TailBuffer::new(max_bytes);
    let mut stderr_buf = TailBuffer::new(max_bytes);

    let mut stream = client.logs(container_id, Some(opts));

    while let Some(result) = stream.next().await {
        match result {
            Ok(output) => match output {
                LogOutput::StdOut { message } => {
                    stdout_buf.push(&message);
                }
                LogOutput::StdErr { message } => {
                    stderr_buf.push(&message);
                }
                _ => {}
            },
            Err(e) => {
                debug!(container_id, error = %e, "error reading container logs");
                break;
            }
        }
    }

    (stdout_buf.into_string(), stderr_buf.into_string())
}

#[cfg(test)]
mod sandbox_host_config_tests {
    use super::*;
    use crate::ResourceLimits;
    use aithericon_executor_domain::{ExecutionSpec, RunDirectory};
    use std::path::PathBuf;
    use std::time::Duration;

    fn sandbox() -> SandboxConfig {
        SandboxConfig {
            nsjail_bin: "nsjail".into(),
            memory_limit: Some(64 * 1024 * 1024),
            cpu_ms_per_sec: Some(500),
            pids_max: Some(128),
            rlimit_fsize_mb: None,
            rlimit_nofile: None,
            allow_network: false,
            tmpfs_size_mb: 64,
            sandbox_uid: 99999,
            readonly_mounts: vec![],
            writable_mounts: vec![],
        }
    }

    fn docker_config() -> DockerConfig {
        DockerConfig {
            image: "alpine".into(),
            command: vec![],
            entrypoint: None,
            env: Default::default(),
            pull_policy: PullPolicy::IfNotPresent,
            resource_limits: None,
            network_mode: Some("bridge".into()),
            extra_volumes: vec![],
            remove_container: true,
        }
    }

    fn run_ctx() -> RunContext {
        let rd = RunDirectory::new(&PathBuf::from("/data/exec"), "docker-1");
        RunContext::for_test(
            "docker-1",
            ExecutionSpec {
                backend: "docker".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({}),
                config_ref: None,
            },
            rd,
            Duration::from_secs(60),
        )
    }

    #[test]
    fn sandbox_off_leaves_host_config_untouched() {
        let body = build_container_body(&docker_config(), &run_ctx(), None);
        let hc = body.host_config.unwrap();
        assert_eq!(
            hc.network_mode.as_deref(),
            Some("bridge"),
            "job network_mode must be preserved when no sandbox"
        );
        assert!(hc.cap_drop.is_none());
        assert!(hc.readonly_rootfs.is_none());
        assert!(hc.pids_limit.is_none());
        assert!(body.user.is_none());
    }

    #[test]
    fn sandbox_enforces_isolation_floors() {
        let sb = sandbox();
        let body = build_container_body(&docker_config(), &run_ctx(), Some(&sb));
        let hc = body.host_config.unwrap();
        // network denied — overrides the job's "bridge"
        assert_eq!(hc.network_mode.as_deref(), Some("none"));
        // all caps dropped + no privilege escalation
        assert_eq!(hc.cap_drop, Some(vec!["ALL".to_string()]));
        assert!(hc
            .security_opt
            .unwrap()
            .iter()
            .any(|o| o == "no-new-privileges:true"));
        // read-only rootfs + private writable /tmp
        assert_eq!(hc.readonly_rootfs, Some(true));
        assert!(hc.tmpfs.unwrap().contains_key("/tmp"));
        // resource caps
        assert_eq!(hc.memory, Some(64 * 1024 * 1024));
        assert_eq!(hc.pids_limit, Some(128));
        assert_eq!(hc.cpu_period, Some(100_000));
        assert_eq!(hc.cpu_quota, Some(50_000));
        // non-root user
        assert_eq!(body.user.as_deref(), Some("99999:99999"));
    }

    #[test]
    fn sandbox_allow_network_keeps_job_network() {
        let mut sb = sandbox();
        sb.allow_network = true;
        let body = build_container_body(&docker_config(), &run_ctx(), Some(&sb));
        assert_eq!(
            body.host_config.unwrap().network_mode.as_deref(),
            Some("bridge"),
            "allow_network must NOT force network none"
        );
    }

    #[test]
    fn sandbox_memory_takes_min_of_job_and_cap() {
        let mut cfg = docker_config();
        // job asks for 32 MiB; sandbox cap is 64 MiB → effective = 32 MiB (min)
        cfg.resource_limits = Some(ResourceLimits {
            memory_bytes: Some(32 * 1024 * 1024),
            cpu_shares: None,
            cpu_quota: None,
        });
        let body = build_container_body(&cfg, &run_ctx(), Some(&sandbox()));
        assert_eq!(body.host_config.unwrap().memory, Some(32 * 1024 * 1024));
    }
}
