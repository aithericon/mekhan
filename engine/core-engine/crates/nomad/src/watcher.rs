//! NomadWatcher: event stream observer that publishes allocation state changes to NATS.
//!
//! Connects to Nomad's event stream API, watches for allocation lifecycle events,
//! fetches Petri routing metadata from the Nomad job API, and publishes `ExternalSignal`
//! messages to `petri.signal.{net_id}.{place_name}`.
//!
//! Net-agnostic — a single instance handles all nets via meta-tag routing.
//!
//! Uses shared infrastructure from `petri-scheduler-bridge`:
//! - [`SignalPublisher`] for NATS signal delivery with JetStream dedup
//! - [`CheckpointStore`] for persisting the Nomad Raft index across restarts
//! - [`RoutingMeta`] for per-status signal routing from job metadata
//! - [`run_with_reconnect`](petri_scheduler_bridge::backoff::run_with_reconnect) for the reconnect loop

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use tokio::io::AsyncBufReadExt;
use tokio::sync::RwLock;
use tokio_util::io::StreamReader;

use petri_domain::ExternalSignal;
use petri_scheduler_bridge::{
    nomad_event_index_key, signal_subject, CheckpointStore, RoutingMeta, SignalPublisher,
    DEV_BOOTSTRAP_CLUSTER_KEY,
};

use crate::config::NomadConfig;
use crate::models::{Allocation, EventStreamData, Job};
use crate::status_mapping;

/// Errors from the Nomad event watcher.
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    /// HTTP client error (connection, timeout, TLS).
    #[error("HTTP client error: {0}")]
    HttpClient(#[from] reqwest::Error),

    /// I/O error during event stream reading.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// TLS certificate configuration failure.
    #[error("TLS configuration error: {0}")]
    TlsConfig(String),

    /// Event stream protocol error.
    #[error("Event stream error: {0}")]
    EventStream(String),

    /// NATS communication error.
    #[error("NATS error: {0}")]
    Nats(String),
}

/// Nomad event stream watcher.
///
/// Connects to Nomad's event stream, extracts allocation lifecycle events,
/// fetches Petri routing metadata from the Nomad job API (cached per job_id),
/// and publishes signals to NATS for the correct net and place.
pub struct NomadWatcher {
    config: NomadConfig,
    http_client: reqwest::Client,
    signal_publisher: SignalPublisher,
    checkpoint: CheckpointStore,
    /// Per-cluster checkpoint namespace (= the datacenter `resource_id`, or
    /// [`DEV_BOOTSTRAP_CLUSTER_KEY`] for the env-built client). Prefixes the
    /// checkpoint key so N clusters sharing the one `PETRI_WATCHER` KV bucket
    /// never clobber each other's event-index cursor.
    cluster_key: String,
    /// Cache of job_id -> Petri routing meta. `None` means "fetched but not a Petri job".
    meta_cache: RwLock<HashMap<String, Option<RoutingMeta>>>,
}

impl NomadWatcher {
    /// Create a new watcher from the env/dev-bootstrap config.
    ///
    /// Initializes the checkpoint KV bucket for restart resilience. Uses the
    /// reserved [`DEV_BOOTSTRAP_CLUSTER_KEY`] namespace — for a resource-driven
    /// cluster, use [`NomadWatcher::from_connection`] with the `resource_id`.
    ///
    /// # Arguments
    /// * `config` - Nomad connection configuration
    /// * `nats` - JetStream context for publishing signals
    pub async fn new(
        config: NomadConfig,
        nats: async_nats::jetstream::Context,
    ) -> Result<Self, WatcherError> {
        Self::from_connection(config, nats, DEV_BOOTSTRAP_CLUSTER_KEY).await
    }

    /// Create a watcher for a specific cluster from a resolved connection.
    ///
    /// `cluster_key` (the datacenter `resource_id`) namespaces this watcher's
    /// checkpoint cursor so concurrent clusters on the one KV bucket resume their
    /// OWN event-stream index after a restart (no cross-contamination).
    pub async fn from_connection(
        config: NomadConfig,
        nats: async_nats::jetstream::Context,
        cluster_key: impl Into<String>,
    ) -> Result<Self, WatcherError> {
        // Build a streaming-specific HTTP client: no overall timeout (event streams
        // stay open indefinitely), TCP keepalive, and conservative pool settings.
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(60))
            .pool_idle_timeout(None)
            .pool_max_idle_per_host(1);

        if let Some(ref ca_path) = config.ca_cert {
            let cert_bytes = std::fs::read(ca_path).map_err(|e| {
                WatcherError::TlsConfig(format!("Failed to read CA cert '{}': {}", ca_path, e))
            })?;
            let cert = reqwest::Certificate::from_pem(&cert_bytes).map_err(|e| {
                WatcherError::TlsConfig(format!("Invalid PEM in CA cert '{}': {}", ca_path, e))
            })?;
            builder = builder.add_root_certificate(cert);
        }

        let http_client = builder.build()?;

        let signal_publisher = SignalPublisher::new(nats.clone());
        let checkpoint = CheckpointStore::new(&nats).await;

        Ok(Self {
            config,
            http_client,
            signal_publisher,
            cluster_key: cluster_key.into(),
            meta_cache: RwLock::new(HashMap::new()),
            checkpoint,
        })
    }

    /// Per-cluster KV key for the last-processed Nomad event stream Raft index.
    ///
    /// Delegates to the shared key builder in `petri-scheduler-bridge` so the
    /// per-cluster scheme is single-sourced (and unit-tested there).
    fn checkpoint_key(&self) -> String {
        nomad_event_index_key(&self.cluster_key)
    }

    /// Load the last checkpointed Nomad event stream Raft index.
    async fn load_checkpoint_index(&self) -> Option<u64> {
        let value = self.checkpoint.load(&self.checkpoint_key()).await?;
        let index: u64 = value.parse().ok()?;
        tracing::info!(index = index, "Loaded checkpoint index from NATS KV");
        Some(index)
    }

    /// Save the current Nomad event stream Raft index.
    async fn save_checkpoint_index(&self, index: u64) {
        self.checkpoint
            .save(&self.checkpoint_key(), &index.to_string())
            .await;
    }

    /// Clear the saved checkpoint (e.g., when the saved index is stale).
    async fn clear_checkpoint_index(&self) {
        self.checkpoint.clear(&self.checkpoint_key()).await;
    }

    /// Run the watcher loop with automatic reconnection.
    ///
    /// This is a long-running async task. Connects to Nomad's event stream,
    /// processes allocation events, and publishes signals to NATS.
    /// Reconnects with exponential backoff on disconnection.
    ///
    /// # Shutdown
    /// Pass a `shutdown` receiver to gracefully stop the watcher.
    pub async fn run(&self, shutdown: tokio::sync::broadcast::Receiver<()>) {
        petri_scheduler_bridge::backoff::run_with_reconnect(shutdown, "Nomad", || {
            self.stream_events()
        })
        .await;
    }

    /// Connect to the event stream and process events until disconnected.
    async fn stream_events(&self) -> Result<(), WatcherError> {
        let mut url = format!(
            "{}/v1/event/stream?topic=Allocation&namespace=*&region={}",
            self.config.addr.trim_end_matches('/'),
            self.config.region
        );

        // Resume from checkpoint if available.
        if let Some(saved_index) = self.load_checkpoint_index().await {
            url.push_str(&format!("&index={}", saved_index + 1));
            tracing::info!(
                resume_index = saved_index + 1,
                "Resuming Nomad event stream from checkpoint"
            );
        }

        let mut req = self.http_client.get(&url);

        if let Some(ref token) = self.config.token {
            req = req.header("X-Nomad-Token", token);
        }

        let resp = req.send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // If the checkpoint index is stale (Raft compacted), clear it so
            // the next reconnect starts from the current position.
            if status.as_u16() == 400 {
                tracing::warn!(
                    "Checkpoint index appears stale — clearing and will retry from current position"
                );
                self.clear_checkpoint_index().await;
            }
            return Err(WatcherError::EventStream(format!(
                "Nomad event stream failed ({}): {}",
                status, body
            )));
        }

        tracing::info!("Connected to Nomad event stream");

        // Stream ndjson lines from the response body using BufReader (matches
        // the proven legacy pattern — FramedRead+LinesCodec can stall on chunked
        // transfer encoding).
        let byte_stream = resp
            .bytes_stream()
            .map(|result| result.map_err(std::io::Error::other));

        let stream_reader = StreamReader::new(byte_stream);
        let reader = tokio::io::BufReader::new(stream_reader);
        let mut lines = reader.lines();

        // Idle-line timeout. Nomad's event stream sends keepalive `{}` lines
        // roughly every 10s (configurable via NOMAD_HEARTBEAT_INTERVAL on the
        // server side; default ~10s). A 60s gap with zero data — not even a
        // heartbeat — means the TCP connection is dead-but-not-erroring (we
        // saw this happen on the Slurm executor watcher after laptop sleep:
        // the underlying socket "looks fine" but no bytes ever arrive). Time
        // out and return Err so `run_with_reconnect` tears down the HTTP
        // stream and reconnects from the saved checkpoint.
        const IDLE_LINE_TIMEOUT: Duration = Duration::from_secs(60);

        loop {
            let line = match tokio::time::timeout(IDLE_LINE_TIMEOUT, lines.next_line()).await {
                Ok(Ok(Some(line))) => line,
                Ok(Ok(None)) => {
                    tracing::info!("Nomad event stream ended (EOF) — reconnect loop will retry");
                    break;
                }
                Ok(Err(e)) => return Err(WatcherError::Io(e)),
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = IDLE_LINE_TIMEOUT.as_secs(),
                        "Nomad event stream idle (no lines, not even heartbeat) — triggering reconnect"
                    );
                    return Err(WatcherError::EventStream(format!(
                        "event stream idle for {}s — triggering reconnect",
                        IDLE_LINE_TIMEOUT.as_secs()
                    )));
                }
            };

            // Skip empty heartbeat lines
            if line.trim().is_empty() || line.trim() == "{}" {
                continue;
            }

            match serde_json::from_str::<EventStreamData>(&line) {
                Ok(data) => {
                    for entry in &data.events {
                        tracing::trace!(
                            topic = %entry.topic,
                            event_type = %entry.type_field,
                            key = %entry.key,
                            index = data.index,
                            has_alloc = entry.payload.allocation.is_some(),
                            "Event stream entry received"
                        );
                        if let Some(ref alloc) = entry.payload.allocation {
                            self.process_allocation(alloc).await;
                        }
                    }
                    // Checkpoint the Raft index after processing the batch.
                    if data.index > 0 {
                        self.save_checkpoint_index(data.index).await;
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        line_len = line.len(),
                        "Failed to parse event stream line (may be heartbeat)"
                    );
                }
            }
        }

        Ok(())
    }

    /// Fetch Petri routing metadata for a job, using the cache.
    ///
    /// Nomad's event stream does not embed the full Job in allocation events,
    /// so we fetch `GET /v1/job/{id}` on first encounter and cache the result.
    async fn get_petri_meta(&self, job_id: &str) -> Option<RoutingMeta> {
        // Fast path: already cached
        {
            let cache = self.meta_cache.read().await;
            if let Some(cached) = cache.get(job_id) {
                return cached.clone();
            }
        }

        // Fetch from Nomad
        let meta = self.fetch_job_meta(job_id).await;

        // Cache (even None — avoids re-fetching non-Petri jobs)
        {
            let mut cache = self.meta_cache.write().await;
            cache.insert(job_id.to_string(), meta.clone());
        }

        meta
    }

    /// Fetch job meta from `GET /v1/job/{id}`.
    async fn fetch_job_meta(&self, job_id: &str) -> Option<RoutingMeta> {
        let url = format!(
            "{}/v1/job/{}",
            self.config.addr.trim_end_matches('/'),
            job_id
        );

        let mut req = self.http_client.get(&url);
        if let Some(ref token) = self.config.token {
            req = req.header("X-Nomad-Token", token);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, job_id, "Failed to fetch job for meta lookup");
                return None;
            }
        };

        if !resp.status().is_success() {
            tracing::debug!(
                status = %resp.status(),
                job_id,
                "Job meta lookup returned non-200"
            );
            return None;
        }

        let job: Job = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, job_id, "Failed to parse job response");
                return None;
            }
        };

        RoutingMeta::from_meta_tags(&job.meta)
    }

    /// Process a single allocation event.
    ///
    /// Fetches Petri routing metadata (cached), maps task events to signals,
    /// and publishes to NATS with deterministic `Nats-Msg-Id` headers for
    /// JetStream deduplication.
    ///
    /// Nomad's event stream sends the full allocation snapshot (including the
    /// complete cumulative `task_state.events` list) on every update. We
    /// re-publish all mapped events each time — JetStream silently drops
    /// duplicates via the message ID, so no in-memory cursor is needed.
    /// This makes watcher restarts safe: re-processed events produce the
    /// same message IDs and are deduplicated at the stream level.
    async fn process_allocation(&self, alloc: &Allocation) {
        let meta = match self.get_petri_meta(&alloc.job_id).await {
            Some(m) => m,
            None => return, // Not a Petri-managed job
        };

        // Extract task events for the configured task name
        if let Some(task_state) = alloc.task_states.get(&self.config.task_name) {
            tracing::debug!(
                alloc_id = %alloc.id,
                job_id = %alloc.job_id,
                client_status = %alloc.client_status,
                event_count = task_state.events.len(),
                "Processing allocation task events"
            );

            let mut saw_terminal = false;
            for (idx, task_event) in task_state.events.iter().enumerate() {
                if let Some(job_status) = status_mapping::map_task_event(task_event) {
                    let target_place = meta.place_for_status(job_status.as_str());
                    // Deterministic ID: same alloc + same event index = same message.
                    // JetStream deduplicates within the stream's duplicate_window.
                    let msg_id = format!("{}-{}", alloc.id, idx);

                    tracing::debug!(
                        alloc_id = %alloc.id,
                        event_idx = idx,
                        event_type = %task_event.type_field,
                        mapped_status = %job_status.as_str(),
                        target_place = %target_place,
                        msg_id = %msg_id,
                        "Signaling task event"
                    );

                    if job_status.is_terminal() {
                        saw_terminal = true;
                    }

                    let signal = ExternalSignal {
                        source: "nomad".to_string(),
                        signal_key: meta.signal_key.clone(),
                        payload: serde_json::json!({
                            "source": "nomad",
                            "scheduler_job_id": alloc.job_id,
                            "allocation_id": alloc.id,
                            "job_status": job_status,
                            "exit_code": task_event.exit_code,
                            "message": task_event.display_message,
                            "node_id": alloc.node_id,
                            "node_name": alloc.node_name,
                        }),
                        timestamp: Utc::now(),
                        dedup_id: Some(msg_id.clone()),
                    };

                    let subject = signal_subject(&meta.net_id, target_place);
                    self.signal_publisher
                        .publish(&subject, &signal, &msg_id)
                        .await;
                }
            }
            // Evict cache entry once the job reaches a terminal state so stale
            // metadata doesn't accumulate for completed/failed jobs.
            if saw_terminal {
                self.meta_cache.write().await.remove(&alloc.job_id);
            }
        } else {
            // No task state for our task — fall back to alloc-level status
            if let Some(job_status) = status_mapping::map_alloc_client_status(alloc) {
                // Only signal terminal states at the alloc level to avoid noise
                if job_status.is_terminal() {
                    let target_place = meta.place_for_status(job_status.as_str());
                    let msg_id = format!("{}-alloc-{}", alloc.id, alloc.client_status);

                    let signal = ExternalSignal {
                        source: "nomad".to_string(),
                        signal_key: meta.signal_key.clone(),
                        payload: serde_json::json!({
                            "source": "nomad",
                            "scheduler_job_id": alloc.job_id,
                            "allocation_id": alloc.id,
                            "job_status": job_status,
                            "client_status": alloc.client_status,
                            "node_id": alloc.node_id,
                            "node_name": alloc.node_name,
                        }),
                        timestamp: Utc::now(),
                        dedup_id: Some(msg_id.clone()),
                    };

                    let subject = signal_subject(&meta.net_id, target_place);
                    self.signal_publisher
                        .publish(&subject, &signal, &msg_id)
                        .await;

                    // Evict cache — terminal alloc-level status means the job is done.
                    self.meta_cache.write().await.remove(&alloc.job_id);
                }
            }
        }
    }
}
