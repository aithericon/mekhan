//! mekhan HTTP serve bridge (docs/32 — multi-endpoint file-servers, Phase 3b).
//!
//! `GET /api/v1/data/entries/{content_hash}/content` resolves a logical entry to
//! its physical copies, picks a servable endpoint, and streams the bytes back to
//! the browser by `access_method`:
//!
//! * **`local_mount`** — the bytes live on a filesystem mount reachable only from
//!   a capacity group's co-located runner. mekhan is cred-free here: it publishes
//!   a [`ServeRequest`] to `fileserve.<group>.read` over the bare NATS client with
//!   a reply inbox, drains the runner's OPEN → CHUNK* → CLOSE (or ERROR) frames,
//!   acks each chunk for flow control, and relays the CHUNK bytes into the HTTP
//!   body. **The wire contract here is LOCKED to the runner-side handler** in
//!   `executor-worker/src/fileserve.rs` (Phase 3a) — the structs below mirror it
//!   byte-for-byte.
//! * **`object_store` / `s3`** — mint a presigned GET URL and 302 the browser
//!   straight to the store (default), or proxy the bytes through mekhan when
//!   `config.proxy_s3_reads` is set. (opendal path — feature `migration-driver`.)
//! * **`sftp`** — stream in-process through opendal. (feature `migration-driver`.)
//!
//! Endpoint SELECTION for this phase is a simple preference order
//! (`local_mount` → `object_store` → `s3` → `sftp`), highest-priority endpoint
//! within the chosen method. Full cost-first routing + verification gating is
//! Phase 5 — see the TODO in [`pick_endpoint`].

use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::file_servers::queries::ServeCandidate;
use crate::file_servers::model::FileServerEndpoint;

// ---------------------------------------------------------------------------
// Wire contract — MUST match executor-worker/src/fileserve.rs (Phase 3a).
// mekhan and the executor live in separate workspaces; these are the
// redeclared mirror structs (serde shapes are the contract, not the types).
// ---------------------------------------------------------------------------

/// The NATS subject a serve request is published to for capacity `group`.
pub fn fileserve_subject(group: &str) -> String {
    format!("fileserve.{group}.read")
}

/// The per-request ACK subject mekhan publishes flow-control acks to.
pub fn ack_subject(reply: &str) -> String {
    format!("{reply}.ack")
}

/// In-flight window: the runner streams at most this many CHUNK frames ahead of
/// mekhan's acks. Mirrors `executor_worker::fileserve::WINDOW`.
pub const WINDOW: u64 = 16;

/// A serve request mekhan publishes to `fileserve.<group>.read`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeRequest {
    /// mekhan-minted correlation id, echoed on every reply frame.
    pub req_id: String,
    /// The endpoint's local mount root (mekhan-authoritative; the jail boundary).
    pub root: String,
    /// Server-relative path under `root` to read.
    pub canonical_path: String,
    /// Optional byte offset to seek to before reading (HTTP Range start).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Optional cap on bytes to read after `offset` (HTTP Range length).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
    /// Originating workspace (audit/correlation only; not a security boundary).
    pub workspace_id: String,
}

/// Terminal failure classes carried on a [`ReplyFrame::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeErrorKind {
    NotFound,
    ReadError,
    PathJail,
    Io,
}

/// One reply frame streamed to the request's `reply` inbox by the runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplyFrame {
    Open {
        req_id: String,
        seq: u64,
        content_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_size: Option<u64>,
    },
    Chunk {
        req_id: String,
        seq: u64,
        bytes: Vec<u8>,
    },
    Close {
        req_id: String,
        final_seq: u64,
        count: u64,
    },
    Error {
        req_id: String,
        kind: ServeErrorKind,
        message: String,
    },
}

/// A flow-control ACK mekhan publishes to `<reply>.ack` to advance the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeAck {
    /// The highest CHUNK `seq` mekhan has consumed (cumulative).
    pub seq: u64,
}

// ---------------------------------------------------------------------------
// HTTP Range parsing.
// ---------------------------------------------------------------------------

/// A parsed single-range request: `(offset, length)`. `length == None` means
/// "to end of file". Only `bytes=START-` and `bytes=START-END` forms are
/// honoured (the common browser/media cases); `bytes=-SUFFIX` and multi-range
/// are NOT supported (the serve protocol has no "from the end" primitive — the
/// runner seeks from an absolute offset only), so they fall through to a full
/// read (`None`).
pub fn parse_range(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let raw = headers.get(header::RANGE)?.to_str().ok()?;
    let spec = raw.strip_prefix("bytes=")?;
    // Reject multi-range (comma) and suffix ranges (leading '-').
    if spec.contains(',') || spec.starts_with('-') {
        return None;
    }
    let (start_s, end_s) = spec.split_once('-')?;
    let start: u64 = start_s.trim().parse().ok()?;
    let end_s = end_s.trim();
    if end_s.is_empty() {
        // `bytes=START-` → from START to EOF.
        return Some((start, None));
    }
    let end: u64 = end_s.parse().ok()?;
    if end < start {
        return None;
    }
    // HTTP ranges are inclusive: length = end - start + 1.
    Some((start, Some(end - start + 1)))
}

// ---------------------------------------------------------------------------
// Endpoint selection.
// ---------------------------------------------------------------------------

/// Preference order over `access_method` for this phase. Prefer the cheapest
/// reachable transport: a co-located `local_mount` (no egress) first, then the
/// built-in `object_store`, then external `s3`, then `sftp`.
const METHOD_PREFERENCE: &[&str] = &["local_mount", "object_store", "s3", "sftp"];

/// Pick the endpoint to serve from among a hash's resolved copies.
///
/// Phase-3b policy: walk [`METHOD_PREFERENCE`]; for the first method any
/// candidate offers, return the highest-priority candidate of that method.
/// Candidates arrive already priority-ordered from `serve_candidates`, so the
/// first match per method is the preferred one.
///
/// TODO(Phase 5): full cost-first routing (zone affinity, verification status
/// gating, health) + tie-breaks. For now this is a deterministic static order.
pub fn pick_endpoint(candidates: &[ServeCandidate]) -> Option<&ServeCandidate> {
    for method in METHOD_PREFERENCE {
        if let Some(c) = candidates
            .iter()
            .find(|c| c.endpoint.access_method == *method)
        {
            return Some(c);
        }
    }
    // Fall back to the single highest-priority candidate of any (unknown) method.
    candidates.first()
}

// ---------------------------------------------------------------------------
// local_mount: NATS relay.
// ---------------------------------------------------------------------------

/// How long to wait for the runner's first reply frame (OPEN/ERROR) before
/// giving up with 504. A co-located runner answers in milliseconds; a generous
/// cap covers a momentarily-busy group.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(15);
/// Idle timeout between subsequent frames once streaming has begun.
const FRAME_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Serve a `local_mount` copy by relaying the runner's NATS frame stream into
/// the HTTP body.
///
/// Resolves the endpoint's `group_id`, publishes a [`ServeRequest`] to
/// `fileserve.<group>.read` with a fresh reply inbox, and:
///
/// 1. Awaits the first frame. ERROR before any byte → 404 (not_found/path_jail)
///    or 500 (io/read_error). OPEN → set Content-Type / Content-Length and begin
///    streaming.
/// 2. Streams CHUNK bytes into [`Body::from_stream`], acking each consumed chunk
///    on `<reply>.ack` to keep the runner's in-flight window open. A mid-stream
///    ERROR aborts the body (the bytes already sent can't be unsent).
///
/// `filename` is used only for the `Content-Disposition` header.
pub async fn serve_local_mount(
    client: &async_nats::Client,
    endpoint: &FileServerEndpoint,
    server_relative_path: &str,
    filename: &str,
    workspace_id: &str,
    range: Option<(u64, Option<u64>)>,
) -> Response {
    let Some(group) = endpoint.group_id.as_deref().filter(|g| !g.is_empty()) else {
        return crate::models::error::ApiError::internal(
            "local_mount endpoint has no group_id to dispatch to",
        )
        .into_response();
    };

    let req_id = uuid::Uuid::new_v4().to_string();
    let reply = client.new_inbox();
    let (offset, length) = match range {
        Some((o, l)) => (Some(o), l),
        None => (None, None),
    };

    // Subscribe to the reply inbox BEFORE publishing so no frame is missed.
    let mut frames = match client.subscribe(reply.clone()).await {
        Ok(s) => s,
        Err(e) => {
            return crate::models::error::ApiError::internal(format!(
                "failed to subscribe serve reply inbox: {e}"
            ))
            .into_response();
        }
    };

    let request = ServeRequest {
        req_id: req_id.clone(),
        root: endpoint.root.clone(),
        canonical_path: server_relative_path.to_string(),
        offset,
        length,
        workspace_id: workspace_id.to_string(),
    };
    let payload = match serde_json::to_vec(&request) {
        Ok(p) => p,
        Err(e) => {
            return crate::models::error::ApiError::internal(format!(
                "failed to serialize serve request: {e}"
            ))
            .into_response();
        }
    };

    let subject = fileserve_subject(group);
    if let Err(e) = client
        .publish_with_reply(subject.clone(), reply.clone(), payload.into())
        .await
    {
        return crate::models::error::ApiError::internal(format!(
            "failed to publish serve request: {e}"
        ))
        .into_response();
    }

    // (1) First frame: OPEN (begin streaming) or ERROR (terminal, no body yet).
    let first = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, frames.next_frame()).await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return crate::models::error::ApiError::service_unavailable(
                "serve reply stream closed before any frame",
            )
            .into_response();
        }
        Err(_) => {
            return crate::models::error::ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timed out waiting for serving runner (group has no live worker?)",
            )
            .into_response();
        }
    };

    let (content_type, total_size) = match first {
        ReplyFrame::Open {
            content_type,
            total_size,
            ..
        } => (content_type, total_size),
        ReplyFrame::Error { kind, message, .. } => {
            return error_frame_to_response(kind, &message);
        }
        // CHUNK/CLOSE before OPEN is a protocol violation by the runner.
        other => {
            return crate::models::error::ApiError::internal(format!(
                "serve protocol error: expected OPEN, got {other:?}"
            ))
            .into_response();
        }
    };

    // (2) Stream the remaining frames (CHUNK* → CLOSE) into the body, acking as
    // we consume. A mid-stream ERROR truncates the body (best effort — the bytes
    // already flushed to the client cannot be recalled).
    let ack_subject = ack_subject(&reply);
    let client = client.clone();
    let stream = async_stream::stream! {
        let mut consumed: u64 = 0;
        loop {
            match tokio::time::timeout(FRAME_IDLE_TIMEOUT, frames.next_frame()).await {
                Ok(Some(ReplyFrame::Chunk { seq, bytes, .. })) => {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(bytes));
                    consumed = consumed.max(seq);
                    // Cumulative ack — keep the runner's WINDOW open. Cheap; the
                    // runner takes max() so dups/out-of-order are harmless.
                    let ack = serde_json::to_vec(&ServeAck { seq: consumed })
                        .expect("ServeAck serializes");
                    let _ = client.publish(ack_subject.clone(), ack.into()).await;
                }
                Ok(Some(ReplyFrame::Close { .. })) => break,
                Ok(Some(ReplyFrame::Error { kind, message, .. })) => {
                    tracing::warn!(?kind, %message, "serve mid-stream error; truncating body");
                    yield Err(std::io::Error::other(format!("serve error: {message}")));
                    break;
                }
                Ok(Some(ReplyFrame::Open { .. })) => {
                    // A second OPEN is a protocol error; stop.
                    break;
                }
                Ok(None) => break, // inbox closed
                Err(_) => {
                    // Idle: producer gone without CLOSE.
                    yield Err(std::io::Error::other("serve stream idle (runner gone)"));
                    break;
                }
            }
        }
    };

    let mut resp_headers = HeaderMap::new();
    if let Ok(v) = header::HeaderValue::from_str(&content_type) {
        resp_headers.insert(header::CONTENT_TYPE, v);
    }
    if let Some(size) = total_size {
        if let Ok(v) = header::HeaderValue::from_str(&size.to_string()) {
            resp_headers.insert(header::CONTENT_LENGTH, v);
        }
    }
    resp_headers.insert(header::ACCEPT_RANGES, header::HeaderValue::from_static("bytes"));
    let disposition = format!("inline; filename=\"{}\"", sanitize_filename(filename));
    if let Ok(v) = header::HeaderValue::from_str(&disposition) {
        resp_headers.insert(header::CONTENT_DISPOSITION, v);
    }

    // A Range request that produced a capped read gets 206; a full read gets 200.
    let status = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    (status, resp_headers, Body::from_stream(stream)).into_response()
}

/// Map a pre-first-byte ERROR frame to the right HTTP status.
fn error_frame_to_response(kind: ServeErrorKind, message: &str) -> Response {
    match kind {
        ServeErrorKind::NotFound | ServeErrorKind::PathJail => {
            crate::models::error::ApiError::not_found(format!("file not servable: {message}"))
                .into_response()
        }
        ServeErrorKind::ReadError | ServeErrorKind::Io => {
            crate::models::error::ApiError::internal(format!("serve read failed: {message}"))
                .into_response()
        }
    }
}

/// Strip characters that would break a quoted `Content-Disposition` filename
/// (quotes, control chars, path separators). Conservative — falls back to a
/// generic name when nothing usable remains.
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\' && *c != '/')
        .collect();
    if cleaned.trim().is_empty() {
        "download".to_string()
    } else {
        cleaned
    }
}

/// Thin helper trait: pull the next typed [`ReplyFrame`] off a NATS subscriber,
/// skipping malformed payloads (logged, not fatal — a corrupt frame from a
/// rogue publisher must not wedge the stream).
trait NextFrame {
    async fn next_frame(&mut self) -> Option<ReplyFrame>;
}

impl NextFrame for async_nats::Subscriber {
    async fn next_frame(&mut self) -> Option<ReplyFrame> {
        use futures::StreamExt;
        loop {
            let msg = self.next().await?;
            match serde_json::from_slice::<ReplyFrame>(&msg.payload) {
                Ok(f) => return Some(f),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping malformed serve reply frame");
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_servers::model::FileServerEndpoint;
    use chrono::Utc;

    fn ep(method: &str, priority: i32) -> FileServerEndpoint {
        FileServerEndpoint {
            id: uuid::Uuid::new_v4(),
            file_server_id: uuid::Uuid::new_v4(),
            access_method: method.to_string(),
            root: "/mnt/data".to_string(),
            resource_ref: None,
            group_id: Some("grp-1".to_string()),
            status: "online".to_string(),
            verification_status: "verified".to_string(),
            last_verified: None,
            last_seen: None,
            priority,
            config: serde_json::json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn cand(method: &str, priority: i32) -> ServeCandidate {
        ServeCandidate {
            path: "a/b.txt".to_string(),
            endpoint: ep(method, priority),
        }
    }

    #[test]
    fn subject_and_ack_match_runner_contract() {
        assert_eq!(fileserve_subject("grp-uuid"), "fileserve.grp-uuid.read");
        assert_eq!(ack_subject("_INBOX.abc"), "_INBOX.abc.ack");
    }

    #[test]
    fn request_omits_offset_length_when_none() {
        let req = ServeRequest {
            req_id: "r1".into(),
            root: "/mnt".into(),
            canonical_path: "a.txt".into(),
            offset: None,
            length: None,
            workspace_id: "ws".into(),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("offset").is_none(), "offset must be absent when None");
        assert!(v.get("length").is_none(), "length must be absent when None");
    }

    #[test]
    fn request_includes_offset_length_when_set() {
        let req = ServeRequest {
            req_id: "r1".into(),
            root: "/mnt".into(),
            canonical_path: "a.txt".into(),
            offset: Some(100),
            length: Some(50),
            workspace_id: "ws".into(),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["offset"], 100);
        assert_eq!(v["length"], 50);
    }

    #[test]
    fn reply_frame_open_parses_from_runner_shape() {
        let json = r#"{"type":"open","req_id":"r","seq":0,"content_type":"text/plain","total_size":42}"#;
        let f: ReplyFrame = serde_json::from_str(json).unwrap();
        match f {
            ReplyFrame::Open {
                total_size,
                content_type,
                seq,
                ..
            } => {
                assert_eq!(seq, 0);
                assert_eq!(total_size, Some(42));
                assert_eq!(content_type, "text/plain");
            }
            other => panic!("expected OPEN, got {other:?}"),
        }
    }

    #[test]
    fn reply_frame_chunk_bytes_are_u8_array() {
        let json = r#"{"type":"chunk","req_id":"r","seq":1,"bytes":[1,2,3]}"#;
        let f: ReplyFrame = serde_json::from_str(json).unwrap();
        match f {
            ReplyFrame::Chunk { seq, bytes, .. } => {
                assert_eq!(seq, 1);
                assert_eq!(bytes, vec![1u8, 2, 3]);
            }
            other => panic!("expected CHUNK, got {other:?}"),
        }
    }

    #[test]
    fn reply_frame_error_kind_snake_case() {
        let json = r#"{"type":"error","req_id":"r","kind":"path_jail","message":"x"}"#;
        let f: ReplyFrame = serde_json::from_str(json).unwrap();
        match f {
            ReplyFrame::Error { kind, .. } => assert_eq!(kind, ServeErrorKind::PathJail),
            other => panic!("expected ERROR, got {other:?}"),
        }
    }

    #[test]
    fn ack_serializes_to_seq_only() {
        let v = serde_json::to_value(ServeAck { seq: 7 }).unwrap();
        assert_eq!(v, serde_json::json!({"seq": 7}));
    }

    #[test]
    fn range_open_ended() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=1000-".parse().unwrap());
        assert_eq!(parse_range(&h), Some((1000, None)));
    }

    #[test]
    fn range_closed_is_inclusive_length() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=0-499".parse().unwrap());
        // 0..=499 inclusive → 500 bytes.
        assert_eq!(parse_range(&h), Some((0, Some(500))));
    }

    #[test]
    fn range_suffix_and_multirange_unsupported() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=-500".parse().unwrap());
        assert_eq!(parse_range(&h), None);
        let mut h2 = HeaderMap::new();
        h2.insert(header::RANGE, "bytes=0-1,2-3".parse().unwrap());
        assert_eq!(parse_range(&h2), None);
    }

    #[test]
    fn range_absent_is_none() {
        let h = HeaderMap::new();
        assert_eq!(parse_range(&h), None);
    }

    #[test]
    fn pick_prefers_local_mount_then_object_store() {
        let cands = vec![cand("sftp", 100), cand("object_store", 5), cand("local_mount", 1)];
        // local_mount wins despite lowest priority — it's the cheapest transport.
        assert_eq!(pick_endpoint(&cands).unwrap().endpoint.access_method, "local_mount");
    }

    #[test]
    fn pick_falls_through_method_order() {
        let cands = vec![cand("sftp", 50), cand("s3", 10)];
        // No local_mount/object_store → s3 beats sftp.
        assert_eq!(pick_endpoint(&cands).unwrap().endpoint.access_method, "s3");
    }

    #[test]
    fn pick_highest_priority_within_method() {
        // Two object_store endpoints; candidates arrive priority-ordered so the
        // first match (priority 9) wins.
        let cands = vec![cand("object_store", 9), cand("object_store", 1)];
        assert_eq!(pick_endpoint(&cands).unwrap().endpoint.priority, 9);
    }

    #[test]
    fn pick_empty_is_none() {
        assert!(pick_endpoint(&[]).is_none());
    }

    #[test]
    fn sanitize_filename_strips_unsafe() {
        assert_eq!(sanitize_filename("a/b\"c.txt"), "abc.txt");
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename("///"), "download");
    }

    #[test]
    fn error_frame_status_mapping() {
        use axum::response::IntoResponse;
        let r = error_frame_to_response(ServeErrorKind::NotFound, "nope");
        assert_eq!(r.into_response().status(), StatusCode::NOT_FOUND);
        let r = error_frame_to_response(ServeErrorKind::PathJail, "esc");
        assert_eq!(r.into_response().status(), StatusCode::NOT_FOUND);
        let r = error_frame_to_response(ServeErrorKind::Io, "io");
        assert_eq!(r.into_response().status(), StatusCode::INTERNAL_SERVER_ERROR);
        let r = error_frame_to_response(ServeErrorKind::ReadError, "rd");
        assert_eq!(r.into_response().status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
