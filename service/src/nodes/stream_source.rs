//! `StreamSource` node declaration (docs/25 §9 Phase 3 —
//! workflow-as-streaming-endpoint INGRESS). Declares `Out` [`Channel`]s an
//! external producer feeds through a mekhan ingress endpoint; each declared
//! channel surfaces as a named source handle, mirroring how an
//! `AutomatedStep`'s `Out` channels derive ports. NO control-flow handles in
//! v1: no inbound control edge, no default `out` — the channel handles are
//! the node's only handles. Lowering (WI-2) synthesizes the standard
//! per-channel place `p_{id}_{name}`.

use crate::compiler::interface::NodeKind;
use crate::models::template::{ChannelDirection, Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static STREAM_SOURCE_DECL: NodeDecl = NodeDecl {
    wire_name: "stream_source",
    display_label: "Stream Source",
    description: Some(
        "Workflow ingress for live streams (docs/25). Declares OUT channels an \
         external producer feeds through a mekhan streaming endpoint; downstream \
         edges wire off the named channel handles.",
    ),
    kind: NodeKind::StreamSource,
    lowers_to_air: true,
    is_join: false,
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::stream_source::lower_stream_source),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    // Structural rules (WI-2): ≥1 channel, all Out, jetstream|nats-latest
    // transports only, no control-flow edges. See
    // `compiler::validate::validate_stream_source`.
    validate: Some(crate::compiler::validate::validate_stream_source),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_passthrough),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // No inbound control edge in v1 — a StreamSource is fed by the external
    // ingress, not by the net.
    vec![]
}

fn output_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // One pass-through output port per declared `Out` channel (mirrors
    // `automated_step::output_ports`'s channel-handle derivation) — a
    // downstream edge wires off it by `sourceHandle == <name>`. No standard
    // `out` handle: the channel handles are the node's only handles.
    let WorkflowNodeData::StreamSource { channels, .. } = data else {
        unreachable!("stream_source::output_ports on non-StreamSource variant");
    };
    channels
        .iter()
        .filter(|c| matches!(c.direction, ChannelDirection::Out))
        .map(|c| Port {
            id: c.name.clone(),
            label: c.name.clone(),
            fields: vec![],
        })
        .collect()
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::StreamSource { channels, .. } = data else {
        unreachable!("stream_source::yjs_encode on non-StreamSource variant");
    };
    // `channels` is `#[serde(default, skip_serializing_if = Vec::is_empty)]`;
    // it must be written explicitly (when non-empty) or the graph→Y.Doc seed +
    // Y.Doc→graph reconstruction would silently drop the declared streaming
    // channels. Empty ⇒ absent key (round-trips to `[]`). Mirrors
    // `automated_step::yjs_encode`'s channels handling verbatim.
    if !channels.is_empty() {
        let ch_val = serde_json::to_value(channels).unwrap_or_default();
        config.insert(txn, "channels", json_value_to_any(&ch_val));
    }
}
