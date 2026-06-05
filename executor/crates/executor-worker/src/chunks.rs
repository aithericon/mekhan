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

use std::sync::Arc;

use async_nats::{jetstream, Client};
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
/// and selected by a channel's declared transport tag; a producer `write`s
/// framed envelopes and `close`s with a final EOF, a consumer `subscribe`s and
/// drains envelopes in order.
///
/// The `locator` is **opaque to the port** — it is whatever the producer's `open`
/// descriptor carried (today `executor.datastream.{exec}.{channel}`), and each
/// adapter interprets it in its OWN address space: the NATS adapters treat it as
/// a subject, the object-store adapter treats it as a key prefix. That opacity is
/// the load-bearing decoupling — the port assumes no NATS (or any) transport
/// shape, so a non-pub/sub transport (S3) drops in with no port change.
#[async_trait::async_trait]
pub trait StreamTransport: Send + Sync {
    /// Publish one binary envelope at `locator` (producer write). Where the
    /// adapter awaits durable acceptance (JetStream ack, an object PUT) this
    /// back-pressures a too-fast producer (docs/25 §5). `seq` keys per-locator
    /// dedup/ordering.
    async fn write(&self, locator: &str, envelope: &ChunkMessage)
        -> Result<(), async_nats::Error>;

    /// Publish the in-band EOF sentinel at `locator` (producer close). The
    /// consumer drains up to and including this and ends its stream.
    async fn close(&self, locator: &str, final_seq: u64) -> Result<(), async_nats::Error>;

    /// Subscribe to `locator` and forward each decoded envelope into `sink` in
    /// `seq` order until EOF (consumer read). Spawns a task; returns its handle.
    async fn subscribe(
        &self,
        locator: String,
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

/// Lossy-latest core-NATS adapter — the semantic OPPOSITE of
/// [`JetStreamTransport`], and what proves the [`StreamTransport`] dispatch seam
/// is real (two adapters, selected by the channel's declared transport tag, over
/// the SAME producer/consumer SDK).
///
/// Bytes ride PLAIN core NATS (no JetStream): no persistence, no replay, no
/// per-element ack, and — on the read side — NO [`ReorderBuffer`]. The
/// consequences are the point, not a defect:
///
///   * **No back-pressure.** `write` publishes fire-and-forget and returns
///     immediately (it never awaits an ack), so a fast producer is never slowed
///     by a slow consumer — the producer's cadence is preserved.
///   * **Lossy / latest-wins.** A consumer only receives elements published
///     *after* it subscribed; one that joins late or falls behind silently
///     misses elements. This is the right transport for a high-rate liveness
///     stream (camera frames, telemetry) where currency beats completeness.
///
/// Same in-band EOF sentinel as JetStream (a `ChunkMessage{is_eof:true}` on the
/// data subject) so the consumer's read loop terminates identically.
#[derive(Clone)]
pub struct NatsLatestTransport {
    nats: Client,
}

impl NatsLatestTransport {
    pub fn new(nats: Client) -> Self {
        Self { nats }
    }
}

#[async_trait::async_trait]
impl StreamTransport for NatsLatestTransport {
    async fn write(
        &self,
        subject: &str,
        envelope: &ChunkMessage,
    ) -> Result<(), async_nats::Error> {
        // Fire-and-forget core publish: NO ack await ⇒ NO back-pressure (lossy).
        // The `Nats-Msg-Id` header is harmless here (core NATS ignores it); we
        // reuse the shared encoder so the wire framing matches JetStream exactly.
        let msg_id = format!("{subject}-{}", envelope.seq);
        let (headers, body) = encode_envelope(envelope, &msg_id);
        self.nats
            .publish_with_headers(subject.to_string(), headers, body.into())
            .await?;
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
        let mut sub = self.nats.subscribe(subject.clone()).await?;
        debug!(%subject, "nats-latest consumer subscribed (lossy, no replay)");

        let handle = tokio::spawn(async move {
            // NO reorder buffer: forward each message the instant it arrives.
            // Core NATS delivers a single subject's messages in publish order but
            // offers no gap-fill or dedup — that best-effort ordering IS the
            // latest-wins contract.
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    next = sub.next() => {
                        let Some(msg) = next else { break; };
                        let env = decode_envelope(msg.headers.as_ref(), &msg.payload);
                        let is_eof = env.is_eof;
                        if sink.send(env).await.is_err() {
                            break;
                        }
                        if is_eof {
                            break;
                        }
                    }
                }
            }
        });
        Ok(handle)
    }
}

/// Object-store adapter — a DURABLE, replayable [`StreamTransport`] over any
/// OpenDAL-backed object store (S3 / GCS / Azure / local-fs), selected by the
/// `transport: "s3"` tag. This is the adapter that proves the port is genuinely
/// transport-SHAPE-agnostic, not just NATS-flavour-agnostic: where the NATS
/// adapters are pub/sub (a subject, live subscribers, a JetStream log), this one
/// is a key/value store with **no subscribe primitive at all**. The same
/// producer (`open_output(...).write`) and consumer (`for x in stream(...)`) SDK
/// drive it unchanged — only the executor-side adapter differs.
///
/// Wire model: each envelope becomes ONE object at `{prefix}{locator}/c{seq:020}`
/// (the opaque `locator`'s dots become path separators, so a stream is an
/// isolated "directory" of per-`seq` chunk objects). `write` awaits the PUT — the
/// object store's durability ack IS the back-pressure (docs/25 §5). The EOF
/// sentinel is the object at `c{final_seq:020}` with `is_eof`. There is no live
/// fan-out, so `subscribe` **polls** the next key in order: it reads `c0, c1, …`,
/// blocking-with-backoff on a not-yet-written key, until it reads the EOF object.
/// That gives the consumer the OPPOSITE guarantees of `nats-latest`: lossless,
/// strictly ordered, and fully **replayable** — a consumer that starts long after
/// the producer finished still reads every element from `c0`. The right transport
/// for large/durable blobs (model checkpoints, datasets, archived media) where
/// completeness and replay beat currency.
#[cfg(feature = "opendal")]
#[derive(Clone)]
pub struct S3Transport {
    operator: ::opendal::Operator,
    /// Store-level key prefix (the executor's `[storage].prefix`), prepended to
    /// every stream so datastream objects sit beside (not on top of) artifacts.
    prefix: String,
}

#[cfg(feature = "opendal")]
impl S3Transport {
    pub fn new(operator: ::opendal::Operator, prefix: String) -> Self {
        Self { operator, prefix }
    }
}

/// Object-key prefix for a stream's opaque `locator`. The locator is the same
/// string the NATS adapters use as a subject; here its dots become path
/// separators so each stream is an isolated directory of chunk objects.
#[cfg(feature = "opendal")]
fn s3_key_prefix(prefix: &str, locator: &str) -> String {
    format!("{}{}/", prefix, locator.replace('.', "/"))
}

/// The per-`seq` chunk object key (zero-padded so lexical order == numeric).
#[cfg(feature = "opendal")]
fn s3_chunk_key(prefix: &str, locator: &str, seq: u64) -> String {
    format!("{}c{:020}", s3_key_prefix(prefix, locator), seq)
}

/// Frame one envelope into an object body: `[ct_len: u16 LE][ct][is_eof: u8][payload…]`.
/// `seq` rides the object KEY, not the body.
#[cfg(feature = "opendal")]
fn s3_encode(env: &ChunkMessage) -> Vec<u8> {
    let ct = env.content_type.as_bytes();
    let ct_len = ct.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(2 + ct_len + 1 + env.payload.len());
    out.extend_from_slice(&(ct_len as u16).to_le_bytes());
    out.extend_from_slice(&ct[..ct_len]);
    out.push(u8::from(env.is_eof));
    out.extend_from_slice(&env.payload);
    out
}

/// Inverse of [`s3_encode`]; `seq` comes from the object key the caller read.
#[cfg(feature = "opendal")]
fn s3_decode(seq: u64, body: &[u8]) -> ChunkMessage {
    if body.len() < 3 {
        return ChunkMessage {
            seq,
            content_type: String::new(),
            payload: body.to_vec(),
            is_eof: false,
        };
    }
    let ct_len = u16::from_le_bytes([body[0], body[1]]) as usize;
    let ct_end = (2 + ct_len).min(body.len());
    let content_type = String::from_utf8_lossy(&body[2..ct_end]).into_owned();
    let is_eof = body.get(ct_end).map(|b| *b == 1).unwrap_or(false);
    let payload = body.get(ct_end + 1..).unwrap_or(&[]).to_vec();
    ChunkMessage {
        seq,
        content_type,
        payload,
        is_eof,
    }
}

/// Poll cadence when the next chunk object has not been written yet. Snappy
/// enough for live producer→consumer overlap, cheap enough to idle on.
#[cfg(feature = "opendal")]
const S3_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

#[cfg(feature = "opendal")]
#[async_trait::async_trait]
impl StreamTransport for S3Transport {
    async fn write(
        &self,
        locator: &str,
        envelope: &ChunkMessage,
    ) -> Result<(), async_nats::Error> {
        let key = s3_chunk_key(&self.prefix, locator, envelope.seq);
        // Await the PUT — durable acceptance IS the back-pressure here.
        self.operator.write(&key, s3_encode(envelope)).await?;
        Ok(())
    }

    async fn close(&self, locator: &str, final_seq: u64) -> Result<(), async_nats::Error> {
        let eof = ChunkMessage {
            seq: final_seq,
            content_type: String::new(),
            payload: Vec::new(),
            is_eof: true,
        };
        self.write(locator, &eof).await
    }

    async fn subscribe(
        &self,
        locator: String,
        sink: mpsc::Sender<ChunkMessage>,
        shutdown: CancellationToken,
    ) -> Result<JoinHandle<()>, async_nats::Error> {
        let operator = self.operator.clone();
        let prefix = self.prefix.clone();
        debug!(%locator, "object-store consumer subscribed (durable, replay from c0)");

        let handle = tokio::spawn(async move {
            // No subscribe primitive — drain the chunk objects in `seq` order,
            // polling the next key until it lands (or we're torn down). Lossless
            // + ordered by construction; no ReorderBuffer needed.
            let mut next: u64 = 0;
            loop {
                if shutdown.is_cancelled() {
                    break;
                }
                let key = s3_chunk_key(&prefix, &locator, next);
                match operator.read(&key).await {
                    Ok(buf) => {
                        let env = s3_decode(next, &buf.to_vec());
                        let is_eof = env.is_eof;
                        if sink.send(env).await.is_err() {
                            break;
                        }
                        if is_eof {
                            break;
                        }
                        next += 1;
                    }
                    Err(e) if e.kind() == ::opendal::ErrorKind::NotFound => {
                        // Not written yet — back off, then retry the same `seq`.
                        tokio::select! {
                            biased;
                            _ = shutdown.cancelled() => break,
                            _ = tokio::time::sleep(S3_POLL_INTERVAL) => {}
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, %key, "object-store read error — ending stream");
                        break;
                    }
                }
            }
        });
        Ok(handle)
    }
}

/// Resolves a channel's declared transport tag to its [`StreamTransport`]
/// adapter. ONE registry per worker, cloned cheaply (Arc-backed) into each job's
/// IPC sidecar. The producer's executor dispatches on the manifest entry's
/// `transport`; the consumer's executor dispatches on the tag lifted from the
/// `open` descriptor — so the same channel is read the way it was written,
/// selected by data, not hardcoded.
#[derive(Clone)]
pub struct TransportRegistry {
    jetstream: Arc<JetStreamTransport>,
    nats_latest: Arc<NatsLatestTransport>,
    /// Durable object-store adapter (`transport: "s3"`). `None` until an OpenDAL
    /// operator is attached via [`TransportRegistry::with_object_store`] — so a
    /// `transport: "s3"` channel on a worker with no `[storage]` configured fails
    /// loudly (`get` → `None`) instead of silently mis-routing. Only present with
    /// the `opendal` feature.
    #[cfg(feature = "opendal")]
    object_store: Option<Arc<S3Transport>>,
}

impl TransportRegistry {
    /// Build the registry from the worker's JetStream context + core NATS client
    /// (both already connected at worker startup — no second connection). The
    /// object-store adapter is opt-in via [`with_object_store`](Self::with_object_store).
    pub fn new(jetstream: jetstream::Context, nats: Client) -> Self {
        Self {
            jetstream: Arc::new(JetStreamTransport::new(jetstream)),
            nats_latest: Arc::new(NatsLatestTransport::new(nats)),
            #[cfg(feature = "opendal")]
            object_store: None,
        }
    }

    /// Attach the durable object-store adapter (`transport: "s3"`), built from an
    /// OpenDAL operator the service binary derives from the executor's
    /// `[storage]` config. Builder-style so the three `new(...)` call sites wire
    /// it uniformly.
    #[cfg(feature = "opendal")]
    pub fn with_object_store(mut self, operator: ::opendal::Operator, prefix: String) -> Self {
        self.object_store = Some(Arc::new(S3Transport::new(operator, prefix)));
        self
    }

    /// Resolve a transport tag to its adapter. `""` defaults to JetStream (older
    /// specs/descriptors that predate the field). An UNKNOWN tag — or a `"s3"`
    /// tag with no object store attached — returns `None` so the caller fails
    /// loudly rather than silently mis-routing bytes.
    pub fn get(&self, tag: &str) -> Option<Arc<dyn StreamTransport>> {
        match tag {
            "" | "jetstream" => Some(self.jetstream.clone()),
            "nats-latest" => Some(self.nats_latest.clone()),
            #[cfg(feature = "opendal")]
            "s3" => self
                .object_store
                .clone()
                .map(|t| t as Arc<dyn StreamTransport>),
            _ => None,
        }
    }

    /// Ensure any durable streams the adapters require exist. Only JetStream
    /// needs one; core NATS (`nats-latest`) is connectionless pub/sub and the
    /// object store creates objects on demand.
    pub async fn ensure_streams(
        jetstream: &jetstream::Context,
        replicas: usize,
    ) -> Result<(), async_nats::Error> {
        JetStreamTransport::ensure_stream(jetstream, replicas).await
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

    #[cfg(feature = "opendal")]
    #[test]
    fn s3_key_scheme_dots_become_path() {
        // The opaque locator's dots map to key-path separators, so a stream is an
        // isolated directory of zero-padded chunk objects under the store prefix.
        let locator = datastream_subject("exec-9", "frames");
        assert_eq!(
            s3_chunk_key("executor/", &locator, 7),
            "executor/executor/datastream/exec-9/frames/c00000000000000000007"
        );
        // Empty prefix still yields a clean directory.
        assert_eq!(
            s3_key_prefix("", &locator),
            "executor/datastream/exec-9/frames/"
        );
    }

    #[cfg(feature = "opendal")]
    #[test]
    fn s3_envelope_body_roundtrip() {
        let env = ChunkMessage {
            seq: 5,
            content_type: "image/jpeg".to_string(),
            payload: vec![0xff, 0xd8, 0x00, 0x10, 0xff],
            is_eof: false,
        };
        // seq is NOT in the body — the reader supplies it from the key.
        let decoded = s3_decode(env.seq, &s3_encode(&env));
        assert_eq!(decoded.seq, 5);
        assert_eq!(decoded.content_type, "image/jpeg");
        assert_eq!(decoded.payload, vec![0xff, 0xd8, 0x00, 0x10, 0xff]);
        assert!(!decoded.is_eof);
    }

    #[cfg(feature = "opendal")]
    #[test]
    fn s3_eof_body_roundtrip() {
        let eof = ChunkMessage {
            seq: 20,
            content_type: String::new(),
            payload: Vec::new(),
            is_eof: true,
        };
        let decoded = s3_decode(eof.seq, &s3_encode(&eof));
        assert_eq!(decoded.seq, 20);
        assert!(decoded.is_eof);
        assert!(decoded.payload.is_empty());
    }
}
