//! Workflow-as-streaming-endpoint HTTP surface (docs/25 §9 Phase 3, WI-3/WI-4).
//!
//! INGRESS — mekhan as the **virtual producer** for a `stream_source` node:
//! there is no executor job behind the node, so mekhan mints the deterministic
//! virtual execution id `st-{instance_id}-{node_id}` and publishes on the SAME
//! NATS surfaces a real job would (data envelopes on `EXECUTOR_DATASTREAM`,
//! `control_emit` brackets on `EXECUTOR_EVENTS` with Petri routing metadata),
//! so the engine and downstream consumers see an indistinguishable producer.
//!
//!   * `POST …/sources/{node_id}/channels/{channel}/data` — stream the raw
//!     request body into a DATA-plane Out channel: `open` descriptor on first
//!     byte, ~64 KiB envelopes with per-publish JetStream-ack backpressure,
//!     `is_eof` + `close` at body end (suppressed by `?append=1`; `?eof=1`
//!     with an empty body closes a previously appended stream).
//!   * `POST …/sources/{node_id}/channels/{channel}/emit` — one fused
//!     control-plane episode: `open` + `item`(idx 0..n) + `close`(count=n)
//!     under one fresh `episode_uid`.
//!
//! EGRESS — `GET …/sinks/{node_id}/data` resolves the sink's parked `open`
//! descriptor out of the `step_execution` projection (the WI-2 sink lowering
//! parks it in `p_{node_id}_data`), extracts the data-plane subject, and
//! streams the bytes through the shared tap core in `handlers::executions`.

use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::handlers::executions::{flag_on, tap_datastream_subject, validate_subject_token};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{
    Channel, ChannelDirection, ChannelPlane, ElementType, WorkflowGraph, WorkflowNodeData,
};
use crate::streams::{
    control_inbox_place, data_close_payload, datastream_subject, descriptor_subject,
    ensure_datastream_stream, ensure_events_stream, open_descriptor, publish_control_emit,
    publish_envelope, resume_seq, routing_metadata, virtual_execution_id, ControlEmitEvent,
    ControlKind, HDR_SEQ,
};
use crate::AppState;

/// Max bytes per published data envelope. Mirrors the executor SDK's writer
/// chunking ballpark — small enough that the per-publish ack keeps memory
/// bounded, large enough that JetStream isn't drowned in tiny messages.
const ENVELOPE_CHUNK: usize = 64 * 1024;

/// What an ingress/egress request resolved against the instance's template:
/// the engine net id plus the stream node's declared channels.
struct StreamNode {
    net_id: String,
    status: String,
    channels: Vec<Channel>,
}

/// Which stream-node variant a request must address.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamNodeKind {
    Source,
    Sink,
}

/// Resolve `(instance, node)` → the instance's net id/status + the node's
/// declared channels, enforcing that the node exists in the instance's
/// template version and is the expected `stream_source` / `stream_sink`
/// variant. 404 for unknown instance/node, 400 for a node of the wrong kind.
async fn load_stream_node(
    state: &AppState,
    instance_id: Uuid,
    node_id: &str,
    want: StreamNodeKind,
) -> Result<StreamNode, ApiError> {
    let row: Option<(String, String, Uuid, i32)> = sqlx::query_as(
        "SELECT net_id, status, template_id, template_version \
         FROM workflow_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((net_id, status, template_id, template_version)) = row else {
        return Err(ApiError::not_found("instance not found"));
    };

    let graph_row: Option<(serde_json::Value,)> =
        sqlx::query_as("SELECT graph FROM workflow_templates WHERE id = $1 AND version = $2")
            .bind(template_id)
            .bind(template_version)
            .fetch_optional(&state.db)
            .await?;
    let Some((graph_json,)) = graph_row else {
        return Err(ApiError::internal(
            "instance's template version no longer exists",
        ));
    };
    let graph: WorkflowGraph = serde_json::from_value(graph_json)
        .map_err(|e| ApiError::internal(format!("template graph is invalid: {e}")))?;

    let node = graph
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .ok_or_else(|| {
            ApiError::not_found(format!("node '{node_id}' not found in instance template"))
        })?;

    let channels = match (&node.data, want) {
        (WorkflowNodeData::StreamSource { channels, .. }, StreamNodeKind::Source)
        | (WorkflowNodeData::StreamSink { channels, .. }, StreamNodeKind::Sink) => channels.clone(),
        _ => {
            let expected = match want {
                StreamNodeKind::Source => "stream_source",
                StreamNodeKind::Sink => "stream_sink",
            };
            return Err(ApiError::bad_request(format!(
                "node '{node_id}' is not a {expected} node"
            )));
        }
    };

    Ok(StreamNode {
        net_id,
        status,
        channels,
    })
}

/// The ingress endpoints feed a LIVE net — reject when the instance isn't
/// running (a completed/cancelled net has no one to consume the deposit).
fn require_running(node: &StreamNode) -> Result<(), ApiError> {
    if node.status != "running" {
        return Err(ApiError::conflict(format!(
            "instance is not running (status: {})",
            node.status
        )));
    }
    Ok(())
}

/// Find the named OUT channel on the declared plane. 404 for an undeclared
/// channel name, 400 for a declared channel of the wrong direction/plane.
fn require_out_channel<'a>(
    channels: &'a [Channel],
    name: &str,
    plane: ChannelPlane,
) -> Result<&'a Channel, ApiError> {
    let ch = channels
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| ApiError::not_found(format!("channel '{name}' is not declared")))?;
    if !matches!(ch.direction, ChannelDirection::Out) {
        return Err(ApiError::bad_request(format!(
            "channel '{name}' is an In channel; the ingress endpoints feed Out channels"
        )));
    }
    let plane_ok = matches!(
        (&ch.plane, &plane),
        (ChannelPlane::Data, ChannelPlane::Data) | (ChannelPlane::Control, ChannelPlane::Control)
    );
    if !plane_ok {
        let (want, hint) = match plane {
            ChannelPlane::Data => ("data", "use …/emit for a control channel"),
            ChannelPlane::Control => ("control", "use …/data for a data channel"),
        };
        return Err(ApiError::bad_request(format!(
            "channel '{name}' is not a {want}-plane channel ({hint})"
        )));
    }
    Ok(ch)
}

/// Query flags for the data-ingress POST.
#[derive(Debug, Default, Deserialize)]
pub struct SourcePushQuery {
    /// Keep the stream open after this body: skip the EOF + close bracket so a
    /// later POST can continue the seq numbering.
    #[serde(default)]
    append: Option<String>,
    /// Close the stream: publish the EOF sentinel + `close` emit even when the
    /// body is empty (finishes a previously `?append=1`-fed stream).
    #[serde(default)]
    eof: Option<String>,
}

impl SourcePushQuery {
    fn append(&self) -> bool {
        flag_on(self.append.as_deref())
    }
    fn eof(&self) -> bool {
        flag_on(self.eof.as_deref())
    }
}

/// Summary returned by the data-ingress POST.
#[derive(Debug, Serialize, ToSchema)]
pub struct SourcePushResponse {
    /// The virtual execution id mekhan minted (`st-{instance_id}-{node_id}`).
    pub execution_id: String,
    /// The JetStream subject the bytes were published on.
    pub subject: String,
    /// Data envelopes published by THIS request.
    pub chunks: u64,
    /// Payload bytes published by this request.
    pub bytes: u64,
    /// Next envelope seq on the subject after this request (also the running
    /// element count while the stream is open).
    pub next_seq: u64,
    /// Whether the stream was closed (EOF sentinel + `close` emit published).
    pub closed: bool,
}

/// POST /api/v1/instances/{instance_id}/sources/{node_id}/channels/{channel}/data
///
/// Feed raw bytes into a `stream_source` node's DATA-plane Out channel —
/// mekhan acts as the virtual producer. On the first body byte the `open`
/// control emit (carrying the `{transport, subject, content_type}` descriptor)
/// is routed into the node's control inbox; the body is then chunked into
/// binary envelopes (~64 KiB, per-publish JetStream ack = backpressure) with
/// monotonic `seq` resuming from the subject's last envelope, so `?append=1`
/// re-POSTs continue the numbering. At body end the EOF sentinel + `close`
/// emit are published unless `?append=1`; `?eof=1` with an empty body closes a
/// previously appended stream.
#[utoipa::path(
    post,
    path = "/api/v1/instances/{instance_id}/sources/{node_id}/channels/{channel}/data",
    request_body(
        content = Vec<u8>,
        content_type = "application/octet-stream",
        description = "Raw stream bytes. The request Content-Type is forwarded as the channel's content_type (fallback: the channel's declared Binary content_type)."
    ),
    params(
        ("instance_id" = Uuid, Path, description = "Workflow instance id."),
        ("node_id" = String, Path, description = "stream_source node id in the instance's template graph."),
        ("channel" = String, Path, description = "Declared data-plane Out channel name."),
        ("append" = Option<String>, Query, description = "Keep the stream open after this body (skip EOF + close) so a later POST continues the seq numbering."),
        ("eof" = Option<String>, Query, description = "Close the stream even with an empty body (publish EOF sentinel + close emit)."),
    ),
    responses(
        (status = 200, description = "Bytes published; summary of what was written.", body = SourcePushResponse),
        (status = 400, description = "Malformed path segment, wrong channel direction/plane, or body read error.", body = ErrorResponse),
        (status = 404, description = "Unknown instance / node / channel.", body = ErrorResponse),
        (status = 409, description = "Instance is not running.", body = ErrorResponse),
        (status = 502, description = "NATS/JetStream publish failed.", body = ErrorResponse),
    ),
    tag = "streams",
)]
pub async fn push_stream_source_data(
    State(state): State<AppState>,
    Path((instance_id, node_id, channel)): Path<(Uuid, String, String)>,
    Query(query): Query<SourcePushQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<SourcePushResponse>, ApiError> {
    validate_subject_token("node_id", &node_id)?;
    validate_subject_token("channel", &channel)?;

    let node = load_stream_node(&state, instance_id, &node_id, StreamNodeKind::Source).await?;
    require_running(&node)?;
    let ch = require_out_channel(&node.channels, &channel, ChannelPlane::Data)?;

    // Channel content_type: request header wins, fall back to the channel's
    // declared Binary content_type, then the generic octet-stream.
    let declared_ct = match &ch.element {
        ElementType::Binary { content_type } => Some(content_type.clone()),
        _ => None,
    };
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or(declared_ct)
        .unwrap_or_else(|| "application/octet-stream".to_string());

    let execution_id = virtual_execution_id(instance_id, &node_id);
    let subject = datastream_subject(&execution_id, &channel);
    let metadata = routing_metadata(&node.net_id, &control_inbox_place(&node_id), &execution_id);
    let js = state.nats.jetstream();

    // Ensure both streams exist (idempotent get_or_create, same configs the
    // executor/engine use) — mekhan may be the first publisher on a fresh NATS.
    let stream = ensure_datastream_stream(js)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e))?;
    ensure_events_stream(js)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e))?;

    // Resume the per-subject envelope numbering from the last published
    // envelope so an `?append=1` re-POST stays dense and monotonic.
    let mut next_seq = match stream.get_last_raw_message_by_subject(&subject).await {
        Ok(last) => resume_seq(last.headers.get(HDR_SEQ).map(|v| v.as_str())),
        Err(e)
            if matches!(
                e.kind(),
                async_nats::jetstream::stream::LastRawMessageErrorKind::NoMessageFound
            ) =>
        {
            0
        }
        Err(e) => {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                format!("datastream last-seq lookup: {e}"),
            ));
        }
    };

    let map_publish = |e: String| ApiError::new(StatusCode::BAD_GATEWAY, e);

    let mut opened = false;
    let mut chunks: u64 = 0;
    let mut bytes_written: u64 = 0;
    let mut data_stream = body.into_data_stream();
    while let Some(frame) = data_stream.next().await {
        let mut frame: Bytes =
            frame.map_err(|e| ApiError::bad_request(format!("body read: {e}")))?;
        if frame.is_empty() {
            continue;
        }
        if !opened {
            // First byte: publish the `open` descriptor emit so the consumer
            // can connect EARLY, while we're still streaming. Dedup-id'd
            // once-per-channel, so an `?append=1` re-POST's open is dropped by
            // JetStream / the engine's DedupIndex.
            let open = ControlEmitEvent {
                execution_id: execution_id.clone(),
                channel: channel.clone(),
                kind: ControlKind::Open,
                payload_json: open_descriptor(&subject, Some(&content_type)).to_string(),
                item_idx: 0,
                count: 0,
                episode_uid: String::new(),
                metadata: metadata.clone(),
            };
            publish_control_emit(js, &open).await.map_err(map_publish)?;
            opened = true;
        }
        while !frame.is_empty() {
            let take = frame.len().min(ENVELOPE_CHUNK);
            let chunk = frame.split_to(take);
            publish_envelope(js, &subject, next_seq, &content_type, chunk, false)
                .await
                .map_err(map_publish)?;
            next_seq += 1;
            chunks += 1;
            bytes_written += take as u64;
        }
    }

    // Body end: close the stream (EOF sentinel + `close` emit with the total
    // element count) unless `?append=1` keeps it open for a follow-up POST.
    // `?eof=1` forces the close even when this body was empty.
    let close = query.eof() || !query.append();
    if close {
        publish_envelope(js, &subject, next_seq, "", Bytes::new(), true)
            .await
            .map_err(map_publish)?;
        let close_emit = ControlEmitEvent {
            execution_id: execution_id.clone(),
            channel: channel.clone(),
            kind: ControlKind::Close,
            // Data-plane close carries `{count, status}` in the payload (the
            // `count` FIELD is control-plane-only — stays 0, mirroring the SDK).
            payload_json: data_close_payload(next_seq),
            item_idx: 0,
            count: 0,
            episode_uid: String::new(),
            metadata,
        };
        publish_control_emit(js, &close_emit)
            .await
            .map_err(map_publish)?;
    }

    Ok(Json(SourcePushResponse {
        execution_id,
        subject,
        chunks,
        bytes: bytes_written,
        next_seq,
        closed: close,
    }))
}

/// Request body of the control-plane emit endpoint: the episode's items.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SourceEmitRequest {
    /// The episode's items, in order. Each is published as one `item` emit
    /// (`item_idx` = its position); the episode closes with `count = len`.
    pub items: Vec<serde_json::Value>,
}

/// Summary returned by the control-plane emit endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct SourceEmitResponse {
    /// The virtual execution id mekhan minted (`st-{instance_id}-{node_id}`).
    pub execution_id: String,
    /// The fresh per-request episode correlation id stamped on every emit.
    pub episode_uid: String,
    /// Number of items published.
    pub items: u64,
}

/// POST /api/v1/instances/{instance_id}/sources/{node_id}/channels/{channel}/emit
///
/// Publish one fused CONTROL-plane episode into a `stream_source` node's
/// channel: `open` + `item`(idx 0..n) + `close`(count=n), all under one fresh
/// `episode_uid` minted per request — so the engine's `each` join fires
/// downstream once per item and a `gather` join can collect the episode as one
/// counted barrier. Each emit is routed into the node's control inbox via the
/// same `control_emit` metadata a real executor job would stamp.
#[utoipa::path(
    post,
    path = "/api/v1/instances/{instance_id}/sources/{node_id}/channels/{channel}/emit",
    request_body = SourceEmitRequest,
    params(
        ("instance_id" = Uuid, Path, description = "Workflow instance id."),
        ("node_id" = String, Path, description = "stream_source node id in the instance's template graph."),
        ("channel" = String, Path, description = "Declared control-plane Out channel name."),
    ),
    responses(
        (status = 200, description = "Episode published (open + items + close).", body = SourceEmitResponse),
        (status = 400, description = "Malformed path segment or wrong channel direction/plane.", body = ErrorResponse),
        (status = 404, description = "Unknown instance / node / channel.", body = ErrorResponse),
        (status = 409, description = "Instance is not running.", body = ErrorResponse),
        (status = 502, description = "NATS/JetStream publish failed.", body = ErrorResponse),
    ),
    tag = "streams",
)]
pub async fn emit_stream_source_items(
    State(state): State<AppState>,
    Path((instance_id, node_id, channel)): Path<(Uuid, String, String)>,
    Json(req): Json<SourceEmitRequest>,
) -> Result<Json<SourceEmitResponse>, ApiError> {
    validate_subject_token("node_id", &node_id)?;
    validate_subject_token("channel", &channel)?;

    let node = load_stream_node(&state, instance_id, &node_id, StreamNodeKind::Source).await?;
    require_running(&node)?;
    require_out_channel(&node.channels, &channel, ChannelPlane::Control)?;

    let execution_id = virtual_execution_id(instance_id, &node_id);
    let metadata = routing_metadata(&node.net_id, &control_inbox_place(&node_id), &execution_id);
    let js = state.nats.jetstream();
    ensure_events_stream(js)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e))?;

    let map_publish = |e: String| ApiError::new(StatusCode::BAD_GATEWAY, e);
    let episode_uid = Uuid::new_v4().to_string();
    let emit =
        |kind: ControlKind, payload_json: String, item_idx: u64, count: u64| ControlEmitEvent {
            execution_id: execution_id.clone(),
            channel: channel.clone(),
            kind,
            payload_json,
            item_idx,
            count,
            episode_uid: episode_uid.clone(),
            metadata: metadata.clone(),
        };

    // open — uniform episode lifecycle marker on the control plane (no payload).
    publish_control_emit(js, &emit(ControlKind::Open, String::new(), 0, 0))
        .await
        .map_err(map_publish)?;
    // item(0..n) — one element each, idx-stamped for the gather reorder.
    for (idx, item) in req.items.iter().enumerate() {
        let payload_json = serde_json::to_string(item)
            .map_err(|e| ApiError::bad_request(format!("item {idx} not serializable: {e}")))?;
        publish_control_emit(js, &emit(ControlKind::Item, payload_json, idx as u64, 0))
            .await
            .map_err(map_publish)?;
    }
    // close — stamps the total count so a gather barrier knows the episode size.
    let count = req.items.len() as u64;
    publish_control_emit(js, &emit(ControlKind::Close, String::new(), 0, count))
        .await
        .map_err(map_publish)?;

    Ok(Json(SourceEmitResponse {
        execution_id,
        episode_uid,
        items: count,
    }))
}

/// Query for the sink egress tap (same `follow` semantics as
/// `tap_channel_data`).
#[derive(Debug, Default, Deserialize)]
pub struct SinkTapQuery {
    #[serde(default)]
    follow: Option<String>,
}

/// GET /api/v1/instances/{instance_id}/sinks/{node_id}/data
///
/// Stream a `stream_sink` node's sunk bytes out of the platform. The WI-2 sink
/// lowering parks the producer's `open` transport descriptor in
/// `p_{node_id}_data`, which the step_executions projector captures as the
/// node's `outputs` — this endpoint resolves that descriptor, extracts the
/// data-plane subject, and streams the reordered payload bytes through the
/// same tap core as `GET /executions/{id}/channels/{ch}/data` (replay from
/// seq 0; `?follow=1` live-tails until EOF). Until the upstream producer has
/// opened the channel there is no descriptor row yet — the endpoint answers
/// 409 with `Retry-After: 2` so clients poll instead of hanging.
#[utoipa::path(
    get,
    path = "/api/v1/instances/{instance_id}/sinks/{node_id}/data",
    params(
        ("instance_id" = Uuid, Path, description = "Workflow instance id."),
        ("node_id" = String, Path, description = "stream_sink node id in the instance's template graph."),
        ("follow" = Option<String>, Query, description = "Live-tail an in-progress stream: `follow=1` widens the idle patience so long gaps don't end it early (ends at EOF or client disconnect)."),
    ),
    responses(
        (status = 200, description = "Concatenated sunk payload bytes; Content-Type echoes the channel envelope's content_type.", content_type = "application/octet-stream"),
        (status = 400, description = "Malformed node_id path component or node is not a stream_sink.", body = ErrorResponse),
        (status = 404, description = "Unknown instance / node.", body = ErrorResponse),
        (status = 409, description = "The producer has not opened the stream yet (no parked descriptor). Retry-After: 2.", body = ErrorResponse),
        (status = 502, description = "JetStream consumer could not be opened.", body = ErrorResponse),
    ),
    tag = "streams",
)]
pub async fn tap_stream_sink_data(
    State(state): State<AppState>,
    Path((instance_id, node_id)): Path<(Uuid, String)>,
    Query(query): Query<SinkTapQuery>,
) -> Result<Response, ApiError> {
    validate_subject_token("node_id", &node_id)?;

    // Node-kind gate (also yields an honest 404 for unknown instance/node).
    // No running check: replaying a finished instance's sunk stream is legal
    // for as long as the datastream retention keeps the bytes.
    load_stream_node(&state, instance_id, &node_id, StreamNodeKind::Sink).await?;

    // Resolve the parked open descriptor via the step_executions projection.
    let row: Option<(Option<serde_json::Value>,)> = sqlx::query_as(
        "SELECT outputs FROM step_execution \
         WHERE instance_id = $1 AND node_id = $2 AND outputs IS NOT NULL \
         ORDER BY iteration_index DESC LIMIT 1",
    )
    .bind(instance_id)
    .bind(&node_id)
    .fetch_optional(&state.db)
    .await?;

    let subject = row
        .and_then(|(outputs,)| outputs)
        .as_ref()
        .and_then(descriptor_subject);
    let Some(subject) = subject else {
        // Descriptor not parked yet — the producer hasn't opened the channel.
        // 409 + Retry-After so clients poll rather than hang.
        return Ok((
            StatusCode::CONFLICT,
            [(header::RETRY_AFTER, "2")],
            Json(
                ErrorResponse::new(
                    "stream not opened yet: no descriptor parked for this sink (retry shortly)",
                )
                .with_code("conflict"),
            ),
        )
            .into_response());
    };

    // Defensive: the descriptor came out of the engine event log, but it
    // becomes a JetStream filter subject — refuse anything that isn't a
    // concrete datastream subject (no wildcards / whitespace).
    if !subject.starts_with("executor.datastream.")
        || subject
            .chars()
            .any(|c| c == '*' || c == '>' || c.is_whitespace())
    {
        return Err(ApiError::internal(format!(
            "parked descriptor carries an unusable subject: {subject}"
        )));
    }

    tap_datastream_subject(&state, subject, flag_on(query.follow.as_deref())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::ChannelTransport;

    fn channel(name: &str, direction: ChannelDirection, plane: ChannelPlane) -> Channel {
        Channel {
            name: name.to_string(),
            direction,
            plane,
            element: ElementType::Binary {
                content_type: "video/mp4".to_string(),
            },
            transport: ChannelTransport::Jetstream,
        }
    }

    #[test]
    fn require_out_channel_accepts_matching_plane() {
        let chans = vec![
            channel("frames", ChannelDirection::Out, ChannelPlane::Data),
            channel("alerts", ChannelDirection::Out, ChannelPlane::Control),
        ];
        assert!(require_out_channel(&chans, "frames", ChannelPlane::Data).is_ok());
        assert!(require_out_channel(&chans, "alerts", ChannelPlane::Control).is_ok());
    }

    #[test]
    fn require_out_channel_rejects_unknown_name() {
        let chans = vec![channel("frames", ChannelDirection::Out, ChannelPlane::Data)];
        let err = require_out_channel(&chans, "nope", ChannelPlane::Data).unwrap_err();
        assert_eq!(err.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn require_out_channel_rejects_in_direction() {
        let chans = vec![channel("frames", ChannelDirection::In, ChannelPlane::Data)];
        let err = require_out_channel(&chans, "frames", ChannelPlane::Data).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn require_out_channel_rejects_wrong_plane() {
        let chans = vec![channel("frames", ChannelDirection::Out, ChannelPlane::Data)];
        // Data channel addressed via the control-plane /emit endpoint → 400.
        let err = require_out_channel(&chans, "frames", ChannelPlane::Control).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        // Control channel addressed via the data-plane /data endpoint → 400.
        let chans = vec![channel(
            "alerts",
            ChannelDirection::Out,
            ChannelPlane::Control,
        )];
        let err = require_out_channel(&chans, "alerts", ChannelPlane::Data).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn push_query_flag_combinations() {
        let q = |append: Option<&str>, eof: Option<&str>| SourcePushQuery {
            append: append.map(str::to_string),
            eof: eof.map(str::to_string),
        };
        // Default POST → close at body end.
        assert!(!q(None, None).append());
        assert!(!q(None, None).eof());
        // ?append=1 keeps the stream open; bare ?append counts as on.
        assert!(q(Some("1"), None).append());
        assert!(q(Some(""), None).append());
        assert!(!q(Some("false"), None).append());
        // ?eof=1 forces the close (used with an empty body).
        assert!(q(None, Some("1")).eof());
    }

    #[test]
    fn require_running_gates_on_status() {
        let node = |status: &str| StreamNode {
            net_id: "mekhan-x".to_string(),
            status: status.to_string(),
            channels: vec![],
        };
        assert!(require_running(&node("running")).is_ok());
        let err = require_running(&node("completed")).unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
    }
}
