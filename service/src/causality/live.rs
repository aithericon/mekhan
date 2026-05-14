//! Live metric/log broadcast channel for SSE fan-out.
//!
//! Mirrors the pattern used by `petri-lab/core-engine/crates/executor/src/watcher.rs`:
//! - `broadcast::Sender` for live fan-out to many SSE clients
//! - `RwLock<Vec<..>>` ring buffer (cap 10k) for backfill snapshots on reconnect
//! - `AtomicU64` monotonic sequence so clients can dedup + detect gaps
//!
//! Events are emitted from `causality::ingest` immediately after a successful
//! INSERT into `hpi_metrics` / `hpi_logs`, preserving the single-consumer ingest
//! invariant (we tap the consumer, we do not spawn a parallel one).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::broadcast;
use utoipa::ToSchema;

const SSE_BUFFER_CAP: usize = 10_000;
const BROADCAST_CAP: usize = 1024;
const ARTIFACT_BUFFER_CAP: usize = 2_000;
const ARTIFACT_BROADCAST_CAP: usize = 256;

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct LiveMetricEvent {
    pub seq: u64,
    pub process_id: String,
    pub signal_key: Option<String>,
    pub key: String,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct LiveLogEvent {
    pub seq: u64,
    pub process_id: String,
    pub signal_key: Option<String>,
    pub level: String,
    pub source: Option<String>,
    pub message: String,
    pub detail: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}

/// Emitted when a catalogue entry INSERT succeeds for a process-tagged
/// artifact. The SSE handler filters by `process_id` and optionally by
/// `category` / `user_metadata.render_hint` before forwarding to clients.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct LiveArtifactEvent {
    pub seq: u64,
    pub process_id: String,
    pub artifact_id: String,
    pub execution_id: String,
    pub name: String,
    pub category: String,
    pub filename: String,
    pub mime_type: Option<String>,
    pub storage_path: Option<String>,
    pub size_bytes: Option<i64>,
    pub process_step: Option<String>,
    pub signal_key: Option<String>,
    pub user_metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

pub struct LiveBroadcasts {
    metrics_tx: broadcast::Sender<LiveMetricEvent>,
    metrics_buf: RwLock<Vec<LiveMetricEvent>>,
    metrics_seq: AtomicU64,
    logs_tx: broadcast::Sender<LiveLogEvent>,
    logs_buf: RwLock<Vec<LiveLogEvent>>,
    logs_seq: AtomicU64,
    artifacts_tx: broadcast::Sender<LiveArtifactEvent>,
    artifacts_buf: RwLock<Vec<LiveArtifactEvent>>,
    artifacts_seq: AtomicU64,
}

impl LiveBroadcasts {
    pub fn new() -> Arc<Self> {
        let (metrics_tx, _) = broadcast::channel(BROADCAST_CAP);
        let (logs_tx, _) = broadcast::channel(BROADCAST_CAP);
        let (artifacts_tx, _) = broadcast::channel(ARTIFACT_BROADCAST_CAP);
        Arc::new(Self {
            metrics_tx,
            metrics_buf: RwLock::new(Vec::with_capacity(SSE_BUFFER_CAP)),
            metrics_seq: AtomicU64::new(0),
            logs_tx,
            logs_buf: RwLock::new(Vec::with_capacity(SSE_BUFFER_CAP)),
            logs_seq: AtomicU64::new(0),
            artifacts_tx,
            artifacts_buf: RwLock::new(Vec::with_capacity(ARTIFACT_BUFFER_CAP)),
            artifacts_seq: AtomicU64::new(0),
        })
    }

    pub fn subscribe_metrics(&self) -> broadcast::Receiver<LiveMetricEvent> {
        self.metrics_tx.subscribe()
    }

    pub fn subscribe_logs(&self) -> broadcast::Receiver<LiveLogEvent> {
        self.logs_tx.subscribe()
    }

    pub fn subscribe_artifacts(&self) -> broadcast::Receiver<LiveArtifactEvent> {
        self.artifacts_tx.subscribe()
    }

    /// Snapshot of the metrics ring buffer.
    /// Returns events in order (oldest first) and the first buffered seq (or 0 if empty).
    pub fn metrics_snapshot(&self) -> (Vec<LiveMetricEvent>, u64) {
        let buf = self.metrics_buf.read().unwrap();
        let first_seq = buf.first().map(|e| e.seq).unwrap_or(0);
        (buf.clone(), first_seq)
    }

    pub fn logs_snapshot(&self) -> (Vec<LiveLogEvent>, u64) {
        let buf = self.logs_buf.read().unwrap();
        let first_seq = buf.first().map(|e| e.seq).unwrap_or(0);
        (buf.clone(), first_seq)
    }

    pub fn artifacts_snapshot(&self) -> (Vec<LiveArtifactEvent>, u64) {
        let buf = self.artifacts_buf.read().unwrap();
        let first_seq = buf.first().map(|e| e.seq).unwrap_or(0);
        (buf.clone(), first_seq)
    }

    pub fn emit_metric(
        &self,
        process_id: String,
        signal_key: Option<String>,
        key: String,
        value: f64,
        timestamp: DateTime<Utc>,
    ) {
        let seq = self.metrics_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let event = LiveMetricEvent {
            seq,
            process_id,
            signal_key,
            key,
            value,
            timestamp,
        };
        {
            let mut buf = self.metrics_buf.write().unwrap();
            buf.push(event.clone());
            let overflow = buf.len().saturating_sub(SSE_BUFFER_CAP);
            if overflow > 0 {
                buf.drain(..overflow);
            }
        }
        let _ = self.metrics_tx.send(event);
    }

    pub fn emit_log(
        &self,
        process_id: String,
        signal_key: Option<String>,
        level: String,
        source: Option<String>,
        message: String,
        detail: serde_json::Value,
        timestamp: DateTime<Utc>,
    ) {
        let seq = self.logs_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let event = LiveLogEvent {
            seq,
            process_id,
            signal_key,
            level,
            source,
            message,
            detail,
            timestamp,
        };
        {
            let mut buf = self.logs_buf.write().unwrap();
            buf.push(event.clone());
            let overflow = buf.len().saturating_sub(SSE_BUFFER_CAP);
            if overflow > 0 {
                buf.drain(..overflow);
            }
        }
        let _ = self.logs_tx.send(event);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn emit_artifact(
        &self,
        process_id: String,
        artifact_id: String,
        execution_id: String,
        name: String,
        category: String,
        filename: String,
        mime_type: Option<String>,
        storage_path: Option<String>,
        size_bytes: Option<i64>,
        process_step: Option<String>,
        signal_key: Option<String>,
        user_metadata: serde_json::Value,
        created_at: DateTime<Utc>,
    ) {
        let seq = self.artifacts_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let event = LiveArtifactEvent {
            seq,
            process_id,
            artifact_id,
            execution_id,
            name,
            category,
            filename,
            mime_type,
            storage_path,
            size_bytes,
            process_step,
            signal_key,
            user_metadata,
            created_at,
        };
        {
            let mut buf = self.artifacts_buf.write().unwrap();
            buf.push(event.clone());
            let overflow = buf.len().saturating_sub(ARTIFACT_BUFFER_CAP);
            if overflow > 0 {
                buf.drain(..overflow);
            }
        }
        let _ = self.artifacts_tx.send(event);
    }
}
