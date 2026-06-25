//! Platform-admin JetStream introspection.
//!
//! Read-only window onto the NATS JetStream store behind mekhan/engine/executor
//! so an operator can debug stuck nets from the browser instead of shelling into
//! a box with `nats stream report`. Three endpoints, all gated on
//! `is_platform_admin`:
//!
//!   GET /api/v1/admin/jetstream/streams                  — every stream + counts
//!   GET /api/v1/admin/jetstream/streams/{name}           — one stream + consumers
//!   GET /api/v1/admin/jetstream/streams/{name}/messages  — peek raw messages
//!
//! The message peek is non-destructive: it walks `get_raw_message(seq)` backwards
//! from the requested sequence (no consumer, no ack), so inspecting a dead-letter
//! stream (`MEKHAN_SILENT_DROPS`, `runner-jobs_dlq`, …) never disturbs delivery.
//! It is bounded by a scan budget so a sparse multi-million-message stream can't
//! turn one request into a long sequential walk.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

/// One stream's headline metrics — the table row on the JetStream debug page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JsStreamSummary {
    pub name: String,
    /// Subjects the stream captures (from its config).
    pub subjects: Vec<String>,
    pub messages: u64,
    pub bytes: u64,
    pub first_seq: u64,
    pub last_seq: u64,
    pub consumer_count: usize,
    /// Distinct subjects currently present in the stream.
    pub subjects_count: u64,
    /// Messages deleted/purged out of the [first_seq, last_seq] window.
    pub deleted_count: u64,
    /// RFC3339 timestamp of the oldest message still present.
    pub first_ts: String,
    /// RFC3339 timestamp of the newest message.
    pub last_ts: String,
}

/// One consumer bound to a stream — the lag/pending view that explains a
/// stuck net (high `num_pending` / `num_ack_pending` = nobody draining).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JsConsumerSummary {
    pub name: String,
    pub durable: bool,
    pub filter_subjects: Vec<String>,
    /// Messages delivered but not yet acked.
    pub num_ack_pending: usize,
    /// Messages redelivered after an ack timeout.
    pub num_redelivered: usize,
    /// Pull requests currently parked, waiting for messages.
    pub num_waiting: usize,
    /// Messages not yet delivered to any client.
    pub num_pending: u64,
    /// Highest stream sequence delivered to a client.
    pub delivered_stream_seq: u64,
    /// Highest stream sequence acked (the floor below which everything is done).
    pub ack_floor_stream_seq: u64,
}

/// Stream detail = the summary plus the consumers bound to it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JsStreamDetail {
    #[serde(flatten)]
    pub stream: JsStreamSummary,
    pub consumers: Vec<JsConsumerSummary>,
}

/// One peeked message. `payload_json` is populated when the body parses as JSON
/// (the common case — engine/executor envelopes are JSON); `payload_text` is the
/// lossy-UTF8 fallback, truncated to [`MAX_PAYLOAD_PREVIEW`] with `truncated` set.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JsMessage {
    pub seq: u64,
    pub subject: String,
    pub time: String,
    /// Total payload size in bytes (before any preview truncation).
    pub size: usize,
    pub headers: Vec<JsHeader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_json: Option<Value>,
    pub payload_text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JsHeader {
    pub name: String,
    pub value: String,
}

/// A page of peeked messages, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JsMessagesResponse {
    pub stream: String,
    pub messages: Vec<JsMessage>,
    /// Lowest sequence still present in the stream.
    pub first_seq: u64,
    /// Highest sequence in the stream.
    pub last_seq: u64,
    /// Pass `before_seq = next_before_seq` to fetch the previous (older) page;
    /// `null` once the oldest message has been returned.
    pub next_before_seq: Option<u64>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct PeekMessagesQuery {
    /// Return messages with sequence ≤ this value (default: the stream tail).
    /// Use the previous page's `next_before_seq` to paginate backwards.
    pub before_seq: Option<u64>,
    /// Max messages to return (default 50, capped at 200).
    pub limit: Option<usize>,
}

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
const MAX_PAYLOAD_PREVIEW: usize = 16 * 1024;
/// Upper bound on sequence numbers probed in one peek, so a stream riddled with
/// deletions can't turn a 50-message request into a million-step walk.
const SCAN_BUDGET: usize = 2_000;

fn ts_rfc3339(unix: i64, nanos: u32) -> String {
    chrono::DateTime::from_timestamp(unix, nanos)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

fn summarize_stream(info: &async_nats::jetstream::stream::Info) -> JsStreamSummary {
    let s = &info.state;
    JsStreamSummary {
        name: info.config.name.clone(),
        subjects: info.config.subjects.clone(),
        messages: s.messages,
        bytes: s.bytes,
        first_seq: s.first_sequence,
        last_seq: s.last_sequence,
        consumer_count: s.consumer_count,
        subjects_count: s.subjects_count,
        deleted_count: s.deleted_count.unwrap_or(0),
        first_ts: ts_rfc3339(s.first_timestamp.unix_timestamp(), s.first_timestamp.nanosecond()),
        last_ts: ts_rfc3339(s.last_timestamp.unix_timestamp(), s.last_timestamp.nanosecond()),
    }
}

fn summarize_consumer(info: &async_nats::jetstream::consumer::Info) -> JsConsumerSummary {
    let mut filter_subjects = info.config.filter_subjects.clone();
    if filter_subjects.is_empty() && !info.config.filter_subject.is_empty() {
        filter_subjects.push(info.config.filter_subject.clone());
    }
    JsConsumerSummary {
        name: info.name.clone(),
        durable: info.config.durable_name.is_some(),
        filter_subjects,
        num_ack_pending: info.num_ack_pending,
        num_redelivered: info.num_redelivered,
        num_waiting: info.num_waiting,
        num_pending: info.num_pending,
        delivered_stream_seq: info.delivered.stream_sequence,
        ack_floor_stream_seq: info.ack_floor.stream_sequence,
    }
}

/// List every JetStream stream with its headline counts.
#[utoipa::path(
    get,
    path = "/api/v1/admin/jetstream/streams",
    responses(
        (status = 200, description = "All JetStream streams", body = Vec<JsStreamSummary>),
        (status = 403, description = "Platform admin required", body = ErrorResponse),
    ),
    tag = "jetstream-admin",
)]
pub async fn list_streams(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<JsStreamSummary>>, ApiError> {
    require_platform_admin(&user)?;

    let js = state.nats.jetstream();
    let mut out = Vec::new();
    let mut streams = js.streams();
    while let Some(item) = streams.next().await {
        match item {
            Ok(info) => out.push(summarize_stream(&info)),
            Err(e) => {
                return Err(ApiError::internal(format!("list streams: {e}")));
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(out))
}

/// One stream's detail plus every consumer bound to it.
#[utoipa::path(
    get,
    path = "/api/v1/admin/jetstream/streams/{name}",
    params(("name" = String, Path, description = "JetStream stream name")),
    responses(
        (status = 200, description = "Stream detail + consumers", body = JsStreamDetail),
        (status = 403, description = "Platform admin required", body = ErrorResponse),
        (status = 404, description = "No such stream", body = ErrorResponse),
    ),
    tag = "jetstream-admin",
)]
pub async fn get_stream(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
) -> Result<Json<JsStreamDetail>, ApiError> {
    require_platform_admin(&user)?;

    let js = state.nats.jetstream();
    let mut stream = js
        .get_stream(&name)
        .await
        .map_err(|e| ApiError::not_found(format!("stream {name}: {e}")))?;
    let info = stream
        .info()
        .await
        .map_err(|e| ApiError::internal(format!("stream info {name}: {e}")))?
        .clone();

    let mut consumers = Vec::new();
    let mut it = stream.consumers();
    while let Some(item) = it.next().await {
        match item {
            Ok(c) => consumers.push(summarize_consumer(&c)),
            Err(e) => return Err(ApiError::internal(format!("list consumers {name}: {e}"))),
        }
    }
    consumers.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(JsStreamDetail {
        stream: summarize_stream(&info),
        consumers,
    }))
}

/// Non-destructively peek raw messages from a stream, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/admin/jetstream/streams/{name}/messages",
    params(
        ("name" = String, Path, description = "JetStream stream name"),
        PeekMessagesQuery,
    ),
    responses(
        (status = 200, description = "A page of raw messages", body = JsMessagesResponse),
        (status = 403, description = "Platform admin required", body = ErrorResponse),
        (status = 404, description = "No such stream", body = ErrorResponse),
    ),
    tag = "jetstream-admin",
)]
pub async fn peek_messages(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
    Query(q): Query<PeekMessagesQuery>,
) -> Result<Json<JsMessagesResponse>, ApiError> {
    require_platform_admin(&user)?;

    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let js = state.nats.jetstream();
    let mut stream = js
        .get_stream(&name)
        .await
        .map_err(|e| ApiError::not_found(format!("stream {name}: {e}")))?;
    let info = stream
        .info()
        .await
        .map_err(|e| ApiError::internal(format!("stream info {name}: {e}")))?
        .clone();

    let first_seq = info.state.first_sequence;
    let last_seq = info.state.last_sequence;

    let mut messages = Vec::new();
    let mut next_before_seq = None;

    if last_seq >= first_seq && first_seq > 0 {
        // Walk backwards from the requested ceiling, skipping deleted seqs, until
        // we've collected `limit` messages or exhausted the scan budget.
        let ceiling = q.before_seq.unwrap_or(last_seq).min(last_seq);
        let mut seq = ceiling;
        let mut scanned = 0usize;
        while seq >= first_seq && messages.len() < limit && scanned < SCAN_BUDGET {
            match stream.get_raw_message(seq).await {
                Ok(raw) => messages.push(render_message(raw)),
                Err(_) => { /* deleted / missing sequence — skip */ }
            }
            scanned += 1;
            if seq == 0 {
                break;
            }
            seq -= 1;
        }
        // More older messages remain iff we stopped above the floor.
        if seq >= first_seq && seq > 0 && (messages.len() >= limit || scanned >= SCAN_BUDGET) {
            next_before_seq = Some(seq);
        }
    }

    Ok(Json(JsMessagesResponse {
        stream: name,
        messages,
        first_seq,
        last_seq,
        next_before_seq,
    }))
}

fn render_message(raw: async_nats::jetstream::message::StreamMessage) -> JsMessage {
    let size = raw.payload.len();
    let payload_json = serde_json::from_slice::<Value>(&raw.payload).ok();
    let full_text = String::from_utf8_lossy(&raw.payload);
    let truncated = full_text.len() > MAX_PAYLOAD_PREVIEW;
    let payload_text = if truncated {
        full_text.chars().take(MAX_PAYLOAD_PREVIEW).collect()
    } else {
        full_text.into_owned()
    };

    let mut headers = Vec::new();
    for (k, vals) in raw.headers.iter() {
        let joined = vals
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        headers.push(JsHeader {
            name: k.to_string(),
            value: joined,
        });
    }
    headers.sort_by(|a, b| a.name.cmp(&b.name));

    JsMessage {
        seq: raw.sequence,
        subject: raw.subject.to_string(),
        time: ts_rfc3339(raw.time.unix_timestamp(), raw.time.nanosecond()),
        size,
        headers,
        payload_json,
        payload_text,
        truncated,
    }
}

fn require_platform_admin(user: &AuthUser) -> Result<(), ApiError> {
    if user.is_platform_admin {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "JetStream introspection requires platform admin",
        ))
    }
}
