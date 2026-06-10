//! Workflow-as-streaming-endpoint runtime plumbing (docs/25 §9 Phase 3,
//! WI-3/WI-4) — mekhan as a **virtual stream producer** and stable sink egress.
//!
//! A `stream_source` node has no executor job behind it: mekhan itself mints a
//! deterministic *virtual execution id* (`st-{instance_id}-{node_id}`) and
//! publishes on the SAME NATS surfaces a real executor job would, so the
//! engine and any downstream consumer see an indistinguishable producer:
//!
//!   * data-plane bytes → `EXECUTOR_DATASTREAM` JetStream stream, subject
//!     `executor.datastream.{execution_id}.{channel}`, framed as binary
//!     envelopes (payload in the body, `seq`/`content_type`/EOF in NATS
//!     headers — mirrors `executor_worker::chunks::encode_envelope`);
//!   * control-plane brackets/items → [`ControlEmitEvent`] JSON on the
//!     `EXECUTOR_EVENTS` stream, subject
//!     `executor.events.{execution_id}.control_emit`, with the Petri routing
//!     metadata tags the engine's `ExecutorWatcher` reads
//!     (`petri_net_id` / `petri_place` / `petri_event_control_emit`).
//!
//! The executor crates are NOT mekhan deps (same precedent as
//! `handlers::executions`): every wire constant and the `ControlEmitEvent`
//! shape are inline mirrors. The serde field names below MUST match
//! `aithericon_executor_domain::event::ControlEmitEvent` byte-for-byte — the
//! engine deserializes this struct off the wire.

use std::collections::HashMap;

use async_nats::jetstream;
use serde::{Deserialize, Serialize};

/// NATS header carrying the envelope's monotonic `seq` (executor `HDR_SEQ`).
pub(crate) const HDR_SEQ: &str = "X-Chunk-Seq";
/// NATS header carrying the envelope's `content_type` (executor `HDR_CONTENT_TYPE`).
pub(crate) const HDR_CONTENT_TYPE: &str = "X-Chunk-Content-Type";
/// NATS header marking the in-band EOF sentinel — `"1"` when set (executor `HDR_EOF`).
pub(crate) const HDR_EOF: &str = "X-Chunk-Eof";

/// JetStream stream that carries data-plane byte streams (executor
/// `DATASTREAM_STREAM`).
pub(crate) const DATASTREAM_STREAM: &str = "EXECUTOR_DATASTREAM";

/// JetStream stream that carries mid-execution events incl. `control_emit`
/// (executor reporter / engine `ExecutorWatcher` events stream).
pub(crate) const EXECUTOR_EVENTS_STREAM: &str = "EXECUTOR_EVENTS";

/// Petri meta tag: net id (engine `petri_scheduler_bridge::meta::META_NET_ID`).
pub(crate) const META_NET_ID: &str = "petri_net_id";
/// Petri meta tag: default/fallback signal place (`META_PLACE`).
pub(crate) const META_PLACE: &str = "petri_place";
/// Petri meta tag: signal key echoed into the `ExternalSignal` (`META_SIGNAL_KEY`).
pub(crate) const META_SIGNAL_KEY: &str = "petri_signal_key";
/// Petri meta tag routing `control_emit` events to the node's control inbox —
/// `META_EVENT_PREFIX` + the `"control_emit"` category. The engine's
/// `RoutingMeta::from_meta_tags` strips the prefix and `handle_control_emit`
/// looks up `place_for_event("control_emit")`.
pub(crate) const META_EVENT_CONTROL_EMIT: &str = "petri_event_control_emit";

/// The deterministic virtual execution id mekhan mints for a `stream_source` /
/// `stream_sink` node — there is no executor job, so mekhan IS the producer.
/// Deterministic (not a fresh uuid per request) so `?append=1` re-POSTs and
/// the egress tap all address the same subject.
pub fn virtual_execution_id(instance_id: uuid::Uuid, node_id: &str) -> String {
    format!("st-{instance_id}-{node_id}")
}

/// Data-plane subject for one channel's byte stream (mirrors
/// `executor_worker::chunks::datastream_subject`).
pub fn datastream_subject(execution_id: &str, channel: &str) -> String {
    format!("executor.datastream.{execution_id}.{channel}")
}

/// Control-emit subject (mirrors `ControlEmitEvent::subject()` — pattern
/// `executor.events.{execution_id}.control_emit`).
pub fn control_emit_subject(execution_id: &str) -> String {
    format!("executor.events.{execution_id}.control_emit")
}

/// The control inbox place the WI-2 lowering synthesizes for a node with OUT
/// channels — where the engine deposits `control_emit` tokens (mirrors
/// `compiler::lower::channels::control_inbox`).
pub fn control_inbox_place(node_id: &str) -> String {
    format!("p_{node_id}_control_in")
}

/// `open` vs. `item` vs. `close` — inline mirror of
/// `aithericon_executor_domain::event::ControlKind` (serde `snake_case`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Open,
    Item,
    Close,
}

/// Inline mirror of `aithericon_executor_domain::event::ControlEmitEvent` —
/// the JSON the engine's `ExecutorWatcher::handle_control_emit` deserializes.
/// Field names/order are the wire contract; do not rename.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlEmitEvent {
    /// The (virtual) execution this emit belongs to.
    pub execution_id: String,
    /// The declared `out` channel name the token is emitted into.
    pub channel: String,
    /// open vs. item vs. close.
    pub kind: ControlKind,
    /// JSON-serialized control-token payload (empty string for a close that
    /// carries no value on the control plane).
    pub payload_json: String,
    /// 0-based element index within the episode (carried on `Item`).
    pub item_idx: u64,
    /// Total item count, carried on a control-plane `Close` emit (0 otherwise;
    /// a DATA-plane close carries its count inside `payload_json` instead).
    pub count: u64,
    /// Per-episode correlation id (control plane); empty for data open/close.
    pub episode_uid: String,
    /// Petri routing metadata — `petri_net_id` + `petri_place` +
    /// `petri_event_control_emit` (+ `petri_signal_key`). The engine reads the
    /// net + control-inbox place out of this; without it the emit is dropped.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl ControlEmitEvent {
    /// NATS subject this emit is published on.
    pub fn subject(&self) -> String {
        control_emit_subject(&self.execution_id)
    }

    /// JetStream dedup id — exact mirror of the executor-domain `msg_id()`
    /// keying: control-plane brackets/items key on `episode_uid` (+ index for
    /// items); data-plane open/close key on the channel alone (once per
    /// channel per execution).
    pub fn msg_id(&self) -> String {
        match self.kind {
            ControlKind::Open => {
                if self.episode_uid.is_empty() {
                    format!("{}-data-{}-open", self.execution_id, self.channel)
                } else {
                    format!(
                        "{}-control-{}-{}-open",
                        self.execution_id, self.channel, self.episode_uid
                    )
                }
            }
            ControlKind::Item => format!(
                "{}-control-{}-{}-item-{}",
                self.execution_id, self.channel, self.episode_uid, self.item_idx
            ),
            ControlKind::Close => {
                if self.episode_uid.is_empty() {
                    format!("{}-data-{}-close", self.execution_id, self.channel)
                } else {
                    format!(
                        "{}-control-{}-{}-close",
                        self.execution_id, self.channel, self.episode_uid
                    )
                }
            }
        }
    }
}

/// Build the Petri routing metadata tags stamped on every virtual-producer
/// emit. Exact tag keys from the engine's `petri_scheduler_bridge::meta`:
/// `petri_net_id` + `petri_place` are REQUIRED for `RoutingMeta::from_meta_tags`
/// to parse at all; `petri_event_control_emit` is the route
/// `handle_control_emit` actually deposits through (the fallback place is set
/// to the same inbox — harmless, the control-emit path never consults it).
pub fn routing_metadata(
    net_id: &str,
    control_inbox: &str,
    signal_key: &str,
) -> HashMap<String, String> {
    HashMap::from([
        (META_NET_ID.to_string(), net_id.to_string()),
        (META_PLACE.to_string(), control_inbox.to_string()),
        (META_SIGNAL_KEY.to_string(), signal_key.to_string()),
        (
            META_EVENT_CONTROL_EMIT.to_string(),
            control_inbox.to_string(),
        ),
    ])
}

/// Build the data-plane `open` transport descriptor the consumer connects off
/// — `{transport, subject, content_type?}` (docs/25 §6; mirrors the producer
/// SDK's `_descriptor()`). Transport is always `jetstream` for the mekhan
/// virtual producer (the ingress endpoint publishes to JetStream only).
pub fn open_descriptor(subject: &str, content_type: Option<&str>) -> serde_json::Value {
    let mut descriptor = serde_json::json!({
        "transport": "jetstream",
        "subject": subject,
    });
    if let Some(ct) = content_type {
        descriptor["content_type"] = serde_json::Value::String(ct.to_string());
    }
    descriptor
}

/// Build the data-plane `close` payload (`{count, status}` — mirrors the
/// producer SDK's `_emit_close`).
pub fn data_close_payload(count: u64) -> String {
    serde_json::json!({ "count": count, "status": "ok" }).to_string()
}

/// `Nats-Msg-Id` for one binary envelope — keyed on subject+seq exactly like
/// `executor_worker::chunks` (`format!("{subject}-{seq}")`) so JetStream
/// dedups a re-published seq within the duplicate window.
pub fn chunk_msg_id(subject: &str, seq: u64) -> String {
    format!("{subject}-{seq}")
}

/// Encode one binary envelope's NATS headers (mirror of
/// `executor_worker::chunks::encode_envelope` — metadata in headers, raw
/// payload bytes as the message body).
pub fn envelope_headers(
    seq: u64,
    content_type: &str,
    is_eof: bool,
    msg_id: &str,
) -> async_nats::HeaderMap {
    let mut headers = async_nats::HeaderMap::new();
    headers.insert("Nats-Msg-Id", msg_id);
    headers.insert(HDR_SEQ, seq.to_string().as_str());
    headers.insert(HDR_CONTENT_TYPE, content_type);
    if is_eof {
        headers.insert(HDR_EOF, "1");
    }
    headers
}

/// Next envelope seq given the LAST envelope's `X-Chunk-Seq` header on the
/// subject (from `get_last_raw_message_by_subject`). `None` / garbled → the
/// subject is fresh, start at 0; otherwise continue at `last + 1` so an
/// `?append=1` re-POST keeps the per-subject numbering dense and monotonic.
pub fn resume_seq(last_seq_header: Option<&str>) -> u64 {
    last_seq_header
        .and_then(|s| s.parse::<u64>().ok())
        .map(|last| last + 1)
        .unwrap_or(0)
}

/// Extract the data-plane subject out of a parked `open` descriptor as the
/// step_executions projection captured it. The WI-2 sink lowering parks the
/// open token in `p_{node_id}_data`; depending on hoisting the row's `outputs`
/// may be the bare descriptor (`{transport, subject, ...}`), the inbox token
/// (`{channel, kind: "open", payload: {descriptor}}`), or a wrapper envelope —
/// so this scans the JSON tree (depth-bounded) for an object whose `subject`
/// is a datastream subject, falling back to any top-level `subject` string.
pub fn descriptor_subject(outputs: &serde_json::Value) -> Option<String> {
    fn scan(v: &serde_json::Value, depth: u8) -> Option<String> {
        let obj = v.as_object()?;
        if let Some(s) = obj.get("subject").and_then(|s| s.as_str()) {
            if s.starts_with("executor.datastream.") {
                return Some(s.to_string());
            }
        }
        if depth == 0 {
            return None;
        }
        for child in obj.values() {
            if let Some(found) = scan(child, depth - 1) {
                return Some(found);
            }
        }
        None
    }
    scan(outputs, 4).or_else(|| {
        outputs
            .get("subject")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    })
}

/// Ensure the `EXECUTOR_DATASTREAM` stream exists — idempotent, same config
/// as `executor_worker::chunks::JetStreamTransport::ensure_stream` (the
/// executor and mekhan race benignly to create it first).
pub async fn ensure_datastream_stream(
    js: &jetstream::Context,
) -> Result<jetstream::stream::Stream, String> {
    js.get_or_create_stream(jetstream::stream::Config {
        name: DATASTREAM_STREAM.to_string(),
        subjects: vec!["executor.datastream.>".to_string()],
        retention: jetstream::stream::RetentionPolicy::Limits,
        max_age: std::time::Duration::from_secs(24 * 60 * 60),
        // 2-minute dedup window for `Nats-Msg-Id = {subject}-{seq}`.
        duplicate_window: std::time::Duration::from_secs(120),
        num_replicas: 1,
        storage: jetstream::stream::StorageType::File,
        ..Default::default()
    })
    .await
    .map_err(|e| format!("ensure {DATASTREAM_STREAM}: {e}"))
}

/// Ensure the `EXECUTOR_EVENTS` stream exists — idempotent, same config as
/// the engine `ExecutorWatcher::ensure_stream` (which also get_or_creates it).
pub async fn ensure_events_stream(js: &jetstream::Context) -> Result<(), String> {
    js.get_or_create_stream(jetstream::stream::Config {
        name: EXECUTOR_EVENTS_STREAM.to_string(),
        subjects: vec!["executor.events.>".to_string()],
        max_age: std::time::Duration::from_secs(24 * 60 * 60),
        duplicate_window: std::time::Duration::from_secs(120),
        ..Default::default()
    })
    .await
    .map(|_| ())
    .map_err(|e| format!("ensure {EXECUTOR_EVENTS_STREAM}: {e}"))
}

/// Publish one [`ControlEmitEvent`] to the events stream with its dedup
/// `Nats-Msg-Id`, awaiting the JetStream ack (mirrors the executor's
/// `NatsEventEmitter::emit_control`, but fallible — the ingress endpoint
/// surfaces a failed emit instead of fire-and-forgetting it).
pub async fn publish_control_emit(
    js: &jetstream::Context,
    event: &ControlEmitEvent,
) -> Result<(), String> {
    let payload =
        serde_json::to_vec(event).map_err(|e| format!("serialize ControlEmitEvent: {e}"))?;
    let mut headers = async_nats::HeaderMap::new();
    headers.insert("Nats-Msg-Id", event.msg_id().as_str());
    let ack = js
        .publish_with_headers(event.subject(), headers, payload.into())
        .await
        .map_err(|e| format!("publish control_emit: {e}"))?;
    ack.await
        .map_err(|e| format!("control_emit ack: {e}"))
        .map(|_| ())
}

/// Publish one binary envelope onto the data-plane subject, awaiting the
/// JetStream ack (per-publish backpressure — mirrors
/// `executor_worker::chunks::JetStreamTransport::write`).
pub async fn publish_envelope(
    js: &jetstream::Context,
    subject: &str,
    seq: u64,
    content_type: &str,
    payload: axum::body::Bytes,
    is_eof: bool,
) -> Result<(), String> {
    let msg_id = chunk_msg_id(subject, seq);
    let headers = envelope_headers(seq, content_type, is_eof, &msg_id);
    let ack = js
        .publish_with_headers(subject.to_string(), headers, payload)
        .await
        .map_err(|e| format!("publish envelope seq {seq}: {e}"))?;
    ack.await
        .map_err(|e| format!("envelope ack seq {seq}: {e}"))
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_event(kind: ControlKind, episode_uid: &str) -> ControlEmitEvent {
        ControlEmitEvent {
            execution_id: "st-11111111-2222-3333-4444-555555555555-src_1".to_string(),
            channel: "frames".to_string(),
            kind,
            payload_json: r#"{"transport":"jetstream"}"#.to_string(),
            item_idx: 3,
            count: 7,
            episode_uid: episode_uid.to_string(),
            metadata: HashMap::new(),
        }
    }

    /// The serde wire shape MUST match `aithericon_executor_domain::event::
    /// ControlEmitEvent` byte-for-byte — the engine deserializes this JSON.
    /// Encode a fixture and assert the exact serialized string (struct field
    /// order = declaration order for serde_json on a plain struct).
    #[test]
    fn control_emit_event_wire_shape_exact() {
        let mut event = fixture_event(ControlKind::Open, "");
        event.payload_json = "{}".to_string();
        event.item_idx = 0;
        event.count = 0;
        event
            .metadata
            .insert("petri_net_id".to_string(), "mekhan-abc".to_string());
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"execution_id":"st-11111111-2222-3333-4444-555555555555-src_1","channel":"frames","kind":"open","payload_json":"{}","item_idx":0,"count":0,"episode_uid":"","metadata":{"petri_net_id":"mekhan-abc"}}"#
        );
    }

    /// `kind` is `snake_case` on the wire (`open` | `item` | `close`).
    #[test]
    fn control_kind_snake_case() {
        assert_eq!(
            serde_json::to_string(&ControlKind::Open).unwrap(),
            "\"open\""
        );
        assert_eq!(
            serde_json::to_string(&ControlKind::Item).unwrap(),
            "\"item\""
        );
        assert_eq!(
            serde_json::to_string(&ControlKind::Close).unwrap(),
            "\"close\""
        );
        // And round-trips back.
        assert_eq!(
            serde_json::from_str::<ControlKind>("\"close\"").unwrap(),
            ControlKind::Close
        );
    }

    /// Dedup-id mirror: data-plane (empty episode_uid) keys per channel; the
    /// control plane keys per episode (+ index for items) — exactly the
    /// executor-domain `msg_id()` namespace.
    #[test]
    fn msg_id_mirrors_executor_domain() {
        let exec = "st-11111111-2222-3333-4444-555555555555-src_1";
        assert_eq!(
            fixture_event(ControlKind::Open, "").msg_id(),
            format!("{exec}-data-frames-open")
        );
        assert_eq!(
            fixture_event(ControlKind::Close, "").msg_id(),
            format!("{exec}-data-frames-close")
        );
        assert_eq!(
            fixture_event(ControlKind::Open, "ep1").msg_id(),
            format!("{exec}-control-frames-ep1-open")
        );
        assert_eq!(
            fixture_event(ControlKind::Item, "ep1").msg_id(),
            format!("{exec}-control-frames-ep1-item-3")
        );
        assert_eq!(
            fixture_event(ControlKind::Close, "ep1").msg_id(),
            format!("{exec}-control-frames-ep1-close")
        );
    }

    #[test]
    fn subjects_and_ids() {
        let id = uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let exec = virtual_execution_id(id, "src_1");
        assert_eq!(exec, "st-11111111-2222-3333-4444-555555555555-src_1");
        assert_eq!(
            datastream_subject(&exec, "frames"),
            format!("executor.datastream.{exec}.frames")
        );
        assert_eq!(
            control_emit_subject(&exec),
            format!("executor.events.{exec}.control_emit")
        );
        assert_eq!(control_inbox_place("src_1"), "p_src_1_control_in");
    }

    /// The exact meta tag keys the engine's `RoutingMeta::from_meta_tags`
    /// parses: `petri_net_id` + `petri_place` (required), `petri_signal_key`,
    /// and the `control_emit` event route under `petri_event_control_emit`.
    #[test]
    fn routing_metadata_exact_tag_keys() {
        let meta = routing_metadata("mekhan-abc", "p_src_1_control_in", "st-x-src_1");
        assert_eq!(meta.len(), 4);
        assert_eq!(meta["petri_net_id"], "mekhan-abc");
        assert_eq!(meta["petri_place"], "p_src_1_control_in");
        assert_eq!(meta["petri_signal_key"], "st-x-src_1");
        assert_eq!(meta["petri_event_control_emit"], "p_src_1_control_in");
    }

    /// Envelope NATS headers mirror `executor_worker::chunks::encode_envelope`:
    /// `Nats-Msg-Id` + `X-Chunk-Seq` + `X-Chunk-Content-Type`, and
    /// `X-Chunk-Eof: 1` ONLY on the EOF sentinel.
    #[test]
    fn envelope_header_construction() {
        let headers = envelope_headers(41, "video/mp4", false, "subj-41");
        assert_eq!(headers.get("Nats-Msg-Id").unwrap().as_str(), "subj-41");
        assert_eq!(headers.get(HDR_SEQ).unwrap().as_str(), "41");
        assert_eq!(headers.get(HDR_CONTENT_TYPE).unwrap().as_str(), "video/mp4");
        assert!(headers.get(HDR_EOF).is_none());

        let eof = envelope_headers(42, "", true, "subj-42");
        assert_eq!(eof.get(HDR_EOF).unwrap().as_str(), "1");
        assert_eq!(eof.get(HDR_SEQ).unwrap().as_str(), "42");
    }

    #[test]
    fn chunk_msg_id_keyed_on_subject_and_seq() {
        assert_eq!(
            chunk_msg_id("executor.datastream.x.ch", 5),
            "executor.datastream.x.ch-5"
        );
    }

    /// Seq resumption: fresh subject → 0; last envelope seq N → N+1 (so an
    /// `?append=1` re-POST continues the numbering); garbled header → 0.
    #[test]
    fn seq_resumption() {
        assert_eq!(resume_seq(None), 0);
        assert_eq!(resume_seq(Some("0")), 1);
        assert_eq!(resume_seq(Some("41")), 42);
        assert_eq!(resume_seq(Some("not-a-number")), 0);
    }

    #[test]
    fn open_descriptor_shape() {
        let d = open_descriptor("executor.datastream.st-x-n.frames", Some("video/mp4"));
        assert_eq!(
            d,
            serde_json::json!({
                "transport": "jetstream",
                "subject": "executor.datastream.st-x-n.frames",
                "content_type": "video/mp4",
            })
        );
        let bare = open_descriptor("executor.datastream.st-x-n.frames", None);
        assert!(bare.get("content_type").is_none());
        assert_eq!(bare["transport"], "jetstream");
    }

    #[test]
    fn data_close_payload_shape() {
        assert_eq!(data_close_payload(7), r#"{"count":7,"status":"ok"}"#);
    }

    /// Descriptor-subject extraction tolerates every plausible parked shape:
    /// the bare descriptor, the control-inbox token wrapper
    /// (`{channel, kind, payload}` — the `control_emit_token` shape), and a
    /// projection envelope nesting; and rejects output rows with no subject.
    #[test]
    fn descriptor_subject_extraction() {
        let subj = "executor.datastream.st-x-sink_1.frames";
        // Bare descriptor.
        let bare = serde_json::json!({"transport": "jetstream", "subject": subj});
        assert_eq!(descriptor_subject(&bare).as_deref(), Some(subj));
        // Inbox-token wrapped (`control_emit_token` open shape).
        let wrapped = serde_json::json!({
            "channel": "frames",
            "kind": "open",
            "payload": {"transport": "jetstream", "subject": subj},
        });
        assert_eq!(descriptor_subject(&wrapped).as_deref(), Some(subj));
        // Deeper projection envelope.
        let nested = serde_json::json!({
            "detail": {"outputs": {"descriptor": {"subject": subj}}},
        });
        assert_eq!(descriptor_subject(&nested).as_deref(), Some(subj));
        // A non-datastream `subject` string at top level is still honored as
        // the documented fallback…
        let other = serde_json::json!({"subject": "somewhere.else"});
        assert_eq!(
            descriptor_subject(&other).as_deref(),
            Some("somewhere.else")
        );
        // …but no subject at all → None (→ 409 retry at the handler).
        let none = serde_json::json!({"status": "running"});
        assert_eq!(descriptor_subject(&none), None);
        assert_eq!(descriptor_subject(&serde_json::Value::Null), None);
    }
}
