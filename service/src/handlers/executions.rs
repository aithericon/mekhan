//! Datastream tap — a generic byte pipe over a single execution's data-plane
//! channel.
//!
//! The executor publishes out-of-band channel bytes onto the
//! `EXECUTOR_DATASTREAM` JetStream stream, one subject per channel
//! (`executor.datastream.{execution_id}.{channel}`), as framed binary
//! envelopes: the raw payload bytes ride the NATS message body and the
//! per-envelope metadata (`seq`, `content_type`, EOF sentinel) rides NATS
//! headers (see `executor_worker::chunks::encode_envelope`). The executor
//! crates aren't mekhan deps, so the header names are inlined here rather than
//! taken as a crate dependency.
//!
//! This handler opens an ephemeral pull consumer filtered to the channel's
//! subject, peeks the first envelope to set the response `Content-Type`, then
//! streams each envelope's payload bytes out chunk-by-chunk (never buffering
//! the whole clip) and ends when it sees the `is_eof` sentinel. It is a generic
//! passthrough — no codec/audio logic lives here; `content_type` is forwarded
//! verbatim. A small `seq`-keyed reorder buffer re-imposes order (mirroring the
//! producer's own consumer in `executor_worker::chunks`), and every read is
//! bounded by an idle timeout so a never-closed / control-plane / nonexistent
//! channel returns promptly instead of hanging the connection.
//!
//! Because it replays from `seq 0` and yields each envelope the moment it lands,
//! the tap already follows an *in-progress* producer — a client can start
//! reading before the stream is done. The `?follow=1` flag widens the per-read
//! idle patience (`FOLLOW_IDLE` vs the default `IDLE_TIMEOUT`) so a live stream
//! with long quiet gaps (a paused encoder, a sparse live feed) is tailed to its
//! real `is_eof` rather than abandoned after the short replay timeout. Follow
//! still ends at `is_eof` or client disconnect; the wider idle is only a
//! backstop against a wedged producer.

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use async_nats::jetstream;
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use serde::Deserialize;

use crate::models::error::ApiError;
use crate::AppState;

/// NATS header carrying the envelope's monotonic `seq` (executor `HDR_SEQ`).
const HDR_SEQ: &str = "X-Chunk-Seq";
/// NATS header carrying the envelope's `content_type` (executor `HDR_CONTENT_TYPE`).
const HDR_CONTENT_TYPE: &str = "X-Chunk-Content-Type";
/// NATS header marking the in-band EOF sentinel — `"1"` when set (executor `HDR_EOF`).
const HDR_EOF: &str = "X-Chunk-Eof";

/// JetStream stream that carries data-plane byte streams (executor `DATASTREAM_STREAM`).
const DATASTREAM_STREAM: &str = "EXECUTOR_DATASTREAM";

/// Max time to wait on the next envelope before treating the channel as drained.
/// Bounds the request for a completed replay (all envelopes already present →
/// fires once at the tail) and, critically, caps a never-closed / control-plane
/// / nonexistent channel so it can't hold the HTTP connection forever. Also fed
/// to the consumer's `inactive_threshold` so NATS reaps the ephemeral consumer
/// shortly after the client disconnects.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-read idle patience in `?follow=1` mode. A live producer may pause for long
/// stretches (a paused encoder, a sparse feed) without closing the stream, so
/// follow waits much longer between envelopes than a replay. It is only a
/// backstop against a wedged producer — a follow normally ends at `is_eof` or
/// when the client disconnects (which drops this future and lets NATS reap the
/// consumer via `inactive_threshold`), not at this timeout.
const FOLLOW_IDLE: Duration = Duration::from_secs(300);

/// Query for the datastream tap. `follow=1` (or `true`/`yes`/`on`/bare `?follow`)
/// switches from a bounded replay to a live tail.
#[derive(Debug, Default, Deserialize)]
pub struct TapQuery {
    #[serde(default)]
    follow: Option<String>,
}

impl TapQuery {
    /// Truthy if `follow` is present and not an explicit falsy value. A bare
    /// `?follow` (empty value) counts as on.
    fn follow(&self) -> bool {
        match self.follow.as_deref() {
            None => false,
            Some(v) => !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            ),
        }
    }
}

/// Reject a path component that would break the `.`-delimited NATS subject or
/// widen the filter into a wildcard tap. `execution_id` / `channel` are
/// untrusted path params; both are Rhai-identifier-safe slugs at the producer,
/// so any `.`/`*`/`>`/whitespace/`/` here is malformed and we refuse it rather
/// than risk subject injection / an over-broad subscription.
fn validate_subject_token(label: &str, value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c == '.' || c == '*' || c == '>' || c == '/' || c.is_whitespace())
    {
        return Err(ApiError::bad_request(format!(
            "invalid {label}: must be a non-empty token without '.', '*', '>', '/', or whitespace"
        )));
    }
    Ok(())
}

/// Per-tap reorder buffer keyed on `seq`, mirroring
/// `executor_worker::chunks::ReorderBuffer` (producer seq starts at 0). A
/// duplicate / already-delivered seq is dropped; contiguous runs forward in
/// order; trailing out-of-order remainder is flushed (sorted) at stream end.
struct Reorder {
    next: u64,
    pending: BTreeMap<u64, Bytes>,
}

impl Reorder {
    fn new() -> Self {
        Self {
            next: 0,
            pending: BTreeMap::new(),
        }
    }

    fn insert(&mut self, seq: u64, payload: Bytes) {
        if seq < self.next || self.pending.contains_key(&seq) {
            return;
        }
        self.pending.insert(seq, payload);
    }

    /// Forward the contiguous run now ready (in `seq` order), advancing the cursor.
    fn drain_ready(&mut self) -> Vec<Bytes> {
        let mut out = Vec::new();
        while let Some(b) = self.pending.remove(&self.next) {
            self.next += 1;
            if !b.is_empty() {
                out.push(b);
            }
        }
        out
    }

    /// Emit whatever is still buffered, in `seq` order, tolerating a gap at the
    /// very end of the stream (a missing seq we'll never receive).
    fn flush(&mut self) -> Vec<Bytes> {
        std::mem::take(&mut self.pending)
            .into_values()
            .filter(|b| !b.is_empty())
            .collect()
    }
}

/// GET /api/v1/executions/{execution_id}/channels/{channel}/data
///
/// Stream the raw, reordered, concatenated payload bytes of one execution's
/// data-plane channel. The response `Content-Type` is taken from the channel's
/// first envelope (default `application/octet-stream`). The stream ends at the
/// `is_eof` envelope, or — for a never-closed / nonexistent channel — when no
/// further envelope arrives within the idle window (returns what was seen; an
/// empty channel yields a 200 with no body). The ephemeral consumer is reaped
/// by NATS via `inactive_threshold` once this response future is dropped.
///
/// `?follow=1` tails a live, still-producing stream: it widens the idle window
/// to `FOLLOW_IDLE` so long quiet gaps don't end the stream early. Either mode
/// yields envelopes the moment they land (the body is HTTP-chunked), so a client
/// can play / render audio-video while the producer is still emitting.
#[utoipa::path(
    get,
    path = "/api/v1/executions/{execution_id}/channels/{channel}/data",
    params(
        ("execution_id" = String, Path, description = "AutomatedStep execution id (the `execution_id` stamped on the parked output envelope)."),
        ("channel" = String, Path, description = "Data-plane channel name (Rhai-identifier-safe slug)."),
        ("follow" = Option<String>, Query, description = "Live-tail an in-progress stream: `follow=1` widens the idle patience so long gaps don't end it early (ends at EOF or client disconnect)."),
    ),
    responses(
        (status = 200, description = "Concatenated channel payload bytes; Content-Type echoes the channel envelope's content_type.", content_type = "application/octet-stream"),
        (status = 400, description = "Malformed execution_id or channel path component.", body = crate::models::error::ErrorResponse),
        (status = 502, description = "JetStream consumer could not be opened.", body = crate::models::error::ErrorResponse),
    ),
    tag = "executions",
)]
pub async fn tap_channel_data(
    State(state): State<AppState>,
    Path((execution_id, channel)): Path<(String, String)>,
    Query(query): Query<TapQuery>,
) -> Result<Response, ApiError> {
    validate_subject_token("execution_id", &execution_id)?;
    validate_subject_token("channel", &channel)?;
    // Replay (bounded) vs follow (live tail with wide idle patience).
    let idle = if query.follow() {
        FOLLOW_IDLE
    } else {
        IDLE_TIMEOUT
    };

    let subject = format!("executor.datastream.{execution_id}.{channel}");
    let js = state.nats.jetstream().clone();

    // Ephemeral consumer (no durable name) filtered to this channel's subject,
    // replaying from the beginning — same idiom as `instance_jetstream_events`.
    // `inactive_threshold` lets NATS reap it shortly after we stop pulling
    // (client disconnect / stream end) rather than leaning on the server default.
    let stream_h = js.get_stream(DATASTREAM_STREAM).await.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("datastream stream unavailable: {e}"),
        )
    })?;
    let consumer = stream_h
        .create_consumer(jetstream::consumer::pull::Config {
            filter_subject: subject.clone(),
            deliver_policy: jetstream::consumer::DeliverPolicy::All,
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            inactive_threshold: IDLE_TIMEOUT,
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("datastream consumer: {e}")))?;
    let mut messages = consumer
        .messages()
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("datastream messages: {e}")))?;

    // Peek the first envelope so the response Content-Type is correct before the
    // body starts (HTTP headers can't be set mid-body). Every envelope on the
    // channel carries the same `content_type`, so the first one suffices. The
    // peeked payload is seeded into the reorder buffer (not emitted directly) so
    // ordering is enforced uniformly. Bounded by `idle`: a nonexistent /
    // control-plane channel falls through to a 200-empty response (in follow
    // mode this also waits for a producer that hasn't started emitting yet).
    let mut reorder = Reorder::new();
    let mut content_type = "application/octet-stream".to_string();
    let mut ended = false;
    // Single-shot peek (not a loop — every outcome is terminal): take exactly one
    // envelope to settle the Content-Type, or fall through to a 200-empty response.
    match tokio::time::timeout(idle, messages.next()).await {
        Ok(Some(item)) => {
            let msg = item.map_err(|e| {
                ApiError::new(StatusCode::BAD_GATEWAY, format!("datastream read: {e}"))
            })?;
            let (seq, ct, is_eof) = parse_headers(msg.headers.as_ref());
            let payload = Bytes::from(msg.payload.to_vec());
            let _ = msg.ack().await;
            if let Some(ct) = ct {
                if !ct.is_empty() {
                    content_type = ct;
                }
            }
            if is_eof {
                // EOF before any data — empty channel; respond 200, no body.
                ended = true;
            } else {
                reorder.insert(seq, payload);
            }
        }
        // Idle (unknown execution_id / control channel) or stream end: nothing to serve.
        Ok(None) | Err(_) => {
            ended = true;
        }
    }

    // Body: drain the buffer (the peeked envelope), then continue pulling and
    // forwarding contiguous runs until the EOF sentinel or an idle timeout.
    // Yields `Result<Bytes, io::Error>` chunk-by-chunk.
    let body = async_stream::stream! {
        for b in reorder.drain_ready() {
            yield Ok::<Bytes, io::Error>(b);
        }
        if !ended {
            loop {
                match tokio::time::timeout(idle, messages.next()).await {
                    Ok(Some(Ok(msg))) => {
                        let (seq, _ct, is_eof) = parse_headers(msg.headers.as_ref());
                        let payload = Bytes::from(msg.payload.to_vec());
                        let _ = msg.ack().await;
                        if is_eof {
                            break;
                        }
                        reorder.insert(seq, payload);
                        for b in reorder.drain_ready() {
                            yield Ok(b);
                        }
                    }
                    Ok(Some(Err(e))) => {
                        yield Err(io::Error::other(e));
                        break;
                    }
                    Ok(None) => break, // consumer stream ended
                    Err(_) => break,   // idle — producer gone / done, no EOF coming
                }
            }
        }
        for b in reorder.flush() {
            yield Ok(b);
        }
    };

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        Body::from_stream(body),
    )
        .into_response())
}

/// Parse `(seq, content_type, is_eof)` out of an envelope's NATS headers. A
/// missing/garbled `seq` defaults to 0 (mirrors `executor_worker::chunks`).
fn parse_headers(headers: Option<&async_nats::HeaderMap>) -> (u64, Option<String>, bool) {
    let seq = header_str(headers, HDR_SEQ)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let content_type = header_str(headers, HDR_CONTENT_TYPE);
    let is_eof = header_str(headers, HDR_EOF).as_deref() == Some("1");
    (seq, content_type, is_eof)
}

/// Read a NATS header value as an owned `String`.
fn header_str(headers: Option<&async_nats::HeaderMap>, name: &str) -> Option<String> {
    headers
        .and_then(|h| h.get(name))
        .map(|v| v.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::TapQuery;

    fn q(follow: Option<&str>) -> TapQuery {
        TapQuery {
            follow: follow.map(|s| s.to_string()),
        }
    }

    #[test]
    fn follow_flag_truthiness() {
        // Absent → replay (default).
        assert!(!q(None).follow());
        // Bare `?follow` (empty value) and the usual truthy spellings → on.
        for v in ["", "1", "true", "TRUE", "yes", "on", "follow"] {
            assert!(q(Some(v)).follow(), "{v:?} should be truthy");
        }
        // Explicit falsy spellings → off.
        for v in ["0", "false", "No", "off", " false "] {
            assert!(!q(Some(v)).follow(), "{v:?} should be falsy");
        }
    }
}
