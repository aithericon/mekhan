//! `Start` node declaration. Entry-point variant: declared input shape lands
//! on the `initial` output port; no inbound edges; lowering parks a write-once
//! envelope (`parks_data_envelope: true`) so downstream borrows can read
//! `start.<field>`.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static START_DECL: NodeDecl = NodeDecl {
    wire_name: "start",
    display_label: "Start",
    description: Some(
        "Workflow entry point. Declares the initial token shape and \
         (optionally) registers a named HPI process.",
    ),
    kind: NodeKind::Start,
    lowers_to_air: true,
    is_join: false,
    // Start parks a fork of its seed token at `p_{id}_data` so downstream
    // guards / mappings can borrow `start.<field>` via read-arc synthesis.
    parks_data_envelope: true,
    lower: Some(crate::compiler::lower::start::lower_start),
    input_ports: input_ports,
    output_ports: output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    // No per-node structural rule (Start cardinality is checked graph-wide in
    // `validate`, not per-node).
    validate: None,
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_start),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Start has no inbound edges — the editor refuses to draw edges into a
    // Start node. Empty list surfaces any malformed graph that does so as
    // `UnknownTargetPort` in `validate_edges_typed`.
    vec![]
}

fn output_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // Single declared `initial` port — the shape of the seed token this
    // Start emits. The runtime schema layer enforces tokens entering the
    // graph match this port's `fields`.
    let WorkflowNodeData::Start { initial, .. } = data else {
        unreachable!("start::output_ports on non-Start variant");
    };
    vec![initial.clone()]
}

fn yjs_encode(
    txn: &mut yrs::TransactionMut<'_>,
    config: &yrs::MapRef,
    data: &WorkflowNodeData,
) {
    use yrs::Map;
    let WorkflowNodeData::Start {
        initial,
        process_name,
        ..
    } = data
    else {
        unreachable!("start::yjs_encode on non-Start variant");
    };
    let initial_val = serde_json::to_value(initial)
        .unwrap_or(serde_json::Value::Object(Default::default()));
    config.insert(txn, "initial", json_value_to_any(&initial_val));
    // Opt-in per-instance process-name template. Persist iff non-empty so
    // the graph→Y.Doc seed path (`createTemplate`) and publish's Y.Doc
    // reconstruction don't silently drop it.
    if let Some(pn) = process_name.as_deref().filter(|s| !s.is_empty()) {
        config.insert(txn, "processName", pn.to_string());
    }
}
