//! Nomad-backed lease allocator (the `scheduler_flavor = "nomad"` leg).
//!
//! This is the second concrete [`AllocatorClient`] alongside the Slurm leg
//! (`slurm_allocator.rs`). Like that module it is a JOIN POINT in the dependency
//! graph: it needs both petri-nomad's `NomadClient`/`NomadConfig` HTTP primitives
//! AND petri-application's [`AllocatorClient`] trait. petri-nomad does not depend
//! on petri-application (so it cannot impl the trait) and petri-application does
//! not depend on petri-nomad (so the trait cannot live next to the Nomad code).
//! petri-api depends on both, so the adapter lives here, behind the existing
//! `nomad` cargo feature.
//!
//! ## What the lease maps to (the unified drain model)
//!
//! Symmetric with Slurm: a lease holds an instance that runs ONE persistent
//! drain executor (Pool mode, lease-scoped namespace `lease-<grant_id>`). The
//! leased loop body just ENQUEUES each job to that namespace; the warm executor
//! pulls + runs them all, keeping venv/model/GPU state across iterations.
//!
//!   - **acquire** → dispatch a long-running drain-executor *parameterized* job.
//!     The dispatched (child) job IS the persistent executor — its
//!     `DispatchedJobID` becomes the lease `alloc_id`. The lease env
//!     (`LEASE_NAMESPACE`/`LEASE_MAX_JOBS`/`LEASE_IDLE_TIMEOUT`) rides as Nomad
//!     dispatch `Meta` (payload-free; meta is ≤16KB). Returns the lease JSON
//!     `{ alloc_id, executor_namespace, scheduler: { flavor: "nomad", eval_id } }`
//!     (node/expiry omitted — placement is async).
//!   - **release** → `nomad job stop` (`DELETE /v1/job/{id}`) on the dispatched
//!     job — SIGTERM → the drain executor graceful-drains in-flight + exits,
//!     freeing the alloc. Tolerant of an already-gone job (idempotent release).
//!
//! `node`/`expiry` are `""` (empty, not null — the `Lease__datacenter` schema
//! types every field as a required String): unlike Slurm's synchronous
//! `scontrol`, the Nomad alloc placement is not resolved at dispatch time
//! (`NomadWatcher` streams running/completed signals asynchronously; acquire does
//! not block on placement). No device/`gpu_uuid` field — no allocator reports it.
//!
//! ## Idempotency
//!
//! The handler passes the replay-safe `grant_id` (`instance_id:node_id`). Nomad
//! dispatch is not natively idempotent, but the lease lifecycle is loop-scoped
//! (acquire-once / release-once) and the grant token is journaled, so a re-fire
//! is bounded by the replay contract. A best-effort pre-dispatch probe for an
//! existing dispatched child of this grant could be added later (the Nomad
//! analogue of Slurm's `squeue --name`); v1 relies on the journaled grant token.
//!
//! ## Connection
//!
//! Nomad uses its own HTTP connection params from a held [`NomadConfig`] (built
//! from the `NOMAD_*` env at registration). The trait's `allocator_url`/`token`
//! args are HTTP-centric for the *generic* allocator and IGNORED here.

#[cfg(feature = "nomad")]
use std::collections::HashMap;

#[cfg(feature = "nomad")]
use serde_json::{json, Value as JsonValue};

#[cfg(feature = "nomad")]
use petri_application::resource_lease_handlers::{
    AllocatorClient, AllocatorError, StageOutcome, StageTemplateArgs,
};
#[cfg(feature = "nomad")]
use petri_nomad::config::NomadConfig;
#[cfg(feature = "nomad")]
use petri_nomad::models::{DispatchJobRequest, DispatchJobResponse, JobStopResponse};

/// The parameterized Nomad job ID the drain-executor lease dispatches against.
///
/// Registered into Nomad from `engine/infra/nomad/lease-executor-job-template.json`
/// by the `scheduler-up` recipe (the Nomad analogue of `mekhan-lease-executor.sh`).
/// Overridable via `NOMAD_LEASE_JOB_TEMPLATE`.
#[cfg(feature = "nomad")]
const DEFAULT_LEASE_JOB_TEMPLATE: &str = "petri-lease-executor";

/// Nomad-backed [`AllocatorClient`]: holds a lease by dispatching ONE persistent
/// drain executor (parameterized job), releases via `nomad job stop`.
#[cfg(feature = "nomad")]
pub struct NomadAllocatorClient {
    config: NomadConfig,
    http: reqwest::Client,
    /// The parameterized job ID dispatched per-acquire (the drain executor).
    lease_job_template: String,
    /// Bounded budget for the best-effort post-dispatch placement-node poll.
    /// Defaults to ~30s; tests set 0 to skip the poll entirely.
    placement_poll_budget: std::time::Duration,
}

#[cfg(feature = "nomad")]
impl NomadAllocatorClient {
    /// Build a client from a resolved connection (the datacenter resource's
    /// `effect_config`), NOT env. The [`NomadConfig`] is already built by the
    /// `ClusterRegistry` from the parsed connection. Alias of
    /// [`NomadAllocatorClient::new`] for symmetry with the slurm leg and to mark
    /// the multi-cluster (resource-driven) build path.
    pub fn from_connection(config: NomadConfig) -> Result<Self, AllocatorError> {
        Self::new(config)
    }

    /// Build a client around an explicit [`NomadConfig`].
    pub fn new(config: NomadConfig) -> Result<Self, AllocatorError> {
        let http = config
            .build_http_client()
            .map_err(|e| AllocatorError::Transport(format!("nomad http client: {e}")))?;
        let lease_job_template = std::env::var("NOMAD_LEASE_JOB_TEMPLATE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_LEASE_JOB_TEMPLATE.to_string());
        Ok(Self {
            config,
            http,
            lease_job_template,
            placement_poll_budget: std::time::Duration::from_secs(30),
        })
    }

    /// Build a client from the `NOMAD_*` environment, if `NOMAD_ADDR` is set
    /// (gating mirrors [`NomadConfig::from_env`]). Returns `None` when Nomad is
    /// not configured, so the dispatcher's nomad leg is simply absent.
    pub fn from_env() -> Option<Self> {
        NomadConfig::from_env().and_then(|c| Self::new(c).ok())
    }

    /// Build a full URL for a Nomad API endpoint (mirrors `NomadClient::url`).
    fn url(&self, path: &str) -> String {
        format!(
            "{}/v1/{}?region={}",
            self.config.addr.trim_end_matches('/'),
            path.trim_start_matches('/'),
            self.config.region
        )
    }

    /// Add the ACL token header when configured.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref token) = self.config.token {
            req.header("X-Nomad-Token", token)
        } else {
            req
        }
    }

    /// Dispatch ONE persistent drain executor and return the lease JSON.
    async fn acquire_lease(
        &self,
        grant_id: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, AllocatorError> {
        // The lease-scoped NATS namespace the drain executor consumes and the
        // body enqueues to. `grant_id` is `instance_id:node_id`; the `:` (and
        // any `/`) must be sanitised — it would otherwise break the NATS stream
        // name (`lease-…_medium`) / subject token (`lease-….medium.>`). Same
        // rule as the Slurm leg.
        let executor_namespace = format!("lease-{}", grant_id.replace([':', '/'], "-"));

        // The loop's maxIterations is the natural cap, but acquire only sees the
        // claim request — default generously (high cap + long idle window). The
        // drain executor self-exits on either bound or on `job stop`.
        let max_jobs = request
            .get("max_jobs")
            .and_then(|v| v.as_u64())
            .unwrap_or(100_000);
        let idle_secs = request
            .get("idle_timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(300);

        // Lease env rides as dispatch Meta (the parameterized job declares these
        // as MetaOptional and maps them onto the executor task env). Nomad's
        // dispatch payload has a tight (16KB) limit and is meta-only here.
        let mut meta: HashMap<String, String> = HashMap::new();
        meta.insert("LEASE_NAMESPACE".to_string(), executor_namespace.clone());
        meta.insert("LEASE_MAX_JOBS".to_string(), max_jobs.to_string());
        meta.insert("LEASE_IDLE_TIMEOUT".to_string(), idle_secs.to_string());

        // Held-alloc-death routing (docs/16 §7). The lease handler injected
        // `failure_routing` (petri_net_id/petri_place/petri_signal_key/
        // petri_signal_failed) into the request; stamp those string meta tags
        // onto the dispatched job so the NomadWatcher can route this alloc's
        // TERMINAL signal to the adapter net's `lease_failed` place when it dies.
        // Without this the watcher never tracks the held alloc → death is
        // undetected → the leased loop wedges on a dead namespace.
        if let Some(routing) = request.get("failure_routing").and_then(|v| v.as_object()) {
            for (k, v) in routing {
                if let Some(s) = v.as_str() {
                    meta.insert(k.clone(), s.to_string());
                }
            }
        }

        let dispatch_req = DispatchJobRequest {
            payload: None,
            meta,
        };
        let url = self.url(&format!("job/{}/dispatch", self.lease_job_template));

        let resp = self
            .auth(self.http.post(&url))
            .json(&dispatch_req)
            .send()
            .await
            .map_err(|e| AllocatorError::Transport(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AllocatorError::Status {
                status: status.as_u16(),
                body,
            });
        }

        let dispatch_resp: DispatchJobResponse = resp
            .json()
            .await
            .map_err(|e| AllocatorError::BadResponse(e.to_string()))?;

        tracing::info!(
            grant_id = %grant_id,
            dispatched_job_id = %dispatch_resp.dispatched_job_id,
            executor_namespace = %executor_namespace,
            "nomad lease: dispatched persistent drain executor",
        );

        // Best-effort, BOUNDED post-dispatch poll for the placement node. Unlike
        // Slurm's synchronous scontrol, Nomad placement is async — we poll
        // `GET /v1/allocation?job=<dispatched_job_id>` for up to ~30s to capture
        // the node the drain executor landed on (telemetry parity with Slurm's
        // lease node). On timeout we leave `node` null; the terminal signal's
        // AllocationMetrics will still carry it once the watcher observes the
        // alloc. NON-fatal: any error/timeout just omits the field.
        let node = self
            .poll_placement_node(&dispatch_resp.dispatched_job_id)
            .await;

        // `DatacenterLease`: only `alloc_id` is required. `expiry` is unresolved
        // at dispatch and OMITTED. `node` is included ONLY when the bounded poll
        // resolved it (else omitted, not empty-string). The `scheduler` detail
        // is the typed `nomad` variant carrying the dispatch evaluation id.
        let mut lease = json!({
            "alloc_id": dispatch_resp.dispatched_job_id,
            "executor_namespace": executor_namespace,
            "scheduler": {
                "flavor": "nomad",
                "eval_id": dispatch_resp.eval_id,
            },
        });
        if let Some(node) = node {
            if let Some(obj) = lease.as_object_mut() {
                obj.insert("node".to_string(), json!(node));
            }
        }
        Ok(lease)
    }

    /// BOUNDED, best-effort poll of `GET /v1/allocation?job=<job_id>` for the
    /// placement node name. Polls every 1s up to ~30s; returns the first
    /// allocation's non-empty `NodeName`, or `None` on timeout / any error
    /// (NEVER fails the acquire). On timeout the node is filled later by the
    /// terminal signal's AllocationMetrics.
    async fn poll_placement_node(&self, dispatched_job_id: &str) -> Option<String> {
        use std::time::{Duration, Instant};

        let budget = self.placement_poll_budget;
        // Zero budget → skip the poll entirely (tests / opt-out).
        if budget.is_zero() {
            return None;
        }
        const INTERVAL: Duration = Duration::from_secs(1);

        // Nomad's allocations-list (`/v1/allocations?prefix=` or `?job=`) returns
        // a slim list; the per-job filter is `?job=<id>`. We reuse the watcher's
        // `Allocation` model (its `node_name` field is present on list stubs).
        let url = format!(
            "{}/v1/allocations?job={}&region={}",
            self.config.addr.trim_end_matches('/'),
            dispatched_job_id,
            self.config.region,
        );

        let deadline = Instant::now() + budget;
        loop {
            let resp = match self.auth(self.http.get(&url)).send().await {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    tracing::debug!(
                        status = %r.status(),
                        dispatched_job_id,
                        "nomad lease: placement poll non-200 (will retry within budget)"
                    );
                    if Instant::now() >= deadline {
                        return None;
                    }
                    tokio::time::sleep(INTERVAL).await;
                    continue;
                }
                Err(e) => {
                    tracing::debug!(error = %e, dispatched_job_id, "nomad lease: placement poll transport error");
                    if Instant::now() >= deadline {
                        return None;
                    }
                    tokio::time::sleep(INTERVAL).await;
                    continue;
                }
            };

            if let Ok(allocs) = resp.json::<Vec<petri_nomad::models::Allocation>>().await {
                if let Some(node) = allocs
                    .iter()
                    .map(|a| a.node_name.trim())
                    .find(|n| !n.is_empty())
                {
                    tracing::info!(
                        dispatched_job_id,
                        node,
                        "nomad lease: resolved placement node via post-dispatch poll"
                    );
                    return Some(node.to_string());
                }
            }

            if Instant::now() >= deadline {
                tracing::debug!(
                    dispatched_job_id,
                    "nomad lease: placement node unresolved within budget — leaving node null"
                );
                return None;
            }
            tokio::time::sleep(INTERVAL).await;
        }
    }

    /// Stop the dispatched drain-executor job: `nomad job stop` →
    /// `DELETE /v1/job/{id}`. SIGTERM → graceful drain → exit. Tolerant of an
    /// already-gone job (idempotent release).
    async fn release_lease(&self, alloc_id: &str) -> Result<(), AllocatorError> {
        let url = self.url(&format!("job/{}", alloc_id));
        let resp = self
            .auth(self.http.delete(&url))
            .send()
            .await
            .map_err(|e| AllocatorError::Transport(e.to_string()))?;

        let status = resp.status();
        // 404 is tolerated: the job may already be gone (idempotent release),
        // matching the HTTP leg's contract.
        if !status.is_success() && status.as_u16() != 404 {
            let body = resp.text().await.unwrap_or_default();
            return Err(AllocatorError::Status {
                status: status.as_u16(),
                body,
            });
        }
        // Drain the body if present (some Nomad versions return a stop eval) —
        // best-effort, a missing/empty body is fine.
        let _ = resp.json::<JobStopResponse>().await;
        tracing::info!(alloc_id = %alloc_id, "nomad lease: released (job stop)");
        Ok(())
    }

    /// Render a [`StageTemplateArgs`] into a Nomad PARAMETERIZED job and REGISTER
    /// it via `PUT /v1/job/{slug}` (the register endpoint, NOT dispatch). Returns
    /// `remote_ref = slug`.
    ///
    /// The registered job mirrors the canonical shape (`ensure_parameterized_jobs`
    /// / the test-harness `register_test_job_template`): `Type: "batch"`, a
    /// `ParameterizedJob` section whose `MetaOptional` declares EXACTLY the routing
    /// meta keys the later `submit` dispatch path sends (`RoutingMeta::to_meta_tags`
    /// → `petri_net_id`/`petri_place`/`petri_signal_key`/`petri_signal_*`), so the
    /// staged job is dispatchable. One TaskGroup runs the `spec.entrypoint` (or a
    /// no-op default) with the requested CPU/MemoryMB/GPU device. `spec.image`, if
    /// present, is set as the `docker` task driver image; otherwise `raw_exec`.
    ///
    /// Tolerates 200/201 (registered) and 409 (already registered) as success.
    async fn stage_template(
        &self,
        args: &StageTemplateArgs,
    ) -> Result<StageOutcome, AllocatorError> {
        let job = self.render_parameterized_job(args);
        let url = self.url(&format!("job/{}", args.slug));

        let resp = self
            .auth(self.http.put(&url))
            .json(&job)
            .send()
            .await
            .map_err(|e| AllocatorError::Transport(e.to_string()))?;

        let status = resp.status();
        // 409 (already registered) is treated as success — staging is idempotent.
        if !status.is_success() && status.as_u16() != 409 {
            let body = resp.text().await.unwrap_or_default();
            return Err(AllocatorError::Status {
                status: status.as_u16(),
                body,
            });
        }

        tracing::info!(
            slug = %args.slug,
            status = %status.as_u16(),
            "nomad stage_template: registered parameterized job",
        );
        Ok(StageOutcome {
            remote_ref: args.slug.clone(),
        })
    }

    /// Build the `{ "Job": { … } }` register body for a parameterized job from
    /// the typed [`StageTemplateArgs`]. The `MetaOptional` keys MUST cover what
    /// the `submit` dispatch path stamps so the job stays dispatchable.
    fn render_parameterized_job(&self, args: &StageTemplateArgs) -> JsonValue {
        let spec = &args.spec;

        // The routing meta keys `RoutingMeta::to_meta_tags()` sends on dispatch.
        // Declared optional so a dispatch that omits some (e.g. no per-status
        // routes) still validates. Mirrors the canonical registered job shape.
        let meta_optional = json!([
            "petri_net_id",
            "petri_place",
            "petri_signal_key",
            "petri_signal_running",
            "petri_signal_completed",
            "petri_signal_failed",
        ]);

        // Resources from the typed spec; cluster-sane defaults for absent fields.
        let cpu = spec.cpus.filter(|c| *c > 0).unwrap_or(1);
        let mem = spec.mem_mb.filter(|m| *m > 0).unwrap_or(256);
        let mut resources = json!({ "CPU": cpu, "MemoryMB": mem });
        if let Some(gpus) = spec.gpus.filter(|g| *g > 0) {
            // Nomad GPU is a Device constraint. `gpu_type` (if any) names the
            // device; absent → the generic `gpu` device.
            let device_name = spec
                .gpu_type
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "gpu".to_string());
            resources["Devices"] = json!([{ "Name": device_name, "Count": gpus }]);
        }

        // Driver + Config. An `image` → docker driver; otherwise raw_exec running
        // the entrypoint via a shell (defaulting to a no-op `true` so the
        // registered job is valid even before the entrypoint is finalized).
        let entrypoint = spec
            .entrypoint
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "true".to_string());
        let (driver, config) = if let Some(image) = spec.image.clone().filter(|s| !s.is_empty()) {
            (
                "docker",
                json!({ "image": image, "command": "sh", "args": ["-c", entrypoint] }),
            )
        } else {
            (
                "raw_exec",
                json!({ "command": "sh", "args": ["-c", entrypoint] }),
            )
        };

        // Task env from the typed spec.
        let env: serde_json::Map<String, JsonValue> = spec
            .env
            .iter()
            .map(|(k, v)| (k.clone(), JsonValue::String(v.clone())))
            .collect();

        let mut task = json!({
            "Name": "petri-worker",
            "Driver": driver,
            "Config": config,
            "Resources": resources,
        });
        if !env.is_empty() {
            task["Env"] = JsonValue::Object(env);
        }

        // P3 residency + service shaping. Every value below resolves to the
        // EXACT current literal when its Option is None/absent, so a default
        // (no-residency, batch) spec renders the byte-identical job the
        // lease-executor emits today.
        //
        // Datacenters: a non-empty `residency_zone` drives the datacenter list;
        // absent ⇒ today's `["dc1"]`. (The load-bearing residency pin is the
        // `${meta.compliance_zone}` Constraint emitted below — the Datacenters
        // swap is doc-29-specified but only correct if the zone IS a valid Nomad
        // datacenter name; see the module risk note.)
        let datacenters = match spec.residency_zone.as_deref().filter(|s| !s.is_empty()) {
            Some(z) => json!([z]),
            None => json!(["dc1"]),
        };
        // Type + Count. `job_type == "service"` flips Type to "service" and lets
        // `replicas` drive Count; ANY other value (incl. None / "batch") ⇒ the
        // batch literals. `replicas` is read ONLY on the service path, so a stray
        // value on a batch spec cannot perturb the byte-stable batch render.
        let is_service = spec.job_type.as_deref() == Some("service");
        let job_type = if is_service { "service" } else { "batch" };
        let count = if is_service {
            spec.replicas.filter(|n| *n > 0).unwrap_or(1)
        } else {
            1
        };

        let mut job = json!({
            "ID": args.slug,
            "Name": args.slug,
            "Type": job_type,
            "Datacenters": datacenters,
            "TaskGroups": [{
                "Name": "main",
                "Count": count,
                "RestartPolicy": { "Attempts": 0, "Mode": "fail" },
                "ReschedulePolicy": { "Attempts": 0 },
                "Tasks": [task],
            }],
        });

        // `ParameterizedJob` is a BATCH-only stanza — Nomad rejects it on a
        // `service` job ("Parameterized job can only be used with batch or
        // sysbatch scheduler"). The batch lease-executor path is dispatched
        // per-run, so it keeps the stanza (and stays byte-identical to today).
        // The service replica path (job_type=service) is NOT dispatched: it runs
        // at a fixed Count, so it omits the stanza entirely.
        if !is_service {
            job["ParameterizedJob"] = json!({
                "Payload": "optional",
                "MetaRequired": [],
                "MetaOptional": meta_optional,
            });
        }

        // Residency pin: a node Constraint on `${meta.compliance_zone}`, mirroring
        // the GPU-Device `if let Some(...).filter(non-empty)` idiom above. Inserted
        // ONLY when a zone is present, so the None path adds no `Constraints` key
        // at all — the job map stays byte-identical to today (today emits none).
        if let Some(zone) = spec.residency_zone.as_deref().filter(|s| !s.is_empty()) {
            job["Constraints"] = json!([{
                "LTarget": "${meta.compliance_zone}",
                "Operand": "=",
                "RTarget": zone,
            }]);
        }

        // v1 escape hatch: an `hcl_stanza` is advisory only (we register typed
        // JSON, not HCL). Record it so the author intent is visible/diagnosable
        // rather than silently dropped; a full HCL-merge is deferred.
        if let Some(stanza) = args
            .escape_hatch
            .hcl_stanza
            .as_ref()
            .filter(|s| !s.is_empty())
        {
            tracing::warn!(
                slug = %args.slug,
                "nomad stage_template: escape_hatch.hcl_stanza is advisory in v1 (typed JSON \
                 registration only) — not merged into the registered job",
            );
            if let Some(meta) = job.get_mut("Meta").and_then(|m| m.as_object_mut()) {
                meta.insert("petri_stage_hcl_stanza".into(), json!(stanza));
            } else {
                job["Meta"] = json!({ "petri_stage_hcl_stanza": stanza });
            }
        }

        json!({ "Job": job })
    }
}

#[cfg(feature = "nomad")]
#[async_trait::async_trait]
impl AllocatorClient for NomadAllocatorClient {
    async fn acquire(
        &self,
        _allocator_url: &str,
        _token: &str,
        grant_id: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, AllocatorError> {
        // Nomad uses its own held NomadConfig — url/token are HTTP-centric and
        // ignored.
        self.acquire_lease(grant_id, request).await
    }

    async fn release(
        &self,
        _allocator_url: &str,
        _token: &str,
        alloc_id: &str,
    ) -> Result<(), AllocatorError> {
        self.release_lease(alloc_id).await
    }

    async fn stage_template_with_connection(
        &self,
        _config: &JsonValue,
        args: &StageTemplateArgs,
    ) -> Result<StageOutcome, AllocatorError> {
        // Nomad uses its own held NomadConfig — the connection in `config` was
        // already used by the registry to build/resolve THIS client, so the
        // config arg is ignored here (symmetric with acquire/release).
        self.stage_template(args).await
    }
}

/// Dev-only: register the Nomad parameterized jobs the lease/scheduler path
/// dispatches against (`petri-lease-executor`, `petri-executor-worker`).
///
/// GATED behind `NOMAD_AUTOPROVISION_JOBS=1` so it NEVER runs in prod (there
/// the parameterized jobs are managed objects). The in-memory dev nomad
/// agent loses its jobs on restart, so a later `resource_lease_acquire` 500s
/// with 'parameterized job not found'; registering at engine startup (hence
/// on every restart) self-heals that. Reads ready-to-register job bodies
/// (`{ "Job": { ... } }`) from `NOMAD_JOB_TEMPLATE_DIR` (the `scheduler-up` recipe
/// renders the env-interpolated templates there, staying the single source
/// of truth for binary/S3/NATS/SDK interpolation) and POSTs each to
/// `POST {NOMAD_ADDR}/v1/jobs`. Best-effort: failures are logged at WARN and
/// swallowed so a missing dir / unreachable Nomad never crashes startup.
#[cfg(feature = "nomad")]
pub async fn ensure_parameterized_jobs() {
    if std::env::var("NOMAD_AUTOPROVISION_JOBS").ok().as_deref() != Some("1") {
        return;
    }
    let Some(addr) = std::env::var("NOMAD_ADDR").ok().filter(|s| !s.is_empty()) else {
        tracing::warn!(
            "NOMAD_AUTOPROVISION_JOBS=1 but NOMAD_ADDR unset — skipping Nomad job provisioning"
        );
        return;
    };
    let dir = std::env::var("NOMAD_JOB_TEMPLATE_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "engine/infra/nomad".to_string());
    let token = std::env::var("NOMAD_TOKEN").ok().filter(|s| !s.is_empty());
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(%dir, %e, "NOMAD_JOB_TEMPLATE_DIR unreadable — no Nomad jobs provisioned");
            return;
        }
    };
    let http = reqwest::Client::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(?path, %e, "could not read Nomad job file");
                continue;
            }
        };
        let url = format!("{}/v1/jobs", addr.trim_end_matches('/'));
        let mut req = http
            .post(&url)
            .header("content-type", "application/json")
            .body(body);
        if let Some(ref t) = token {
            req = req.header("X-Nomad-Token", t);
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(
                    ?path,
                    "registered Nomad parameterized job (dev autoprovision)"
                );
            }
            Ok(resp) => {
                let status = resp.status();
                let txt = resp.text().await.unwrap_or_default();
                tracing::warn!(?path, %status, body = %txt, "Nomad job register failed (dev autoprovision)");
            }
            Err(e) => {
                tracing::warn!(?path, %e, "Nomad job register request failed (dev autoprovision)")
            }
        }
    }
}

#[cfg(all(test, feature = "nomad"))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A captured HTTP request the fake Nomad recorded.
    #[derive(Default, Clone)]
    struct Captured {
        method: String,
        path: String,
        body: String,
    }

    /// Minimal one-shot fake Nomad HTTP server: accepts a single connection,
    /// records the request line + body, and replies with `response_body` as
    /// 200. Returns the bound `addr` plus a handle to read what it captured.
    /// Dependency-free (no wiremock) — petri-api's dev-deps don't include one.
    async fn fake_nomad(response_body: &'static str) -> (String, Arc<Mutex<Captured>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let captured = Arc::new(Mutex::new(Captured::default()));
        let cap = captured.clone();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                let mut lines = raw.split("\r\n");
                let req_line = lines.next().unwrap_or("");
                let mut parts = req_line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let full_path = parts.next().unwrap_or("").to_string();
                // strip the ?region=… query for stable assertions
                let path = full_path.split('?').next().unwrap_or("").to_string();
                let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                {
                    let mut g = cap.lock().unwrap();
                    g.method = method;
                    g.path = path;
                    g.body = body;
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body,
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (addr, captured)
    }

    fn client_for(addr: &str) -> NomadAllocatorClient {
        let config = NomadConfig {
            addr: addr.to_string(),
            ..NomadConfig::default()
        };
        // bypass env-driven template selection for the test; skip the bounded
        // placement poll (the one-shot fake Nomad only serves the dispatch POST).
        NomadAllocatorClient {
            http: config.build_http_client().unwrap(),
            config,
            lease_job_template: "petri-lease-executor".to_string(),
            placement_poll_budget: std::time::Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn acquire_dispatches_lease_job_and_returns_namespaced_lease() {
        let (addr, captured) = fake_nomad(
            r#"{"DispatchedJobID":"petri-lease-executor/dispatch-99","EvalID":"e1","Index":7}"#,
        )
        .await;
        let client = client_for(&addr);

        // grant_id is instance_id:node_id — the ':' MUST be sanitised.
        let lease = client
            .acquire_lease(
                "inst-1:loop-node",
                &json!({ "max_jobs": 5, "idle_timeout_secs": 120 }),
            )
            .await
            .unwrap();

        // dispatched job id → alloc_id; namespace sanitised; node/expiry OMITTED
        // (async placement) rather than empty-string; typed nomad scheduler detail.
        assert_eq!(
            lease.get("alloc_id").unwrap(),
            "petri-lease-executor/dispatch-99"
        );
        assert_eq!(
            lease.get("executor_namespace").unwrap(),
            "lease-inst-1-loop-node"
        );
        assert!(
            lease.get("node").is_none(),
            "node omitted until placed: {lease}"
        );
        assert!(lease.get("expiry").is_none(), "expiry omitted: {lease}");
        assert!(lease.get("gpu_uuid").is_none(), "gpu_uuid retired: {lease}");
        assert_eq!(lease["scheduler"]["flavor"], "nomad");
        assert_eq!(lease["scheduler"]["eval_id"], "e1");

        let cap = captured.lock().unwrap().clone();
        assert_eq!(cap.method, "POST");
        assert_eq!(cap.path, "/v1/job/petri-lease-executor/dispatch");
        // lease env rides as Meta (no payload)
        assert!(cap.body.contains("\"Meta\""), "body: {}", cap.body);
        assert!(
            cap.body.contains("lease-inst-1-loop-node"),
            "body: {}",
            cap.body
        );
        assert!(cap.body.contains("LEASE_NAMESPACE"), "body: {}", cap.body);
        assert!(
            cap.body.contains("\"LEASE_MAX_JOBS\":\"5\""),
            "body: {}",
            cap.body
        );
        assert!(
            cap.body.contains("\"LEASE_IDLE_TIMEOUT\":\"120\""),
            "body: {}",
            cap.body
        );
        // payload-free dispatch (meta-only)
        assert!(!cap.body.contains("\"Payload\""), "body: {}", cap.body);
    }

    #[tokio::test]
    async fn acquire_stamps_failure_routing_into_dispatch_meta() {
        // The lease handler injects failure_routing; the Nomad leg MUST stamp
        // those keys into the dispatch meta so NomadWatcher routes the held
        // alloc's terminal signal to `lease_failed` on death (docs/16 §7).
        let (addr, captured) =
            fake_nomad(r#"{"DispatchedJobID":"d/7","EvalID":"e","Index":1}"#).await;
        let client = client_for(&addr);
        client
            .acquire_lease(
                "inst-9:lp",
                &json!({
                    "failure_routing": {
                        "petri_net_id": "pool-rid-9",
                        "petri_place": "lease_failed",
                        "petri_signal_key": "inst-9:lp",
                        "petri_signal_failed": "lease_failed",
                    }
                }),
            )
            .await
            .unwrap();
        let cap = captured.lock().unwrap().clone();
        assert!(
            cap.body.contains("petri_signal_failed"),
            "body: {}",
            cap.body
        );
        assert!(cap.body.contains("pool-rid-9"), "body: {}", cap.body);
        assert!(cap.body.contains("lease_failed"), "body: {}", cap.body);
    }

    #[tokio::test]
    async fn acquire_defaults_cap_and_idle_when_request_silent() {
        let (addr, captured) =
            fake_nomad(r#"{"DispatchedJobID":"d/1","EvalID":"e","Index":1}"#).await;
        let client = client_for(&addr);

        let _ = client.acquire_lease("g:n", &json!({})).await.unwrap();

        let cap = captured.lock().unwrap().clone();
        assert!(
            cap.body.contains("\"LEASE_MAX_JOBS\":\"100000\""),
            "body: {}",
            cap.body
        );
        assert!(
            cap.body.contains("\"LEASE_IDLE_TIMEOUT\":\"300\""),
            "body: {}",
            cap.body
        );
    }

    #[tokio::test]
    async fn stage_template_registers_parameterized_job_via_put() {
        // Nomad register returns an eval-ish body on success; we only assert the
        // request line + body shape.
        let (addr, captured) = fake_nomad(r#"{"EvalID":"reg-1","Index":3}"#).await;
        let client = client_for(&addr);

        let args = StageTemplateArgs {
            slug: "train-job".to_string(),
            spec: petri_application::resource_lease_handlers::StageSpec {
                cpus: Some(4),
                gpus: Some(2),
                gpu_type: Some("a100".to_string()),
                mem_mb: Some(8192),
                image: Some("py:3.12".to_string()),
                entrypoint: Some("python run.py".to_string()),
                env: std::iter::once(("FOO".to_string(), "bar".to_string())).collect(),
                ..Default::default()
            },
            escape_hatch: Default::default(),
            package_ref: None,
        };

        let outcome = client.stage_template(&args).await.unwrap();
        assert_eq!(outcome.remote_ref, "train-job");

        let cap = captured.lock().unwrap().clone();
        // The REGISTER endpoint: PUT /v1/job/{slug} (NOT dispatch).
        assert_eq!(cap.method, "PUT");
        assert_eq!(cap.path, "/v1/job/train-job");

        // Full Job spec: parameterized, batch, with the dispatch routing meta keys
        // declared so the staged job is later dispatchable.
        let body: serde_json::Value = serde_json::from_str(&cap.body)
            .unwrap_or_else(|e| panic!("body not JSON: {e}; raw={}", cap.body));
        let job = &body["Job"];
        assert_eq!(job["ID"], "train-job");
        assert_eq!(job["Type"], "batch");
        assert!(job["ParameterizedJob"].is_object(), "body: {}", cap.body);
        let meta_optional = job["ParameterizedJob"]["MetaOptional"]
            .as_array()
            .expect("MetaOptional array");
        for required in [
            "petri_net_id",
            "petri_place",
            "petri_signal_key",
            "petri_signal_completed",
            "petri_signal_failed",
        ] {
            assert!(
                meta_optional.iter().any(|v| v == required),
                "MetaOptional must declare {required}: {}",
                cap.body
            );
        }
        // Resources + GPU device + docker image + entrypoint + env threaded.
        let task = &job["TaskGroups"][0]["Tasks"][0];
        assert_eq!(task["Resources"]["CPU"], 4);
        assert_eq!(task["Resources"]["MemoryMB"], 8192);
        assert_eq!(task["Resources"]["Devices"][0]["Name"], "a100");
        assert_eq!(task["Resources"]["Devices"][0]["Count"], 2);
        assert_eq!(task["Driver"], "docker");
        assert_eq!(task["Config"]["image"], "py:3.12");
        assert_eq!(task["Env"]["FOO"], "bar");
    }

    #[tokio::test]
    async fn render_default_spec_is_byte_identical_batch() {
        // The hard P3 regression guard: a default StageSpec (residency_zone /
        // replicas / job_type all None) — exactly what mekhan's build_staging_net
        // seeds today — must render the byte-identical batch lease-executor job.
        // We pin the WHOLE Job object against a golden literal so any incidental
        // key drift (a stray Constraints:null, a reordered Count, …) fails.
        let client = client_for("http://127.0.0.1:1"); // never dialed; render is pure.
        let args = StageTemplateArgs {
            slug: "train-job".to_string(),
            spec: petri_application::resource_lease_handlers::StageSpec {
                cpus: Some(4),
                gpus: Some(2),
                gpu_type: Some("a100".to_string()),
                mem_mb: Some(8192),
                image: Some("py:3.12".to_string()),
                entrypoint: Some("python run.py".to_string()),
                env: std::iter::once(("FOO".to_string(), "bar".to_string())).collect(),
                // residency_zone / replicas / job_type all default to None.
                ..Default::default()
            },
            escape_hatch: Default::default(),
            package_ref: None,
        };

        let rendered = client.render_parameterized_job(&args);

        // Golden Job: the EXACT shape the pre-P3 renderer emits for this spec.
        // Type=="batch", Datacenters==["dc1"], Count==1, and NO Constraints key.
        let golden = json!({
            "ID": "train-job",
            "Name": "train-job",
            "Type": "batch",
            "Datacenters": ["dc1"],
            "ParameterizedJob": {
                "Payload": "optional",
                "MetaRequired": [],
                "MetaOptional": [
                    "petri_net_id",
                    "petri_place",
                    "petri_signal_key",
                    "petri_signal_running",
                    "petri_signal_completed",
                    "petri_signal_failed",
                ],
            },
            "TaskGroups": [{
                "Name": "main",
                "Count": 1,
                "RestartPolicy": { "Attempts": 0, "Mode": "fail" },
                "ReschedulePolicy": { "Attempts": 0 },
                "Tasks": [{
                    "Name": "petri-worker",
                    "Driver": "docker",
                    "Config": { "image": "py:3.12", "command": "sh", "args": ["-c", "python run.py"] },
                    "Resources": {
                        "CPU": 4,
                        "MemoryMB": 8192,
                        "Devices": [{ "Name": "a100", "Count": 2 }],
                    },
                    "Env": { "FOO": "bar" },
                }],
            }],
        });

        assert_eq!(
            rendered["Job"], golden,
            "default spec must render byte-identical batch job: {rendered:#}"
        );
        // Belt-and-suspenders: no Constraints key on the None-residency path.
        assert!(
            rendered["Job"].get("Constraints").is_none(),
            "no Constraints key on default spec: {rendered:#}"
        );
    }

    #[tokio::test]
    async fn render_residency_zone_pins_datacenter_and_constraint() {
        let client = client_for("http://127.0.0.1:1");
        let args = StageTemplateArgs {
            slug: "eu-job".to_string(),
            spec: petri_application::resource_lease_handlers::StageSpec {
                residency_zone: Some("eu-west".to_string()),
                ..Default::default()
            },
            escape_hatch: Default::default(),
            package_ref: None,
        };

        let rendered = client.render_parameterized_job(&args);
        let job = &rendered["Job"];

        assert_eq!(job["Datacenters"], json!(["eu-west"]));
        assert_eq!(
            job["Constraints"],
            json!([{
                "LTarget": "${meta.compliance_zone}",
                "Operand": "=",
                "RTarget": "eu-west",
            }])
        );
        // Residency does not change the batch defaults.
        assert_eq!(job["Type"], "batch");
        assert_eq!(job["TaskGroups"][0]["Count"], 1);

        // An EMPTY zone string falls back to the byte-stable default (no
        // Datacenters=[""] / meaningless Constraint).
        let empty_args = StageTemplateArgs {
            slug: "empty-job".to_string(),
            spec: petri_application::resource_lease_handlers::StageSpec {
                residency_zone: Some(String::new()),
                ..Default::default()
            },
            escape_hatch: Default::default(),
            package_ref: None,
        };
        let empty = client.render_parameterized_job(&empty_args);
        assert_eq!(empty["Job"]["Datacenters"], json!(["dc1"]));
        assert!(empty["Job"].get("Constraints").is_none());
    }

    #[tokio::test]
    async fn render_service_type_sets_type_and_count() {
        let client = client_for("http://127.0.0.1:1");

        // service + replicas ⇒ Type=="service", Count==replicas.
        let svc = StageTemplateArgs {
            slug: "svc-job".to_string(),
            spec: petri_application::resource_lease_handlers::StageSpec {
                job_type: Some("service".to_string()),
                replicas: Some(3),
                ..Default::default()
            },
            escape_hatch: Default::default(),
            package_ref: None,
        };
        let r = client.render_parameterized_job(&svc);
        assert_eq!(r["Job"]["Type"], "service");
        assert_eq!(r["Job"]["TaskGroups"][0]["Count"], 3);
        // A `service` job MUST NOT carry the `ParameterizedJob` stanza — Nomad
        // rejects the register with 500 "Parameterized job can only be used with
        // batch or sysbatch scheduler". (Regression guard: a live Nomad register
        // of a residency-pinned model replica caught this; the shape-only checks
        // above did not.)
        assert!(
            r["Job"].get("ParameterizedJob").is_none(),
            "service job must omit ParameterizedJob (Nomad rejects it on non-batch): {r:#}"
        );

        // service + replicas None ⇒ Count 1.
        let svc_default = StageTemplateArgs {
            slug: "svc-default".to_string(),
            spec: petri_application::resource_lease_handlers::StageSpec {
                job_type: Some("service".to_string()),
                replicas: None,
                ..Default::default()
            },
            escape_hatch: Default::default(),
            package_ref: None,
        };
        let rd = client.render_parameterized_job(&svc_default);
        assert_eq!(rd["Job"]["Type"], "service");
        assert_eq!(rd["Job"]["TaskGroups"][0]["Count"], 1);

        // replicas on a BATCH spec (job_type None) is ignored: Type stays
        // "batch", Count stays 1 — replicas never perturbs the batch render.
        let batch_with_replicas = StageTemplateArgs {
            slug: "batch-stray".to_string(),
            spec: petri_application::resource_lease_handlers::StageSpec {
                job_type: None,
                replicas: Some(3),
                ..Default::default()
            },
            escape_hatch: Default::default(),
            package_ref: None,
        };
        let bw = client.render_parameterized_job(&batch_with_replicas);
        assert_eq!(bw["Job"]["Type"], "batch");
        assert_eq!(bw["Job"]["TaskGroups"][0]["Count"], 1);
        // The batch (dispatched lease-executor) path KEEPS the parameterized
        // stanza — it is dispatched per-run.
        assert!(
            bw["Job"]["ParameterizedJob"].is_object(),
            "batch job must keep ParameterizedJob: {bw:#}"
        );
    }

    #[tokio::test]
    async fn stage_template_tolerates_409_already_registered() {
        // A 409 (already registered) is success — staging is idempotent.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let body = "conflict";
                let resp = format!(
                    "HTTP/1.1 409 Conflict\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body,
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        let client = client_for(&addr);
        let args = StageTemplateArgs {
            slug: "dup-job".to_string(),
            spec: Default::default(),
            escape_hatch: Default::default(),
            package_ref: None,
        };
        let outcome = client.stage_template(&args).await.unwrap();
        assert_eq!(outcome.remote_ref, "dup-job");
    }

    #[tokio::test]
    async fn release_stops_the_dispatched_job() {
        let (addr, captured) = fake_nomad(r#"{"EvalID":"stop-1"}"#).await;
        let client = client_for(&addr);

        client
            .release_lease("petri-lease-executor/dispatch-99")
            .await
            .unwrap();

        let cap = captured.lock().unwrap().clone();
        assert_eq!(cap.method, "DELETE");
        assert_eq!(cap.path, "/v1/job/petri-lease-executor/dispatch-99");
    }
}
