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
//!     lease JSON `{ node, gpu_uuid, alloc_id, expiry, executor_namespace }`.
//!   - **release** → `scancel <alloc_id>` (tolerant of an already-gone job) —
//!     SIGTERM drains the executor in-flight + frees the nodes.
//!
//! `gpu_uuid` is `""` on the CPU-only dev cluster (no GPU to bind).
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

use petri_application::resource_lease_handlers::{AllocatorClient, AllocatorError};

#[cfg(feature = "slurm")]
use serde_json::json;
#[cfg(feature = "slurm")]
use petri_slurm::alloc;
#[cfg(feature = "slurm")]
use petri_slurm::ssh::{SshError, SshSession};
#[cfg(feature = "slurm")]
use petri_slurm::SlurmConfig;

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

    /// Acquire (or reuse) a held allocation, resolve its node, launch ONE
    /// persistent drain executor on it (Pool mode, lease-scoped namespace), and
    /// return the lease JSON (`node`/`gpu_uuid`/`alloc_id`/`expiry`/`executor_namespace`)
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
        alloc::srun_lease_executor(
            session,
            &alloc_id,
            &template,
            &executor_namespace,
            max_jobs,
            idle_secs,
        )
        .await?;
        tracing::info!(
            grant_id = %grant_id,
            alloc_id = %alloc_id,
            executor_namespace = %executor_namespace,
            "slurm lease: launched persistent drain executor",
        );

        // `Lease__datacenter` (DatacenterLease) types every field as a required
        // `String`, and the engine validates the grant token against it on
        // injection into the instance's grant-inbox. So absent node/expiry MUST
        // be the empty string, NOT null — `salloc --no-shell` with no time limit
        // has no EndTime, and a still-pending alloc has no NodeList; emitting
        // null there fails schema validation ("null is not of type string") and
        // the grant is silently dropped, wedging the claim.
        let node = JsonValue::String(allocation.node.unwrap_or_default());
        let expiry = JsonValue::String(allocation.expiry.unwrap_or_default());

        Ok(json!({
            "node": node,
            // CPU-only dev cluster: no GPU UUID to bind. Empty string (not null)
            // so the body-visible lease token carries a concrete value.
            "gpu_uuid": "",
            "alloc_id": alloc_id,
            "expiry": expiry,
            "executor_namespace": executor_namespace,
        }))
    }
}

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
        dispatch.acquire("url", "tok", "g1", &json!({})).await.unwrap();

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
