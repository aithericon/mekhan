//! Data-plane byte transport for streaming channels (docs/25 §6).
//!
//! A data channel is out-of-band bytes flowing producer→consumer over a
//! pluggable transport, framed as a sequence of **binary envelopes**
//! (`ChunkMessage { seq, content_type, payload, is_eof }`). The net never sees
//! these bytes — it only sees the `open`/`close` control brackets (1a path).
//!
//! This module provides:
//!
//!   * the [`StreamTransport`] **port** (write/close for a producer, subscribe
//!     for a consumer), and
//!   * one [`JetStreamTransport`] adapter (the v1, reliable-ordered impl) over a
//!     JetStream stream (`EXECUTOR_DATASTREAM`, subject
//!     `executor.datastream.{execution_id}.{channel}`).
//!
//! The data-plane **consumer** (`for elem in aithericon.stream(name)`) reads the
//! PRODUCER's datastream subject (carried in the `open` descriptor the engine
//! delivered as this job's input): the IPC sidecar's `StreamChunks` handler calls
//! [`JetStreamTransport::subscribe`] on that subject and relays each decoded
//! envelope back over the server-stream.
//!
//! The transport gives the consumer two guarantees:
//!
//! 1. **Ordered + lossless transport (JetStream, not core NATS).** A dropped or
//!    reordered element silently corrupts the consumer. So the bytes ride a
//!    JetStream stream with `Nats-Msg-Id`-based dedup and a per-stream ordered
//!    pull consumer.
//! 2. **In-band EOF sentinel.** End-of-stream is a `ChunkMessage{is_eof:true}`
//!    on the SAME subject as the data (a separate close subject could race ahead
//!    of the last element).
//!
//! Belt-and-suspenders: a small `seq`-keyed [`ReorderBuffer`] re-imposes order
//! before forwarding into the consumer, so an out-of-order delivery can never
//! reach the consumer out of order.

use async_nats::jetstream;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use aithericon_executor_ipc::proto::ChunkMessage;

/// NATS header carrying the envelope's `seq`.
const HDR_SEQ: &str = "X-Chunk-Seq";
/// NATS header carrying the envelope's `content_type`.
const HDR_CONTENT_TYPE: &str = "X-Chunk-Content-Type";
/// NATS header marking the in-band EOF sentinel (`"1"` when set).
const HDR_EOF: &str = "X-Chunk-Eof";

/// Encode a binary envelope to its NATS wire form: metadata in headers, the raw
/// `payload` bytes as the message body (no JSON-array-of-bytes bloat, no base64).
/// The `Nats-Msg-Id` (caller-supplied, keyed on subject+seq) drives JetStream
/// dedup.
pub fn encode_envelope(msg: &ChunkMessage, msg_id: &str) -> (async_nats::HeaderMap, Vec<u8>) {
    let mut headers = async_nats::HeaderMap::new();
    headers.insert("Nats-Msg-Id", msg_id);
    headers.insert(HDR_SEQ, msg.seq.to_string().as_str());
    headers.insert(HDR_CONTENT_TYPE, msg.content_type.as_str());
    if msg.is_eof {
        headers.insert(HDR_EOF, "1");
    }
    (headers, msg.payload.clone())
}

/// Decode a binary envelope from the NATS headers + raw body of a JetStream
/// message. A missing/garbled `seq` header defaults to 0; a missing EOF header
/// means a data element.
pub fn decode_envelope(headers: Option<&async_nats::HeaderMap>, body: &[u8]) -> ChunkMessage {
    let seq = headers
        .and_then(|h| h.get(HDR_SEQ))
        .and_then(|v| v.as_str().parse::<u64>().ok())
        .unwrap_or(0);
    let content_type = headers
        .and_then(|h| h.get(HDR_CONTENT_TYPE))
        .map(|v| v.as_str().to_string())
        .unwrap_or_default();
    let is_eof = headers
        .and_then(|h| h.get(HDR_EOF))
        .map(|v| v.as_str() == "1")
        .unwrap_or(false);
    ChunkMessage {
        seq,
        content_type,
        payload: body.to_vec(),
        is_eof,
    }
}

/// JetStream stream that carries data-plane byte streams.
pub const DATASTREAM_STREAM: &str = "EXECUTOR_DATASTREAM";

/// Build the JetStream subject a single channel's byte stream is published to.
/// Pattern: `executor.datastream.{execution_id}.{channel}`.
pub fn datastream_subject(execution_id: &str, channel: &str) -> String {
    format!("executor.datastream.{execution_id}.{channel}")
}

/// The data-plane byte transport **port** (docs/25 §6). Adapters are pluggable
/// (JetStream is the v1 impl; S3 / lossy-latest are P2). A producer `write`s
/// framed envelopes onto a subject and `close`s with a final EOF; a consumer
/// `subscribe`s and drains envelopes in order.
#[async_trait::async_trait]
pub trait StreamTransport: Send + Sync {
    /// Publish one binary envelope onto `subject` (producer write). Ordered:
    /// awaiting the ack lets the transport's window back-pressure a too-fast
    /// producer (docs/25 §5). `seq` keys per-subject dedup.
    async fn write(&self, subject: &str, envelope: &ChunkMessage)
        -> Result<(), async_nats::Error>;

    /// Publish the in-band EOF sentinel onto `subject` (producer close). The
    /// consumer drains up to and including this and ends its stream.
    async fn close(&self, subject: &str, final_seq: u64) -> Result<(), async_nats::Error>;

    /// Subscribe to `subject` and forward each decoded envelope into `sink` in
    /// `seq` order until EOF (consumer read). Spawns a task; returns its handle.
    async fn subscribe(
        &self,
        subject: String,
        sink: mpsc::Sender<ChunkMessage>,
        shutdown: CancellationToken,
    ) -> Result<JoinHandle<()>, async_nats::Error>;
}

/// JetStream adapter — the v1, reliable-ordered [`StreamTransport`]. Wraps the
/// `EXECUTOR_DATASTREAM` stream; publish uses `Nats-Msg-Id` dedup, subscribe
/// uses an ordered pull consumer + a [`ReorderBuffer`].
#[derive(Clone)]
pub struct JetStreamTransport {
    jetstream: jetstream::Context,
}

impl JetStreamTransport {
    pub fn new(jetstream: jetstream::Context) -> Self {
        Self { jetstream }
    }

    /// Ensure the `EXECUTOR_DATASTREAM` stream exists. Idempotent
    /// (`get_or_create_stream`). Called once at worker startup, like the
    /// `EXECUTOR_STATUS`/`EXECUTOR_EVENTS` streams.
    pub async fn ensure_stream(
        jetstream: &jetstream::Context,
        replicas: usize,
    ) -> Result<(), async_nats::Error> {
        let _ = jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: DATASTREAM_STREAM.to_string(),
                subjects: vec!["executor.datastream.>".to_string()],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_age: std::time::Duration::from_secs(24 * 60 * 60),
                // 2-minute dedup window for `Nats-Msg-Id = {subject}-{seq}`.
                duplicate_window: std::time::Duration::from_secs(120),
                num_replicas: replicas,
                storage: jetstream::stream::StorageType::File,
                ..Default::default()
            })
            .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl StreamTransport for JetStreamTransport {
    async fn write(
        &self,
        subject: &str,
        envelope: &ChunkMessage,
    ) -> Result<(), async_nats::Error> {
        let msg_id = format!("{subject}-{}", envelope.seq);
        let (headers, body) = encode_envelope(envelope, &msg_id);
        let ack = self
            .jetstream
            .publish_with_headers(subject.to_string(), headers, body.into())
            .await?;
        // Await the ack so the stream's bounded window back-pressures a too-fast
        // producer (docs/25 §5 — backpressure lives in the data plane).
        ack.await?;
        Ok(())
    }

    async fn close(&self, subject: &str, final_seq: u64) -> Result<(), async_nats::Error> {
        let eof = ChunkMessage {
            seq: final_seq,
            content_type: String::new(),
            payload: Vec::new(),
            is_eof: true,
        };
        self.write(subject, &eof).await
    }

    async fn subscribe(
        &self,
        subject: String,
        sink: mpsc::Sender<ChunkMessage>,
        shutdown: CancellationToken,
    ) -> Result<JoinHandle<()>, async_nats::Error> {
        let stream = self.jetstream.get_stream(DATASTREAM_STREAM).await?;
        let consumer = stream
            .create_consumer(jetstream::consumer::pull::Config {
                filter_subject: subject.clone(),
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            })
            .await?;
        let mut messages = consumer.messages().await?;
        debug!(%subject, "datastream consumer subscribed");

        let handle = tokio::spawn(async move {
            let mut buf = ReorderBuffer::new();
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    next = messages.next() => {
                        let Some(msg) = next else { break; };
                        let msg = match msg {
                            Ok(m) => m,
                            Err(e) => { warn!(error = %e, "datastream message error"); continue; }
                        };
                        let env = decode_envelope(msg.headers.as_ref(), &msg.payload);
                        let ready = buf.accept(env);
                        let mut done = false;
                        for c in ready {
                            let is_eof = c.is_eof;
                            if sink.send(c).await.is_err() {
                                done = true;
                                break;
                            }
                            if is_eof {
                                done = true;
                                break;
                            }
                        }
                        let _ = msg.ack().await;
                        if done { break; }
                    }
                }
            }
        });
        Ok(handle)
    }
}

/// Per-stream reorder buffer keyed on `seq`.
///
/// The JetStream ordered consumer already delivers in publish order, but a
/// producer may publish across distinct firings/tasks slightly out of order;
/// this buffer guarantees the consumer never sees a gap or a swap. It releases
/// the contiguous prefix starting at `next` whenever a new message fills the
/// gap, and passes the EOF sentinel through only after every element before it
/// has been released.
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
    /// `seq` order). A duplicate seq (`< next` or already pending) is dropped —
    /// JetStream `Nats-Msg-Id` dedup is the primary guard; this is a secondary
    /// one.
    fn accept(&mut self, msg: ChunkMessage) -> Vec<ChunkMessage> {
        if msg.seq < self.next || self.pending.contains_key(&msg.seq) {
            // Already delivered or buffered — drop the duplicate.
            return Vec::new();
        }
        self.pending.insert(msg.seq, msg);

        let mut ready = Vec::new();
        while let Some(m) = self.pending.remove(&self.next) {
            self.next += 1;
            ready.push(m);
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(seq: u64, eof: bool) -> ChunkMessage {
        ChunkMessage {
            seq,
            content_type: if eof {
                String::new()
            } else {
                "application/json".to_string()
            },
            payload: if eof {
                Vec::new()
            } else {
                format!("{seq}").into_bytes()
            },
            is_eof: eof,
        }
    }

    #[test]
    fn envelope_header_roundtrip() {
        let env = ChunkMessage {
            seq: 7,
            content_type: "image/jpeg".to_string(),
            payload: vec![0xff, 0xd8, 0x00, 0x10],
            is_eof: false,
        };
        let (headers, body) = encode_envelope(&env, "subj-7");
        let decoded = decode_envelope(Some(&headers), &body);
        assert_eq!(decoded.seq, 7);
        assert_eq!(decoded.content_type, "image/jpeg");
        assert_eq!(decoded.payload, vec![0xff, 0xd8, 0x00, 0x10]);
        assert!(!decoded.is_eof);
    }

    #[test]
    fn envelope_eof_roundtrip() {
        let env = ChunkMessage {
            seq: 3,
            content_type: String::new(),
            payload: Vec::new(),
            is_eof: true,
        };
        let (headers, body) = encode_envelope(&env, "subj-3");
        let decoded = decode_envelope(Some(&headers), &body);
        assert_eq!(decoded.seq, 3);
        assert!(decoded.is_eof);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn reorder_releases_contiguous_prefix_in_order() {
        let mut buf = ReorderBuffer::new();
        // Out of order: 2 then 0 then 1.
        assert!(buf.accept(chunk(2, false)).is_empty());
        let r0 = buf.accept(chunk(0, false));
        assert_eq!(r0.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![0]);
        // Filling seq 1 releases 1 AND the buffered 2.
        let r1 = buf.accept(chunk(1, false));
        assert_eq!(r1.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![1, 2]);
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

    #[test]
    fn datastream_subject_scheme() {
        assert_eq!(
            datastream_subject("exec-9", "frames"),
            "executor.datastream.exec-9.frames"
        );
    }
}
