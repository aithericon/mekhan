//! `Trigger` node declaration. The protocol exception: pre-compile dispatcher
//! concern, no AIR shape, no `NodeInterface` entry. `lower: None` and
//! `lowers_to_air: false` are load-bearing — the dispatcher in
//! `compiler/lower/mod.rs` reads these to skip Trigger correctly.
//!
//! Trigger DOES participate in editor concerns (palette, property panel,
//! cosmetic output port for edge drawing) — those flow from `input_ports` /
//! `output_ports` and the descriptor on `GET /api/v1/node-types`.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static TRIGGER_DECL: NodeDecl = NodeDecl {
    wire_name: "trigger",
    display_label: "Trigger",
    description: Some(
        "External event source that fires the workflow. Pre-compile concern; \
         not part of the AIR.",
    ),
    kind: NodeKind::Trigger,
    lowers_to_air: false,
    is_join: false,
    parks_data_envelope: false,
    lower: None,
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    // Trigger is a pre-compile dispatcher concern: it never lowers, so its
    // `out_shape` is never consulted by `analyze` (Trigger nodes are excluded
    // from the topo order's shape pass). Declaring the pass-through hook anyway
    // keeps the `token_shape_hook_declared_for_every_variant` conformance test
    // honest (every variant declares one). No per-node structural rule —
    // Trigger validation lives in the dedicated `validate_triggers` pass.
    validate: None,
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_passthrough),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Triggers are never edge targets. The editor refuses to draw an edge
    // into a Trigger node; returning `[]` makes any malformed graph that
    // does attempt it surface as `UnknownTargetPort` in `validate_edges_typed`.
    vec![]
}

fn output_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Cosmetic single `out` port — `validate_edges_typed` skips
    // type-checking when the source is a Trigger, and payload-mapping
    // validation handles the field-level contract.
    vec![Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: vec![],
    }]
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::Trigger {
        source,
        concurrency,
        payload_mapping,
        reply_default,
        enabled,
        ..
    } = data
    else {
        unreachable!("trigger::yjs_encode on non-Trigger variant");
    };
    let source_val = serde_json::to_value(source).unwrap_or_default();
    config.insert(txn, "source", json_value_to_any(&source_val));
    let concurrency_val = serde_json::to_value(concurrency).unwrap_or_default();
    config.insert(txn, "concurrency", json_value_to_any(&concurrency_val));
    let mapping_val =
        serde_json::to_value(payload_mapping).unwrap_or(serde_json::Value::Array(vec![]));
    config.insert(txn, "payloadMapping", json_value_to_any(&mapping_val));
    if let Some(rd) = reply_default {
        let rd_val = serde_json::to_value(rd).unwrap_or_default();
        config.insert(txn, "replyDefault", json_value_to_any(&rd_val));
    }
    config.insert(txn, "enabled", *enabled);
}
