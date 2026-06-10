//! `StreamSink` node declaration (docs/25 §9 Phase 3 —
//! workflow-as-streaming-endpoint EGRESS). Declares exactly ONE `In`
//! [`Channel`] (cardinality enforced by validation in WI-2, not the type) the
//! upstream producer edge wires to; mekhan exposes the sunk stream on an
//! egress endpoint. NO control-flow handles in v1: no outbound control edge,
//! no default `in` — the single channel handle is the node's only handle.

use crate::compiler::interface::NodeKind;
use crate::models::template::{ChannelDirection, Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static STREAM_SINK_DECL: NodeDecl = NodeDecl {
    wire_name: "stream_sink",
    display_label: "Stream Sink",
    description: Some(
        "Workflow egress for live streams (docs/25). Declares one IN channel an \
         upstream producer edge wires to; mekhan exposes the sunk stream on a \
         streaming endpoint.",
    ),
    kind: NodeKind::StreamSink,
    lowers_to_air: true,
    is_join: false,
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::stream_sink::lower_stream_sink),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    // Structural rules (WI-2): exactly one In channel, no livekit transport,
    // no outbound edges. See `compiler::validate::validate_stream_sink`.
    validate: Some(crate::compiler::validate::validate_stream_sink),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_passthrough),
};

fn input_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // One pass-through input port per declared `In` channel (mirrors
    // `automated_step::input_ports`'s channel-handle derivation) — the
    // upstream producer edge wires to it by `targetHandle == <name>`.
    // Validation (WI-2) enforces exactly one entry; the derivation stays
    // shape-agnostic.
    let WorkflowNodeData::StreamSink { channels, .. } = data else {
        unreachable!("stream_sink::input_ports on non-StreamSink variant");
    };
    channels
        .iter()
        .filter(|c| matches!(c.direction, ChannelDirection::In))
        .map(|c| Port {
            id: c.name.clone(),
            label: c.name.clone(),
            fields: vec![],
        })
        .collect()
}

fn output_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // No outbound control edge in v1 — a StreamSink terminates the stream at
    // the mekhan egress; nothing on the net consumes it downstream.
    vec![]
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::StreamSink { channels, .. } = data else {
        unreachable!("stream_sink::yjs_encode on non-StreamSink variant");
    };
    // Mirrors `automated_step::yjs_encode`'s channels handling verbatim —
    // explicit write when non-empty, absent key when empty (round-trips to
    // `[]`); see `stream_source::yjs_encode`.
    if !channels.is_empty() {
        let ch_val = serde_json::to_value(channels).unwrap_or_default();
        config.insert(txn, "channels", json_value_to_any(&ch_val));
    }
}
