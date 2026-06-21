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

use aithericon_executor_domain::{
    ExecutionEvent, ExecutionJob, ExecutionStatus, RunDirectory, StatusUpdate,
};
use aithericon_executor_logs::LogSink;
use aithericon_executor_metrics::MetricSink;
use aithericon_executor_process::ProcessBackend;
use aithericon_executor_storage::ArtifactStore;
use aithericon_executor_worker::{
    handle_execution, BackendRegistry, CancellationRegistry, CleanupPolicy, JetStreamTransport,
    JobExecutor, NatsCancelListener, SidecarLogConfig, StagingPipeline, StatusReporter,
    TransportRegistry,
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
    /// Transport REGISTRY mirroring the worker's, over this context's isolated
    /// NATS — backs a producer job's `PublishChunk`. No object store is wired in
    /// tests, so an "s3" channel fails loudly.
    transports: Option<TransportRegistry>,
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
            None, // No broker secrets in tests
            None, // No nix hook in tests
        ));

        let status_stream_name = format!("STATUS_{prefix}");
        let events_stream_name = format!("EVENTS_{prefix}");
        let cancel_registry = CancellationRegistry::new();

        // Data-plane byte transport over this context's NATS. The
        // `EXECUTOR_DATASTREAM` stream is global (not prefix-isolated), but
        // subjects carry the execution_id so distinct tests never collide;
        // ensure-stream is idempotent.
        JetStreamTransport::ensure_stream(&js, 1)
            .await
            .expect("ensure EXECUTOR_DATASTREAM");
        let transports: Option<TransportRegistry> =
            Some(TransportRegistry::new(js.clone(), nats_client.clone()));

        Self {
            prefix,
            storage,
            reporter,
            registry,
            pipeline,
            base_dir,
            cancel_registry,
            transports,
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
                // Subject is `{prefix}.executor.status.{ws}.{execution_id}.{status}`
                // (ADR-09 inserted the `{ws}` segment). `*` matches the single
                // workspace token, then the execution_id, then the status tail.
                filter_subject: format!("{}.executor.status.*.{}.>", self.prefix, execution_id),
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
                // Subject is `{prefix}.executor.events.{ws}.{execution_id}.{category}`
                // (ADR-09 inserted the `{ws}` segment); `*` matches the workspace token.
                filter_subject: format!("{}.executor.events.*.{}.>", self.prefix, execution_id),
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
            log_config: SidecarLogConfig::default(),
            completion_tracker: None,
            transports: self.transports.clone(),
            serve_group: None,
            max_output_inline_bytes: aithericon_executor_worker::DEFAULT_MAX_OUTPUT_INLINE_BYTES,
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

    /// Drive a single job through `JobExecutor::execute` directly (no apalis
    /// monitor), returning its terminal status. Gives a test deterministic
    /// control over one delivery — used to exercise the duplicate-delivery path.
    pub async fn execute_once(&self, job: &ExecutionJob) -> ExecutionStatus {
        let executor = JobExecutor {
            reporter: self.reporter.clone(),
            registry: self.registry.clone(),
            pipeline: self.pipeline.clone(),
            base_dir: self.base_dir.clone(),
            artifact_store: None,
            cleanup_policy: CleanupPolicy::Retain,
            metric_sink: None,
            log_sink: None,
            cancel_registry: self.cancel_registry.clone(),
            log_config: SidecarLogConfig::default(),
            completion_tracker: None,
            transports: self.transports.clone(),
            serve_group: None,
            max_output_inline_bytes: aithericon_executor_worker::DEFAULT_MAX_OUTPUT_INLINE_BYTES,
        };
        executor.execute(job).await
    }

    /// Pre-create the run-directory lock for `execution_id`, simulating a
    /// concurrent "winner" delivery that already holds it. The next `execute()`
    /// for the same id then takes the duplicate-skip path (lock acquisition
    /// fails), exactly as a redelivery / parallel pool consumer would.
    pub async fn precreate_run_lock(&self, execution_id: &str) {
        let run_dir = RunDirectory::new(&self.base_dir, execution_id);
        tokio::fs::create_dir_all(&run_dir.root)
            .await
            .expect("create run dir for lock");
        tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(run_dir.root.join(".lock"))
            .await
            .expect("create run-dir lock");
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
            log_config: SidecarLogConfig::default(),
            completion_tracker: None,
            transports: self.transports.clone(),
            serve_group: None,
            max_output_inline_bytes: aithericon_executor_worker::DEFAULT_MAX_OUTPUT_INLINE_BYTES,
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
            log_config,
            completion_tracker: None,
            transports: self.transports.clone(),
            serve_group: None,
            max_output_inline_bytes: aithericon_executor_worker::DEFAULT_MAX_OUTPUT_INLINE_BYTES,
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
            log_config: SidecarLogConfig::default(),
            completion_tracker: Some(tracker.clone()),
            transports: self.transports.clone(),
            serve_group: None,
            max_output_inline_bytes: aithericon_executor_worker::DEFAULT_MAX_OUTPUT_INLINE_BYTES,
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
            log_config: SidecarLogConfig::default(),
            completion_tracker: None,
            transports: self.transports.clone(),
            serve_group: None,
            max_output_inline_bytes: aithericon_executor_worker::DEFAULT_MAX_OUTPUT_INLINE_BYTES,
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

    /// Publish a cancel onto this context's prefixed `EXECUTOR_CANCEL` JetStream
    /// stream. The prefix is the per-test isolation seam (production publishes the
    /// unprefixed `executor.cancel.{id}`); the stream is ensured by
    /// [`Self::start_cancel_listener`], which the caller must invoke first.
    pub async fn publish_cancel(&self, execution_id: &str) {
        let subject = format!(
            "{}.{}",
            self.prefix,
            aithericon_executor_domain::cancel_subject(execution_id)
        );
        self.jetstream
            .publish(subject, bytes::Bytes::new())
            .await
            .expect("failed to publish cancel message")
            .await
            .expect("failed to ack cancel publish");
    }

    /// Start a `NatsCancelListener` bound to this context's prefixed
    /// `EXECUTOR_CANCEL` JetStream stream, triggering cancellation via the shared
    /// `cancel_registry`. Ensures the stream and binds the consumer before
    /// returning, so a `publish_cancel` sequenced after this call is delivered.
    pub async fn start_cancel_listener(&self, shutdown: CancellationToken) -> JoinHandle<()> {
        NatsCancelListener::start(
            self.jetstream.clone(),
            self.cancel_registry.clone(),
            Some(&self.prefix),
            1,
            shutdown,
        )
        .await
        .expect("failed to start JetStream cancel listener")
    }

    /// Delete test streams and run directories (best-effort).
    pub async fn cleanup(&self) {
        let _ = self.jetstream.delete_stream(&self.status_stream_name).await;
        let _ = self.jetstream.delete_stream(&self.events_stream_name).await;
        let _ = self
            .jetstream
            .delete_stream(aithericon_executor_domain::cancel_stream_name(Some(
                &self.prefix,
            )))
            .await;
        // apalis creates per-priority streams: {namespace}_{priority}
        for priority in &["high", "medium", "low"] {
            let stream_name = format!("{}_jobs_{priority}", self.prefix);
            let _ = self.jetstream.delete_stream(&stream_name).await;
        }
        // Clean up run directories
        let _ = std::fs::remove_dir_all(&self.base_dir);
    }
}
