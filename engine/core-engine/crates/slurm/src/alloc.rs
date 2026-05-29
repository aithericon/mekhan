//! Slurm allocation primitives (L0) over an [`SshSession`].
//!
//! These are the low-level building blocks for the resource-lease flow: hold a
//! Slurm allocation without running anything (`salloc --no-shell`), look up the
//! node it landed on (`scontrol show job`), run a command *on* a held
//! allocation (`srun --jobid=`), and release it (`scancel`).
//!
//! Unlike `SlurmClient`'s batch path (`sbatch` → fire-and-forget job), an
//! allocation held by `salloc --no-shell` stays alive (holding the nodes) until
//! it is explicitly cancelled or its time limit expires. That is the lease.
//!
//! ## Idempotency
//!
//! Every allocation is stamped with `--job-name=petri-<grant_id>` and
//! `--comment=<grant_id>` so a caller can find an already-held allocation for a
//! given grant via `squeue --name=petri-<grant_id>` (left to the higher layer).
//!
//! These functions take an `&SshSession` so the caller owns the
//! lazy-connect / reconnect-once lifecycle (mirroring `SlurmClient`).

use serde_json::Value as JsonValue;

use crate::models;
use crate::ssh::{SshError, SshSession};

/// Errors from Slurm allocation operations.
#[derive(Debug, thiserror::Error)]
pub enum AllocError {
    /// Underlying SSH command failed or the session is broken.
    #[error("ssh: {0}")]
    Ssh(#[from] SshError),

    /// `salloc` returned without a parseable `Granted job allocation <id>` line.
    #[error("salloc returned no alloc id: {0}")]
    NoAllocId(String),

    /// `scontrol` output could not be parsed for the requested field.
    #[error("scontrol parse failed: {0}")]
    Parse(String),
}

/// The result of holding a Slurm allocation.
///
/// `node` / `expiry` are `None` while the allocation is still pending (Slurm
/// reports `NodeList=(null)` / `EndTime=Unknown` until the nodes are granted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    /// Slurm allocation / job id (from `salloc`).
    pub alloc_id: String,
    /// Allocated node (from `scontrol show job ... NodeList`), if assigned yet.
    pub node: Option<String>,
    /// Allocation end time (from `scontrol show job ... EndTime`), if known.
    pub expiry: Option<String>,
}

/// Shell-escape a value for single-quoted embedding (`'…'`).
///
/// Mirrors the escaping `SlurmClient::submit` uses for `--comment` / env vars:
/// close the quote, emit an escaped quote, reopen. Safe for arbitrary content.
fn sq(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Build the `salloc --no-shell` command string for a grant.
///
/// Holds an allocation WITHOUT running anything. `-N1` requests a single node;
/// `--no-shell` returns immediately after the grant instead of dropping into a
/// subshell. The grant id is stamped into both `--job-name` (so the allocation
/// is discoverable via `squeue --name`) and `--comment` (idempotency lookup).
///
/// `request` is the resolved effect request JSON; recognised optional keys:
/// - `flags` (string): extra raw flags appended verbatim (e.g. `"-p debug -t 00:10:00"`).
/// - `partition` (string): mapped to `-p <partition>`.
/// - `time_limit` (string): mapped to `-t <time_limit>`.
/// - `cpus` (number/string): mapped to `-n <cpus>`.
///
/// stderr is merged into stdout (`2>&1`) because `salloc` prints
/// `Granted job allocation <id>` to stderr.
pub fn salloc_no_shell_command(grant_id: &str, request: &JsonValue) -> String {
    let mut flags = String::new();

    if let Some(partition) = request.get("partition").and_then(|v| v.as_str()) {
        flags.push_str(&format!(" -p '{}'", sq(partition)));
    }
    if let Some(time_limit) = request.get("time_limit").and_then(|v| v.as_str()) {
        flags.push_str(&format!(" -t '{}'", sq(time_limit)));
    }
    if let Some(cpus) = request.get("cpus") {
        // Accept either a JSON number or a string.
        let cpus = cpus
            .as_u64()
            .map(|n| n.to_string())
            .or_else(|| cpus.as_str().map(|s| s.to_string()));
        if let Some(cpus) = cpus {
            flags.push_str(&format!(" -n '{}'", sq(&cpus)));
        }
    }
    if let Some(extra) = request.get("flags").and_then(|v| v.as_str()) {
        flags.push(' ');
        flags.push_str(extra);
    }

    format!(
        "salloc --no-shell -N1 --job-name='petri-{}' --comment='{}'{} 2>&1",
        sq(grant_id),
        sq(grant_id),
        flags,
    )
}

/// Hold a Slurm allocation for `grant_id` WITHOUT running anything.
///
/// Runs `salloc --no-shell -N1 …` and parses the granted job id from the
/// output. Does NOT resolve the node — call [`scontrol_node`] afterwards (the
/// node may still be pending at the moment salloc returns).
pub async fn salloc_no_shell(
    ssh: &SshSession,
    grant_id: &str,
    request: &JsonValue,
) -> Result<String, AllocError> {
    let command = salloc_no_shell_command(grant_id, request);
    let output = ssh.exec(&command).await?;

    models::parse_granted_job_id(&output).ok_or_else(|| AllocError::NoAllocId(output.trim().to_string()))
}

/// Look up an existing (still-live) allocation held for `grant_id`.
///
/// The L1 idempotency probe (mirrors the HTTP allocator's `Idempotency-Key`
/// contract): an allocation is stamped `--job-name=petri-<grant_id>`, so a
/// re-fire can discover the already-held allocation instead of allocating a
/// second one. Runs `squeue --name='petri-<grant_id>' -h -o '%i' -t …` and
/// returns the first live job id, or `None` if no live allocation exists.
///
/// Only *active* states are considered (`PENDING,RUNNING,CONFIGURING,
/// COMPLETING`) — a finished/cancelled job of the same name must NOT be
/// reused (it no longer holds the nodes). The `%i` job-id field is trimmed and
/// the first non-empty line is returned.
pub async fn squeue_find_by_name(
    ssh: &SshSession,
    grant_id: &str,
) -> Result<Option<String>, AllocError> {
    let command = format!(
        "squeue --name='petri-{}' -h -o '%i' -t PENDING,RUNNING,CONFIGURING,COMPLETING",
        sq(grant_id),
    );
    let output = ssh.exec(&command).await?;
    Ok(output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string))
}

/// Look up the node (and expiry) an allocation landed on.
///
/// Runs `scontrol show job <alloc_id> -o` and parses `NodeList` + `EndTime`.
/// Tolerates a still-pending allocation: `NodeList=(null)` / `EndTime=Unknown`
/// collapse to `None` rather than erroring. Errors only if `scontrol` returns
/// no record at all (`JobId` absent), which means the id is unknown to Slurm.
pub async fn scontrol_node(ssh: &SshSession, alloc_id: &str) -> Result<Allocation, AllocError> {
    let command = format!("scontrol show job '{}' -o", sq(alloc_id));
    let output = ssh.exec(&command).await?;

    // Sanity check: a real record carries a JobId field.
    if models::scontrol_field(&output, "JobId").is_none() {
        return Err(AllocError::Parse(format!(
            "scontrol show job {} returned no JobId record: {}",
            alloc_id,
            output.trim()
        )));
    }

    let node = models::scontrol_field(&output, "NodeList")
        .as_deref()
        .and_then(models::scontrol_value_present);
    let expiry = models::scontrol_field(&output, "EndTime")
        .as_deref()
        .and_then(models::scontrol_value_present);

    Ok(Allocation {
        alloc_id: alloc_id.to_string(),
        node,
        expiry,
    })
}

/// Release a held allocation: `scancel <alloc_id>`.
pub async fn scancel(ssh: &SshSession, alloc_id: &str) -> Result<(), AllocError> {
    let command = format!("scancel '{}'", sq(alloc_id));
    ssh.exec(&command).await?;
    Ok(())
}

/// Run a command ON an already-held allocation: `srun --jobid=<alloc_id> -- <cmd…>`.
///
/// `--jobid=` attaches the step to the existing allocation rather than queuing
/// a new one, so the work runs on the leased nodes. Returns the command's
/// stdout. Used by the L2 body-exec path's bare-command form (e.g. tests).
pub async fn srun_into_alloc(
    ssh: &SshSession,
    alloc_id: &str,
    cmd: &str,
) -> Result<String, AllocError> {
    // `--` terminates srun flag parsing; the body runs under `bash -c` so that
    // a multi-token / piped `cmd` behaves like a shell command line.
    let command = format!("srun --jobid='{}' -- bash -c '{}'", sq(alloc_id), sq(cmd));
    let output = ssh.exec(&command).await?;
    Ok(output)
}

/// Build the `srun --jobid=<alloc_id> … <template_path>` command string.
///
/// The L2 body-dispatch form: instead of `sbatch <template>` (which queues a
/// NEW job), this attaches a step to an already-held allocation via
/// `--jobid=<alloc_id>`, so the worker template runs ON the leased nodes. It
/// mirrors `SlurmClient::submit`'s sbatch command shape exactly, minus the
/// batch-only flags (`--parsable`):
///
/// - `--comment='<routing-json>'` — watcher routing metadata (same as sbatch).
/// - `--job-name='petri-<signal_key>'` — discoverability (same as sbatch).
/// - `--export=ALL,PETRI_TOKEN_DATA='…',EXECUTOR_TARGET_EXEC_ID='…'` — the job
///   token data + the executor PerJob consumer target exec id (same as sbatch).
/// - `<template_path>` — the same worker template script the sbatch path runs.
///
/// All embedded values are single-quote escaped via [`sq`].
pub fn srun_into_alloc_template_command(
    alloc_id: &str,
    comment_json: &str,
    job_name: &str,
    token_data_json: &str,
    execution_id: &str,
    template_path: &str,
) -> String {
    format!(
        "srun --jobid='{}' --comment='{}' --job-name='petri-{}' --export=ALL,PETRI_TOKEN_DATA='{}',EXECUTOR_TARGET_EXEC_ID='{}' {}",
        sq(alloc_id),
        sq(comment_json),
        sq(job_name),
        sq(token_data_json),
        sq(execution_id),
        template_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_salloc_command_minimal() {
        let cmd = salloc_no_shell_command("grant-abc", &json!({}));
        assert!(cmd.starts_with("salloc --no-shell -N1"));
        assert!(cmd.contains("--job-name='petri-grant-abc'"));
        assert!(cmd.contains("--comment='grant-abc'"));
        assert!(cmd.ends_with(" 2>&1"));
    }

    #[test]
    fn test_salloc_command_with_flags() {
        let req = json!({
            "partition": "debug",
            "time_limit": "00:10:00",
            "cpus": 4,
            "flags": "--gres=gpu:1"
        });
        let cmd = salloc_no_shell_command("g1", &req);
        assert!(cmd.contains("-p 'debug'"));
        assert!(cmd.contains("-t '00:10:00'"));
        assert!(cmd.contains("-n '4'"));
        assert!(cmd.contains("--gres=gpu:1"));
    }

    #[test]
    fn test_salloc_command_cpus_as_string() {
        let cmd = salloc_no_shell_command("g1", &json!({ "cpus": "8" }));
        assert!(cmd.contains("-n '8'"), "{}", cmd);
    }

    #[test]
    fn test_salloc_command_escapes_grant_id() {
        // A grant id with a quote must not break out of the single-quoted arg.
        let cmd = salloc_no_shell_command("a'b", &json!({}));
        assert!(cmd.contains("--job-name='petri-a'\\''b'"), "{}", cmd);
    }

    #[test]
    fn test_squeue_find_command_shape() {
        // Verifies the idempotency-probe command embeds the petri-<grant> name
        // and only matches live states. (Pure shape — no SSH.)
        let grant_id = "inst-1:node-2";
        let expected = format!(
            "squeue --name='petri-{}' -h -o '%i' -t PENDING,RUNNING,CONFIGURING,COMPLETING",
            grant_id,
        );
        assert!(expected.contains("--name='petri-inst-1:node-2'"));
        assert!(expected.contains("-t PENDING,RUNNING,CONFIGURING,COMPLETING"));
    }

    #[test]
    fn test_srun_command_shape() {
        // Pure string-shape assertion; no SSH involved.
        let alloc_id = "12345";
        let cmd = "echo lease-ok";
        let expected = format!("srun --jobid='{}' -- bash -c '{}'", alloc_id, cmd);
        assert_eq!(expected, "srun --jobid='12345' -- bash -c 'echo lease-ok'");
    }

    #[test]
    fn test_srun_template_command_shape() {
        // The L2 body-dispatch form: srun into a held alloc running the worker
        // template, carrying the same routing/env flags as the sbatch path.
        let cmd = srun_into_alloc_template_command(
            "12345",
            "{\"petri_net_id\":\"n\"}",
            "key:0",
            "{\"run_id\":\"r\"}",
            "exec-1",
            "/opt/petri/templates/default.sh",
        );
        assert!(cmd.starts_with("srun --jobid='12345'"), "{}", cmd);
        assert!(cmd.contains("--comment='{\"petri_net_id\":\"n\"}'"), "{}", cmd);
        assert!(cmd.contains("--job-name='petri-key:0'"), "{}", cmd);
        assert!(
            cmd.contains("--export=ALL,PETRI_TOKEN_DATA='{\"run_id\":\"r\"}',EXECUTOR_TARGET_EXEC_ID='exec-1'"),
            "{}",
            cmd
        );
        assert!(cmd.ends_with(" /opt/petri/templates/default.sh"), "{}", cmd);
        // No batch-only --parsable flag (srun is not sbatch).
        assert!(!cmd.contains("--parsable"), "{}", cmd);
    }

    #[test]
    fn test_srun_template_command_escapes() {
        // A signal key with a quote must not break out of the single-quoted arg.
        let cmd = srun_into_alloc_template_command(
            "a'b",
            "{}",
            "k'1",
            "{}",
            "e'1",
            "/t.sh",
        );
        assert!(cmd.contains("--jobid='a'\\''b'"), "{}", cmd);
        assert!(cmd.contains("--job-name='petri-k'\\''1'"), "{}", cmd);
    }
}
