use apalis_core::backend::Backend;
use apalis_core::codec::json::JsonCodec;
use apalis_core::codec::Codec;
use apalis_core::error::Error;
use apalis_core::layers::{Ack, AckLayer};
use apalis_core::poller::Poller;
use apalis_core::request::{Parts, Request};
use apalis_core::response::Response;
use apalis_core::storage::Storage;
use apalis_core::task::attempt::Attempt;
use apalis_core::task::namespace::Namespace;
use apalis_core::task::task_id::TaskId;
use apalis_core::worker::{Context as WorkerContext, Worker};
use async_nats::jetstream::{self, consumer, stream};
use async_nats::{Client, ConnectError, HeaderMap};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::channel::mpsc::{self, Sender};
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[cfg(feature = "otel")]
use crate::otel::{NatsHeaderExtractor, NatsHeaderInjector};
#[cfg(feature = "otel")]
use opentelemetry::trace::{Span as OtelSpan, SpanKind, Status, TraceContextExt, Tracer};
#[cfg(feature = "otel")]
use opentelemetry::{global, Context as OtelContext, KeyValue};
#[cfg(feature = "otel")]
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Priority levels for jobs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum Priority {
    /// High priority jobs
    High,
    /// Medium priority jobs
    Medium,
    /// Low priority jobs
    Low,
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Priority::High => write!(f, "high"),
            Priority::Medium => write!(f, "medium"),
            Priority::Low => write!(f, "low"),
        }
    }
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Medium
    }
}

/// Configuration for NATS storage
#[derive(Debug, Clone)]
pub struct Config {
    /// The namespace for all streams (e.g., "apalis")
    pub namespace: String,
    /// Maximum number of delivery attempts before moving to DLQ
    pub max_deliver: i64,
    /// Ack wait time (how long to wait for a job to be acknowledged)
    pub ack_wait: Duration,
    /// Number of replicas for streams
    pub num_replicas: usize,
    /// Enable dead letter queue
    pub enable_dlq: bool,
    /// Maximum number of pending acknowledgments per consumer
    pub max_ack_pending: i64,
    /// Backoff schedule for transient failures (Nak delays by attempt index)
    /// If shorter than delivered attempts, the last value is used for subsequent attempts.
    pub nak_backoff: Vec<Duration>,
    /// Consumer behavior — Pool (durable shared) or PerJob (ephemeral, exact filter).
    pub consumer_mode: ConsumerMode,
    /// Enable OpenTelemetry tracing
    #[cfg(feature = "otel")]
    pub enable_tracing: bool,
}

/// How a worker connects to the JetStream stream.
///
/// Subject hierarchy is always `{namespace}.{priority}.{job_suffix}` regardless of
/// mode — Pool wildcards over the suffix, PerJob exact-matches a single suffix.
#[derive(Debug, Clone)]
pub enum ConsumerMode {
    /// Durable shared consumer named `{namespace}_{priority}_consumer`,
    /// filter `{namespace}.{priority}.>`. Multiple workers compete via
    /// round-robin. Suits long-lived deployments (k8s, docker daemon).
    Pool,
    /// Ephemeral consumer (no durable_name) with exact subject filter
    /// `{namespace}.{priority}.{exec_id}`. Auto-deletes on disconnect/ack.
    /// Suits one-shot dispatchers (sbatch, k8s Jobs, lambda) where each
    /// worker is bound to a specific dispatched job.
    PerJob {
        /// The execution id to filter on. The publisher must publish to
        /// `{namespace}.{priority}.{exec_id}` for this consumer to receive.
        exec_id: String,
    },
    /// Durable consumer SHARED-STREAM but PARTITION-scoped: it binds the same
    /// per-priority streams as `Pool` (`{namespace}_{priority}`) yet filters to
    /// one partition `{namespace}.{priority}.{partition}.>` and uses a
    /// partition-scoped durable name `{namespace}_{priority}_{partition}_consumer`.
    ///
    /// This gives EXCLUSIVE routing to one logical consumer (e.g. one lab
    /// runner) without a stream per consumer: many partitions share the one
    /// `{namespace}` stream-set, each draining only its own partition's
    /// subjects. The publisher must publish to
    /// `{namespace}.{priority}.{partition}.{exec_id}` (the partition segment
    /// precedes the job suffix). Suits an unbounded fleet of exclusively-routed
    /// workers where `Pool`'s shared queue (any worker wins) is wrong and
    /// `PerJob`'s stream-per-namespace would explode the stream count.
    PartitionedPool {
        /// The partition key this consumer drains (e.g. a runner id). Only
        /// subjects under `{namespace}.{priority}.{partition}.>` are delivered.
        partition: String,
    },
}

impl Default for ConsumerMode {
    fn default() -> Self {
        ConsumerMode::Pool
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            namespace: "apalis".to_string(),
            max_deliver: 3,
            ack_wait: Duration::from_secs(30),
            num_replicas: 1,
            enable_dlq: true,
            max_ack_pending: 100, // Allow up to 100 unacknowledged messages per consumer
            nak_backoff: vec![Duration::from_millis(500), Duration::from_secs(2)],
            consumer_mode: ConsumerMode::Pool,
            #[cfg(feature = "otel")]
            enable_tracing: true,
        }
    }
}

/// NATS poll error
#[derive(Debug, Error)]
pub enum NatsPollError {
    /// NATS client error
    #[error("NATS error: {0}")]
    Nats(String),
    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    /// Storage error
    #[error("Storage error: {0}")]
    Storage(String),
}

// Implementation for all NATS error types
impl From<async_nats::Error> for NatsPollError {
    fn from(err: async_nats::Error) -> Self {
        NatsPollError::Nats(err.to_string())
    }
}

/// Job wrapper for NATS
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NatsJob<T> {
    pub id: TaskId,
    pub data: T,
    pub priority: Priority,
    pub attempts: Attempt,
    pub created_at: DateTime<Utc>,
    pub namespace: Namespace,
}

/// Context for NATS jobs
#[derive(Debug, Clone, Default)]
pub struct NatsContext {
    pub(crate) message: Option<Arc<jetstream::Message>>,
    #[cfg(feature = "otel")]
    trace_context: Option<OtelContext>,
}

impl NatsContext {
    /// Create a new context with a message
    pub fn with_message(message: jetstream::Message) -> Self {
        #[cfg(feature = "otel")]
        {
            // Extract trace context from message headers
            let trace_context = global::get_text_map_propagator(|propagator| {
                propagator.extract(&NatsHeaderExtractor::new_from_message(&message.message))
            });

            Self {
                message: Some(Arc::new(message)),
                trace_context: Some(trace_context),
            }
        }

        #[cfg(not(feature = "otel"))]
        Self {
            message: Some(Arc::new(message)),
        }
    }

    /// Get the underlying NATS message
    pub fn message(&self) -> Option<&jetstream::Message> {
        self.message.as_ref().map(|m| m.as_ref())
    }

    /// Get the OpenTelemetry trace context
    #[cfg(feature = "otel")]
    pub fn trace_context(&self) -> Option<&OtelContext> {
        self.trace_context.as_ref()
    }
}

/// Queue info for NATS
#[derive(Debug, Clone)]
pub struct NatsQueueInfo {
    /// The stream name
    pub stream: String,
    /// Number of pending messages
    pub pending: u64,
}

/// NATS JetStream storage implementation for Apalis jobs.
///
/// Use [`NatsStorage::new`] or [`NatsStorage::new_with_config`] to initialize the backend and
/// create the required streams (one per priority and an optional DLQ stream).
///
/// See the crate-level docs and README for end-to-end examples.
pub struct NatsStorage<T> {
    client: Client,
    pub(crate) jetstream: jetstream::Context,
    pub(crate) config: Config,
    consumers: Arc<std::sync::Mutex<HashMap<Priority, consumer::Consumer<consumer::pull::Config>>>>,
    _phantom: PhantomData<T>,
}

impl<T> fmt::Debug for NatsStorage<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NatsStorage")
            .field("config", &self.config)
            .finish()
    }
}

impl<T> Clone for NatsStorage<T> {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            jetstream: self.jetstream.clone(),
            config: self.config.clone(),
            consumers: Arc::clone(&self.consumers),
            _phantom: PhantomData,
        }
    }
}

/// Connect to NATS with basic URL
///
/// For simple connections without authentication.
///
/// # Example
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = apalis_nats::connect("nats://localhost:4222").await?;
/// # Ok(())
/// # }
/// ```
pub async fn connect(url: impl async_nats::ToServerAddrs) -> Result<Client, ConnectError> {
    async_nats::connect(url).await
}

/// Connect to NATS with credentials file
///
/// Authenticates using a `.creds` file containing JWT and NKey seed.
///
/// # Example
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = apalis_nats::connect_with_credentials(
///     "nats://connect.ngs.global",
///     "path/to/my.creds"
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub async fn connect_with_credentials(
    url: impl async_nats::ToServerAddrs,
    creds_path: impl AsRef<std::path::Path>,
) -> Result<Client, ConnectError> {
    async_nats::ConnectOptions::with_credentials_file(creds_path.as_ref())
        .await?
        .connect(url)
        .await
}

/// Connect to NATS with username and password
///
/// # Example
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = apalis_nats::connect_with_user_pass(
///     "nats://localhost:4222",
///     "myuser",
///     "mypassword"
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub async fn connect_with_user_pass(
    url: impl async_nats::ToServerAddrs,
    user: impl Into<String>,
    password: impl Into<String>,
) -> Result<Client, ConnectError> {
    async_nats::ConnectOptions::with_user_and_password(user.into(), password.into())
        .connect(url)
        .await
}

/// Connect to NATS with custom options
///
/// Provides full control over connection configuration including authentication,
/// client name, and other advanced options.
///
/// # Example
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = apalis_nats::connect_with_options(
///     "nats://localhost:4222",
///     async_nats::ConnectOptions::new()
///         .name("my-worker")
///         .credentials_file("path/to/my.creds").await?
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub async fn connect_with_options(
    url: impl async_nats::ToServerAddrs,
    options: async_nats::ConnectOptions,
) -> Result<Client, ConnectError> {
    options.connect(url).await
}

impl<T> NatsStorage<T>
where
    T: Serialize + DeserializeOwned + Send + 'static,
{
    /// Create a new NATS storage instance
    pub async fn new(client: Client) -> Result<Self, NatsPollError> {
        Self::new_with_config(client, Config::default()).await
    }

    /// Create a new NATS storage instance with custom config
    pub async fn new_with_config(client: Client, config: Config) -> Result<Self, NatsPollError> {
        let jetstream = jetstream::new(client.clone());

        // Create streams for each priority level. Subject filter is a wildcard
        // over `{namespace}.{priority}.>` so publishers can route by job suffix
        // (e.g., `apalis.medium.<exec_id>`); a Pool consumer wildcards over the
        // same hierarchy, a PerJob consumer exact-matches one suffix.
        for priority in [Priority::High, Priority::Medium, Priority::Low] {
            let stream_name = format!("{}_{}", config.namespace, priority);
            let subject_prefix = format!("{}.{}", config.namespace, priority);

            let stream_config = stream::Config {
                name: stream_name.clone(),
                subjects: vec![format!("{}.>", subject_prefix)],
                // Message retention settings
                max_age: Duration::from_secs(7 * 24 * 60 * 60), // 7 days
                storage: stream::StorageType::File,
                num_replicas: config.num_replicas,
                // Work queue optimizations
                retention: stream::RetentionPolicy::WorkQueue, // Automatically remove acknowledged messages
                discard: stream::DiscardPolicy::Old, // When stream is full, discard old messages
                duplicate_window: Duration::from_secs(120), // Prevent duplicate messages within 2 minutes
                ..Default::default()
            };

            // Create or update stream
            match jetstream.get_or_create_stream(stream_config).await {
                Ok(_) => tracing::info!("Stream {} ready", stream_name),
                Err(e) => {
                    tracing::error!("Failed to create stream {}: {}", stream_name, e);
                    return Err(NatsPollError::Nats(e.to_string()));
                }
            }
        }

        // Create DLQ stream if enabled
        if config.enable_dlq {
            let dlq_stream_name = format!("{}_dlq", config.namespace);
            let dlq_subject = format!("{}.dlq", config.namespace);

            let dlq_config = stream::Config {
                name: dlq_stream_name.clone(),
                subjects: vec![dlq_subject],
                max_age: Duration::from_secs(30 * 24 * 60 * 60), // 30 days
                storage: stream::StorageType::File,
                num_replicas: config.num_replicas,
                ..Default::default()
            };

            match jetstream.get_or_create_stream(dlq_config).await {
                Ok(_) => tracing::info!("DLQ stream {} ready", dlq_stream_name),
                Err(e) => {
                    tracing::error!("Failed to create DLQ stream {}: {}", dlq_stream_name, e);
                    return Err(NatsPollError::Nats(e.to_string()));
                }
            }
        }

        Ok(Self {
            client,
            jetstream,
            config,
            consumers: Arc::new(std::sync::Mutex::new(HashMap::new())),
            _phantom: PhantomData,
        })
    }

    /// Get the stream name for a priority level
    pub(crate) fn get_stream_name(&self, priority: Priority) -> String {
        format!("{}_{}", self.config.namespace, priority)
    }

    /// Get the subject for a priority level
    fn get_subject(&self, priority: Priority) -> String {
        format!("{}.{}", self.config.namespace, priority)
    }

    /// Push a job with a specific priority
    ///
    /// Jobs are published to priority-specific streams and will be processed
    /// in priority order (High -> Medium -> Low).
    ///
    /// # Example
    /// ```no_run
    /// # use apalis_nats::{NatsStorage, Priority};
    /// # async fn example(storage: NatsStorage<String>) -> Result<(), Box<dyn std::error::Error>> {
    /// storage.push_with_priority("urgent_task".to_string(), Priority::High).await?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg_attr(feature = "otel", tracing::instrument(skip(self, job)))]
    pub async fn push_with_priority(
        &self,
        job: T,
        priority: Priority,
    ) -> Result<TaskId, NatsPollError> {
        let task_id = TaskId::new();
        let nats_job = NatsJob {
            id: task_id.clone(),
            data: job,
            priority,
            attempts: Attempt::new(),
            created_at: Utc::now(),
            namespace: Namespace::from(self.config.namespace.clone()),
        };

        let payload = serde_json::to_vec(&nats_job)?;
        // Publish to `{namespace}.{priority}.{task_id}` so the wildcard stream
        // filter accepts it and Pool consumers (`{ns}.{prio}.>`) catch it.
        let subject = format!("{}.{}", self.get_subject(priority), task_id);

        // Prepare headers - when OTel is enabled, we create a producer span synchronously,
        // inject its context into headers, then let the span complete.
        // We use a block scope so the span is dropped before any await points.
        let headers = {
            #[cfg(feature = "otel")]
            if self.config.enable_tracing {
                // Get the parent context from the current tracing span.
                // The #[instrument] attribute ensures this reflects the caller's span.
                let parent_ctx = tracing::Span::current().context();

                // Create the producer span as a child of the current span
                let tracer = global::tracer("apalis-nats");
                let mut span = tracer
                    .span_builder("job.push")
                    .with_kind(SpanKind::Producer)
                    .with_attributes(vec![
                        KeyValue::new("job.id", task_id.to_string()),
                        KeyValue::new("job.priority", priority.to_string()),
                        KeyValue::new("job.namespace", self.config.namespace.clone()),
                    ])
                    .start_with_context(&tracer, &parent_ctx);

                // Get the span context directly from the span we just created
                let span_context = span.span_context().clone();

                // Create a context with this span context for injection
                let inject_ctx = OtelContext::new().with_remote_span_context(span_context);

                // Use global propagator for injection (same as extraction uses)
                let mut injector = NatsHeaderInjector::new(HeaderMap::new());
                global::get_text_map_propagator(|propagator| {
                    propagator.inject_context(&inject_ctx, &mut injector);
                });

                // Mark span successful before it ends
                span.set_status(Status::Ok);

                injector.into()
            } else {
                HeaderMap::new()
            }

            #[cfg(not(feature = "otel"))]
            HeaderMap::new()
        };

        // Publish with headers - this is the async part, no span held
        self.jetstream
            .publish_with_headers(subject, headers, Bytes::from(payload))
            .await
            .map_err(|e| NatsPollError::Nats(e.to_string()))?
            .await
            .map_err(|e| NatsPollError::Nats(e.to_string()))?;

        Ok(task_id)
    }

    /// Push a job with a specific priority and trace context
    ///
    /// This method allows manual specification of the OpenTelemetry trace context
    /// that will be propagated to the job consumer. This is useful when you need
    /// to link job processing to a specific trace.
    ///
    /// Only available when the `otel` feature is enabled.
    #[cfg(feature = "otel")]
    pub async fn push_with_priority_and_context(
        &self,
        job: T,
        priority: Priority,
        parent_context: &OtelContext,
    ) -> Result<TaskId, NatsPollError> {
        let task_id = TaskId::new();
        let nats_job = NatsJob {
            id: task_id.clone(),
            data: job,
            priority,
            attempts: Attempt::new(),
            created_at: Utc::now(),
            namespace: Namespace::from(self.config.namespace.clone()),
        };

        let payload = serde_json::to_vec(&nats_job)?;
        let subject = format!("{}.{}", self.get_subject(priority), task_id);

        // Create the producer span with the provided parent context, inject headers, then drop span
        // This must be done synchronously to avoid holding span across await points
        let headers = {
            let tracer = global::tracer("apalis-nats");
            let mut span = tracer
                .span_builder("job.push")
                .with_kind(SpanKind::Producer)
                .with_attributes(vec![
                    KeyValue::new("job.id", task_id.to_string()),
                    KeyValue::new("job.priority", priority.to_string()),
                    KeyValue::new("job.namespace", self.config.namespace.clone()),
                ])
                .start_with_context(&tracer, parent_context);

            // Get the span context directly from the span we just created
            let span_context = span.span_context().clone();

            // Create a context with this span context for injection
            let inject_ctx = OtelContext::new().with_remote_span_context(span_context);

            // Use global propagator for injection (same as extraction uses)
            let mut injector = NatsHeaderInjector::new(HeaderMap::new());
            global::get_text_map_propagator(|propagator| {
                propagator.inject_context(&inject_ctx, &mut injector);
            });

            // Mark span successful before it ends
            span.set_status(Status::Ok);
            injector.into()
        };

        // Publish with headers
        self.jetstream
            .publish_with_headers(subject, headers, Bytes::from(payload))
            .await
            .map_err(|e| NatsPollError::Nats(e.to_string()))?
            .await
            .map_err(|e| NatsPollError::Nats(e.to_string()))?;

        Ok(task_id)
    }

    /// Create or update a shared consumer for a specific priority.
    /// Uses `create_consumer` (CreateOrUpdate semantics) so config
    /// changes take effect on deploy without manual consumer deletion.
    async fn get_or_create_consumer(
        &self,
        priority: Priority,
    ) -> Result<consumer::Consumer<consumer::pull::Config>, NatsPollError> {
        // Try cache first
        if let Some(existing) = self
            .consumers
            .lock()
            .map_err(|_| NatsPollError::Storage("Consumer cache poisoned".into()))?
            .get(&priority)
            .cloned()
        {
            return Ok(existing);
        }

        let stream_name = self.get_stream_name(priority);
        let priority_subject = self.get_subject(priority); // "{namespace}.{priority}"

        // Mode-dependent consumer identity and filter. Pool keeps the historical
        // shared durable consumer (worker pool semantics). PerJob is one-shot,
        // ephemeral, and filters to a single exec_id — no inter-process race.
        let (consumer_name, durable_name, filter_subject, inactive_threshold) =
            match &self.config.consumer_mode {
                ConsumerMode::Pool => {
                    let name = format!("{}_{}_consumer", self.config.namespace, priority);
                    (
                        name.clone(),
                        Some(name),
                        format!("{}.>", priority_subject),
                        Duration::from_secs(300), // 5 minutes
                    )
                }
                ConsumerMode::PerJob { exec_id } => {
                    let name =
                        format!("{}_{}_oneshot_{}", self.config.namespace, priority, exec_id);
                    (
                        name,
                        None, // ephemeral — no durable_name
                        format!("{}.{}", priority_subject, exec_id),
                        Duration::from_secs(60), // shorter TTL for one-shot
                    )
                }
                ConsumerMode::PartitionedPool { partition } => {
                    // Shared stream (same as Pool), partition-scoped durable +
                    // filter. The filter `{ns}.{prio}.{partition}.>` keeps this
                    // consumer exclusive to its partition while the stream stays
                    // shared across all partitions of this namespace.
                    let name = format!(
                        "{}_{}_{}_consumer",
                        self.config.namespace, priority, partition
                    );
                    (
                        name.clone(),
                        Some(name),
                        format!("{}.{}.>", priority_subject, partition),
                        Duration::from_secs(300), // durable, like Pool
                    )
                }
            };

        let config = consumer::pull::Config {
            name: Some(consumer_name.clone()),
            durable_name,
            // Work queue settings - ensure only one worker gets each message
            ack_policy: consumer::AckPolicy::Explicit,
            ack_wait: self.config.ack_wait,
            max_deliver: self.config.max_deliver,
            filter_subject,
            // Critical for work queue behavior - only deliver to one consumer
            deliver_policy: consumer::DeliverPolicy::All,
            // Control message delivery
            max_ack_pending: self.config.max_ack_pending,
            // Server-side backoff for redeliveries (ack_wait timeout or NAK).
            // Spreads retries so they don't all fire during the same instability window.
            // NATS requires max_deliver > len(backoff), so truncate if needed.
            backoff: {
                let max_len = (self.config.max_deliver as usize).saturating_sub(1);
                let mut b = self.config.nak_backoff.clone();
                b.truncate(max_len);
                b
            },
            // Replay policy - start from beginning or new messages only
            replay_policy: consumer::ReplayPolicy::Instant,
            // Inactive threshold - remove consumer if inactive
            inactive_threshold,
            ..Default::default()
        };

        let stream = self
            .jetstream
            .get_stream(stream_name)
            .await
            .map_err(|e| NatsPollError::Nats(e.to_string()))?;
        let consumer = stream
            .create_consumer(config)
            .await
            .map_err(|e| NatsPollError::Nats(e.to_string()))?;

        // Insert into cache and return a clone
        let mut guard = self
            .consumers
            .lock()
            .map_err(|_| NatsPollError::Storage("Consumer cache poisoned".into()))?;
        guard.insert(priority, consumer.clone());
        Ok(consumer)
    }

    /// Drop the cached `Consumer` handle for a priority so the next
    /// `get_or_create_consumer` call re-fetches fresh state from the server.
    /// Used by the fetch loop when a `messages()` stream errors or ends,
    /// which typically indicates the server-side consumer was deleted
    /// (e.g., hit `inactive_threshold`) and must be re-created.
    fn invalidate_consumer(&self, priority: Priority) {
        match self.consumers.lock() {
            Ok(mut guard) => {
                guard.remove(&priority);
            }
            Err(_) => tracing::error!(
                priority = %priority,
                "consumer cache mutex poisoned; cannot invalidate"
            ),
        }
    }
}

/// Open a long-lived `messages()` stream for the given priority's durable
/// pull consumer. Returns `None` on any failure after logging the error;
/// the caller should back off and retry on the next iteration.
///
/// On failure we also invalidate the consumer cache so that a stale cached
/// handle (pointing at a server-side consumer that no longer exists) is
/// re-created from scratch on the next attempt.
async fn open_messages_stream<T>(
    storage: &NatsStorage<T>,
    priority: Priority,
) -> Option<async_nats::jetstream::consumer::pull::Stream>
where
    T: Serialize + DeserializeOwned + Send + 'static,
{
    match storage.get_or_create_consumer(priority).await {
        Ok(consumer) => match consumer.messages().await {
            Ok(stream) => {
                tracing::info!(priority = %priority, "opened messages stream");
                Some(stream)
            }
            Err(e) => {
                tracing::error!(
                    priority = %priority,
                    error = %e,
                    "failed to open messages stream; will retry"
                );
                storage.invalidate_consumer(priority);
                None
            }
        },
        Err(e) => {
            tracing::error!(
                priority = %priority,
                error = %e,
                "failed to get/create consumer; will retry"
            );
            None
        }
    }
}

impl<T> Storage for NatsStorage<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    type Job = T;
    type Error = NatsPollError;
    type Context = NatsContext;
    type Compact = Vec<u8>;

    async fn push_request(
        &mut self,
        req: Request<Self::Job, Self::Context>,
    ) -> Result<Parts<Self::Context>, Self::Error> {
        let task_id = self
            .push_with_priority(req.args, Priority::default())
            .await?;
        let mut parts = Parts::default();
        parts.task_id = task_id;
        parts.context = NatsContext::default();
        parts.namespace = Some(Namespace::from(self.config.namespace.clone()));
        Ok(parts)
    }

    async fn push_raw_request(
        &mut self,
        _req: Request<Self::Compact, Self::Context>,
    ) -> Result<Parts<Self::Context>, Self::Error> {
        // For now, we don't support raw requests directly
        Err(NatsPollError::Storage(
            "Raw requests not supported".to_string(),
        ))
    }

    async fn schedule_request(
        &mut self,
        _request: Request<Self::Job, Self::Context>,
        _on: i64,
    ) -> Result<Parts<Self::Context>, Self::Error> {
        // Scheduling is not yet implemented for NATS
        Err(NatsPollError::Storage(
            "Scheduling not yet implemented".to_string(),
        ))
    }

    async fn len(&mut self) -> Result<i64, Self::Error> {
        let mut total = 0u64;

        for priority in [Priority::High, Priority::Medium, Priority::Low] {
            let stream_name = self.get_stream_name(priority);
            match self.jetstream.get_stream(stream_name).await {
                Ok(mut stream) => {
                    if let Ok(info) = stream.info().await {
                        total += info.state.messages;
                    }
                }
                Err(_) => continue,
            }
        }

        Ok(total as i64)
    }

    async fn fetch_by_id(
        &mut self,
        _job_id: &TaskId,
    ) -> Result<Option<Request<Self::Job, Self::Context>>, Self::Error> {
        // NATS streams don't support direct lookup by ID
        Ok(None)
    }

    async fn update(&mut self, _job: Request<Self::Job, Self::Context>) -> Result<(), Self::Error> {
        // NATS streams don't support in-place updates
        Err(NatsPollError::Storage("Updates not supported".to_string()))
    }

    async fn reschedule(
        &mut self,
        _job: Request<Self::Job, Self::Context>,
        _wait: Duration,
    ) -> Result<(), Self::Error> {
        // Rescheduling would require republishing to the stream
        Err(NatsPollError::Storage(
            "Rescheduling not yet implemented".to_string(),
        ))
    }

    async fn is_empty(&mut self) -> Result<bool, Self::Error> {
        Ok(self.len().await? == 0)
    }

    async fn vacuum(&mut self) -> Result<usize, Self::Error> {
        // NATS streams automatically clean up acknowledged messages
        Ok(0)
    }
}

impl<T, C, Res> Ack<T, Res, C> for NatsStorage<T>
where
    T: Sync + Send + Serialize + DeserializeOwned + 'static,
    C: Codec<Compact = Vec<u8>> + Send + 'static,
    Res: Serialize + Sync + Send + 'static,
{
    type Context = NatsContext;
    type AckError = NatsPollError;

    async fn ack(
        &mut self,
        ctx: &Self::Context,
        response: &Response<Res>,
    ) -> Result<(), Self::AckError> {
        // Get the NATS message from context
        if let Some(msg) = ctx.message() {
            match &response.inner {
                Ok(_) => {
                    // Job succeeded - acknowledge the message
                    msg.ack()
                        .await
                        .map_err(|e| NatsPollError::Nats(e.to_string()))?;
                    tracing::debug!("Acknowledged message for task {}", response.task_id);
                }
                Err(e) => {
                    // Check if we should move to DLQ
                    let info = msg.info().map_err(|e| NatsPollError::Nats(e.to_string()))?;
                    let should_dlq = match e {
                        Error::Abort(_) => true, // Non-transient errors go to DLQ
                        _ => {
                            // Check if we've exceeded max deliveries
                            info.delivered >= self.config.max_deliver
                        }
                    };

                    if should_dlq && self.config.enable_dlq {
                        // Move to DLQ by publishing to DLQ stream
                        let dlq_subject = format!("{}.dlq", self.config.namespace);

                        // Determine DLQ reason
                        let dlq_reason = match e {
                            Error::Abort(_) => "abort_error",
                            _ => "max_deliver_exceeded",
                        };

                        // Create DLQ message with metadata
                        let dlq_job = json!({
                            "original_task_id": response.task_id.to_string(),
                            "error": e.to_string(),
                            "attempts": format!("{:?}", response.attempt),
                            "delivered_count": info.delivered,
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                            "dlq_reason": dlq_reason,
                            "payload": msg.payload.clone(),
                        });

                        // Publish to DLQ
                        let body =
                            serde_json::to_vec(&dlq_job).map_err(NatsPollError::Serialization)?;
                        self.jetstream
                            .publish(dlq_subject, body.into())
                            .await
                            .map_err(|e| NatsPollError::Nats(e.to_string()))?
                            .await
                            .map_err(|e| NatsPollError::Nats(e.to_string()))?;

                        // Acknowledge the original message to remove it
                        msg.ack()
                            .await
                            .map_err(|e| NatsPollError::Nats(e.to_string()))?;

                        tracing::warn!(
                            "Moved task {} to DLQ after {} deliveries",
                            response.task_id,
                            info.delivered
                        );
                    } else {
                        // Check error type to determine acknowledgment strategy
                        match e {
                            Error::Abort(_) => {
                                // Non-transient error - terminate to prevent redelivery
                                msg.ack_with(jetstream::AckKind::Term)
                                    .await
                                    .map_err(|e| NatsPollError::Nats(e.to_string()))?;
                                tracing::warn!(
                                    "Terminated message for task {} due to abort error",
                                    response.task_id
                                );
                            }
                            _ => {
                                // Transient error - negative acknowledge for retry, with backoff
                                let idx = info.delivered.saturating_sub(1) as usize;
                                let delay = if self.config.nak_backoff.is_empty() {
                                    None
                                } else if idx < self.config.nak_backoff.len() {
                                    Some(self.config.nak_backoff[idx])
                                } else {
                                    Some(*self.config.nak_backoff.last().unwrap())
                                };

                                msg.ack_with(jetstream::AckKind::Nak(delay))
                                    .await
                                    .map_err(|e| NatsPollError::Nats(e.to_string()))?;
                                if let Some(d) = delay {
                                    tracing::debug!(
                                        "Nacked message for task {} for retry in {:?} (attempt {})",
                                        response.task_id,
                                        d,
                                        info.delivered
                                    );
                                } else {
                                    tracing::debug!(
                                        "Nacked message for task {} for retry (attempt {})",
                                        response.task_id,
                                        info.delivered
                                    );
                                }
                            }
                        }
                    }
                }
            }
        } else {
            tracing::warn!("No NATS message in context for task {}", response.task_id);
        }
        Ok(())
    }
}

impl<T> Backend<Request<T, NatsContext>> for NatsStorage<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    type Stream = BoxStream<'static, Result<Option<Request<T, NatsContext>>, Error>>;
    type Layer =
        AckLayer<Sender<(NatsContext, Response<Vec<u8>>)>, T, NatsContext, JsonCodec<Vec<u8>>>;
    type Codec = JsonCodec<Vec<u8>>;

    fn poll(self, worker: &Worker<WorkerContext>) -> Poller<Self::Stream, Self::Layer> {
        let _worker_id = worker.id().to_string();

        // Create channels for job streaming and acknowledgments
        let (mut job_tx, job_rx) =
            mpsc::channel::<Result<Option<Request<T, NatsContext>>, Error>>(10);
        let (ack_tx, mut ack_rx) = mpsc::channel::<(NatsContext, Response<Vec<u8>>)>(10);

        // Create the AckLayer with the sender
        let layer = AckLayer::new(ack_tx);

        // Clone storage for the ack task
        let mut ack_storage = self.clone();

        // Spawn dedicated ack handling task
        tokio::spawn(async move {
            while let Some((ctx, resp)) = ack_rx.next().await {
                if let Err(e) = <NatsStorage<T> as Ack<T, Vec<u8>, JsonCodec<Vec<u8>>>>::ack(
                    &mut ack_storage,
                    &ctx,
                    &resp,
                )
                .await
                {
                    tracing::error!("Failed to acknowledge message: {}", e);
                }
            }
        });

        // Spawn the fetch loop: three long-lived `Messages` streams, one per
        // priority, multiplexed via `tokio::select! { biased; ... }`. The
        // library internally refills pull requests with flow control, so we
        // neither leak server-side pulls nor saturate `max_waiting`. On any
        // stream error or end-of-stream we invalidate the cached Consumer
        // handle and rebuild the stream on the next outer iteration, which
        // recovers from server-side consumer deletion (e.g., `inactive_threshold`)
        // without needing a process restart.
        tokio::spawn(async move {
            let mut high_stream: Option<async_nats::jetstream::consumer::pull::Stream> = None;
            let mut med_stream: Option<async_nats::jetstream::consumer::pull::Stream> = None;
            let mut low_stream: Option<async_nats::jetstream::consumer::pull::Stream> = None;
            let reopen_backoff = Duration::from_millis(500);

            loop {
                // Stop if the consuming worker is gone: a dropped/aborted worker
                // drops the `job_rx` receiver, closing `job_tx`. Without this the
                // task would otherwise spin forever — once the worker's streams
                // are deleted, every `next()` returns `None`, and the rebuild
                // branch keeps re-creating consumers against a vanished stream
                // ("stream not found"). That orphaned spin leaks a task per
                // dropped worker and, on a shared test broker, starves the
                // runtime enough to flake unrelated tests. Both the idle-park
                // exit (a deleted stream wakes `next()` → loop) and the
                // backoff-`continue` re-enter here, so one check covers all paths.
                if job_tx.is_closed() {
                    tracing::debug!("job receiver dropped; exiting fetch task");
                    return;
                }

                // Lazily (re)open any missing streams. Each branch logs on
                // failure so operators see *why* nothing is flowing instead
                // of observing silent idleness.
                if high_stream.is_none() {
                    high_stream = open_messages_stream(&self, Priority::High).await;
                }
                if med_stream.is_none() {
                    med_stream = open_messages_stream(&self, Priority::Medium).await;
                }
                if low_stream.is_none() {
                    low_stream = open_messages_stream(&self, Priority::Low).await;
                }

                if high_stream.is_none() || med_stream.is_none() || low_stream.is_none() {
                    tokio::time::sleep(reopen_backoff).await;
                    continue;
                }

                let h = high_stream.as_mut().unwrap();
                let m = med_stream.as_mut().unwrap();
                let l = low_stream.as_mut().unwrap();

                let (priority, item) = tokio::select! {
                    biased;
                    r = h.next() => (Priority::High, r),
                    r = m.next() => (Priority::Medium, r),
                    r = l.next() => (Priority::Low, r),
                };

                match item {
                    Some(Ok(msg)) => match serde_json::from_slice::<NatsJob<T>>(&msg.payload) {
                        Ok(job) => {
                            let ctx = NatsContext::with_message(msg);
                            let request = Request::new_with_ctx(job.data, ctx);
                            if job_tx.send(Ok(Some(request))).await.is_err() {
                                tracing::debug!("job channel closed; exiting fetch task");
                                return;
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                priority = %priority,
                                error = %e,
                                "failed to deserialize job payload; terminating message"
                            );
                            if let Err(ack_err) = msg.ack_with(jetstream::AckKind::Term).await {
                                tracing::error!(
                                    priority = %priority,
                                    error = %ack_err,
                                    "failed to Term malformed message"
                                );
                            }
                        }
                    },
                    Some(Err(e)) => {
                        tracing::error!(
                            priority = %priority,
                            error = %e,
                            "messages stream error; invalidating consumer cache and rebuilding"
                        );
                        self.invalidate_consumer(priority);
                        match priority {
                            Priority::High => high_stream = None,
                            Priority::Medium => med_stream = None,
                            Priority::Low => low_stream = None,
                        }
                    }
                    None => {
                        tracing::warn!(
                            priority = %priority,
                            "messages stream ended (consumer likely deleted server-side); rebuilding"
                        );
                        self.invalidate_consumer(priority);
                        match priority {
                            Priority::High => high_stream = None,
                            Priority::Medium => med_stream = None,
                            Priority::Low => low_stream = None,
                        }
                    }
                }
            }
        });

        // Return the job stream as a boxed stream
        let stream = job_rx.boxed();

        Poller::new_with_layer(
            stream,
            async {
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            },
            layer,
        )
    }
}
