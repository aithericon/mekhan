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
#[cfg(feature = "vault-secrets")]
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::Utc;

use aithericon_executor_domain::{ExecutionJob, ExecutionSpec, JobPriority};
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

/// Per-job publish subject `{namespace}.{priority}.{execution_id}`. A per-job
/// namespace (`lease-<grant_id>`) targets the lease-scoped queue drained by a
/// persistent executor; the fixed daemon namespace is byte-identical.
fn subject_for(namespace: &str, priority: ApalisPriority, execution_id: &str) -> String {
    format!("{}.{}.{}", namespace, priority.as_str(), execution_id)
}

// ---------------------------------------------------------------------------
// ExecutorNatsClient
// ---------------------------------------------------------------------------

/// NATS-based executor client.
///
/// Submits `ExecutionJob` to the executor's apalis-nats job stream and
/// cancels running executions via ephemeral NATS publish.
pub struct ExecutorNatsClient {
    nats_client: async_nats::Client,
    jetstream: async_nats::jetstream::Context,
    net_id: String,
    fallback_place: String,
    signal_routes: HashMap<String, String>,
    event_routes: HashMap<String, String>,
    namespace: String,
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
Ok(ExecutionJob {
    execution_id,
    spec,
    metadata: routing_meta.clone(),
    timeout,
    priority,
    stream_events,
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

        // Ensure the target stream exists (idempotent, cached per stream name).
        self.ensure_stream(priority, ns).await?;

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
            namespace: ns,
        };

        let payload = serde_json::to_vec(&envelope).map_err(|e| {
            ExecutorError::Fatal(format!("Failed to serialize NatsJob envelope: {}", e))
        })?;

        // Per-job subject: `{namespace}.{priority}.{execution_id}`. Pool
        // consumers (daemon mode) match via `{ns}.{prio}.>` wildcard; PerJob
        // consumers (one-shot sbatch) exact-match their assigned exec_id.
        let subject = subject_for(ns, priority, &execution_id);

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
        // Cancel uses ephemeral core NATS (not JetStream) — fire-and-forget.
        let subject = aithericon_executor_domain::cancel_subject(execution_id);

        self.nats_client
            .publish(subject.clone(), Bytes::new())
            .await
            .map_err(|e| {
                ExecutorError::CancellationFailed(format!("NATS publish failed: {}", e))
            })?;

        tracing::info!(
            execution_id = %execution_id,
            subject = %subject,
            "Published cancellation request"
        );

        Ok(())
    }

    async fn feed_chunk(
        &self,
        execution_id: &str,
        value: serde_json::Value,
        sequence: u64,
        is_eof: bool,
    ) -> Result<(), ExecutorError> {
        // Chunks are published to `executor.chunks.{execution_id}`.
        let subject = format!("executor.chunks.{}", execution_id);

        let payload = serde_json::json!({
            "value_json": serde_json::to_string(&value).unwrap_or_default(),
            "sequence": sequence,
            "is_eof": is_eof,
        });

        let payload_vec = serde_json::to_vec(&payload).map_err(|e| {
            ExecutorError::Fatal(format!("Failed to serialize chunk: {}", e))
        })?;

        let mut headers = async_nats::HeaderMap::new();
        // Use the same Nats-Msg-Id format as the listener's dedup window.
        headers.insert("Nats-Msg-Id", format!("{}-{}", execution_id, sequence));

        self.jetstream
            .publish_with_headers(subject, headers, Bytes::from(payload_vec))
            .await
            .map_err(|e| ExecutorError::SubmissionFailed(format!("NATS chunk publish failed: {}", e)))?
            .await
            .map_err(|e| {
                ExecutorError::SubmissionFailed(format!("NATS chunk publish ack failed: {}", e))
            })?;

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
            spec: serde_json::from_value(serde_json::json!({
                "backend": "process",
                "config": { "command": "echo" }
            }))
            .unwrap(),
            metadata: HashMap::new(),
            timeout: None,
            priority: JobPriority::default(),
            stream_events: None,
            wrapped_secrets: None,
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
        assert_eq!(
            subject_for(ns, ApalisPriority::Medium, "exec-42"),
            "lease-x.medium.exec-42"
        );
        assert_eq!(
            stream_name_for(ns, ApalisPriority::Medium),
            "lease-x_medium"
        );

        // Absent per-job ns → byte-identical to the fixed-namespace daemon path.
        let none_ns: Option<String> = None;
        let ns = none_ns.as_deref().unwrap_or(fixed);
        assert_eq!(ns, "executor_jobs");
        assert_eq!(
            subject_for(ns, ApalisPriority::Medium, "exec-42"),
            "executor_jobs.medium.exec-42"
        );
        assert_eq!(
            stream_name_for(ns, ApalisPriority::Medium),
            "executor_jobs_medium"
        );
    }
}
