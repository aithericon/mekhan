//! Slurm-backed lease allocator (the `scheduler_flavor = "slurm"` leg) and the
//! per-fire flavor dispatcher that routes between it and the generic HTTP
//! allocator.
//!
//! This module is the JOIN POINT in the dependency graph: it needs both
//! petri-slurm's [`SshSession`]/[`SlurmConfig`]/`alloc` primitives AND
//! petri-application's [`AllocatorClient`] trait. petri-slurm does not depend
//! on petri-application (so it cannot impl the trait) and petri-application does
//! not depend on petri-slurm (so the trait cannot live next to the Slurm code).
//! petri-api depends on both, so the adapter lives here, behind the existing
//! `slurm` cargo feature.
//!
//! ## What the lease maps to
//!
//! Unlike the batch (`sbatch`) path, a lease HOLDS an allocation without running
//! anything: `salloc --no-shell` grants the nodes and returns immediately,
//! keeping them reserved until `scancel`. So:
//!   - **acquire** → `salloc --no-shell` (or reuse an existing allocation for the
//!     same `grant_id` — idempotency), `scontrol show job` to resolve the node,
//!     then `srun --jobid` a persistent drain executor onto the held alloc
//!     (Pool mode, lease-scoped namespace `lease-<grant_id>`), returning the
//!     lease JSON `{ alloc_id, node?, expiry?, executor_namespace,
//!     scheduler: { flavor: "slurm", partition? } }`.
//!   - **release** → `scancel <alloc_id>` (tolerant of an already-gone job) —
//!     SIGTERM drains the executor in-flight + frees the nodes.
//!
//! `node`/`expiry` are omitted until the allocation is placed/timed.
//!
//! ## Idempotency (mirrors the HTTP `Idempotency-Key` contract)
//!
//! The handler passes the replay-safe `grant_id` (`instance_id:node_id`). Before
//! allocating, `acquire` probes `squeue --name=petri-<grant_id>` for a still-live
//! allocation and reuses it — so a re-fire (e.g. a crash between the salloc and
//! the journal append) returns the same allocation rather than holding a second
//! set of nodes.
//!
//! ## Connection
//!
//! Slurm uses its own SSH connection params from a held [`SlurmConfig`] (built
//! from the `SLURM_*` env at registration). The trait's `allocator_url`/`token`
//! args are HTTP-centric and IGNORED here. The SSH session is held lazily and
//! reconnected-once on a dropped connection, mirroring `SlurmClient`.

use std::sync::Arc;

use serde_json::Value as JsonValue;

use petri_application::resource_lease_handlers::{
    AllocatorClient, AllocatorError, MaterializeImageArgs, MaterializeOutcome, StageOutcome,
    StageTemplateArgs,
};

#[cfg(feature = "slurm")]
use petri_slurm::alloc;
#[cfg(feature = "slurm")]
use petri_slurm::ssh::{SshError, SshSession};
#[cfg(feature = "slurm")]
use petri_slurm::SlurmConfig;
#[cfg(feature = "slurm")]
use serde_json::json;

/// Slurm-internal allocation error, mapped into [`AllocatorError`] at the trait
/// boundary.
#[cfg(feature = "slurm")]
#[derive(Debug, thiserror::Error)]
pub enum SlurmAllocError {
    #[error("ssh: {0}")]
    Ssh(#[from] SshError),
    #[error("slurm alloc: {0}")]
    Alloc(#[from] alloc::AllocError),
    #[error("salloc returned no alloc id: {0}")]
    NoAllocId(String),
    #[error("unparseable command output: {0}")]
    BadOutput(String),
}

#[cfg(feature = "slurm")]
impl From<SlurmAllocError> for AllocatorError {
    fn from(e: SlurmAllocError) -> Self {
        match e {
            // A broken SSH transport is the closest analogue to an HTTP
            // transport error.
            SlurmAllocError::Ssh(_) => AllocatorError::Transport(e.to_string()),
            SlurmAllocError::Alloc(alloc::AllocError::Ssh(_)) => {
                AllocatorError::Transport(e.to_string())
            }
            // Everything else is a malformed/empty Slurm response.
            _ => AllocatorError::BadResponse(e.to_string()),
        }
    }
}

/// Slurm-backed [`AllocatorClient`]: holds a lease via `salloc --no-shell`,
/// resolves the node via `scontrol`, releases via `scancel`. SSH connection
/// lifecycle mirrors `SlurmClient` (lazy connect, reconnect-once).
#[cfg(feature = "slurm")]
pub struct SlurmAllocatorClient {
    config: SlurmConfig,
    /// SSH session with lazy init + reconnect-on-failure.
    ssh: tokio::sync::Mutex<Option<SshSession>>,
}

#[cfg(feature = "slurm")]
impl SlurmAllocatorClient {
    /// Build a client around an explicit [`SlurmConfig`].
    pub fn new(config: SlurmConfig) -> Self {
        Self {
            config,
            ssh: tokio::sync::Mutex::new(None),
        }
    }

    /// Build a client from a resolved connection (the datacenter resource's
    /// `effect_config`), NOT env. The [`SlurmConfig`] is already built by the
    /// `ClusterRegistry` from the parsed connection (it owns the temp-file PEM
    /// path). Alias of [`SlurmAllocatorClient::new`] for symmetry with the
    /// nomad leg and to mark the multi-cluster (resource-driven) build path.
    pub fn from_connection(config: SlurmConfig) -> Self {
        Self::new(config)
    }

    /// Build a client from the `SLURM_*` environment, if `SLURM_SSH_HOST` is set
    /// (gating mirrors [`SlurmConfig::from_env`]). Returns `None` when Slurm is
    /// not configured, so the dispatcher's slurm leg is simply absent.
    pub fn from_env() -> Option<Self> {
        SlurmConfig::from_env().map(Self::new)
    }

    /// Execute an SSH command with lazy connect + reconnect-once, mirroring
    /// `SlurmClient::exec_with_reconnect`.
    async fn exec(&self, command: &str) -> Result<String, SshError> {
        let mut guard = self.ssh.lock().await;

        if guard.is_none() {
            *guard = Some(SshSession::connect(&self.config).await?);
        }

        match guard.as_ref().unwrap().exec(command).await {
            Ok(output) => Ok(output),
            Err(SshError::Connection(_)) => {
                tracing::warn!("Slurm allocator SSH connection lost, reconnecting for retry");
                *guard = Some(SshSession::connect(&self.config).await?);
                guard.as_ref().unwrap().exec(command).await
            }
            Err(e) => Err(e),
        }
    }

    /// As [`Self::exec`] but with an explicit per-call timeout. The session's
    /// default `command_timeout_secs` (~60s) is tuned for quick allocator
    /// commands (`salloc`/`squeue`/`scancel`) and is too short for an
    /// `apptainer pull` that downloads + converts a multi-hundred-MB OCI image
    /// to a `.sif` — see `materialize_image`.
    async fn exec_with_timeout(
        &self,
        command: &str,
        timeout: std::time::Duration,
    ) -> Result<String, SshError> {
        let mut guard = self.ssh.lock().await;

        if guard.is_none() {
            *guard = Some(SshSession::connect(&self.config).await?);
        }

        match guard.as_ref().unwrap().exec_with_timeout(command, timeout).await {
            Ok(output) => Ok(output),
            Err(SshError::Connection(_)) => {
                tracing::warn!("Slurm allocator SSH connection lost, reconnecting for retry");
                *guard = Some(SshSession::connect(&self.config).await?);
                guard
                    .as_ref()
                    .unwrap()
                    .exec_with_timeout(command, timeout)
                    .await
            }
            Err(e) => Err(e),
        }
    }

    /// Acquire (or reuse) a held allocation, resolve its node, launch ONE
    /// persistent drain executor on it (Pool mode, lease-scoped namespace), and
    /// return the lease JSON (`alloc_id`/`node?`/`expiry?`/`executor_namespace`/`scheduler`)
    /// the acquire handler consumes.
    async fn acquire_lease(
        &self,
        grant_id: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, SlurmAllocError> {
        let mut guard = self.ssh.lock().await;
        if guard.is_none() {
            *guard = Some(SshSession::connect(&self.config).await?);
        }
        // The `alloc::*` primitives take `&SshSession`; run them through the
        // held session. A dropped connection surfaces as `AllocError::Ssh`
        // (mapped to Transport) — the next fire re-establishes lazily.
        let session = guard.as_ref().unwrap();

        // Idempotency: reuse a still-live allocation for this grant if one
        // exists (mirrors the HTTP Idempotency-Key contract).
        let alloc_id = match alloc::squeue_find_by_name(session, grant_id).await? {
            Some(existing) => {
                tracing::info!(
                    grant_id = %grant_id,
                    alloc_id = %existing,
                    "slurm lease: reusing existing allocation",
                );
                existing
            }
            None => {
                let id = alloc::salloc_no_shell(session, grant_id, request).await?;
                tracing::info!(
                    grant_id = %grant_id,
                    alloc_id = %id,
                    "slurm lease: allocated",
                );
                id
            }
        };

        if alloc_id.trim().is_empty() {
            return Err(SlurmAllocError::NoAllocId(grant_id.to_string()));
        }

        // Resolve the node (and expiry if known). May still be pending — the
        // handler tolerates a null node.
        let allocation = alloc::scontrol_node(session, &alloc_id).await?;

        // The lease-scoped NATS namespace the persistent drain executor consumes
        // and the leased loop body enqueues to. `grant_id` is `instance_id:node_id`;
        // the `:` (and any `/`) must be sanitised because it would otherwise break
        // the NATS stream name (`lease-…_medium`) / subject token (`lease-….medium.>`).
        let executor_namespace = format!("lease-{}", grant_id.replace([':', '/'], "-"));

        // Launch ONE persistent drain executor on the held allocation, DETACHED.
        // It runs in Pool/drain mode (no EXECUTOR_TARGET_EXEC_ID) consuming the
        // lease-scoped namespace, and pulls EVERY job the leased loop enqueues
        // there — keeping warm state (venv/model/GPU) across iterations. A SYNC
        // srun would block `acquire` for the whole lease, so we fire-and-forget.
        // `scancel` on release sends SIGTERM → graceful 30s drain → exit;
        // `LEASE_IDLE_TIMEOUT` is the belt-and-suspenders self-exit on a wedge.
        //
        // `max_jobs`/`idle_secs` come from the claim request when present; `acquire`
        // only sees the claim, not the loop's maxIterations, so default generously
        // (a high cap + a long idle window). The natural cap is the loop's
        // maxIterations, which release (scancel) enforces anyway.
        let max_jobs = request
            .get("max_jobs")
            .and_then(|v| v.as_u64())
            .unwrap_or(100_000);
        let idle_secs = request
            .get("idle_timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(300);
        let template = format!(
            "{}/mekhan-lease-executor.sh",
            self.config.template_dir.trim_end_matches('/'),
        );
        // Container binding (docs/22): mekhan threads a `container` blob in the
        // claim request when the step's job template binds a `container_image`
        // resource. Absent / unparseable → native execution (no wrap). A
        // malformed blob is logged and dropped rather than failing the lease.
        let container: Option<alloc::ContainerSpec> = request.get("container").and_then(|v| {
            serde_json::from_value::<alloc::ContainerSpec>(v.clone())
                .map_err(|e| tracing::warn!(error = %e, "slurm lease: ignoring malformed container spec"))
                .ok()
        });
        alloc::srun_lease_executor(
            session,
            &alloc_id,
            &template,
            &executor_namespace,
            max_jobs,
            idle_secs,
            container.as_ref(),
        )
        .await?;
        tracing::info!(
            grant_id = %grant_id,
            alloc_id = %alloc_id,
            executor_namespace = %executor_namespace,
            "slurm lease: launched persistent drain executor",
        );

        // `DatacenterLease`: `alloc_id` is the only required field. `node`/
        // `expiry` are optional and OMITTED when the allocator hasn't reported
        // them — `salloc --no-shell` with no time limit has no EndTime, and a
        // still-pending alloc has no NodeList. (The old required-String schema
        // forced an empty-string-not-null workaround; with the optional shape we
        // just leave the key out.) The `scheduler` detail is the typed `slurm`
        // variant; `partition` is left None until we parse it from the alloc.
        let mut lease = serde_json::Map::new();
        lease.insert("alloc_id".into(), json!(alloc_id));
        if let Some(node) = allocation.node.filter(|s| !s.is_empty()) {
            lease.insert("node".into(), json!(node));
        }
        if let Some(expiry) = allocation.expiry.filter(|s| !s.is_empty()) {
            lease.insert("expiry".into(), json!(expiry));
        }
        lease.insert("executor_namespace".into(), json!(executor_namespace));
        lease.insert("scheduler".into(), json!({ "flavor": "slurm" }));

        Ok(JsonValue::Object(lease))
    }

    /// Render the typed stage spec → an sbatch script and DELIVER it over SSH to
    /// `{template_dir}/{slug}.sh`. Returns `remote_ref = the remote path`.
    ///
    /// Basic delivery (Phase 4): a single-quoted heredoc `cat > path && chmod +x`
    /// via the held SSH session. `escape_hatch.sbatch_directives` is spliced
    /// verbatim after the typed `#SBATCH` directives. NOTE: Slurm is implemented
    /// compile-correct + best-effort and is NOT live-tested in this phase.
    async fn stage_template(
        &self,
        args: &StageTemplateArgs,
    ) -> Result<JsonValue, SlurmAllocError> {
        let mut guard = self.ssh.lock().await;
        if guard.is_none() {
            *guard = Some(SshSession::connect(&self.config).await?);
        }
        let session = guard.as_ref().unwrap();

        // Reconstruct the spec JSON the renderer reads (typed → json). Keeping the
        // renderer JSON-driven lets it live in petri-slurm without a petri-application
        // dep (the StageSpec type lives in petri-application).
        let spec = json!({
            "cpus": args.spec.cpus,
            "gpus": args.spec.gpus,
            "gpu_type": args.spec.gpu_type,
            "mem_mb": args.spec.mem_mb,
            "time_limit": args.spec.time_limit,
            "partition": args.spec.partition,
            "entrypoint": args.spec.entrypoint,
        });
        // mekhan models `sbatch_directives` as one element per directive line;
        // the renderer takes a single verbatim block, so join with newlines
        // (None when empty → renderer omits the block).
        let directives = (!args.escape_hatch.sbatch_directives.is_empty())
            .then(|| args.escape_hatch.sbatch_directives.join("\n"));
        let script = alloc::render_sbatch_script(
            &args.slug,
            &spec,
            directives.as_deref(),
            &args.spec.env,
        );

        let remote_path = format!(
            "{}/{}.sh",
            self.config.template_dir.trim_end_matches('/'),
            args.slug,
        );
        alloc::deliver_template_file(session, &remote_path, &script).await?;

        tracing::info!(
            slug = %args.slug,
            remote_path = %remote_path,
            "slurm stage_template: delivered sbatch script",
        );
        Ok(json!({ "remote_ref": remote_path }))
    }

    /// Pull an OCI image to a content-addressed Apptainer `.sif` on the login node
    /// and repoint the stable by-ref symlink (docs/22). Returns the digest +
    /// `.sif` path. Uses the v1 shared-FS convention `/shared/sif` (+ a shared
    /// `APPTAINER_CACHEDIR`) — the compiler embeds the matching
    /// `/shared/sif/by-ref/<stem>.sif` path. Both are hard-coded conventions in
    /// v1; a per-datacenter override is a later refinement.
    async fn materialize_image(
        &self,
        args: &MaterializeImageArgs,
    ) -> Result<MaterializeOutcome, SlurmAllocError> {
        let script = alloc::render_apptainer_pull_script(
            &args.image_ref,
            args.registry_username.as_deref(),
            args.registry_password.as_deref(),
            SHARED_SIF_ROOT,
            SHARED_APPTAINER_CACHE,
        );
        // Image pulls are slow (download + squashfs conversion); use a generous
        // timeout, NOT the ~60s default tuned for quick allocator commands — a
        // first pull of even a small image (e.g. python:3.12-slim, ~70s)
        // otherwise trips the default and the SSH session is torn down.
        let stdout = self
            .exec_with_timeout(&script, std::time::Duration::from_secs(MATERIALIZE_PULL_TIMEOUT_SECS))
            .await?;
        let (digest, sif_path, size_bytes) = alloc::parse_materialize_output(&stdout)
            .ok_or_else(|| SlurmAllocError::BadOutput(format!(
                "apptainer pull produced no PETRI_MATERIALIZE line: {stdout}"
            )))?;
        tracing::info!(
            image_ref = %args.image_ref,
            digest = %digest,
            sif_path = %sif_path,
            "slurm materialize_image: pulled image to .sif",
        );
        Ok(MaterializeOutcome {
            digest,
            sif_path,
            size_bytes,
        })
    }
}

/// v1 shared-FS conventions for container materialization (docs/22). The
/// compiler embeds the matching by-ref path under [`SHARED_SIF_ROOT`]; keep the
/// two in sync. A per-datacenter override is a later refinement.
pub const SHARED_SIF_ROOT: &str = "/shared/sif";
pub const SHARED_APPTAINER_CACHE: &str = "/shared/apptainer-cache";

/// SSH timeout for an `apptainer pull` (download + squashfs conversion). Far
/// longer than the default `command_timeout_secs` (~60s) because a real image
/// pull legitimately takes minutes; a large image on a cold cache can be slow.
pub const MATERIALIZE_PULL_TIMEOUT_SECS: u64 = 1800;

#[cfg(feature = "slurm")]
#[async_trait::async_trait]
impl AllocatorClient for SlurmAllocatorClient {
    async fn acquire(
        &self,
        _allocator_url: &str,
        _token: &str,
        grant_id: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, AllocatorError> {
        // Slurm uses its own held SlurmConfig — url/token are HTTP-centric and
        // ignored.
        Ok(self.acquire_lease(grant_id, request).await?)
    }

    async fn release(
        &self,
        _allocator_url: &str,
        _token: &str,
        alloc_id: &str,
    ) -> Result<(), AllocatorError> {
        // `scancel` is tolerant of an already-gone job (exit 0 / harmless
        // stderr), matching the idempotent-release contract.
        let command = format!("scancel '{}'", alloc_id.replace('\'', "'\\''"));
        self.exec(&command)
            .await
            .map_err(|e| AllocatorError::from(SlurmAllocError::Ssh(e)))?;
        tracing::info!(alloc_id = %alloc_id, "slurm lease: released");
        Ok(())
    }

    async fn stage_template_with_connection(
        &self,
        _config: &JsonValue,
        args: &StageTemplateArgs,
    ) -> Result<StageOutcome, AllocatorError> {
        // Slurm uses its own held SlurmConfig (the registry built THIS client from
        // the connection in `config`); the config arg is ignored here, symmetric
        // with acquire/release.
        let out = self.stage_template(args).await?;
        let remote_ref = out
            .get("remote_ref")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AllocatorError::BadResponse("slurm stage returned no remote_ref".into()))?
            .to_string();
        Ok(StageOutcome { remote_ref })
    }

    async fn materialize_image_with_connection(
        &self,
        _config: &JsonValue,
        args: &MaterializeImageArgs,
    ) -> Result<MaterializeOutcome, AllocatorError> {
        // Slurm uses its own held SlurmConfig (the registry built THIS client
        // from the connection in `config`); the config arg is ignored, symmetric
        // with stage_template_with_connection.
        Ok(self.materialize_image(args).await?)
    }
}

/// Per-fire flavor dispatcher: a single registered [`AllocatorClient`] that
/// routes each acquire/release on the `scheduler_flavor` the handler extracted
/// from the resolved `effect_config` (`"http"`/`""` default → the generic HTTP
/// allocator; `"slurm"` → the SSH/salloc-backed [`SlurmAllocatorClient`];
/// `"nomad"` → the dispatch-backed [`NomadAllocatorClient`]).
///
/// It overrides ONLY the flavor-aware seam methods
/// (`acquire_with_flavor`/`release_with_flavor`); the bare `acquire`/`release`
/// fall through to the http leg so a direct (flavorless) caller still works.
///
/// An UNKNOWN flavor is a HARD error (not a silent fall-through to http): an
/// unconfigured/misspelled flavor must fail loudly rather than misroute a lease
/// to the generic HTTP allocator (which would POST to whatever
/// `allocator_url` the datacenter secret carried — wrong backend, silent).
pub struct FlavorDispatchAllocatorClient {
    http: Arc<dyn AllocatorClient>,
    /// Present only when the `slurm` feature is on AND `SLURM_*` env is set.
    slurm: Option<Arc<dyn AllocatorClient>>,
    /// Present only when the `nomad` feature is on AND `NOMAD_ADDR` is set.
    nomad: Option<Arc<dyn AllocatorClient>>,
}

impl FlavorDispatchAllocatorClient {
    pub fn new(
        http: Arc<dyn AllocatorClient>,
        slurm: Option<Arc<dyn AllocatorClient>>,
        nomad: Option<Arc<dyn AllocatorClient>>,
    ) -> Self {
        Self { http, slurm, nomad }
    }

    /// Resolve the leg for a flavor. `"slurm"`/`"nomad"` require the respective
    /// leg to be present (feature on + env set); an unknown flavor is a hard
    /// error so a misconfiguration fails loudly instead of misrouting to http.
    fn leg(&self, scheduler_flavor: &str) -> Result<&Arc<dyn AllocatorClient>, AllocatorError> {
        match scheduler_flavor {
            // "http" / "" (flavorless) → the generic HTTP allocator.
            "http" | "" => Ok(&self.http),
            "slurm" => self.slurm.as_ref().ok_or_else(|| {
                AllocatorError::BadResponse(
                    "scheduler_flavor=slurm but no Slurm allocator is configured (set SLURM_SSH_HOST and build with the `slurm` feature)"
                        .into(),
                )
            }),
            "nomad" => self.nomad.as_ref().ok_or_else(|| {
                AllocatorError::BadResponse(
                    "scheduler_flavor=nomad but no Nomad allocator is configured (set NOMAD_ADDR and build with the `nomad` feature)"
                        .into(),
                )
            }),
            other => Err(AllocatorError::BadResponse(format!(
                "unknown scheduler_flavor {other:?} — expected http|slurm|nomad"
            ))),
        }
    }
}

#[async_trait::async_trait]
impl AllocatorClient for FlavorDispatchAllocatorClient {
    // The bare methods are required by the trait. A flavorless caller defaults
    // to the http leg (the historical behaviour).
    async fn acquire(
        &self,
        allocator_url: &str,
        token: &str,
        grant_id: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, AllocatorError> {
        self.http
            .acquire(allocator_url, token, grant_id, request)
            .await
    }

    async fn release(
        &self,
        allocator_url: &str,
        token: &str,
        alloc_id: &str,
    ) -> Result<(), AllocatorError> {
        self.http.release(allocator_url, token, alloc_id).await
    }

    async fn acquire_with_flavor(
        &self,
        scheduler_flavor: &str,
        allocator_url: &str,
        token: &str,
        grant_id: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, AllocatorError> {
        self.leg(scheduler_flavor)?
            .acquire(allocator_url, token, grant_id, request)
            .await
    }

    async fn release_with_flavor(
        &self,
        scheduler_flavor: &str,
        allocator_url: &str,
        token: &str,
        alloc_id: &str,
    ) -> Result<(), AllocatorError> {
        self.leg(scheduler_flavor)?
            .release(allocator_url, token, alloc_id)
            .await
    }

    async fn stage_template_with_connection(
        &self,
        config: &JsonValue,
        args: &StageTemplateArgs,
    ) -> Result<StageOutcome, AllocatorError> {
        // The env-fallback path (no ClusterRegistry installed): route staging on
        // the `scheduler_flavor` the handler read off the effect_config, exactly
        // like acquire/release. The resolved leg (slurm/nomad) holds its own
        // connection, so we pass `config` through (its leaf ignores it).
        let flavor = config
            .get("scheduler_flavor")
            .and_then(|v| v.as_str())
            .unwrap_or("http");
        self.leg(flavor)?
            .stage_template_with_connection(config, args)
            .await
    }

    async fn materialize_image_with_connection(
        &self,
        config: &JsonValue,
        args: &MaterializeImageArgs,
    ) -> Result<MaterializeOutcome, AllocatorError> {
        // Same env-fallback routing as stage: dispatch on `scheduler_flavor`.
        // Only the slurm leg implements the Apptainer pull; other legs return
        // the unsupported error from the trait default.
        let flavor = config
            .get("scheduler_flavor")
            .and_then(|v| v.as_str())
            .unwrap_or("http");
        self.leg(flavor)?
            .materialize_image_with_connection(config, args)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Records which leg it is and counts calls, so routing can be asserted
    /// without a live cluster.
    struct FakeLeg {
        label: &'static str,
        acquires: AtomicUsize,
        releases: AtomicUsize,
    }

    impl FakeLeg {
        fn new(label: &'static str) -> Arc<Self> {
            Arc::new(Self {
                label,
                acquires: AtomicUsize::new(0),
                releases: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait::async_trait]
    impl AllocatorClient for FakeLeg {
        async fn acquire(
            &self,
            _allocator_url: &str,
            _token: &str,
            _grant_id: &str,
            _request: &JsonValue,
        ) -> Result<JsonValue, AllocatorError> {
            self.acquires.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "leg": self.label, "alloc_id": "x" }))
        }

        async fn release(
            &self,
            _allocator_url: &str,
            _token: &str,
            _alloc_id: &str,
        ) -> Result<(), AllocatorError> {
            self.releases.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn dispatch_routes_http_by_default_and_on_http_flavor() {
        let http = FakeLeg::new("http");
        let slurm = FakeLeg::new("slurm");
        let nomad = FakeLeg::new("nomad");
        let dispatch = FlavorDispatchAllocatorClient::new(
            http.clone(),
            Some(slurm.clone()),
            Some(nomad.clone()),
        );

        // explicit "http"
        let out = dispatch
            .acquire_with_flavor("http", "url", "tok", "g1", &json!({}))
            .await
            .unwrap();
        assert_eq!(out.get("leg").unwrap(), "http");
        // empty flavor (flavorless via the seam) also goes to http
        dispatch
            .acquire_with_flavor("", "url", "tok", "g1", &json!({}))
            .await
            .unwrap();
        // bare acquire (flavorless) also goes to http
        dispatch
            .acquire("url", "tok", "g1", &json!({}))
            .await
            .unwrap();

        assert_eq!(http.acquires.load(Ordering::SeqCst), 3);
        assert_eq!(slurm.acquires.load(Ordering::SeqCst), 0);
        assert_eq!(nomad.acquires.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dispatch_routes_slurm_flavor_to_slurm_leg() {
        let http = FakeLeg::new("http");
        let slurm = FakeLeg::new("slurm");
        let dispatch = FlavorDispatchAllocatorClient::new(http.clone(), Some(slurm.clone()), None);

        let out = dispatch
            .acquire_with_flavor("slurm", "url", "tok", "g1", &json!({}))
            .await
            .unwrap();
        assert_eq!(out.get("leg").unwrap(), "slurm");

        dispatch
            .release_with_flavor("slurm", "url", "tok", "alloc-9")
            .await
            .unwrap();

        assert_eq!(slurm.acquires.load(Ordering::SeqCst), 1);
        assert_eq!(slurm.releases.load(Ordering::SeqCst), 1);
        assert_eq!(http.acquires.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dispatch_routes_nomad_flavor_to_nomad_leg() {
        let http = FakeLeg::new("http");
        let nomad = FakeLeg::new("nomad");
        let dispatch = FlavorDispatchAllocatorClient::new(http.clone(), None, Some(nomad.clone()));

        let out = dispatch
            .acquire_with_flavor("nomad", "url", "tok", "g1", &json!({}))
            .await
            .unwrap();
        assert_eq!(out.get("leg").unwrap(), "nomad");

        dispatch
            .release_with_flavor("nomad", "url", "tok", "alloc-9")
            .await
            .unwrap();

        assert_eq!(nomad.acquires.load(Ordering::SeqCst), 1);
        assert_eq!(nomad.releases.load(Ordering::SeqCst), 1);
        assert_eq!(http.acquires.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dispatch_slurm_flavor_without_leg_is_an_error() {
        let http = FakeLeg::new("http");
        let dispatch = FlavorDispatchAllocatorClient::new(http.clone(), None, None);

        let err = dispatch
            .acquire_with_flavor("slurm", "url", "tok", "g1", &json!({}))
            .await
            .unwrap_err();
        match err {
            AllocatorError::BadResponse(msg) => assert!(msg.contains("slurm")),
            other => panic!("expected BadResponse, got {other:?}"),
        }
        assert_eq!(http.acquires.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dispatch_nomad_flavor_without_leg_is_an_error() {
        let http = FakeLeg::new("http");
        let dispatch = FlavorDispatchAllocatorClient::new(http.clone(), None, None);

        let err = dispatch
            .acquire_with_flavor("nomad", "url", "tok", "g1", &json!({}))
            .await
            .unwrap_err();
        match err {
            AllocatorError::BadResponse(msg) => assert!(msg.contains("nomad")),
            other => panic!("expected BadResponse, got {other:?}"),
        }
        assert_eq!(http.acquires.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dispatch_unknown_flavor_is_a_hard_error_not_http() {
        // The load-bearing fix: an unconfigured/misspelled flavor must fail
        // loudly, NOT silently fall through to the generic HTTP allocator.
        let http = FakeLeg::new("http");
        let slurm = FakeLeg::new("slurm");
        let nomad = FakeLeg::new("nomad");
        let dispatch = FlavorDispatchAllocatorClient::new(
            http.clone(),
            Some(slurm.clone()),
            Some(nomad.clone()),
        );

        let err = dispatch
            .acquire_with_flavor("k8s", "url", "tok", "g1", &json!({}))
            .await
            .unwrap_err();
        match err {
            AllocatorError::BadResponse(msg) => {
                assert!(msg.contains("k8s"));
                assert!(msg.contains("http|slurm|nomad"));
            }
            other => panic!("expected BadResponse, got {other:?}"),
        }
        // NOT routed to http (the old silent fall-through), nor any backend.
        assert_eq!(http.acquires.load(Ordering::SeqCst), 0);
        assert_eq!(slurm.acquires.load(Ordering::SeqCst), 0);
        assert_eq!(nomad.acquires.load(Ordering::SeqCst), 0);
    }
}
