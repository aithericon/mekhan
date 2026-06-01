use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use apalis::prelude::*;
use apalis_nats::NatsStorage;
use async_nats::jetstream;
use async_nats::jetstream::consumer::pull::Config as ConsumerConfig;
use async_nats::jetstream::consumer::PullConsumer;
use futures::StreamExt;
use tracing::{debug, warn};
use uuid::Uuid;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use aithericon_executor_domain::{ExecutionEvent, ExecutionJob, RunDirectory, StatusUpdate};
use aithericon_executor_logs::LogSink;
use aithericon_executor_metrics::MetricSink;
use aithericon_executor_process::ProcessBackend;
use aithericon_executor_storage::ArtifactStore;
use aithericon_executor_worker::{
    handle_execution, BackendRegistry, CancellationRegistry, ChunkRegistry, CleanupPolicy,
    JobExecutor, NatsCancelListener, SidecarLogConfig, StagingPipeline, StatusReporter,
};

use crate::nats::shared_nats_url;

/// Per-test context providing full NATS stream isolation via UUID-prefixed streams.
///
/// Each context creates its own NATS client connection so that aborting one
/// test's worker doesn't disrupt other tests sharing the container.
pub struct ExecutorTestContext {
    pub prefix: String,
    pub storage: NatsStorage<ExecutionJob>,
    pub reporter: StatusReporter,
    pub registry: Arc<BackendRegistry>,
    pub pipeline: Arc<StagingPipeline>,
    pub base_dir: PathBuf,
    pub cancel_registry: CancellationRegistry,
    pub chunk_registry: ChunkRegistry,
    nats_client: async_nats::Client,
    jetstream: jetstream::Context,
    status_stream_name: String,
    events_stream_name: String,
}

impl ExecutorTestContext {
    /// Create a new test context with a unique UUID prefix.
    ///
    /// Each call creates a fresh NATS client connection to the shared container.
    pub async fn new() -> Self {
        Self::build(None).await
    }

    /// Create a new test context with a pre-configured artifact store.
    ///
    /// The store is wired into both the staging pipeline (for `InputSource::StoragePath`)
    /// and the worker (for artifact uploads via IPC sidecar).
    pub async fn new_with_store(store: Arc<dyn ArtifactStore>) -> Self {
        Self::build(Some(store)).await
    }

    async fn build(store: Option<Arc<dyn ArtifactStore>>) -> Self {
        let prefix = format!("test_{}", Uuid::new_v4().simple());
        let url = shared_nats_url().await;

        // Each test gets its own NATS connection to avoid cross-test interference
        let client = async_nats::connect(url)
            .await
            .expect("failed to connect to shared NATS testcontainer");
        let js = jetstream::new(client.clone());

        // Isolated apalis NatsStorage for jobs
        let nats_config = apalis_nats::Config {
            namespace: format!("{prefix}_jobs"),
            max_deliver: 3,
            ack_wait: Duration::from_secs(30),
            num_replicas: 1,
            enable_dlq: false,
            ..Default::default()
        };
        let nats_client = client.clone();
        let storage = NatsStorage::<ExecutionJob>::new_with_config(client, nats_config)
            .await
            .expect("failed to create NatsStorage for test");

        // Isolated StatusReporter
        let reporter = StatusReporter::new_with_prefix(
            js.clone(),
            "test-executor".into(),
            1,
            Some(prefix.clone()),
        )
        .await
        .expect("failed to create StatusReporter for test");

        // Standard backend registry with ProcessBackend
        let registry =
            Arc::new(BackendRegistry::new(Duration::from_secs(30)).register(ProcessBackend::new()));

        // Staging pipeline with default hooks
        // Use /tmp with a short suffix to keep Unix socket paths under SUN_LEN (104 bytes).
        // Full path: /tmp/ex-{8chars}/runs/{execution_id}/ipc.sock
        let short_id = &Uuid::new_v4().simple().to_string()[..8];
        let base_dir = PathBuf::from(format!("/tmp/ex-{short_id}"));
        let pipeline = Arc::new(aithericon_executor_worker::staging::default_pipeline(
            base_dir.clone(),
            store,
            None, // No secret store in tests
            None, // No vault addr in tests
            None, // No nix hook in tests
        ));

        let status_stream_name = format!("STATUS_{prefix}");
        let events_stream_name = format!("EVENTS_{prefix}");
        let cancel_registry = CancellationRegistry::new();
        let chunk_registry = ChunkRegistry::new();

        Self {
            prefix,
            storage,
            reporter,
            registry,
            pipeline,
            base_dir,
            cancel_registry,
            chunk_registry,
            nats_client,
            jetstream: js,
            status_stream_name,
            events_stream_name,
        }
    }

    /// Create a pull consumer on the status stream filtered by execution_id.
    pub async fn status_consumer(&self, name: &str, execution_id: &str) -> PullConsumer {
        let stream = self
            .jetstream
            .get_stream(&self.status_stream_name)
            .await
            .expect("failed to get status stream");

        stream
            .create_consumer(ConsumerConfig {
                durable_name: Some(format!("{}_{}", self.prefix, name)),
                filter_subject: format!("{}.executor.status.{}.>", self.prefix, execution_id),
                ..Default::default()
            })
            .await
            .expect("failed to create status consumer")
    }

    /// Create a pull consumer on the events stream filtered by execution_id.
    pub async fn events_consumer(&self, name: &str, execution_id: &str) -> PullConsumer {
        let stream = self
            .jetstream
            .get_stream(&self.events_stream_name)
            .await
            .expect("failed to get events stream");

        stream
            .create_consumer(ConsumerConfig {
                durable_name: Some(format!("{}_{}", self.prefix, name)),
                filter_subject: format!("{}.executor.events.{}.>", self.prefix, execution_id),
                ..Default::default()
            })
            .await
            .expect("failed to create events consumer")
    }

    /// Collect N events from a consumer, or until timeout.
    ///
    /// Unlike `collect_statuses`, events don't have a terminal state so we
    /// wait for `expected_count` messages or the timeout, whichever comes first.
    pub async fn collect_events(
        &self,
        consumer: &PullConsumer,
        expected_count: usize,
        timeout: Duration,
    ) -> Vec<ExecutionEvent> {
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if events.len() >= expected_count {
                break;
            }

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            let batch_result = tokio::time::timeout(
                remaining,
                consumer
                    .fetch()
                    .max_messages(10)
                    .expires(Duration::from_secs(1))
                    .messages(),
            )
            .await;

            let mut messages = match batch_result {
                Ok(Ok(msgs)) => msgs,
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    if err_str.contains("channel closed") || err_str.contains("connection closed") {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                Err(_) => break,
            };

            while let Some(msg_result) = messages.next().await {
                if let Ok(msg) = msg_result {
                    let _ = msg.ack().await;
                    if let Ok(event) = serde_json::from_slice::<ExecutionEvent>(&msg.payload) {
                        events.push(event);
                    }
                }
            }
        }

        events
    }

    /// Spawn an apalis worker as a background task.
    ///
    /// Returns a `JoinHandle` that can be aborted when the test is done.
    pub fn spawn_worker(&self) -> tokio::task::JoinHandle<()> {
        self.spawn_worker_with(CleanupPolicy::Retain, None)
    }

    /// Spawn an apalis worker with a specific cleanup policy and optional artifact store.
    pub fn spawn_worker_with(
        &self,
        cleanup_policy: CleanupPolicy,
        artifact_store: Option<Arc<dyn ArtifactStore>>,
    ) -> tokio::task::JoinHandle<()> {
        let executor = Arc::new(JobExecutor {
            reporter: self.reporter.clone(),
            registry: self.registry.clone(),
            pipeline: self.pipeline.clone(),
            base_dir: self.base_dir.clone(),
            artifact_store,
            cleanup_policy,
            metric_sink: None,
            log_sink: None,
            cancel_registry: self.cancel_registry.clone(),
            chunk_registry: self.chunk_registry.clone(),
            log_config: SidecarLogConfig::default(),
            completion_tracker: None,
        });
        let storage = self.storage.clone();

        let worker = WorkerBuilder::new("test-worker")
            .concurrency(2)
            .data(executor)
            .backend(storage)
            .build_fn(handle_execution);

        tokio::spawn(async move {
            let _ = Monitor::new().register(worker).run().await;
        })
    }

    /// Spawn an apalis worker with a specific cleanup policy, artifact store, and backend registry.
    ///
    /// Use this to test error paths like "backend not found" by passing an empty registry.
    pub fn spawn_worker_custom(
        &self,
        cleanup_policy: CleanupPolicy,
        artifact_store: Option<Arc<dyn ArtifactStore>>,
        registry: Arc<BackendRegistry>,
    ) -> tokio::task::JoinHandle<()> {
        let executor = Arc::new(JobExecutor {
            reporter: self.reporter.clone(),
            registry,
            pipeline: self.pipeline.clone(),
            base_dir: self.base_dir.clone(),
            artifact_store,
            cleanup_policy,
            metric_sink: None,
            log_sink: None,
            cancel_registry: self.cancel_registry.clone(),
            chunk_registry: self.chunk_registry.clone(),
            log_config: SidecarLogConfig::default(),
            completion_tracker: None,
        });
        let storage = self.storage.clone();

        let worker = WorkerBuilder::new("test-worker")
            .concurrency(2)
            .data(executor)
            .backend(storage)
            .build_fn(handle_execution);

        tokio::spawn(async move {
            let _ = Monitor::new().register(worker).run().await;
        })
    }

    /// Spawn an apalis worker with injected sinks for real sink integration tests.
    pub fn spawn_worker_with_sinks(
        &self,
        cleanup_policy: CleanupPolicy,
        artifact_store: Option<Arc<dyn ArtifactStore>>,
        metric_sink: Option<Arc<dyn MetricSink>>,
        log_sink: Option<Arc<dyn LogSink>>,
        log_config: SidecarLogConfig,
    ) -> tokio::task::JoinHandle<()> {
        let executor = Arc::new(JobExecutor {
            reporter: self.reporter.clone(),
            registry: self.registry.clone(),
            pipeline: self.pipeline.clone(),
            base_dir: self.base_dir.clone(),
            artifact_store,
            cleanup_policy,
            metric_sink,
            log_sink,
            cancel_registry: self.cancel_registry.clone(),
            chunk_registry: self.chunk_registry.clone(),
            log_config,
            completion_tracker: None,
        });
        let storage = self.storage.clone();

        let worker = WorkerBuilder::new("test-worker")
            .concurrency(2)
            .data(executor)
            .backend(storage)
            .build_fn(handle_execution);

        tokio::spawn(async move {
            let _ = Monitor::new().register(worker).run().await;
        })
    }

    /// Spawn an apalis worker with a completion tracker for drain-mode tests.
    ///
    /// Returns the `JoinHandle` and the `CompletionTracker` so tests can
    /// subscribe to the drain signal.
    pub fn spawn_worker_with_tracker(
        &self,
    ) -> (
        tokio::task::JoinHandle<()>,
        std::sync::Arc<aithericon_executor_worker::CompletionTracker>,
    ) {
        let tracker = std::sync::Arc::new(aithericon_executor_worker::CompletionTracker::new());
        let executor = Arc::new(JobExecutor {
            reporter: self.reporter.clone(),
            registry: self.registry.clone(),
            pipeline: self.pipeline.clone(),
            base_dir: self.base_dir.clone(),
            artifact_store: None,
            cleanup_policy: CleanupPolicy::Retain,
            metric_sink: None,
            log_sink: None,
            cancel_registry: self.cancel_registry.clone(),
            chunk_registry: self.chunk_registry.clone(),
            log_config: SidecarLogConfig::default(),
            completion_tracker: Some(tracker.clone()),
        });
        let storage = self.storage.clone();

        let worker = WorkerBuilder::new("test-worker")
            .concurrency(2)
            .data(executor)
            .backend(storage)
            .build_fn(handle_execution);

        let handle = tokio::spawn(async move {
            let _ = Monitor::new().register(worker).run().await;
        });

        (handle, tracker)
    }

    /// Build a `JobExecutor` for direct testing (batch mode, unit tests).
    pub fn build_executor(&self) -> Arc<JobExecutor> {
        Arc::new(JobExecutor {
            reporter: self.reporter.clone(),
            registry: self.registry.clone(),
            pipeline: self.pipeline.clone(),
            base_dir: self.base_dir.clone(),
            artifact_store: None,
            cleanup_policy: CleanupPolicy::Retain,
            metric_sink: None,
            log_sink: None,
            cancel_registry: self.cancel_registry.clone(),
            chunk_registry: self.chunk_registry.clone(),
            log_config: SidecarLogConfig::default(),
            completion_tracker: None,
        })
    }

    /// Compute the expected RunDirectory for a given execution_id.
    pub fn run_dir_for(&self, execution_id: &str) -> RunDirectory {
        RunDirectory::new(&self.base_dir, execution_id)
    }

    /// Push a job to the apalis storage.
    pub async fn push_job(&self, job: ExecutionJob) {
        let mut storage = self.storage.clone();
        apalis_core::storage::Storage::push(&mut storage, job)
            .await
            .expect("failed to push job");
    }

    /// Collect status updates from a consumer until a terminal status or timeout.
    pub async fn collect_statuses(
        &self,
        consumer: &PullConsumer,
        timeout: Duration,
    ) -> Vec<StatusUpdate> {
        let mut statuses = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                warn!(
                    collected = statuses.len(),
                    "collect_statuses timed out before terminal status"
                );
                break;
            }

            let batch_result = tokio::time::timeout(
                remaining,
                consumer
                    .fetch()
                    .max_messages(10)
                    .expires(Duration::from_secs(1))
                    .messages(),
            )
            .await;

            let mut messages = match batch_result {
                Ok(Ok(msgs)) => msgs,
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    if err_str.contains("channel closed") || err_str.contains("connection closed") {
                        warn!(error = %e, "permanent consumer error, stopping");
                        break;
                    }
                    warn!(error = %e, "batch error, retrying");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                Err(_) => break, // overall timeout
            };

            while let Some(msg_result) = messages.next().await {
                match msg_result {
                    Ok(msg) => {
                        let _ = msg.ack().await;
                        match serde_json::from_slice::<StatusUpdate>(&msg.payload) {
                            Ok(update) => {
                                let is_terminal = update.status.is_terminal();
                                debug!(
                                    execution_id = %update.execution_id,
                                    status = %update.status,
                                    "collected status update"
                                );
                                statuses.push(update);
                                if is_terminal {
                                    return statuses;
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to deserialize status update");
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "message error");
                    }
                }
            }
        }

        statuses
    }

    /// Get the status stream name for this test context.
    pub fn status_stream_name(&self) -> &str {
        &self.status_stream_name
    }

    /// Get the events stream name for this test context.
    pub fn events_stream_name(&self) -> &str {
        &self.events_stream_name
    }

    /// Get the JetStream context.
    pub fn jetstream(&self) -> &jetstream::Context {
        &self.jetstream
    }

    /// Get message count in the status stream.
    pub async fn status_message_count(&self) -> u64 {
        let mut stream = self
            .jetstream
            .get_stream(&self.status_stream_name)
            .await
            .expect("failed to get status stream");
        let info = stream.info().await.expect("failed to get stream info");
        info.state.messages
    }

    /// Get the NATS client for this test context.
    pub fn nats_client(&self) -> &async_nats::Client {
        &self.nats_client
    }

    /// Publish a cancel message to `executor.cancel.{execution_id}` via core NATS.
    pub async fn publish_cancel(&self, execution_id: &str) {
        self.nats_client
            .publish(
                aithericon_executor_domain::cancel_subject(execution_id),
                bytes::Bytes::new(),
            )
            .await
            .expect("failed to publish cancel message");
        self.nats_client
            .flush()
            .await
            .expect("failed to flush NATS");
    }

    /// Start a `NatsCancelListener` that subscribes to cancel messages
    /// and triggers cancellation via the shared `cancel_registry`.
    pub async fn start_cancel_listener(&self, shutdown: CancellationToken) -> JoinHandle<()> {
        NatsCancelListener::start(
            self.nats_client.clone(),
            self.cancel_registry.clone(),
            None,
            shutdown,
        )
        .await
        .expect("failed to start NATS cancel listener")
    }

    /// Delete test streams and run directories (best-effort).
    pub async fn cleanup(&self) {
        let _ = self.jetstream.delete_stream(&self.status_stream_name).await;
        let _ = self.jetstream.delete_stream(&self.events_stream_name).await;
        // apalis creates per-priority streams: {namespace}_{priority}
        for priority in &["high", "medium", "low"] {
            let stream_name = format!("{}_jobs_{priority}", self.prefix);
            let _ = self.jetstream.delete_stream(&stream_name).await;
        }
        // Clean up run directories
        let _ = std::fs::remove_dir_all(&self.base_dir);
    }
}
