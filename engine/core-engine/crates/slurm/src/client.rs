//! SlurmClient: `SchedulerClient` implementation using Slurm CLI over SSH.
//!
//! Handles job submission (`sbatch`), cancellation (`scancel`), and status
//! queries (`sacct`) via SSH commands to a Slurm login node.
//! Each client is constructed per-net with routing context (net_id, signal_place)
//! that gets stamped into the job comment for watcher routing.

use std::collections::HashMap;

use tokio::sync::Mutex;

use petri_domain::{JobStatus, SchedulerClient, SchedulerError, SubmitRequest, SubmitResult};
use petri_scheduler_bridge::RoutingMeta;

use crate::config::SlurmConfig;
use crate::ssh::{SshError, SshSession};
use crate::status_mapping;

/// Slurm scheduler client for job submission via SSH + CLI.
///
/// Constructed per-net with routing context embedded in the `--comment` flag.
/// Supports per-status signal routing: each `JobStatus` variant can target
/// a different place via `signal_routes`, with `fallback_place` used for
/// backward-compatible `petri_place` stamping.
///
/// The SSH connection is established lazily on first use and automatically
/// re-established if a command fails with a connection error.
pub struct SlurmClient {
    config: SlurmConfig,
    /// SSH session with lazy init and reconnect-on-failure.
    ssh: Mutex<Option<SshSession>>,
    /// Net ID for this client — stamped into job comment metadata.
    net_id: String,
    /// Per-status signal routes: maps a status name (e.g. "running", "completed")
    /// to the place that should receive the signal for that status.
    signal_routes: HashMap<String, String>,
    /// Fallback place stamped as `petri_place` for backward compatibility.
    fallback_place: String,
}

impl SlurmClient {
    /// Create a new Slurm client with per-status signal routing.
    ///
    /// SSH connection is established lazily on first command.
    ///
    /// # Arguments
    /// * `config` - Slurm SSH configuration
    /// * `net_id` - Petri net ID (stamped into job comment)
    /// * `fallback_place` - Default place stamped as `petri_place`
    /// * `signal_routes` - Per-status routing map (status name → place name)
    pub fn new(
        config: SlurmConfig,
        net_id: impl Into<String>,
        fallback_place: impl Into<String>,
        signal_routes: HashMap<String, String>,
    ) -> Self {
        Self {
            config,
            ssh: Mutex::new(None),
            net_id: net_id.into(),
            signal_routes,
            fallback_place: fallback_place.into(),
        }
    }

    /// Convenience constructor that routes all statuses to a single place.
    pub fn new_single_place(
        config: SlurmConfig,
        net_id: impl Into<String>,
        signal_place: impl Into<String>,
    ) -> Self {
        Self::new(config, net_id, signal_place, HashMap::new())
    }

    /// Execute an SSH command with automatic reconnection on connection failure.
    ///
    /// On a connection error (dead session, network blip), drops the stale
    /// session, establishes a fresh one, and retries the command once.
    async fn exec_with_reconnect(&self, command: &str) -> Result<String, SshError> {
        let mut guard = self.ssh.lock().await;

        // Ensure we have a session
        if guard.is_none() {
            *guard = Some(SshSession::connect(&self.config).await?);
        }

        // Try the command
        match guard.as_ref().unwrap().exec(command).await {
            Ok(output) => Ok(output),
            Err(SshError::Connection(_)) => {
                // Connection is dead — reconnect and retry once
                tracing::warn!("SSH connection lost, reconnecting for retry");
                *guard = Some(SshSession::connect(&self.config).await?);
                guard.as_ref().unwrap().exec(command).await
            }
            Err(e) => Err(e),
        }
    }

    /// Run the worker template ON an already-held Slurm allocation via `srun`.
    ///
    /// The L2 leased-body dispatch path. Where [`SlurmClient::submit`]'s default
    /// branch `sbatch`es a NEW job, this attaches a step to the held allocation
    /// `alloc_id` (`srun --jobid=<alloc_id> … <template>`) so the body runs on
    /// the leased nodes. It carries the same `--comment` routing metadata and
    /// the same `PETRI_TOKEN_DATA` / `EXECUTOR_TARGET_EXEC_ID` env as the sbatch
    /// path, and runs the same worker template (`<template_dir>/<id>.sh`) — only
    /// the launch verb (`srun --jobid` vs `sbatch`) differs.
    ///
    /// Dispatched ASYNC (fire-and-forget) like `sbatch`, NOT synchronously: the
    /// `srun` is launched detached (`nohup … &`) so this method returns the
    /// instant the step starts. This is required by the scheduler-net pipeline —
    /// `forward_to_executor` fires on submit success and only THEN enqueues the
    /// job to apalis for the executor to pull; a blocking `srun` would hold the
    /// `scheduler_submit` effect open until the executor had already idle-timed-
    /// out waiting for a job that hadn't been enqueued yet (PerJob orphan, exit
    /// 75). The body's result flows back over NATS; Slurm-side failures surface
    /// via the watcher's `sig_failed`. Since `srun` returns no parsable batch id,
    /// [`SubmitResult::scheduler_job_id`] is set to the held `alloc_id` for
    /// status correlation (the step lives under that allocation's job id).
    async fn submit_into_alloc(
        &self,
        alloc_id: &str,
        request: &SubmitRequest,
    ) -> Result<SubmitResult, SchedulerError> {
        let comment_json = self.build_comment_json(&request.signal_key);
        let token_data_json = serde_json::to_string(&request.token_data).unwrap_or_default();

        let template_path = format!(
            "{}/{}.sh",
            self.config.template_dir, request.job_template_id
        );

        let command = crate::alloc::srun_into_alloc_template_command(
            alloc_id,
            &comment_json,
            &request.signal_key.replace('\'', "_"),
            &token_data_json,
            &request.execution_id.replace('\'', "_"),
            &template_path,
        );

        // ASYNC / fire-and-forget — the same dispatch contract as `sbatch`
        // (which returns a job-id immediately). scheduler-net's
        // `forward_to_executor` fires on submit SUCCESS (NOT on sig_running) and
        // enqueues the job to apalis; the srun-launched executor (PerJob,
        // idle-waiting) then pulls it. A SYNCHRONOUS srun would block THIS effect
        // from returning until the executor has already exited — so the enqueue
        // (which needs the effect to return) never reaches the still-waiting
        // executor, which orphans (exit 75). Detach instead: `nohup` + redirect
        // every fd + background, so the SSH command returns the instant the step
        // is launched. The body's success/failure flows back over NATS (the
        // executor reports its own result); a Slurm-side crash is caught by the
        // watcher's sig_failed, exactly like the sbatch path.
        let exec_tag = request.execution_id.replace(['\'', '/'], "_");
        let detached =
            format!("nohup {command} > /tmp/petri-srun-{exec_tag}.out 2>&1 < /dev/null & echo dispatched");
        self.exec_with_reconnect(&detached)
            .await
            .map_err(map_ssh_err("srun into allocation dispatch failed"))?;

        tracing::info!(
            alloc_id = %alloc_id,
            template = %request.job_template_id,
            signal_key = %request.signal_key,
            execution_id = %request.execution_id,
            net_id = %self.net_id,
            "Slurm body dispatched onto held allocation via srun (async, detached)"
        );

        // srun yields no parsable batch id; the step lives under the held
        // allocation's job id, so correlate status against alloc_id.
        Ok(SubmitResult {
            scheduler_job_id: alloc_id.to_string(),
        })
    }

    /// Build the routing metadata JSON for the `--comment` flag.
    fn build_comment_json(&self, corr: &str) -> String {
        let routing = RoutingMeta {
            net_id: self.net_id.clone(),
            fallback_place: self.fallback_place.clone(),
            signal_routes: self.signal_routes.clone(),
            event_routes: Default::default(),
            signal_key: corr.to_string(),
        };
        let meta = routing.to_meta_tags();
        serde_json::to_string(&meta).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Map an [`SshError`] to a [`SchedulerError`].
fn map_ssh_err(context: &str) -> impl FnOnce(SshError) -> SchedulerError + '_ {
    move |e: SshError| match &e {
        SshError::Connection(_) => SchedulerError::NotConnected(format!("{}: {}", context, e)),
        SshError::CommandFailed { .. } => {
            SchedulerError::SubmissionFailed(format!("{}: {}", context, e))
        }
    }
}

#[async_trait::async_trait]
impl SchedulerClient for SlurmClient {
    async fn submit(&self, request: SubmitRequest) -> Result<SubmitResult, SchedulerError> {
        // L2 leased-body branch: when the token carries a held allocation id
        // (set by the compiler as `spec.alloc_id` on the leased-body's
        // SchedulerSubmitInput — an opaque Value field, no engine type change),
        // run the body ON that allocation via `srun --jobid` instead of queuing
        // a new `sbatch` job. The id rides `token_data["spec"]["alloc_id"]`.
        let alloc_id = request
            .token_data
            .get("spec")
            .and_then(|s| s.get("alloc_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        if let Some(alloc_id) = alloc_id {
            return self.submit_into_alloc(alloc_id, &request).await;
        }

        let comment_json = self.build_comment_json(&request.signal_key);

        // Build the sbatch command
        // Token data is passed as PETRI_TOKEN_DATA env var
        let token_data_json = serde_json::to_string(&request.token_data).unwrap_or_default();

        let template_path = format!(
            "{}/{}.sh",
            self.config.template_dir, request.job_template_id
        );

        // EXECUTOR_TARGET_EXEC_ID drives the executor's PerJob consumer mode:
        // the spawned sbatch process creates an ephemeral consumer filtered to
        // its own exec_id, so it consumes exactly its dispatched job and no
        // other (no shared-consumer race across sbatches).
        let command = format!(
            "sbatch --parsable --comment='{}' --job-name='petri-{}' --export=ALL,PETRI_TOKEN_DATA='{}',EXECUTOR_TARGET_EXEC_ID='{}' {}",
            comment_json.replace('\'', "'\\''"),
            request.signal_key.replace('\'', "_"),
            token_data_json.replace('\'', "'\\''"),
            request.execution_id.replace('\'', "_"),
            template_path,
        );

        let output = self
            .exec_with_reconnect(&command)
            .await
            .map_err(map_ssh_err("sbatch failed"))?;

        // sbatch --parsable outputs: job_id[;cluster_name]
        let job_id = output
            .trim()
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        if job_id.is_empty() {
            return Err(SchedulerError::SubmissionFailed(format!(
                "sbatch returned empty job ID. Output: {}",
                output.trim()
            )));
        }

        tracing::info!(
            job_id = %job_id,
            template = %request.job_template_id,
            signal_key = %request.signal_key,
            execution_id = %request.execution_id,
            net_id = %self.net_id,
            "Slurm job submitted"
        );

        Ok(SubmitResult {
            scheduler_job_id: job_id,
        })
    }

    async fn cancel(&self, scheduler_job_id: &str) -> Result<(), SchedulerError> {
        let command = format!("scancel {}", scheduler_job_id);

        self.exec_with_reconnect(&command)
            .await
            .map_err(|e| match &e {
                SshError::Connection(_) => {
                    SchedulerError::CancellationFailed(format!("SSH error: {}", e))
                }
                SshError::CommandFailed { .. } => {
                    SchedulerError::CancellationFailed(format!("scancel failed: {}", e))
                }
            })?;

        tracing::info!(
            scheduler_job_id = %scheduler_job_id,
            "Slurm job cancelled"
        );

        Ok(())
    }

    async fn status(&self, scheduler_job_id: &str) -> Result<JobStatus, SchedulerError> {
        let command = format!(
            "sacct -j {} -o State,ExitCode --parsable2 -n --noconvert",
            scheduler_job_id
        );

        let output = self
            .exec_with_reconnect(&command)
            .await
            .map_err(map_ssh_err("sacct failed"))?;

        // Parse the first line: State|ExitCode
        let line = output.lines().next().unwrap_or("").trim();
        if line.is_empty() {
            return Err(SchedulerError::QueryFailed(format!(
                "sacct returned no data for job {}",
                scheduler_job_id
            )));
        }

        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 2 {
            return Err(SchedulerError::QueryFailed(format!(
                "Unexpected sacct output format: {}",
                line
            )));
        }

        let state = parts[0];
        status_mapping::map_slurm_state(state)
            .ok_or_else(|| SchedulerError::QueryFailed(format!("Unknown Slurm state: {}", state)))
    }

    fn name(&self) -> &str {
        "slurm"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_comment_json() {
        let client = SlurmClient::new_single_place(SlurmConfig::default(), "test-net", "inbox");

        let json = client.build_comment_json("train-alpha:0");
        let parsed: HashMap<String, String> = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.get("petri_net_id").unwrap(), "test-net");
        assert_eq!(parsed.get("petri_place").unwrap(), "inbox");
        assert_eq!(parsed.get("petri_signal_key").unwrap(), "train-alpha:0");
    }

    #[test]
    fn test_build_comment_json_with_routes() {
        let mut routes = HashMap::new();
        routes.insert("running".into(), "running_inbox".into());
        routes.insert("completed".into(), "done_inbox".into());

        let client = SlurmClient::new(SlurmConfig::default(), "test-net", "inbox", routes);

        let json = client.build_comment_json("job:0");
        let parsed: HashMap<String, String> = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.get("petri_signal_running").unwrap(), "running_inbox");
        assert_eq!(parsed.get("petri_signal_completed").unwrap(), "done_inbox");
    }

    #[test]
    fn test_parse_sbatch_output() {
        // sbatch --parsable outputs job_id or job_id;cluster
        let output = "12345\n";
        let job_id = output.trim().split(';').next().unwrap_or("").trim();
        assert_eq!(job_id, "12345");

        let output = "12345;cluster1\n";
        let job_id = output.trim().split(';').next().unwrap_or("").trim();
        assert_eq!(job_id, "12345");
    }

    #[test]
    fn test_parse_sacct_status_output() {
        let output = "COMPLETED|0:0\n";
        let line = output.lines().next().unwrap().trim();
        let parts: Vec<&str> = line.split('|').collect();
        assert_eq!(parts[0], "COMPLETED");
        assert_eq!(parts[1], "0:0");

        let status = status_mapping::map_slurm_state(parts[0]);
        assert_eq!(status, Some(JobStatus::Completed));
    }
}
