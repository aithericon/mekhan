//! `End` node declaration. Workflow-exit variant: declared `terminal` input
//! port shape; no outbound edges; optional `resultMapping` stamps the success
//! envelope onto the terminal token's `exit_code`.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static END_DECL: NodeDecl = NodeDecl {
    wire_name: "end",
    display_label: "End",
    description: Some(
        "Workflow exit. Tokens terminate here; an optional result mapping \
         shapes the instance success envelope.",
    ),
    kind: NodeKind::End,
    lowers_to_air: true,
    is_join: false,
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::end::lower_end),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    // No per-node structural rule (End cardinality is checked graph-wide in
    // `validate`). End IS Rhai-bearing (`resultMapping` expressions) — those are
    // syntax+ref-checked via `nodes::guard_rhai_sources` in `validate_guards`,
    // not here. End emits its inbound token unchanged downstream.
    validate: None,
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_passthrough),
};

fn input_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // Single declared `terminal` port describing the expected terminal-token
    // shape. Empty `fields` (the default) accepts any incoming token,
    // preserving back-compat for pre-typed-ports templates.
    let WorkflowNodeData::End { terminal, .. } = data else {
        unreachable!("end::input_ports on non-End variant");
    };
    vec![terminal.clone()]
}

fn output_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // End has no output port — tokens terminate here.
    vec![]
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::End { result_mapping, .. } = data else {
        unreachable!("end::yjs_encode on non-End variant");
    };
    // Omit when empty — the variant's `#[serde(skip_serializing_if = "Vec::is_empty")]`
    // attribute mirrors this in the JSON wire form. Round-trip parity matters
    // for the publish path (graph → Y.Doc → graph reconstruction).
    if !result_mapping.is_empty() {
        let rm_val =
            serde_json::to_value(result_mapping).unwrap_or(serde_json::Value::Array(vec![]));
        config.insert(txn, "resultMapping", json_value_to_any(&rm_val));
    }
}
