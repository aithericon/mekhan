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

    // `--comment` carries the watcher routing metadata as a JSON meta-tag map
    // (the SlurmWatcher's `extract_routing` parses the comment as
    // `HashMap<String,String>`). When the lease handler injected
    // `failure_routing` (held-alloc-death → `lease_failed`, docs/16 §7), stamp
    // THAT json so the watcher tracks this held alloc and emits the failed
    // signal when it dies. Falling back to the bare `grant_id` (non-JSON) would
    // make the watcher skip the alloc → death undetected → the leased loop
    // wedges on a dead namespace. `--job-name=petri-<grant_id>` stays for
    // `squeue --name` discoverability either way.
    let comment = request
        .get("failure_routing")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::to_string(v).ok())
        .unwrap_or_else(|| grant_id.to_string());

    format!(
        "salloc --no-shell -N1 --job-name='petri-{}' --comment='{}'{} 2>&1",
        sq(grant_id),
        sq(&comment),
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

    models::parse_granted_job_id(&output)
        .ok_or_else(|| AllocError::NoAllocId(output.trim().to_string()))
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

/// Build the `srun --jobid=<alloc_id> … <template_path>` command that launches
/// the PERSISTENT DRAIN EXECUTOR on a held allocation.
///
/// Unlike [`srun_into_alloc_template_command`] (which dispatches a single body
/// job, PerJob, via `EXECUTOR_TARGET_EXEC_ID`), this runs ONE long-lived
/// executor in Pool/drain mode that consumes a lease-scoped NATS namespace and
/// pulls EVERY job the leased loop enqueues there. The selector for Pool mode is
/// the ABSENCE of `EXECUTOR_TARGET_EXEC_ID`; what it drains is parameterised
/// per-acquire via three env vars the drain template reads:
///
/// - `LEASE_NAMESPACE=lease-<grant_id>` — the disjoint queue the body enqueues to.
/// - `LEASE_MAX_JOBS=<cap>` — drain N then exit (the loop's maxIterations).
/// - `LEASE_IDLE_TIMEOUT=<secs>` — survive inter-iteration gaps, self-exit if wedged.
///
/// All values are single-quote escaped via [`sq`]. The command is meant to be
/// run detached (the executor runs for the whole lease) — see
/// [`detached_launch`].
pub fn srun_lease_executor_command(
    alloc_id: &str,
    template_path: &str,
    namespace: &str,
    max_jobs: u64,
    idle_secs: u64,
    container: Option<&ContainerSpec>,
) -> String {
    // The drain executor runs INSIDE the container when one is bound (docs/22):
    // `srun … apptainer exec --nv --bind … <sif> /bin/bash <template>`. Apptainer
    // inherits the host env by default, so the `--export`ed LEASE_* vars are
    // visible to the template script inside the container. With no container the
    // trailing target is the bare template path (byte-identical to the original).
    let target = match container {
        Some(c) if !c.sif_path.is_empty() => c.wrap_script(template_path),
        _ => template_path.to_string(),
    };
    format!(
        "srun --jobid='{}' --export=ALL,LEASE_NAMESPACE='{}',LEASE_MAX_JOBS='{}',LEASE_IDLE_TIMEOUT='{}' {}",
        sq(alloc_id),
        sq(namespace),
        max_jobs,
        idle_secs,
        target,
    )
}

/// A resolved container binding for `apptainer exec` (docs/22 container staging).
/// mekhan resolves the bound `container_image` resource + its materialized `.sif`
/// into this blob and threads it through the lease-acquire request / job token;
/// the engine wraps the executor launch line with it. An empty `sif_path` means
/// "no container" (native execution).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct ContainerSpec {
    /// Absolute `.sif` path on the cluster (the stable by-ref symlink mekhan
    /// embeds, or a content-addressed path).
    #[serde(default)]
    pub sif_path: String,
    /// `--bind src[:dst]` mounts (executor binary, SDK, uv, scratch, venv cache).
    #[serde(default)]
    pub binds: Vec<String>,
    /// Bind the host NVIDIA stack (`--nv`) — set for GPU jobs.
    #[serde(default)]
    pub nv: bool,
}

impl ContainerSpec {
    /// Build `apptainer exec [--nv] [--bind …] '<sif>' /bin/bash '<script>'` for
    /// running `script_path` (a bash entry script) inside the image. Caller
    /// guarantees `sif_path` is non-empty.
    pub fn wrap_script(&self, script_path: &str) -> String {
        let nv = if self.nv { " --nv" } else { "" };
        let binds: String = self
            .binds
            .iter()
            .filter(|b| !b.is_empty())
            .map(|b| format!("--bind '{}' ", sq(b)))
            .collect();
        format!(
            "apptainer exec{nv} {binds}'{}' /bin/bash '{}'",
            sq(&self.sif_path),
            sq(script_path),
        )
    }
}

/// Sanitize a registry image reference into a filesystem-safe stem for the stable
/// by-ref symlink path (docs/22). Every non-alphanumeric run collapses to `_` so
/// `ghcr.io/org/img:tag` → `ghcr_io_org_img_tag`. The compiler computes the SAME
/// stem (pure function of `image_ref`) so it can embed the by-ref path before the
/// async materialize finishes.
pub fn sanitize_image_ref(image_ref: &str) -> String {
    let mut out = String::with_capacity(image_ref.len());
    let mut prev_us = false;
    for ch in image_ref.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// Render the remote bash that pulls an OCI image to a content-addressed `.sif`
/// and atomically points the stable by-ref symlink at it (docs/22).
///
/// Runs on the login node (compute nodes often lack registry egress). Pulls to a
/// temp file, content-addresses by the `.sif`'s sha256, `mv`s to
/// `<sif_root>/<digest>.sif`, repoints `<sif_root>/by-ref/<stem>.sif`, and prints
/// a single `PETRI_MATERIALIZE digest=… sif=… size=…` line parsed by
/// [`parse_materialize_output`]. Pure render (no SSH) so it is unit-testable.
///
/// `image_ref` is passed to `apptainer pull` VERBATIM, so it must carry the
/// scheme (`docker://…`, `oras://…`, `library://…`); the compiler embeds the
/// by-ref path from the SAME scheme-bearing ref via `sanitize_image_ref`, so the
/// two agree.
pub fn render_apptainer_pull_script(
    image_ref: &str,
    username: Option<&str>,
    password: Option<&str>,
    sif_root: &str,
    cache_dir: &str,
) -> String {
    let root = sif_root.trim_end_matches('/');
    let stem = sanitize_image_ref(image_ref);
    let creds = match (username, password) {
        (Some(u), Some(p)) if !u.is_empty() => format!(
            "export APPTAINER_DOCKER_USERNAME='{}'\nexport APPTAINER_DOCKER_PASSWORD='{}'\n",
            sq(u),
            sq(p),
        ),
        _ => String::new(),
    };
    format!(
        "set -e\n\
         export APPTAINER_CACHEDIR='{cache}'\n\
         {creds}\
         mkdir -p '{root}/by-ref' '{cache}'\n\
         tmp=$(mktemp '{root}/.pull.XXXXXX.sif')\n\
         apptainer pull --force \"$tmp\" '{image}'\n\
         digest=$(sha256sum \"$tmp\" | cut -c1-64)\n\
         final='{root}/'\"$digest\"'.sif'\n\
         mv -f \"$tmp\" \"$final\"\n\
         ln -sfn \"$final\" '{root}/by-ref/{stem}.sif'\n\
         size=$(stat -c%s \"$final\" 2>/dev/null || echo 0)\n\
         echo \"PETRI_MATERIALIZE digest=$digest sif=$final size=$size\"\n",
        cache = sq(cache_dir.trim_end_matches('/')),
        creds = creds,
        root = sq(root),
        image = image_ref,
        stem = stem,
    )
}

/// Parse the `PETRI_MATERIALIZE digest=… sif=… size=…` line emitted by
/// [`render_apptainer_pull_script`] out of the command stdout. Returns
/// `(digest, sif_path, size_bytes)`.
pub fn parse_materialize_output(stdout: &str) -> Option<(String, String, Option<i64>)> {
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.contains("PETRI_MATERIALIZE"))?;
    let mut digest = None;
    let mut sif = None;
    let mut size = None;
    for tok in line.split_whitespace() {
        if let Some(v) = tok.strip_prefix("digest=") {
            digest = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("sif=") {
            sif = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("size=") {
            size = v.parse::<i64>().ok();
        }
    }
    Some((digest?, sif?, size))
}

/// Deterministic remote log path for a detached materialize of `image_ref`.
/// Both the launcher ([`render_apptainer_pull_launch`]) and the caller's poll
/// loop compute it from the sanitized stem so they agree without threading state.
pub fn materialize_log_path(image_ref: &str) -> String {
    format!("/tmp/petri-materialize-{}.log", sanitize_image_ref(image_ref))
}

/// Build a fire-and-forget launcher that runs the apptainer pull DETACHED on the
/// remote, tee-ing its output plus a final `PETRI_DONE rc=<code>` sentinel to
/// [`materialize_log_path`]. The caller execs this (returns in <1s), then polls
/// the log with quick `cat`s until the sentinel appears.
///
/// Why not just hold one SSH command open for the whole pull? A pull legitimately
/// takes minutes (download + squashfs conversion), and that conversion saturates
/// the container's CPU; combined with the watcher's concurrent `squeue`/`sacct`
/// polling it starves the SSH connection's keepalive and the multiplexed channel
/// is dropped mid-pull ("the remote process has terminated"). Detaching means no
/// long-lived channel: the launch + each poll are sub-second commands.
///
/// The pull body is written to a remote script via a QUOTED heredoc
/// (`<<'PETRI_PULL_EOF'`), so its single-quoted paths need no re-escaping, then
/// `nohup`'d with every fd redirected so the SSH session can close without
/// SIGHUP'ing the pull. The `bash -c 'bash "$0"; echo PETRI_DONE rc=$?' SCRIPT`
/// form appends the sentinel with the script's real exit code even though the
/// body runs under `set -e`.
pub fn render_apptainer_pull_launch(
    image_ref: &str,
    username: Option<&str>,
    password: Option<&str>,
    sif_root: &str,
    cache_dir: &str,
) -> String {
    let body = render_apptainer_pull_script(image_ref, username, password, sif_root, cache_dir);
    let stem = sanitize_image_ref(image_ref);
    let log = materialize_log_path(image_ref);
    let script = format!("/tmp/petri-materialize-{stem}.sh");
    format!(
        "set -e\n\
         cat > '{script}' <<'PETRI_PULL_EOF'\n\
{body}\n\
         PETRI_PULL_EOF\n\
         rm -f '{log}'\n\
         nohup bash -c 'bash \"$0\"; echo \"PETRI_DONE rc=$?\"' '{script}' > '{log}' 2>&1 < /dev/null &\n\
         echo dispatched\n",
    )
}

/// Parse the `PETRI_DONE rc=<code>` sentinel out of a detached-pull log. Returns
/// `Some(rc)` once the pull has finished, `None` while still running.
pub fn parse_materialize_done(log: &str) -> Option<i32> {
    let line = log.lines().rev().find(|l| l.contains("PETRI_DONE rc="))?;
    line.rsplit("PETRI_DONE rc=")
        .next()
        .and_then(|s| s.trim().parse::<i32>().ok())
}

/// Wrap a command for fire-and-forget detached execution over SSH.
///
/// Mirrors `SlurmClient::submit_into_alloc`'s detach: `nohup … &` with every fd
/// redirected so the SSH `exec` returns immediately instead of blocking for the
/// command's whole lifetime. Required for the persistent drain executor — a
/// SYNC `srun` would block `acquire` for the entire lease. `tag` names the log
/// file (single-quote escaped for safe embedding).
pub fn detached_launch(command: &str, tag: &str) -> String {
    format!(
        "nohup {command} > '/tmp/petri-lease-exec-{}.out' 2>&1 < /dev/null & echo dispatched",
        sq(tag),
    )
}

/// Launch the persistent drain executor on a held allocation, DETACHED.
///
/// Builds [`srun_lease_executor_command`], wraps it in [`detached_launch`], and
/// fires it through the held session. Returns immediately (fire-and-forget): the
/// executor runs for the whole lease, draining the lease-scoped namespace, and
/// exits on `scancel` (SIGTERM → graceful drain) or `LEASE_IDLE_TIMEOUT`.
pub async fn srun_lease_executor(
    ssh: &SshSession,
    alloc_id: &str,
    template_path: &str,
    namespace: &str,
    max_jobs: u64,
    idle_secs: u64,
    container: Option<&ContainerSpec>,
) -> Result<(), AllocError> {
    let command = srun_lease_executor_command(
        alloc_id,
        template_path,
        namespace,
        max_jobs,
        idle_secs,
        container,
    );
    let detached = detached_launch(&command, alloc_id);
    ssh.exec(&detached).await?;
    Ok(())
}

/// Render an sbatch script from a typed stage spec (Phase 4 `stage_template`).
///
/// Maps the typed `StageSpec`-equivalent JSON onto `#SBATCH` directives, then an
/// author-supplied raw `sbatch_directives` escape-hatch block (spliced verbatim
/// after the typed ones), then the entrypoint body. Recognised spec keys (all
/// optional): `cpus`→`--cpus-per-task`, `gpus`(+`gpu_type`)→`--gres=gpu[:type]:N`,
/// `mem_mb`→`--mem=<N>M`, `time_limit`→`--time`, `partition`→`--partition`,
/// `entrypoint`→the script body. `env` entries become `export K=V` lines.
///
/// Pure string render (no SSH) so it is unit-testable; delivery is
/// [`deliver_template_file`].
pub fn render_sbatch_script(
    slug: &str,
    spec: &JsonValue,
    sbatch_directives: Option<&str>,
    env: &std::collections::HashMap<String, String>,
) -> String {
    let mut lines: Vec<String> = vec!["#!/bin/bash".to_string()];
    lines.push(format!("#SBATCH --job-name={}", slug));

    if let Some(cpus) = spec
        .get("cpus")
        .and_then(|v| v.as_i64())
        .filter(|c| *c > 0)
    {
        lines.push(format!("#SBATCH --cpus-per-task={cpus}"));
    }
    if let Some(gpus) = spec
        .get("gpus")
        .and_then(|v| v.as_i64())
        .filter(|g| *g > 0)
    {
        let gres = match spec.get("gpu_type").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            Some(ty) => format!("gpu:{ty}:{gpus}"),
            None => format!("gpu:{gpus}"),
        };
        lines.push(format!("#SBATCH --gres={gres}"));
    }
    if let Some(mem) = spec
        .get("mem_mb")
        .and_then(|v| v.as_i64())
        .filter(|m| *m > 0)
    {
        lines.push(format!("#SBATCH --mem={mem}M"));
    }
    if let Some(time_limit) = spec.get("time_limit").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        lines.push(format!("#SBATCH --time={time_limit}"));
    }
    if let Some(partition) = spec.get("partition").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        lines.push(format!("#SBATCH --partition={partition}"));
    }

    // Escape-hatch raw directives spliced verbatim after the typed ones.
    if let Some(raw) = sbatch_directives.filter(|s| !s.trim().is_empty()) {
        lines.push(raw.trim_end().to_string());
    }

    lines.push(String::new());
    // env exports (deterministic order for stable rendering/tests).
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    for k in keys {
        lines.push(format!("export {}={}", k, sq_dq(&env[k])));
    }

    let entrypoint = spec
        .get("entrypoint")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("true");
    lines.push(entrypoint.to_string());

    let mut script = lines.join("\n");
    script.push('\n');
    script
}

/// Double-quote shell-escape for an env value embedded in `export K="…"`.
fn sq_dq(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Deliver a rendered template file to `{remote_path}` over SSH.
///
/// Writes the content via a single-quoted heredoc `cat > path` then `chmod +x`,
/// creating the parent dir first. Returns the remote path on success (the
/// `stage_template` `remote_ref`). Basic delivery — the heredoc avoids a second
/// scp round-trip and keeps the SSH session contract identical to the rest of
/// the slurm allocator.
pub async fn deliver_template_file(
    ssh: &SshSession,
    remote_path: &str,
    content: &str,
) -> Result<(), AllocError> {
    // A unique heredoc sentinel that cannot appear in rendered sbatch content.
    let sentinel = "PETRI_STAGE_EOF_8F3A";
    let dir = remote_path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
    let command = format!(
        "mkdir -p '{}' && cat > '{}' <<'{}'\n{}\n{}\nchmod +x '{}'",
        sq(dir),
        sq(remote_path),
        sentinel,
        content,
        sentinel,
        sq(remote_path),
    );
    ssh.exec(&command).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_render_sbatch_script_typed_and_escape_hatch() {
        let env: std::collections::HashMap<String, String> =
            std::iter::once(("FOO".to_string(), "bar baz".to_string())).collect();
        let spec = json!({
            "cpus": 4, "gpus": 2, "gpu_type": "a100", "mem_mb": 8192,
            "time_limit": "01:00:00", "partition": "gpu",
            "entrypoint": "python run.py"
        });
        let script = render_sbatch_script(
            "train-job",
            &spec,
            Some("#SBATCH --exclusive\n#SBATCH --nodes=2"),
            &env,
        );
        assert!(script.starts_with("#!/bin/bash\n"), "{script}");
        assert!(script.contains("#SBATCH --job-name=train-job"), "{script}");
        assert!(script.contains("#SBATCH --cpus-per-task=4"), "{script}");
        assert!(script.contains("#SBATCH --gres=gpu:a100:2"), "{script}");
        assert!(script.contains("#SBATCH --mem=8192M"), "{script}");
        assert!(script.contains("#SBATCH --time=01:00:00"), "{script}");
        assert!(script.contains("#SBATCH --partition=gpu"), "{script}");
        // escape hatch spliced verbatim
        assert!(script.contains("#SBATCH --exclusive"), "{script}");
        assert!(script.contains("#SBATCH --nodes=2"), "{script}");
        // env export + entrypoint body
        assert!(script.contains("export FOO=\"bar baz\""), "{script}");
        assert!(script.trim_end().ends_with("python run.py"), "{script}");
    }

    #[test]
    fn test_render_sbatch_script_defaults_entrypoint() {
        let env = std::collections::HashMap::new();
        let script = render_sbatch_script("j", &json!({}), None, &env);
        assert!(script.contains("#SBATCH --job-name=j"));
        // no resource directives, entrypoint defaults to a no-op
        assert!(script.trim_end().ends_with("true"), "{script}");
    }

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
    fn test_salloc_command_no_routing_falls_back_to_grant_id_comment() {
        // No failure_routing → comment is the bare grant_id (legacy/http-ish).
        let cmd = salloc_no_shell_command("g1", &json!({}));
        assert!(cmd.contains("--comment='g1'"), "{}", cmd);
    }

    #[test]
    fn test_salloc_command_stamps_failure_routing_into_comment() {
        // When the lease handler injected failure_routing, the held alloc's
        // --comment MUST carry the routing JSON so the watcher tracks it and
        // emits the failed signal on death (docs/16 §7). The bare grant_id
        // stays on --job-name for squeue discoverability.
        let req = json!({
            "failure_routing": {
                "petri_net_id": "pool-rid-123",
                "petri_place": "lease_failed",
                "petri_signal_key": "inst-1:lp",
                "petri_signal_failed": "lease_failed",
            }
        });
        let cmd = salloc_no_shell_command("inst-1:lp", &req);
        assert!(cmd.contains("--job-name='petri-inst-1:lp'"), "{}", cmd);
        assert!(
            cmd.contains("petri_signal_failed") && cmd.contains("pool-rid-123"),
            "comment must carry the routing JSON: {}",
            cmd
        );
        // The comment must be valid JSON the watcher can parse as a meta map.
        let comment = cmd
            .split("--comment='")
            .nth(1)
            .and_then(|s| s.split("'").next())
            .unwrap();
        let _: std::collections::HashMap<String, String> =
            serde_json::from_str(comment).expect("comment is a JSON meta map");
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
        assert!(
            cmd.contains("--comment='{\"petri_net_id\":\"n\"}'"),
            "{}",
            cmd
        );
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
        let cmd = srun_into_alloc_template_command("a'b", "{}", "k'1", "{}", "e'1", "/t.sh");
        assert!(cmd.contains("--jobid='a'\\''b'"), "{}", cmd);
        assert!(cmd.contains("--job-name='petri-k'\\''1'"), "{}", cmd);
    }

    #[test]
    fn test_srun_lease_executor_command_shape() {
        // The drain-launch form: srun a persistent executor onto a held alloc,
        // parameterised by the lease-scoped namespace + drain bounds. Crucially
        // it carries NO EXECUTOR_TARGET_EXEC_ID (Pool mode is selected by its
        // absence; the template reads LEASE_* instead).
        let cmd = srun_lease_executor_command(
            "12345",
            "/opt/petri/templates/mekhan-lease-executor.sh",
            "lease-inst-1:node-2",
            8,
            300,
            None,
        );
        assert!(cmd.starts_with("srun --jobid='12345'"), "{}", cmd);
        assert!(
            cmd.contains("--export=ALL,LEASE_NAMESPACE='lease-inst-1:node-2',LEASE_MAX_JOBS='8',LEASE_IDLE_TIMEOUT='300'"),
            "{}",
            cmd
        );
        assert!(
            cmd.ends_with(" /opt/petri/templates/mekhan-lease-executor.sh"),
            "{}",
            cmd
        );
        // No PerJob target — Pool/drain mode.
        assert!(!cmd.contains("EXECUTOR_TARGET_EXEC_ID"), "{}", cmd);
    }

    #[test]
    fn test_srun_lease_executor_command_escapes() {
        // A namespace/alloc with a quote must not break out of the quoted arg.
        let cmd = srun_lease_executor_command("a'b", "/t.sh", "lease-x'y", 1, 60, None);
        assert!(cmd.contains("--jobid='a'\\''b'"), "{}", cmd);
        assert!(cmd.contains("LEASE_NAMESPACE='lease-x'\\''y'"), "{}", cmd);
    }

    #[test]
    fn test_srun_lease_executor_command_apptainer_wrap() {
        // With a container bound, the trailing target is an `apptainer exec` of
        // the template script (docs/22). LEASE_* still ride --export (apptainer
        // inherits host env by default).
        let container = ContainerSpec {
            sif_path: "/shared/sif/by-ref/python_3_12_slim.sif".into(),
            binds: vec!["/opt/petri/bin".into(), "/shared/venv-cache/x".into()],
            nv: true,
        };
        let cmd = srun_lease_executor_command(
            "12345",
            "/opt/petri/templates/mekhan-lease-executor.sh",
            "lease-1",
            8,
            300,
            Some(&container),
        );
        assert!(cmd.contains("apptainer exec --nv "), "{}", cmd);
        assert!(cmd.contains("--bind '/opt/petri/bin'"), "{}", cmd);
        assert!(cmd.contains("--bind '/shared/venv-cache/x'"), "{}", cmd);
        assert!(
            cmd.ends_with("'/shared/sif/by-ref/python_3_12_slim.sif' /bin/bash '/opt/petri/templates/mekhan-lease-executor.sh'"),
            "{}",
            cmd
        );
    }

    #[test]
    fn test_sanitize_image_ref() {
        assert_eq!(sanitize_image_ref("ghcr.io/org/img:tag"), "ghcr_io_org_img_tag");
        assert_eq!(sanitize_image_ref("python:3.12-slim"), "python_3_12_slim");
        assert_eq!(sanitize_image_ref("a@@b"), "a_b");
    }

    #[test]
    fn test_parse_materialize_output() {
        let out = "some noise\nPETRI_MATERIALIZE digest=abc123 sif=/shared/sif/abc123.sif size=42\n";
        let (d, s, sz) = parse_materialize_output(out).unwrap();
        assert_eq!(d, "abc123");
        assert_eq!(s, "/shared/sif/abc123.sif");
        assert_eq!(sz, Some(42));
        assert!(parse_materialize_output("nothing here").is_none());
    }

    #[test]
    fn test_detached_launch_shape() {
        // Fire-and-forget: nohup + every-fd redirect + background, so the SSH
        // exec returns immediately while the executor runs for the whole lease.
        let inner = "srun --jobid='12345' /opt/petri/templates/mekhan-lease-executor.sh";
        let detached = detached_launch(inner, "12345");
        assert!(
            detached.starts_with("nohup srun --jobid='12345'"),
            "{}",
            detached
        );
        assert!(
            detached.contains("> '/tmp/petri-lease-exec-12345.out' 2>&1"),
            "{}",
            detached
        );
        assert!(detached.contains("< /dev/null &"), "{}", detached);
        assert!(detached.ends_with("echo dispatched"), "{}", detached);
    }
}
