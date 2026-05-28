//! `ParallelSplit` node declaration. One transition with N output ports
//! (one per outgoing edge), each port carrying a fresh copy of the input
//! token. Pure control-flow node — no parked data envelope, no per-config
//! Y.Doc state to encode.
//!
//! Output ports are derived per-node from the actual outgoing edges at
//! editor read time (see existing arm in `models/template.rs::output_ports`).
//! The legacy match arm currently emits a single cosmetic "out" port; the
//! real fan-out is read off `outgoing_edges` during lowering. We mirror
//! that pattern here verbatim — changing it is a separate concern.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};

pub(crate) static PARALLEL_SPLIT_DECL: NodeDecl = NodeDecl {
    wire_name: "parallel_split",
    display_label: "Parallel Split",
    description: Some(
        "Fan out — one inbound token replicated across every outgoing edge \
         in parallel.",
    ),
    kind: NodeKind::ParallelSplit,
    lowers_to_air: true,
    is_join: false,
    // Control-flow node; no parked data envelope. Downstream borrows
    // resolve against the upstream producer of the cloned token, not
    // against the split itself.
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::parallel_split::lower_parallel_split),
    input_ports: input_ports,
    output_ports: output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    validate: Some(crate::compiler::validate::validate_parallel_split),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_passthrough),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Single anonymous Json pass-through input — matches the central
    // `WorkflowNodeData::input_ports` arm for control-flow blocks.
    vec![Port::empty_input()]
}

fn output_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Mirrors the existing `output_ports` arm in `models/template.rs`:
    // ParallelSplit declares a single cosmetic "out" port (empty fields,
    // pass-through). The actual fan-out — one Petri output place per
    // outgoing edge — is materialised at lower time off `outgoing_edges`,
    // not from this declared shape.
    vec![Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: vec![],
    }]
}

fn yjs_encode(
    _txn: &mut yrs::TransactionMut<'_>,
    _config: &yrs::MapRef,
    _data: &WorkflowNodeData,
) {
    // ParallelSplit carries no config beyond label/description (handled
    // outside this fn). Matches the empty arm in
    // `yjs/doc_ops.rs::write_node_config`.
}
