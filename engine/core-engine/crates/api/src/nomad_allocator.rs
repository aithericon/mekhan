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
//!     `{ node, gpu_uuid, alloc_id, expiry, executor_namespace }`.
//!   - **release** → `nomad job stop` (`DELETE /v1/job/{id}`) on the dispatched
//!     job — SIGTERM → the drain executor graceful-drains in-flight + exits,
//!     freeing the alloc. Tolerant of an already-gone job (idempotent release).
//!
//! `node`/`expiry` are `""` (empty, not null — the `Lease__datacenter` schema
//! types every field as a required String): unlike Slurm's synchronous
//! `scontrol`, the Nomad alloc placement is not resolved at dispatch time
//! (`NomadWatcher` streams running/completed signals asynchronously; acquire does
//! not block on placement). `gpu_uuid` is `""` on the CPU dev cluster.
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
use petri_application::resource_lease_handlers::{AllocatorClient, AllocatorError};
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

        // `Lease__datacenter` types every field as a required String — empty,
        // not null. node/expiry are unresolved at dispatch (the watcher streams
        // placement asynchronously); the drain executor self-identifies via the
        // namespace, so those are "" here.
        Ok(json!({
            "node": "",
            "gpu_uuid": "",
            "alloc_id": dispatch_resp.dispatched_job_id,
            "expiry": "",
            "executor_namespace": executor_namespace,
        }))
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
        // bypass env-driven template selection for the test
        NomadAllocatorClient {
            http: config.build_http_client().unwrap(),
            config,
            lease_job_template: "petri-lease-executor".to_string(),
        }
    }

    #[tokio::test]
    async fn acquire_dispatches_lease_job_and_returns_namespaced_lease() {
        let (addr, captured) =
            fake_nomad(r#"{"DispatchedJobID":"petri-lease-executor/dispatch-99","EvalID":"e1","Index":7}"#)
                .await;
        let client = client_for(&addr);

        // grant_id is instance_id:node_id — the ':' MUST be sanitised.
        let lease = client
            .acquire_lease("inst-1:loop-node", &json!({ "max_jobs": 5, "idle_timeout_secs": 120 }))
            .await
            .unwrap();

        // dispatched job id → alloc_id; namespace sanitised; empty-not-null fields.
        assert_eq!(lease.get("alloc_id").unwrap(), "petri-lease-executor/dispatch-99");
        assert_eq!(lease.get("executor_namespace").unwrap(), "lease-inst-1-loop-node");
        assert_eq!(lease.get("node").unwrap(), "");
        assert_eq!(lease.get("gpu_uuid").unwrap(), "");
        assert_eq!(lease.get("expiry").unwrap(), "");

        let cap = captured.lock().unwrap().clone();
        assert_eq!(cap.method, "POST");
        assert_eq!(cap.path, "/v1/job/petri-lease-executor/dispatch");
        // lease env rides as Meta (no payload)
        assert!(cap.body.contains("\"Meta\""), "body: {}", cap.body);
        assert!(cap.body.contains("lease-inst-1-loop-node"), "body: {}", cap.body);
        assert!(cap.body.contains("LEASE_NAMESPACE"), "body: {}", cap.body);
        assert!(cap.body.contains("\"LEASE_MAX_JOBS\":\"5\""), "body: {}", cap.body);
        assert!(cap.body.contains("\"LEASE_IDLE_TIMEOUT\":\"120\""), "body: {}", cap.body);
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
        assert!(cap.body.contains("petri_signal_failed"), "body: {}", cap.body);
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
        assert!(cap.body.contains("\"LEASE_MAX_JOBS\":\"100000\""), "body: {}", cap.body);
        assert!(cap.body.contains("\"LEASE_IDLE_TIMEOUT\":\"300\""), "body: {}", cap.body);
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
