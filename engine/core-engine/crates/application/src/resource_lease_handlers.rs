//! Resource-lease effect handlers (R4a — the `scheduler` deployment backend's
//! `lease` operation).
//!
//! A `datacenter` resource (docs/13) is an external cluster that owns
//! placement. Instead of submitting a job and awaiting its result (the
//! `submit` operation → scheduler-net), the `lease` operation *holds an
//! allocation* on the cluster for the step's duration: acquire a lease, run the
//! body on it, release the lease. The net holds only the lease handle — the
//! external allocator stays the source of truth (no DC-state mirror).
//!
//! Two effects, both categorised under `ServiceCategory::Scheduler`:
//!   - `resource_lease_acquire` (input "request" → output "lease")
//!   - `resource_lease_release` (input "release" → output "released")
//!
//! ## Config / secret injection (per-fire, not per-net)
//!
//! The allocator connection (`{ allocator_url, token }`) arrives on the
//! transition's `effect_config`, with `{{secret:…}}` placeholders resolved
//! just-in-time by `firing.rs` (`aithericon_secrets::resolve_secrets`) into
//! `EffectInput::config` BEFORE `execute()` runs. So the handler reads the
//! resolved `input.config`, and the registration (`net_registry.rs`) needs no
//! per-net connection state — one stateless `HttpAllocatorClient` serves every
//! datacenter, the URL+token differing per fire.
//!
//! ## Replay-safety + idempotency (the load-bearing contract)
//!
//! The engine journals `EffectCompleted{ produced_tokens, effect_result }` after
//! a live `execute()`. On REPLAY (`firing.rs` `ExecutionMode::Replay`) it
//! re-emits the stored `produced_tokens` and calls `replay()` — it does NOT call
//! `execute()`. So the allocator is hit **exactly once per grant, never on
//! replay**. `replay()` here is a no-op (the handlers are stateless — no
//! optimizer-style internal state to rebuild; the lease lives entirely in the
//! journaled token). Defence in depth against a crash *between* the allocator
//! call and the journal append: `execute()` passes the **`grant_id`** (the
//! replay-safe `instance_id:node_id` minted by the compiler — no clock, no RNG)
//! as the allocator's `Idempotency-Key`, so a re-fire returns the same lease.
//! The handler itself uses NO `random()` / clock — every output is a pure
//! function of the input + the allocator's response.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

/// Error from the allocator HTTP API.
#[derive(Debug, thiserror::Error)]
pub enum AllocatorError {
    #[error("allocator transport error: {0}")]
    Transport(String),
    #[error("allocator returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("allocator response was not valid lease JSON: {0}")]
    BadResponse(String),
}

/// Client for a generic HTTP lease allocator. Trait so handlers can be tested
/// against a mock without standing up a real cluster.
///
/// Wire contract (the generic HTTP lease API R4 proves against; Slurm/Nomad
/// adapters become concrete `scheduler_flavor` configs later):
///   - **acquire**: `POST {allocator_url}` with `request` as the JSON body and
///     a bearer `token`; the `grant_id` rides as the `Idempotency-Key` header.
///     Returns the lease JSON `{ node, gpu_uuid, alloc_id, expiry }`.
///   - **release**: `DELETE {allocator_url}/{alloc_id}` with the bearer `token`.
#[async_trait::async_trait]
pub trait AllocatorClient: Send + Sync {
    /// Acquire a lease. `request` is the claim params; `grant_id` is the
    /// idempotency key. Returns the lease JSON.
    async fn acquire(
        &self,
        allocator_url: &str,
        token: &str,
        grant_id: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, AllocatorError>;

    /// Release a lease by its allocator-assigned `alloc_id`.
    async fn release(
        &self,
        allocator_url: &str,
        token: &str,
        alloc_id: &str,
    ) -> Result<(), AllocatorError>;

    /// Flavor-aware acquire. The handler reads `scheduler_flavor` off the
    /// per-fire `effect_config` (default `"http"`) and threads it here so a
    /// single registered dispatcher client can route http vs slurm per fire.
    ///
    /// Default impl ignores the flavor and delegates to [`acquire`], so leaf
    /// clients (e.g. `HttpAllocatorClient`, `SlurmAllocatorClient`) stay
    /// flavor-unaware and byte-identical — only the dispatcher overrides this
    /// to branch on `scheduler_flavor`.
    ///
    /// [`acquire`]: AllocatorClient::acquire
    async fn acquire_with_flavor(
        &self,
        scheduler_flavor: &str,
        allocator_url: &str,
        token: &str,
        grant_id: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, AllocatorError> {
        let _ = scheduler_flavor;
        self.acquire(allocator_url, token, grant_id, request).await
    }

    /// Flavor-aware release. See [`acquire_with_flavor`] for the routing
    /// rationale; default impl delegates to [`release`].
    ///
    /// [`acquire_with_flavor`]: AllocatorClient::acquire_with_flavor
    /// [`release`]: AllocatorClient::release
    async fn release_with_flavor(
        &self,
        scheduler_flavor: &str,
        allocator_url: &str,
        token: &str,
        alloc_id: &str,
    ) -> Result<(), AllocatorError> {
        let _ = scheduler_flavor;
        self.release(allocator_url, token, alloc_id).await
    }

    /// Connection-aware acquire — the multi-cluster seam (docs/16). The handler
    /// passes the WHOLE resolved `effect_config` (`config`) so a registry-backed
    /// client can read the `(resource_id, version)` correlation keys + the
    /// per-flavor connection fields off it, lazily build/resolve the right
    /// [`ClusterClient`], route the acquire, and bump that cluster's active count.
    ///
    /// Default impl preserves the single-cluster behaviour: it parses just
    /// `{ scheduler_flavor, allocator_url, token }` off `config` and delegates to
    /// [`acquire_with_flavor`] — so `FlavorDispatchAllocatorClient` + every leaf
    /// client stay byte-identical. Only the `petri-api` `ClusterRegistry` adapter
    /// overrides this to use the full connection.
    ///
    /// [`acquire_with_flavor`]: AllocatorClient::acquire_with_flavor
    async fn acquire_with_connection(
        &self,
        config: &JsonValue,
        grant_id: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, AllocatorError> {
        let flavor = config
            .get("scheduler_flavor")
            .and_then(|v| v.as_str())
            .unwrap_or("http");
        let allocator_url = config
            .get("allocator_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let token = config.get("token").and_then(|v| v.as_str()).unwrap_or("");
        self.acquire_with_flavor(flavor, allocator_url, token, grant_id, request)
            .await
    }

    /// Connection-aware release — the multi-cluster counterpart to
    /// [`acquire_with_connection`]. Carries the full `effect_config` so the
    /// registry adapter can resolve the cluster + decrement its active count
    /// (arming idle-teardown). Default impl delegates to [`release_with_flavor`].
    ///
    /// [`acquire_with_connection`]: AllocatorClient::acquire_with_connection
    /// [`release_with_flavor`]: AllocatorClient::release_with_flavor
    async fn release_with_connection(
        &self,
        config: &JsonValue,
        alloc_id: &str,
    ) -> Result<(), AllocatorError> {
        let flavor = config
            .get("scheduler_flavor")
            .and_then(|v| v.as_str())
            .unwrap_or("http");
        let allocator_url = config
            .get("allocator_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let token = config.get("token").and_then(|v| v.as_str()).unwrap_or("");
        self.release_with_flavor(flavor, allocator_url, token, alloc_id)
            .await
    }
}

/// `reqwest`-backed allocator client. Stateless — the per-datacenter URL+token
/// arrive per call (from the resolved `effect_config`), so a single instance is
/// shared across every net + datacenter.
pub struct HttpAllocatorClient {
    client: reqwest::Client,
}

impl HttpAllocatorClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .build()
                .expect("reqwest::Client::builder must not fail with default config"),
        }
    }
}

impl Default for HttpAllocatorClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AllocatorClient for HttpAllocatorClient {
    async fn acquire(
        &self,
        allocator_url: &str,
        token: &str,
        grant_id: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, AllocatorError> {
        let resp = self
            .client
            .post(allocator_url)
            .bearer_auth(token)
            // grant_id is the replay-safe idempotency key: a re-fire (e.g. crash
            // before journaling) returns the same lease rather than allocating
            // a second one.
            .header("Idempotency-Key", grant_id)
            .json(request)
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
        resp.json::<JsonValue>()
            .await
            .map_err(|e| AllocatorError::BadResponse(e.to_string()))
    }

    async fn release(
        &self,
        allocator_url: &str,
        token: &str,
        alloc_id: &str,
    ) -> Result<(), AllocatorError> {
        let url = format!("{}/{}", allocator_url.trim_end_matches('/'), alloc_id);
        let resp = self
            .client
            .delete(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| AllocatorError::Transport(e.to_string()))?;

        let status = resp.status();
        // 404 is tolerated: the lease may already be gone (idempotent release).
        if !status.is_success() && status.as_u16() != 404 {
            let body = resp.text().await.unwrap_or_default();
            return Err(AllocatorError::Status {
                status: status.as_u16(),
                body,
            });
        }
        Ok(())
    }
}

// The effect_config is now parsed by the connection-aware trait seam
// (`acquire_with_connection`/`release_with_connection`, leniently) for the
// single-cluster/http path, and by `ClusterConnection::from_effect_config`
// (petri-api) for the multi-cluster ClusterRegistry adapter. The old strict
// `read_connection`/`LeaseConnection` parser is retired — a slurm/nomad
// effect_config carries no `allocator_url`, so a strict require would wrongly
// reject the env-flavor-dispatch path.

// ---------------------------------------------------------------------------
// ResourceLeaseAcquireHandler
// ---------------------------------------------------------------------------

/// Acquires a cluster lease and emits the typed lease token.
///
/// Input port (`request`): `{ grant_id, request: { gpu_count, gpu_type, … } }`.
/// Output port (`lease`): `{ grant_id, node, gpu_uuid, alloc_id, expiry }` (the
/// `DatacenterLease` shape, plus `grant_id` for correlation). `effect_result`
/// journals `{ alloc_id, lease }` so replay re-emits without the allocator.
pub struct ResourceLeaseAcquireHandler {
    client: Arc<dyn AllocatorClient>,
    input_port: String,
    output_port: String,
}

impl ResourceLeaseAcquireHandler {
    pub fn new(
        client: Arc<dyn AllocatorClient>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for ResourceLeaseAcquireHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in resource_lease_acquire handler",
                self.input_port
            ))
        })?;

        // The full resolved effect_config (non-secret connection inline + the
        // unwrapped secret) — carries the per-flavor connection AND the
        // `(resource_id, resource_version)` cache keys the registry adapter
        // resolves the cluster on. The http/single-cluster path still works:
        // the default `acquire_with_connection` impl reads url/token/flavor off
        // it. A missing config is the hard error.
        let config = input.config.clone().ok_or_else(|| {
            EffectError::Fatal(
                "resource_lease handler requires effect_config { allocator_url, token } or a datacenter connection".into(),
            )
        })?;

        // grant_id is the compiler-minted replay-safe correlation key
        // (instance_id:node_id). It is ALSO the allocator idempotency key.
        let grant_id = token
            .get("grant_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EffectError::Fatal("missing grant_id in lease request".into()))?
            .to_string();

        // The claim params the workflow author passed (gpu_count, …). Absent →
        // null body (allocator default placement).
        let mut request_params = token.get("request").cloned().unwrap_or(JsonValue::Null);

        // Held-allocation-death routing (docs/16 §7). The slurm/nomad allocator
        // legs launch a persistent drain executor on the held alloc + must stamp
        // routing meta into the Slurm job comment / Nomad meta so the per-cluster
        // watcher routes the held alloc's TERMINAL signal to this adapter net's
        // `lease_failed` place (→ the loop's fail-fast path). The adapter net is
        // `pool-<resource_id>` (every lease-adapter net's id), and the failure
        // place is the well-known `lease_failed`. We derive that here from the
        // effect_config's `resource_id` correlation key and inject it into the
        // request the allocator receives under `failure_routing`, so the allocator
        // leg doesn't need to re-derive the net id. The http leg ignores it (no
        // persistent executor / watcher). Absent `resource_id` (legacy http) → no
        // routing injected.
        if let Some(rid) = config.get("resource_id").and_then(|v| v.as_str()) {
            if !rid.is_empty() && rid != "_env" {
                let failure_routing = serde_json::json!({
                    "petri_net_id": format!("pool-{rid}"),
                    "petri_place": "lease_failed",
                    "petri_signal_key": grant_id,
                    "petri_signal_failed": "lease_failed",
                });
                match &mut request_params {
                    JsonValue::Object(map) => {
                        map.insert("failure_routing".to_string(), failure_routing);
                    }
                    JsonValue::Null => {
                        request_params = serde_json::json!({ "failure_routing": failure_routing });
                    }
                    // A non-object, non-null request is an author error elsewhere;
                    // leave it untouched rather than clobber.
                    _ => {}
                }
            }
        }

        let lease = self
            .client
            .acquire_with_connection(&config, &grant_id, &request_params)
            .await
            .map_err(|e| EffectError::ExecutionFailed(format!("lease acquire failed: {e}")))?;

        // Typed lease for the body: the allocator's lease fields + grant_id.
        let node = lease.get("node").cloned().unwrap_or(JsonValue::Null);
        let gpu_uuid = lease.get("gpu_uuid").cloned().unwrap_or(JsonValue::Null);
        let alloc_id = lease
            .get("alloc_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::ExecutionFailed("allocator response missing alloc_id".into())
            })?
            .to_string();
        let expiry = lease.get("expiry").cloned().unwrap_or(JsonValue::Null);
        // The lease-scoped NATS namespace the persistent drain executor consumes
        // and the leased loop body enqueues to. The slurm/nomad allocator legs
        // emit it; the HTTP leg does not (no persistent executor), so default to
        // "" — empty-not-null keeps the required-String `Lease__datacenter`
        // schema valid on grant-inbox injection.
        let executor_namespace = lease
            .get("executor_namespace")
            .cloned()
            .unwrap_or(JsonValue::String(String::new()));

        let lease_token = serde_json::json!({
            "grant_id": grant_id,
            "node": node,
            "gpu_uuid": gpu_uuid,
            "alloc_id": alloc_id,
            "expiry": expiry,
            "executor_namespace": executor_namespace,
        });

        tracing::info!(
            grant_id = %grant_id,
            alloc_id = %alloc_id,
            "resource_lease_acquire: lease granted",
        );

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), lease_token.clone());

        Ok(EffectOutput {
            tokens,
            // Journaled so replay re-emits the lease without re-hitting the
            // allocator. The full lease is here for traceability.
            result: serde_json::json!({ "alloc_id": alloc_id, "lease": lease_token }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless: the lease lives entirely in the journaled produced token,
        // which the engine re-emits on replay. Nothing to rebuild here, and —
        // critically — the allocator is NOT called.
    }

    fn name(&self) -> &str {
        "resource_lease_acquire"
    }
}

// ---------------------------------------------------------------------------
// ResourceLeaseReleaseHandler
// ---------------------------------------------------------------------------

/// Releases a cluster lease.
///
/// Input port (`release`): `{ grant_id, alloc_id }`. Output port (`released`):
/// `{ grant_id }`. `effect_result` journals `{ alloc_id, released: true }`.
pub struct ResourceLeaseReleaseHandler {
    client: Arc<dyn AllocatorClient>,
    input_port: String,
    output_port: String,
}

impl ResourceLeaseReleaseHandler {
    pub fn new(
        client: Arc<dyn AllocatorClient>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for ResourceLeaseReleaseHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in resource_lease_release handler",
                self.input_port
            ))
        })?;

        let config = input.config.clone().ok_or_else(|| {
            EffectError::Fatal(
                "resource_lease handler requires effect_config { allocator_url, token } or a datacenter connection".into(),
            )
        })?;

        let grant_id = token
            .get("grant_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EffectError::Fatal("missing grant_id in release request".into()))?
            .to_string();
        let alloc_id = token
            .get("alloc_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EffectError::Fatal("missing alloc_id in release request".into()))?
            .to_string();

        self.client
            .release_with_connection(&config, &alloc_id)
            .await
            .map_err(|e| EffectError::ExecutionFailed(format!("lease release failed: {e}")))?;

        tracing::info!(
            grant_id = %grant_id,
            alloc_id = %alloc_id,
            "resource_lease_release: lease released",
        );

        let released_token = serde_json::json!({ "grant_id": grant_id });

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), released_token);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({ "alloc_id": alloc_id, "released": true }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless + the allocator is NOT called on replay.
    }

    fn name(&self) -> &str {
        "resource_lease_release"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::ExecutionMode;
    use petri_domain::TransitionId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock allocator that records call counts + the last acquire request, and
    /// returns a canned lease. No network — pure in-process double, so the test
    /// can assert "0 allocator calls on replay" deterministically.
    #[derive(Default)]
    struct MockAllocator {
        acquire_calls: AtomicUsize,
        release_calls: AtomicUsize,
        last_grant_id: std::sync::Mutex<Option<String>>,
        last_request: std::sync::Mutex<Option<JsonValue>>,
        last_release_alloc_id: std::sync::Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl AllocatorClient for MockAllocator {
        async fn acquire(
            &self,
            _allocator_url: &str,
            _token: &str,
            grant_id: &str,
            request: &JsonValue,
        ) -> Result<JsonValue, AllocatorError> {
            self.acquire_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_grant_id.lock().unwrap() = Some(grant_id.to_string());
            *self.last_request.lock().unwrap() = Some(request.clone());
            Ok(serde_json::json!({
                "node": "node-7",
                "gpu_uuid": "GPU-abc123",
                "alloc_id": "alloc-42",
                "expiry": "2026-01-01T00:00:00Z",
                "executor_namespace": "lease-instance-1-render"
            }))
        }

        async fn release(
            &self,
            _allocator_url: &str,
            _token: &str,
            alloc_id: &str,
        ) -> Result<(), AllocatorError> {
            self.release_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_release_alloc_id.lock().unwrap() = Some(alloc_id.to_string());
            Ok(())
        }
    }

    fn acquire_input() -> EffectInput {
        let mut inputs = HashMap::new();
        inputs.insert(
            "request".to_string(),
            serde_json::json!({
                "grant_id": "instance-1:render",
                "request": { "gpu_count": 1, "gpu_type": "a100" }
            }),
        );
        EffectInput {
            transition_id: TransitionId::named("t_acquire"),
            inputs,
            // Resolved config (firing.rs already substituted {{secret:...}}).
            config: Some(serde_json::json!({
                "allocator_url": "http://allocator.test/leases",
                "token": "sekret"
            })),
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[tokio::test]
    async fn acquire_emits_typed_lease_and_journals_alloc_id() {
        let alloc = Arc::new(MockAllocator::default());
        let handler = ResourceLeaseAcquireHandler::new(alloc.clone(), "request", "lease");

        let out = handler.execute(acquire_input()).await.unwrap();

        // Output token on "lease" is the typed lease + grant_id.
        let lease = out.tokens.get("lease").expect("lease token");
        assert_eq!(lease["grant_id"], "instance-1:render");
        assert_eq!(lease["node"], "node-7");
        assert_eq!(lease["gpu_uuid"], "GPU-abc123");
        assert_eq!(lease["alloc_id"], "alloc-42");
        assert_eq!(lease["expiry"], "2026-01-01T00:00:00Z");
        // The lease-scoped drain-executor namespace rides the typed lease token.
        assert_eq!(lease["executor_namespace"], "lease-instance-1-render");

        // effect_result journals alloc_id (for replay + traceability).
        assert_eq!(out.result["alloc_id"], "alloc-42");
        assert_eq!(out.result["lease"]["gpu_uuid"], "GPU-abc123");

        // The allocator was called exactly once, with grant_id as the
        // idempotency key and the author's request params passed through.
        assert_eq!(alloc.acquire_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            alloc.last_grant_id.lock().unwrap().as_deref(),
            Some("instance-1:render")
        );
        assert_eq!(
            alloc.last_request.lock().unwrap().as_ref().unwrap()["gpu_count"],
            1
        );
    }

    /// Held-allocation-death fail-fast routing (docs/16 §7): when the
    /// effect_config carries a real datacenter `resource_id`, the acquire handler
    /// injects `failure_routing` into the request the allocator receives, so the
    /// slurm/nomad leg can stamp it into the held alloc's job comment/meta and the
    /// watcher routes the terminal signal to the adapter net's `lease_failed`
    /// place (→ the loop's fail-fast abort). The net id is `pool-<resource_id>`.
    #[tokio::test]
    async fn acquire_injects_lease_failed_routing_for_datacenter() {
        let alloc = Arc::new(MockAllocator::default());
        let handler = ResourceLeaseAcquireHandler::new(alloc.clone(), "request", "lease");

        let mut input = acquire_input();
        input.config = Some(serde_json::json!({
            "scheduler_flavor": "slurm",
            "resource_id": "dc-abc",
            "resource_version": 2,
            "ssh_host": "login.test",
        }));
        handler.execute(input).await.unwrap();

        let req = alloc.last_request.lock().unwrap().clone().unwrap();
        let routing = &req["failure_routing"];
        assert_eq!(routing["petri_net_id"], "pool-dc-abc");
        assert_eq!(routing["petri_place"], "lease_failed");
        assert_eq!(routing["petri_signal_failed"], "lease_failed");
        assert_eq!(routing["petri_signal_key"], "instance-1:render");
        // The author's original request params survive alongside the routing.
        assert_eq!(req["gpu_count"], 1);
    }

    /// The legacy http leg (no `resource_id`, or the dev-bootstrap `_env`) gets
    /// NO failure_routing injected — there is no persistent executor/watcher to
    /// route a held-alloc death from.
    #[tokio::test]
    async fn acquire_omits_routing_for_http_and_env() {
        for rid in [None, Some("_env")] {
            let alloc = Arc::new(MockAllocator::default());
            let handler = ResourceLeaseAcquireHandler::new(alloc.clone(), "request", "lease");
            let mut input = acquire_input();
            let mut cfg = serde_json::json!({ "allocator_url": "http://a.test", "token": "t" });
            if let Some(rid) = rid {
                cfg["resource_id"] = serde_json::json!(rid);
            }
            input.config = Some(cfg);
            handler.execute(input).await.unwrap();
            let req = alloc.last_request.lock().unwrap().clone().unwrap();
            assert!(
                req.get("failure_routing").is_none(),
                "no failure_routing for rid={rid:?}, got {req}"
            );
        }
    }

    /// The load-bearing replay-safety assertion: on REPLAY the engine calls
    /// `replay()` (not `execute()`) and re-emits the journaled token — the
    /// allocator must receive ZERO calls. We model this directly: a live
    /// execute hits the allocator once; a subsequent replay (engine path) does
    /// NOT invoke execute, so the counter stays at 1.
    #[tokio::test]
    async fn replay_does_not_call_allocator() {
        let alloc = Arc::new(MockAllocator::default());
        let handler = ResourceLeaseAcquireHandler::new(alloc.clone(), "request", "lease");

        // Live fire → allocator called once, journaled result captured.
        let out = handler.execute(acquire_input()).await.unwrap();
        assert_eq!(alloc.acquire_calls.load(Ordering::SeqCst), 1);
        let stored_result = out.result.clone();

        // Replay: the engine (firing.rs ExecutionMode::Replay) re-emits the
        // stored produced_tokens and calls replay() — never execute(). We
        // invoke replay() exactly as the engine would.
        let _ = ExecutionMode::Replay; // documents which engine path this models
        handler.replay(&acquire_input(), &stored_result);

        // ZERO additional allocator calls on replay — the lease came from the
        // journal, not a fresh allocation.
        assert_eq!(
            alloc.acquire_calls.load(Ordering::SeqCst),
            1,
            "replay must NOT call the allocator"
        );
    }

    #[tokio::test]
    async fn release_calls_allocator_with_alloc_id_and_emits_grant_id() {
        let alloc = Arc::new(MockAllocator::default());
        let handler = ResourceLeaseReleaseHandler::new(alloc.clone(), "release", "released");

        let mut inputs = HashMap::new();
        inputs.insert(
            "release".to_string(),
            serde_json::json!({ "grant_id": "instance-1:render", "alloc_id": "alloc-42" }),
        );
        let input = EffectInput {
            transition_id: TransitionId::named("t_release"),
            inputs,
            config: Some(serde_json::json!({
                "allocator_url": "http://allocator.test/leases",
                "token": "sekret"
            })),
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let out = handler.execute(input).await.unwrap();

        assert_eq!(out.tokens.get("released").unwrap()["grant_id"], "instance-1:render");
        assert_eq!(out.result["alloc_id"], "alloc-42");
        assert_eq!(out.result["released"], true);

        assert_eq!(alloc.release_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            alloc.last_release_alloc_id.lock().unwrap().as_deref(),
            Some("alloc-42")
        );
    }

    /// Release replay also avoids the allocator.
    #[tokio::test]
    async fn release_replay_does_not_call_allocator() {
        let alloc = Arc::new(MockAllocator::default());
        let handler = ResourceLeaseReleaseHandler::new(alloc.clone(), "release", "released");

        let mut inputs = HashMap::new();
        inputs.insert(
            "release".to_string(),
            serde_json::json!({ "grant_id": "g", "alloc_id": "alloc-42" }),
        );
        let input = EffectInput {
            transition_id: TransitionId::named("t_release"),
            inputs,
            config: Some(serde_json::json!({ "allocator_url": "http://a.test", "token": "" })),
            read_inputs: HashMap::new(),
            process_step: None,
        };

        let out = handler.execute(input.clone()).await.unwrap();
        assert_eq!(alloc.release_calls.load(Ordering::SeqCst), 1);
        handler.replay(&input, &out.result);
        assert_eq!(
            alloc.release_calls.load(Ordering::SeqCst),
            1,
            "release replay must NOT call the allocator"
        );
    }

    /// `scheduler_flavor` defaults to `"http"` when absent — the historical
    /// path is byte-identical. Present, it parses through unchanged. This now
    /// exercises the connection-aware default seam (`acquire_with_connection`),
    /// which is the parser the handlers call.
    #[tokio::test]
    async fn connection_default_seam_defaults_flavor_to_http_leg() {
        // A flavor-unaware MockAllocator: the default `acquire_with_connection`
        // reads url/token/flavor off the config + delegates to `acquire`.
        let alloc = Arc::new(MockAllocator::default());
        let cfg = serde_json::json!({ "allocator_url": "http://a.test", "token": "t" });
        alloc
            .acquire_with_connection(&cfg, "g1", &serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(alloc.acquire_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            alloc.last_grant_id.lock().unwrap().as_deref(),
            Some("g1")
        );
    }

    /// The flavor-aware acquire delegates to the leaf `acquire` for the default
    /// (non-overriding) client, so a flavor-unaware MockAllocator still serves
    /// every flavor — the dispatcher (next phase) is the only overrider.
    #[tokio::test]
    async fn acquire_with_flavor_delegates_by_default() {
        let alloc = Arc::new(MockAllocator::default());
        let lease = alloc
            .acquire_with_flavor(
                "slurm",
                "http://a.test",
                "t",
                "g:1",
                &serde_json::json!({ "gpu_count": 1 }),
            )
            .await
            .unwrap();
        assert_eq!(lease["alloc_id"], "alloc-42");
        assert_eq!(alloc.acquire_calls.load(Ordering::SeqCst), 1);
    }

    /// Missing effect_config → Fatal (the datacenter connection must be wired).
    #[tokio::test]
    async fn acquire_without_config_is_fatal() {
        let alloc = Arc::new(MockAllocator::default());
        let handler = ResourceLeaseAcquireHandler::new(alloc.clone(), "request", "lease");
        let mut input = acquire_input();
        input.config = None;
        let err = handler.execute(input).await.unwrap_err();
        assert!(matches!(err, EffectError::Fatal(_)), "got {err:?}");
        assert_eq!(alloc.acquire_calls.load(Ordering::SeqCst), 0);
    }

    /// An HTTP-level mock proving the wire contract end to end through
    /// `HttpAllocatorClient` (POST → lease JSON; idempotency header sent).
    /// Hand-rolled TCP server (no `wiremock`/`hyper` dev-dep), mirroring
    /// `integration_tests.rs`'s HTTP echo pattern.
    #[tokio::test]
    async fn http_allocator_client_acquire_roundtrip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let captured_srv = captured.clone();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap();
            *captured_srv.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = r#"{"node":"n1","gpu_uuid":"GPU-x","alloc_id":"a99","expiry":"2026-02-02T00:00:00Z"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        });

        let client = HttpAllocatorClient::new();
        let lease = client
            .acquire(
                &format!("http://{addr}/leases"),
                "tok",
                "instance-9:gpu",
                &serde_json::json!({ "gpu_count": 2 }),
            )
            .await
            .expect("acquire roundtrip");

        server.await.unwrap();

        assert_eq!(lease["alloc_id"], "a99");
        assert_eq!(lease["gpu_uuid"], "GPU-x");

        // The request carried the bearer token + the grant_id idempotency key.
        let raw = captured.lock().unwrap().clone();
        assert!(raw.starts_with("POST "), "expected POST, got: {raw}");
        assert!(
            raw.contains("authorization: Bearer tok") || raw.contains("Authorization: Bearer tok"),
            "missing bearer auth header: {raw}"
        );
        assert!(
            raw.contains("idempotency-key: instance-9:gpu")
                || raw.contains("Idempotency-Key: instance-9:gpu"),
            "missing idempotency key header: {raw}"
        );
    }
}
