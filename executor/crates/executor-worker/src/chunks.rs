//! Inbound live data feed for reducer jobs.
//!
//! This is the executor-side receiver for the "live IPC reducer" capability:
//! data chunks produced upstream are fed INTO a still-running executor job and
//! surfaced to the child process over IPC as `for chunk in aithericon.chunks()`.
//!
//! Mirrors `cancel.rs` (`CancellationRegistry` / `NatsCancelListener`) but with
//! two deliberate differences justified by the data semantics:
//!
//! 1. **Ordered + lossless transport (JetStream, not core NATS).** Cancellation
//!    is idempotent and harmless to drop — a lost cancel just means the job runs
//!    to completion. A reducer chunk is the opposite: a dropped or reordered
//!    chunk silently corrupts the fold. So the feed rides a JetStream stream
//!    (`EXECUTOR_CHUNKS`, subject `executor.chunks.{execution_id}`) with
//!    `Nats-Msg-Id`-based dedup and a per-job ordered consumer. (plan decision D2)
//!
//! 2. **In-band EOF sentinel.** End-of-stream is a `ChunkMessage{is_eof:true}`
//!    on the SAME subject as the chunks (a separate close subject could race
//!    ahead of the last chunk). On EOF the per-job channel is closed, which ends
//!    the `StreamChunks` server-stream and stops the Python loop. (plan decision D3)
//!
//! Belt-and-suspenders: even with an ordered consumer, the listener applies a
//! small `sequence`-keyed reorder buffer before forwarding into the channel, so
//! an out-of-order engine firing can never reach the reducer out of order.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_nats::jetstream;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use aithericon_executor_ipc::proto::ChunkMessage;

/// Wire form of a chunk on the `EXECUTOR_CHUNKS` JetStream feed.
///
/// The IPC `ChunkMessage` is a tonic/prost-generated type and does NOT derive
/// serde, so the NATS payload is (de)serialized through this mirror struct and
/// converted. The engine's feed producer (Phase 3) serializes the identical
/// shape; keeping the field names aligned (`value_json`/`sequence`/`is_eof`)
/// makes the wire contract self-evident.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ChunkWire {
    #[serde(default)]
    pub value_json: String,
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub is_eof: bool,
}

impl From<ChunkWire> for ChunkMessage {
    fn from(w: ChunkWire) -> Self {
        ChunkMessage {
            value_json: w.value_json,
            sequence: w.sequence,
            is_eof: w.is_eof,
        }
    }
}

/// JetStream stream that carries inbound reducer chunks.
pub const CHUNKS_STREAM: &str = "EXECUTOR_CHUNKS";

/// Build the JetStream subject a single execution's chunks are published to.
/// Pattern: `executor.chunks.{execution_id}`.
pub fn chunks_subject(execution_id: &str) -> String {
    format!("executor.chunks.{execution_id}")
}

/// Subject filter the per-stream listener subscribes to.
/// `None` → `executor.chunks.*`; `Some("pfx")` → `pfx.executor.chunks.*`.
pub fn chunks_subject_filter(prefix: Option<&str>) -> String {
    match prefix {
        Some(pfx) => format!("{pfx}.executor.chunks.*"),
        None => "executor.chunks.*".to_string(),
    }
}

/// Shared registry mapping execution_id → the chunk sender feeding the IPC
/// sidecar's `StreamChunks` server-stream.
///
/// Thread-safe via `Mutex<HashMap>`. Contention is minimal (register on job
/// start, deregister on job end, `push` on each delivered chunk — a point
/// lookup), so a std Mutex with trivially short critical sections suffices.
/// Same shape as `CancellationRegistry`.
#[derive(Clone, Default)]
pub struct ChunkRegistry {
    inner: Arc<Mutex<HashMap<String, mpsc::Sender<ChunkMessage>>>>,
}

impl ChunkRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a chunk channel for an execution and return the receiver end
    /// to hand to the IPC sidecar. The channel is bounded so a slow/absent
    /// child applies backpressure to the listener rather than buffering
    /// unboundedly (a missing reducer child should stall, not OOM).
    ///
    /// If a sender already existed for this execution_id, it is replaced.
    pub fn register(&self, execution_id: &str) -> mpsc::Receiver<ChunkMessage> {
        let (tx, rx) = mpsc::channel(1024);
        let mut map = self.inner.lock().unwrap();
        map.insert(execution_id.to_string(), tx);
        rx
    }

    /// Deregister the channel (called when execution finishes, any outcome).
    pub fn deregister(&self, execution_id: &str) {
        let mut map = self.inner.lock().unwrap();
        map.remove(execution_id);
    }

    /// Look up the sender for an execution, if registered.
    fn sender(&self, execution_id: &str) -> Option<mpsc::Sender<ChunkMessage>> {
        self.inner.lock().unwrap().get(execution_id).cloned()
    }

    /// Number of currently active (registered) feeds.
    pub fn active_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// Push a chunk to the per-job channel. Returns `true` if the execution was
    /// found and the chunk was accepted, `false` if not found (job already
    /// finished, never opted in, or the child closed the stream).
    async fn push(&self, execution_id: &str, msg: ChunkMessage) -> bool {
        let Some(tx) = self.sender(execution_id) else {
            return false;
        };
        tx.send(msg).await.is_ok()
    }
}

/// Per-execution reorder buffer keyed on `sequence`.
///
/// The JetStream ordered consumer already delivers in publish order, but the
/// engine may publish chunks slightly out of order across distinct effect
/// firings; this buffer guarantees the reducer never sees a gap or a swap.
/// It releases the contiguous prefix starting at `next` whenever a new message
/// fills the gap, and passes the EOF sentinel through only after every chunk
/// before it has been released.
struct ReorderBuffer {
    next: u64,
    pending: std::collections::BTreeMap<u64, ChunkMessage>,
}

impl ReorderBuffer {
    fn new() -> Self {
        Self {
            next: 0,
            pending: std::collections::BTreeMap::new(),
        }
    }

    /// Accept a message, returning the contiguous run now ready to forward (in
    /// `sequence` order). A duplicate sequence (`< next` or already pending) is
    /// dropped — JetStream `Nats-Msg-Id` dedup is the primary guard; this is a
    /// secondary one.
    fn accept(&mut self, msg: ChunkMessage) -> Vec<ChunkMessage> {
        if msg.sequence < self.next || self.pending.contains_key(&msg.sequence) {
            // Already delivered or buffered — drop the duplicate.
            return Vec::new();
        }
        self.pending.insert(msg.sequence, msg);

        let mut ready = Vec::new();
        while let Some(m) = self.pending.remove(&self.next) {
            self.next += 1;
            ready.push(m);
        }
        ready
    }
}

/// Listens on the JetStream `EXECUTOR_CHUNKS` stream and forwards inbound chunks
/// to the matching execution's IPC sidecar via the `ChunkRegistry`.
///
/// Unlike `NatsCancelListener` (core NATS subscribe, lossy-OK), this uses an
/// ordered JetStream pull consumer so no chunk is ever dropped or reordered.
pub struct NatsChunkListener;

impl NatsChunkListener {
    /// Ensure the `EXECUTOR_CHUNKS` stream exists. Idempotent
    /// (`get_or_create_stream`). Called once at worker startup, like the
    /// `EXECUTOR_STATUS`/`EXECUTOR_EVENTS` streams in `StatusReporter::new`.
    pub async fn ensure_stream(
        jetstream: &jetstream::Context,
        replicas: usize,
    ) -> Result<(), async_nats::Error> {
        let _ = jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: CHUNKS_STREAM.to_string(),
                subjects: vec!["executor.chunks.>".to_string()],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_age: std::time::Duration::from_secs(24 * 60 * 60),
                // 2-minute dedup window for `Nats-Msg-Id = {execution_id}-{seq}`.
                duplicate_window: std::time::Duration::from_secs(120),
                num_replicas: replicas,
                storage: jetstream::stream::StorageType::File,
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// Start consuming the `EXECUTOR_CHUNKS` stream. Returns a `JoinHandle` for
    /// the listener task.
    ///
    /// `prefix` follows the same convention as the cancel listener:
    ///   - `None`  → consumes `executor.chunks.*`
    ///   - `Some("pfx")` → consumes `pfx.executor.chunks.*`
    ///
    /// A single shared consumer drains every execution's chunks; the
    /// `execution_id` parsed off the subject's last token routes each message
    /// to the right per-job channel + reorder buffer.
    pub async fn start(
        jetstream: jetstream::Context,
        registry: ChunkRegistry,
        prefix: Option<&str>,
        shutdown: CancellationToken,
    ) -> Result<JoinHandle<()>, async_nats::Error> {
        let filter = chunks_subject_filter(prefix);

        let stream = jetstream.get_stream(CHUNKS_STREAM).await?;
        let consumer = stream
            .create_consumer(jetstream::consumer::pull::Config {
                // Ephemeral pull consumer scoped to the chunk subjects. Deliver
                // everything; per-execution ordering is re-imposed by the
                // reorder buffer keyed on `sequence`.
                filter_subject: filter.clone(),
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            })
            .await?;

        let mut messages = consumer.messages().await?;
        info!(%filter, "NATS chunk listener started");

        let handle = tokio::spawn(async move {
            // One reorder buffer per execution_id.
            let mut buffers: HashMap<String, ReorderBuffer> = HashMap::new();
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => {
                        info!("NATS chunk listener shutting down");
                        break;
                    }
                    next = messages.next() => {
                        let Some(msg) = next else {
                            warn!("NATS chunk subscription closed");
                            break;
                        };
                        let msg = match msg {
                            Ok(m) => m,
                            Err(e) => {
                                warn!(error = %e, "chunk message error");
                                continue;
                            }
                        };

                        // execution_id is the final subject token.
                        let Some(execution_id) =
                            msg.subject.as_str().split('.').next_back().map(str::to_string)
                        else {
                            let _ = msg.ack().await;
                            continue;
                        };

                        let chunk: ChunkMessage =
                            match serde_json::from_slice::<ChunkWire>(&msg.payload) {
                                Ok(c) => c.into(),
                                Err(e) => {
                                    warn!(%execution_id, error = %e, "failed to decode chunk message");
                                    let _ = msg.ack().await;
                                    continue;
                                }
                            };

                        let buf = buffers
                            .entry(execution_id.clone())
                            .or_insert_with(ReorderBuffer::new);
                        let ready = buf.accept(chunk);

                        let mut closed = false;
                        for c in ready {
                            let is_eof = c.is_eof;
                            if !registry.push(&execution_id, c).await {
                                // No live child (already finished / never opted
                                // in). Drop and stop tracking this execution.
                                debug!(
                                    %execution_id,
                                    "chunk for unknown/closed execution dropped"
                                );
                                closed = true;
                                break;
                            }
                            if is_eof {
                                // EOF forwarded — the sidecar will end the
                                // server-stream; drop this execution's channel.
                                registry.deregister(&execution_id);
                                closed = true;
                                break;
                            }
                        }
                        if closed {
                            buffers.remove(&execution_id);
                        }

                        let _ = msg.ack().await;
                    }
                }
            }
        });

        Ok(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(seq: u64, eof: bool) -> ChunkMessage {
        ChunkMessage {
            value_json: if eof { String::new() } else { format!("{seq}") },
            sequence: seq,
            is_eof: eof,
        }
    }

    #[tokio::test]
    async fn register_push_deregister() {
        let registry = ChunkRegistry::new();
        let mut rx = registry.register("exec-1");
        assert_eq!(registry.active_count(), 1);

        assert!(registry.push("exec-1", chunk(0, false)).await);
        let got = rx.recv().await.expect("chunk delivered");
        assert_eq!(got.sequence, 0);

        registry.deregister("exec-1");
        assert_eq!(registry.active_count(), 0);
        assert!(!registry.push("exec-1", chunk(1, false)).await);
    }

    #[tokio::test]
    async fn push_unknown_is_noop() {
        let registry = ChunkRegistry::new();
        assert!(!registry.push("nope", chunk(0, false)).await);
    }

    #[test]
    fn reorder_releases_contiguous_prefix_in_order() {
        let mut buf = ReorderBuffer::new();
        // Out of order: 2 then 0 then 1.
        assert!(buf.accept(chunk(2, false)).is_empty());
        let r0 = buf.accept(chunk(0, false));
        assert_eq!(r0.iter().map(|m| m.sequence).collect::<Vec<_>>(), vec![0]);
        // Filling seq 1 releases 1 AND the buffered 2.
        let r1 = buf.accept(chunk(1, false));
        assert_eq!(r1.iter().map(|m| m.sequence).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn reorder_drops_duplicates() {
        let mut buf = ReorderBuffer::new();
        assert_eq!(buf.accept(chunk(0, false)).len(), 1);
        // Re-delivery of seq 0 (already released) is dropped.
        assert!(buf.accept(chunk(0, false)).is_empty());
    }

    #[test]
    fn reorder_eof_passes_after_chunks() {
        let mut buf = ReorderBuffer::new();
        // EOF at seq 2 arrives before seq 0,1 — must be held.
        assert!(buf.accept(chunk(2, true)).is_empty());
        assert_eq!(buf.accept(chunk(0, false)).len(), 1);
        let r = buf.accept(chunk(1, false));
        assert_eq!(r.len(), 2);
        assert!(r[1].is_eof, "EOF released last, after seq 1");
    }
}
