//! ExecutorNatsClient: submit and cancel execution jobs via NATS.
//!
//! Implements `ExecutorClient` from `petri-domain`. Builds `ExecutionJob` from
//! token data, wraps it in an apalis-nats compatible `NatsJob` envelope, and
//! publishes to the executor's per-priority JetStream stream.
//!
//! ## apalis-nats compatibility
//!
//! The real aithericon-executor uses apalis-nats as its job queue. apalis-nats
//! expects jobs in a `NatsJob<T>` envelope on subjects `{namespace}.{priority}`
//! (e.g. `executor_jobs.medium`) with WorkQueue-retained streams named
//! `{namespace}_{priority}` (e.g. `executor_jobs_medium`).
//!
//! This client idempotently ensures the target stream exists before publishing,
//! so it works regardless of whether the executor started first.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bytes::Bytes;
use chrono::Utc;

use aithericon_executor_domain::{ExecutionJob, ExecutionSpec, JobPriority, DEFAULT_WORKSPACE};
use petri_domain::executor::{
    ExecutionSubmitRequest, ExecutionSubmitResult, ExecutorClient, ExecutorError,
};
use petri_scheduler_bridge::RoutingMeta;

// ---------------------------------------------------------------------------
// apalis-nats envelope types (mirrored, not imported, to avoid dep on apalis)
// ---------------------------------------------------------------------------

/// Mirrors apalis-nats `NatsJob<T>` — the wire format the executor expects.
#[derive(serde::Serialize)]
struct NatsJobEnvelope<'a, T: serde::Serialize> {
    id: String,
    data: &'a T,
    priority: &'static str,
    attempts: u64,
    created_at: chrono::DateTime<Utc>,
    namespace: &'a str,
}

/// apalis-nats priority level.
///
/// Default is `Medium`. Maps to stream name suffix and subject segment.
#[derive(Debug, Clone, Copy, Default)]
pub enum ApalisPriority {
    High,
    #[default]
    Medium,
    Low,
}

impl ApalisPriority {
    /// The lowercase string used in stream names and subjects.
    fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// The PascalCase variant name used in JSON serialization by apalis-nats.
    fn as_json_str(self) -> &'static str {
        match self {
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
        }
    }
}

/// apalis-nats JetStream stream name for a `{namespace}` / `{priority}` pair
/// (e.g. `executor_jobs_medium`, `lease-abc_medium`).
fn stream_name_for(namespace: &str, priority: ApalisPriority) -> String {
    format!("{}_{}", namespace, priority.as_str())
}

/// Split a stamped executor namespace into `(stream_namespace, Option<partition>)`.
///
/// A `runner-jobs/{runner_id}` form (mekhan's presence-controller stamp for a
/// lab-runner-fleet grant) routes to the SHARED `runner-jobs` stream but a
/// per-runner subject partition, so an unbounded runner fleet shares ONE
/// stream-set (`runner-jobs_{priority}`) instead of one per runner. A namespace
/// without `/` — worker pool (`executor-<wire>`), lease (`lease-<grant>`), or
/// the daemon default (`executor_jobs`) — is unpartitioned and routes exactly
/// as before. The `/` is purely a stamping delimiter; it never reaches a NATS
/// subject or stream name (those use the split-off `stream_namespace`).
fn split_namespace(namespace: &str) -> (&str, Option<&str>) {
    match namespace.split_once('/') {
        Some((stream_ns, partition)) => (stream_ns, Some(partition)),
        None => (namespace, None),
    }
}

/// Per-job publish subject. Unpartitioned: `{stream_ns}.{priority}.{execution_id}`
/// (lease/daemon/worker-pool, byte-identical to before). Partitioned:
/// `{stream_ns}.{priority}.{partition}.{execution_id}` — the partition segment
/// (a runner id) precedes the job suffix so the runner's `PartitionedPool`
/// consumer filter `{stream_ns}.{priority}.{partition}.>` drains exactly it.
fn subject_for(
    stream_ns: &str,
    priority: ApalisPriority,
    partition: Option<&str>,
    execution_id: &str,
) -> String {
    match partition {
        Some(p) => format!("{}.{}.{}.{}", stream_ns, priority.as_str(), p, execution_id),
        None => format!("{}.{}.{}", stream_ns, priority.as_str(), execution_id),
    }
}

// ---------------------------------------------------------------------------
// ExecutorNatsClient
// ---------------------------------------------------------------------------

/// NATS-based executor client.
///
/// Submits `ExecutionJob` to the executor's apalis-nats job stream and cancels
/// running executions via the `EXECUTOR_CANCEL` JetStream stream.
pub struct ExecutorNatsClient {
    /// Core NATS client. Retained on the constructor's public shape (and for any
    /// future core-NATS publish); cancel now rides JetStream (`self.jetstream`)
    /// because core interest doesn't reach WebSocket-front-door runners.
    #[allow(dead_code)]
    nats_client: async_nats::Client,
    jetstream: async_nats::jetstream::Context,
    net_id: String,
    fallback_place: String,
    signal_routes: HashMap<String, String>,
    event_routes: HashMap<String, String>,
    namespace: String,
    /// SHARED per-net workspace cell. The executor effect handler is registered
    /// (and this client constructed) BEFORE the scenario load stamps the net's
    /// workspace, so the workspace cannot be a plain field set at construction —
    /// it is read LAZILY at submit time from the same `Arc<RwLock<Option<String>>>`
    /// that `set_workspace_id` writes (mirrors the timer handler's
    /// `with_workspace_cell`). `None` when not wired (SDK/test paths) → falls back
    /// to [`DEFAULT_WORKSPACE`]. The resolved workspace is stamped onto
    /// `ExecutionJob.workspace_id` so the executor's status/event back-channel is
    /// tenant-attributable.
    workspace_cell: Option<Arc<RwLock<Option<String>>>>,
    /// Names of streams ensured this session, keyed by `{ns}_{prio}`. A leased
    /// body targets a per-job namespace (`lease-<grant_id>`) whose stream
    /// differs from the fixed-namespace daemon path, so the ensure cache is
    /// keyed per stream name rather than a single session-wide bool.
    streams_ensured: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Secret store for resolving `{{secret:KEY}}` refs before wrapping.
    #[cfg(feature = "vault-secrets")]
    secret_store: Option<Arc<dyn aithericon_secrets::SecretStore>>,
    /// Wrapper for creating single-use Vault wrapping tokens.
    #[cfg(feature = "vault-secrets")]
    secret_wrapper: Option<Arc<dyn aithericon_secrets::SecretWrapper>>,
    /// TTL for wrapping tokens in seconds (default: 600 = 10 minutes).
    #[cfg(feature = "vault-secrets")]
    wrap_ttl_secs: u64,
}

impl ExecutorNatsClient {
    /// Create a new executor client.
    ///
    /// # Arguments
    /// * `nats_client` - NATS client for ephemeral publishes (cancel)
    /// * `jetstream` - JetStream context for durable publishes (submit)
    /// * `net_id` - Petri net ID for routing metadata
    /// * `fallback_place` - Default signal place name
    /// * `signal_routes` - Per-status signal routes (status -> place)
    /// * `event_routes` - Per-category event routes (category -> place)
    /// * `namespace` - apalis-nats job namespace
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        nats_client: async_nats::Client,
        jetstream: async_nats::jetstream::Context,
        net_id: &str,
        fallback_place: &str,
        signal_routes: HashMap<String, String>,
        event_routes: HashMap<String, String>,
        namespace: &str,
    ) -> Self {
        Self {
            nats_client,
            jetstream,
            net_id: net_id.to_string(),
            fallback_place: fallback_place.to_string(),
            signal_routes,
            event_routes,
            namespace: namespace.to_string(),
            workspace_cell: None,
            streams_ensured: std::sync::Mutex::new(std::collections::HashSet::new()),
            #[cfg(feature = "vault-secrets")]
            secret_store: None,
            #[cfg(feature = "vault-secrets")]
            secret_wrapper: None,
            #[cfg(feature = "vault-secrets")]
            wrap_ttl_secs: 600,
        }
    }

    /// Set the secret store and wrapper for Vault response wrapping.
    ///
    /// When both are set, `submit()` will:
    /// 1. Scan `spec.config` for `{{secret:KEY}}` refs
    /// 2. Resolve the keys via the store
    /// 3. Wrap the resolved values into a single-use Vault wrapping token
    /// 4. Attach the token to the job (refs stay unresolved in the spec)
    #[cfg(feature = "vault-secrets")]
    pub fn set_secret_wrapping(
        &mut self,
        store: Arc<dyn aithericon_secrets::SecretStore>,
        wrapper: Arc<dyn aithericon_secrets::SecretWrapper>,
    ) {
        self.secret_store = Some(store);
        self.secret_wrapper = Some(wrapper);
    }

    /// Set the TTL for wrapping tokens (default: 600 seconds / 10 minutes).
    #[cfg(feature = "vault-secrets")]
    pub fn set_wrap_ttl(&mut self, ttl_secs: u64) {
        self.wrap_ttl_secs = ttl_secs;
    }

    /// Wire the SHARED per-net workspace cell so submit() stamps the firing net's
    /// tenant onto `ExecutionJob.workspace_id`. The registry calls this at
    /// handler registration with `service.workspace_cell()` — the same cell
    /// `set_workspace_id` writes at scenario load, read lazily here.
    pub fn with_workspace_cell(mut self, cell: Arc<RwLock<Option<String>>>) -> Self {
        self.workspace_cell = Some(cell);
        self
    }

    /// Resolve the effective workspace at submit time. Reads the shared cell
    /// (written by `set_workspace_id`); falls back to the [`DEFAULT_WORKSPACE`]
    /// sentinel when the cell is absent (SDK/test paths) or unstamped. The
    /// sentinel agrees byte-for-byte with the worker's empty-`workspace_id`
    /// fall-back, so an unstamped job and an empty-`workspace_id` job land on the
    /// same `executor.status.default.>` back-channel.
    fn workspace_or_default(&self) -> String {
        self.workspace_cell
            .as_ref()
            .and_then(|c| c.read().ok().and_then(|g| g.clone()))
            .filter(|ws| !ws.is_empty())
            .unwrap_or_else(|| DEFAULT_WORKSPACE.to_string())
    }

    /// Build routing metadata to stamp into the execution job.
    ///
    /// Per-request overrides (from `effect_config`) take precedence over the
    /// global routes configured at client construction. This supports scoped
    /// place names when the executor lifecycle runs inside `scoped_prefix`.
    fn build_routing_meta(
        &self,
        signal_key: &str,
        signal_overrides: Option<&HashMap<String, String>>,
        event_overrides: Option<&HashMap<String, String>>,
    ) -> RoutingMeta {
        RoutingMeta {
            net_id: self.net_id.clone(),
            fallback_place: self.fallback_place.clone(),
            signal_routes: signal_overrides
                .cloned()
                .unwrap_or_else(|| self.signal_routes.clone()),
            event_routes: event_overrides
                .cloned()
                .unwrap_or_else(|| self.event_routes.clone()),
            signal_key: signal_key.to_string(),
        }
    }

    /// Build an `ExecutionJob` from the submit request token data.
    ///
    /// Expects `token_data` to contain an `ExecutionSpec`-compatible JSON object.
    /// If a `spec` key is present, it is used directly; otherwise the entire
    /// token_data is tried as the spec.
    fn build_execution_job(
        &self,
        request: &ExecutionSubmitRequest,
        routing_meta: &HashMap<String, String>,
    ) -> Result<ExecutionJob, ExecutorError> {
        // Honour an upstream-stamped execution_id when present so the executor
        // dispatch (e.g. SlurmClient's `EXECUTOR_TARGET_EXEC_ID`) and this
        // publish agree on the subject suffix. Otherwise fall back to the
        // legacy auto-generation.
        let execution_id = request
            .execution_id
            .clone()
            .unwrap_or_else(|| format!("{}-{}", self.net_id, uuid::Uuid::new_v4()));

        let spec_value = if request.token_data.get("spec").is_some() {
            request.token_data["spec"].clone()
        } else {
            request.token_data.clone()
        };

        let spec: ExecutionSpec = serde_json::from_value(spec_value).map_err(|e| {
            ExecutorError::Fatal(format!("Failed to deserialize ExecutionSpec: {}", e))
        })?;

        let timeout = request
            .token_data
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .map(Duration::from_secs);

        let priority = request
            .token_data
            .get("priority")
            .and_then(|v| serde_json::from_value::<JobPriority>(v.clone()).ok())
            .unwrap_or_default();

        // stream_events can be at the top level of token_data or nested inside spec.
        // The spec-nested form is preferred because spec flows through multi-layer
        // bridged nets without requiring every intermediate token type to carry it.
        let stream_events = request
            .token_data
            .get("stream_events")
            .or_else(|| {
                request
                    .token_data
                    .get("spec")
                    .and_then(|s| s.get("stream_events"))
            })
            .and_then(|v| {
                serde_json::from_value::<Vec<aithericon_executor_domain::event::EventCategory>>(
                    v.clone(),
                )
                .ok()
            });

        // The compiler bakes the streaming-channels manifest (docs/25) onto the
        // job token (top level or nested in spec, mirroring `stream_events`). The
        // executor uses it to validate `EmitControl` calls against declared `out`
        // channels. Absent → no channels (empty).
        let channels = request
            .token_data
            .get("channels")
            .or_else(|| {
                request
                    .token_data
                    .get("spec")
                    .and_then(|s| s.get("channels"))
            })
            .and_then(|v| {
                serde_json::from_value::<Vec<aithericon_executor_domain::ChannelManifestEntry>>(
                    v.clone(),
                )
                .ok()
            })
            .unwrap_or_default();

        Ok(ExecutionJob {
            execution_id,
            // Stamp the firing net's workspace (tenant) so the executor's
            // status/event back-channel subjects carry the `{ws}` segment. The
            // PUBLISH subject is unchanged — dispatch isolation (the runner-jobs
            // partition) already routes the job; `workspace_id` is purely for
            // back-channel attribution. Falls back to DEFAULT_WORKSPACE when the
            // net's workspace is unstamped (SDK/test) → `executor.status.default.>`.
            workspace_id: self.workspace_or_default(),
            spec,
            metadata: routing_meta.clone(),
            timeout,
            priority,
            stream_events,
            channels,
            feed_chunks: request.feed_chunks,
            wrapped_secrets: None,
        })
    }

    /// Idempotently ensure the apalis-nats stream for the given priority and
    /// namespace exists.
    ///
    /// Uses `get_or_create_stream` so it's a no-op if the executor already
    /// created it. The config matches what apalis-nats produces. The ensure
    /// cache is keyed by stream name so a per-job namespace (lease-scoped) is
    /// ensured independently of the fixed-namespace daemon path.
    async fn ensure_stream(
        &self,
        priority: ApalisPriority,
        namespace: &str,
    ) -> Result<(), ExecutorError> {
        let stream_name = stream_name_for(namespace, priority);

        if self
            .streams_ensured
            .lock()
            .map(|set| set.contains(&stream_name))
            .unwrap_or(false)
        {
            return Ok(());
        }

        // Wildcard subject filter — publishers route by `{ns}.{prio}.{exec_id}`
        // so the stream accepts both daemon-mode wildcard pulls and one-shot
        // per-exec consumers.
        let subjects = vec![format!("{}.{}.>", namespace, priority.as_str())];

        self.jetstream
            .get_or_create_stream(async_nats::jetstream::stream::Config {
                name: stream_name.clone(),
                subjects,
                retention: async_nats::jetstream::stream::RetentionPolicy::WorkQueue,
                storage: async_nats::jetstream::stream::StorageType::File,
                max_age: Duration::from_secs(7 * 24 * 60 * 60), // 7 days
                duplicate_window: Duration::from_secs(120),
                discard: async_nats::jetstream::stream::DiscardPolicy::Old,
                ..Default::default()
            })
            .await
            .map_err(|e| {
                ExecutorError::SubmissionFailed(format!(
                    "Failed to ensure stream '{}': {}",
                    stream_name, e
                ))
            })?;

        if let Ok(mut set) = self.streams_ensured.lock() {
            set.insert(stream_name);
        }
        Ok(())
    }

    /// Idempotently ensure the `EXECUTOR_CANCEL` JetStream stream exists.
    ///
    /// Cancels ride JetStream (not core NATS) so the signal reaches runners
    /// connected over the WebSocket front door — core pub/sub interest never
    /// propagated across that boundary, so `executor.cancel.*` core publishes
    /// were silently dropped. `Limits` retention (every runner reads the same
    /// cancel) + short max-age (transient signal). Cached per stream name.
    async fn ensure_cancel_stream(&self) -> Result<(), ExecutorError> {
        let stream_name = aithericon_executor_domain::cancel_stream_name(None);

        if self
            .streams_ensured
            .lock()
            .map(|set| set.contains(&stream_name))
            .unwrap_or(false)
        {
            return Ok(());
        }

        self.jetstream
            .get_or_create_stream(async_nats::jetstream::stream::Config {
                name: stream_name.clone(),
                subjects: vec![aithericon_executor_domain::cancel_subject_filter(None)],
                retention: async_nats::jetstream::stream::RetentionPolicy::Limits,
                storage: async_nats::jetstream::stream::StorageType::File,
                max_age: Duration::from_secs(
                    aithericon_executor_domain::CANCEL_STREAM_MAX_AGE_SECS,
                ),
                discard: async_nats::jetstream::stream::DiscardPolicy::Old,
                ..Default::default()
            })
            .await
            .map_err(|e| {
                ExecutorError::SubmissionFailed(format!(
                    "Failed to ensure stream '{stream_name}': {e}"
                ))
            })?;

        if let Ok(mut set) = self.streams_ensured.lock() {
            set.insert(stream_name);
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl ExecutorClient for ExecutorNatsClient {
    async fn submit(
        &self,
        request: ExecutionSubmitRequest,
    ) -> Result<ExecutionSubmitResult, ExecutorError> {
        let priority = ApalisPriority::default(); // Medium

        // Prefer a per-job namespace (a leased body targets `lease-<grant_id>`
        // drained by a persistent executor); fall back to the construction-time
        // fixed namespace for the daemon path.
        let ns = request.namespace.as_deref().unwrap_or(&self.namespace);

        // A `runner-jobs/{runner_id}` namespace routes to the SHARED `runner-jobs`
        // stream but a per-runner subject partition; unpartitioned namespaces
        // (lease/daemon/worker-pool) route exactly as before. The stream is keyed
        // by `stream_ns` only — many partitions share one stream-set.
        let (stream_ns, partition) = split_namespace(ns);

        // Ensure the target stream exists (idempotent, cached per stream name).
        self.ensure_stream(priority, stream_ns).await?;

        let routing = self.build_routing_meta(
            &request.signal_key,
            request.signal_routes.as_ref(),
            request.event_routes.as_ref(),
        );
        let meta_tags = routing.to_meta_tags();
        #[allow(unused_mut)]
        let mut job = self.build_execution_job(&request, &meta_tags)?;
        let execution_id = job.execution_id.clone();

        // If secret wrapping is configured, scan spec for {{secret:KEY}} refs,
        // resolve them, and wrap into a single-use Vault wrapping token.
        // The refs stay unresolved in the spec — only the wrapping token travels on NATS.
        #[cfg(feature = "vault-secrets")]
        if let (Some(store), Some(wrapper)) = (&self.secret_store, &self.secret_wrapper) {
            let spec_json = serde_json::to_value(&job.spec).map_err(|e| {
                ExecutorError::Fatal(format!("Failed to serialize spec for secret scanning: {e}"))
            })?;
            let keys = aithericon_secrets::extract_secret_keys(&spec_json);
            if !keys.is_empty() {
                let mut resolved = HashMap::new();
                for key in &keys {
                    let value = store.get(key).await.map_err(|e| {
                        ExecutorError::Fatal(format!("Failed to resolve secret '{key}': {e}"))
                    })?;
                    resolved.insert(key.clone(), value);
                }
                let wrapping_token = wrapper
                    .wrap(resolved, self.wrap_ttl_secs)
                    .await
                    .map_err(|e| ExecutorError::Fatal(format!("Failed to wrap secrets: {e}")))?;
                job.wrapped_secrets = Some(wrapping_token);
                tracing::debug!(
                    execution_id = %execution_id,
                    secret_count = keys.len(),
                    "Wrapped secrets into Vault wrapping token"
                );
            }
        }

        // Wrap in apalis-nats NatsJob envelope.
        let task_id = ulid::Ulid::new().to_string();
        let envelope = NatsJobEnvelope {
            id: task_id.clone(),
            data: &job,
            priority: priority.as_json_str(),
            attempts: 0,
            created_at: Utc::now(),
            namespace: stream_ns,
        };

        let payload = serde_json::to_vec(&envelope).map_err(|e| {
            ExecutorError::Fatal(format!("Failed to serialize NatsJob envelope: {}", e))
        })?;

        // Per-job subject (partition-aware). Pool consumers (daemon mode) match
        // via `{ns}.{prio}.>`; PerJob consumers (one-shot sbatch) exact-match
        // their exec_id; PartitionedPool consumers (lab runners) match
        // `{ns}.{prio}.{partition}.>`.
        let subject = subject_for(stream_ns, priority, partition, &execution_id);

        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", execution_id.as_str());

        self.jetstream
            .publish_with_headers(subject.clone(), headers, Bytes::from(payload))
            .await
            .map_err(|e| ExecutorError::SubmissionFailed(format!("NATS publish failed: {}", e)))?
            .await
            .map_err(|e| {
                ExecutorError::SubmissionFailed(format!("NATS publish ack failed: {}", e))
            })?;

        tracing::info!(
            execution_id = %execution_id,
            subject = %subject,
            task_id = %task_id,
            signal_key = %request.signal_key,
            "Submitted execution job to executor"
        );

        Ok(ExecutionSubmitResult { execution_id })
    }

    async fn cancel(&self, execution_id: &str) -> Result<(), ExecutorError> {
        // Cancel rides the EXECUTOR_CANCEL JetStream stream, NOT core NATS:
        // core pub/sub interest does not propagate from this internal connection
        // to a runner on the WebSocket front door, so a core publish never
        // reached the runner. JetStream delivery is interest-free.
        self.ensure_cancel_stream().await?;

        let subject = aithericon_executor_domain::cancel_subject(execution_id);

        self.jetstream
            .publish(subject.clone(), Bytes::new())
            .await
            .map_err(|e| ExecutorError::CancellationFailed(format!("NATS publish failed: {e}")))?
            .await
            .map_err(|e| {
                ExecutorError::CancellationFailed(format!("NATS publish ack failed: {e}"))
            })?;

        tracing::info!(
            execution_id = %execution_id,
            subject = %subject,
            "Published cancellation request (JetStream)"
        );

        Ok(())
    }

    fn name(&self) -> &str {
        "executor-nats"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_routing_meta_roundtrip() {
        let routing = RoutingMeta {
            net_id: "exec-net".into(),
            fallback_place: "sig_inbox".into(),
            signal_routes: HashMap::from([("running".into(), "sig_running".into())]),
            event_routes: HashMap::from([("progress".into(), "sig_progress".into())]),
            signal_key: "job-1:0".into(),
        };

        let tags = routing.to_meta_tags();
        let restored = RoutingMeta::from_meta_tags(&tags).unwrap();
        assert_eq!(restored.net_id, "exec-net");
        assert_eq!(restored.place_for_status("running"), "sig_running");
        assert_eq!(restored.place_for_event("progress"), Some("sig_progress"));
    }

    #[test]
    fn test_execution_spec_deserialization() {
        let spec_json = serde_json::json!({
            "backend": "process",
            "inputs": [],
            "outputs": [],
            "config": {
                "command": "python3",
                "args": ["train.py"]
            }
        });

        let spec: ExecutionSpec = serde_json::from_value(spec_json).unwrap();
        assert_eq!(spec.backend, "process");
        assert_eq!(spec.config["command"], "python3");
    }

    #[test]
    fn test_nats_job_envelope_serialization() {
        let job = ExecutionJob {
            execution_id: "test-exec-1".into(),
            workspace_id: "ws-acme".into(),
            spec: serde_json::from_value(serde_json::json!({
                "backend": "process",
                "config": { "command": "echo" }
            }))
            .unwrap(),
            metadata: HashMap::new(),
            timeout: None,
            priority: JobPriority::default(),
            stream_events: None,
            channels: Vec::new(),
            wrapped_secrets: None,
            feed_chunks: false,
        };

        let envelope = NatsJobEnvelope {
            id: "01HXYZ".into(),
            data: &job,
            priority: ApalisPriority::Medium.as_json_str(),
            attempts: 0,
            created_at: Utc::now(),
            namespace: "executor_jobs",
        };

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["priority"], "Medium");
        assert_eq!(json["namespace"], "executor_jobs");
        assert_eq!(json["attempts"], 0);
        assert_eq!(json["data"]["execution_id"], "test-exec-1");
        // workspace_id rides inside the `data` (serialized job) envelope.
        assert_eq!(json["data"]["workspace_id"], "ws-acme");
    }

    #[test]
    fn test_per_job_namespace_routes_subject_and_stream() {
        // A leased body stamps `request.namespace = Some("lease-x")` — the
        // client must publish to the lease-scoped queue, not its fixed default.
        let fixed = "executor_jobs";
        let req_ns = Some("lease-x".to_string());

        // Mirror submit()'s resolution: prefer the per-job ns, else fixed.
        let ns = req_ns.as_deref().unwrap_or(fixed);
        assert_eq!(ns, "lease-x");
        let (stream_ns, partition) = split_namespace(ns);
        assert_eq!((stream_ns, partition), ("lease-x", None));
        assert_eq!(
            subject_for(stream_ns, ApalisPriority::Medium, partition, "exec-42"),
            "lease-x.medium.exec-42"
        );
        assert_eq!(
            stream_name_for(stream_ns, ApalisPriority::Medium),
            "lease-x_medium"
        );

        // Absent per-job ns → byte-identical to the fixed-namespace daemon path.
        let none_ns: Option<String> = None;
        let ns = none_ns.as_deref().unwrap_or(fixed);
        assert_eq!(ns, "executor_jobs");
        let (stream_ns, partition) = split_namespace(ns);
        assert_eq!((stream_ns, partition), ("executor_jobs", None));
        assert_eq!(
            subject_for(stream_ns, ApalisPriority::Medium, partition, "exec-42"),
            "executor_jobs.medium.exec-42"
        );
        assert_eq!(
            stream_name_for(stream_ns, ApalisPriority::Medium),
            "executor_jobs_medium"
        );
    }

    #[test]
    fn test_runner_partition_namespace_routes_shared_stream() {
        // A presence-pool grant stamps `request.namespace =
        // Some("runner-jobs/{runner_id}")`: the SHARED `runner-jobs` stream, a
        // per-runner subject partition.
        let ns = "runner-jobs/8327fc93";
        let (stream_ns, partition) = split_namespace(ns);
        assert_eq!((stream_ns, partition), ("runner-jobs", Some("8327fc93")));
        // Shared stream key — one stream-set for the whole fleet.
        assert_eq!(
            stream_name_for(stream_ns, ApalisPriority::Medium),
            "runner-jobs_medium"
        );
        // Partitioned subject — the runner's PartitionedPool filter
        // (`runner-jobs.medium.8327fc93.>`) drains exactly this.
        assert_eq!(
            subject_for(stream_ns, ApalisPriority::Medium, partition, "exec-42"),
            "runner-jobs.medium.8327fc93.exec-42"
        );
    }
}
