//! `Failure` node declaration. Pass-through control node that stamps the
//! workflow token's `exit_code = { ok: false, error: ... }` envelope, emits a
//! breadcrumb, and fires `process_fail`. The net continues to its normal End —
//! this is a process-level marker, not a net kill-switch.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static FAILURE_DECL: NodeDecl = NodeDecl {
    wire_name: "failure",
    display_label: "Failure",
    description: Some(
        "Mark the owning HPI process as failed with a templated message and \
         stamp the error envelope onto the workflow token. No-op outside a \
         named process.",
    ),
    kind: NodeKind::Failure,
    lowers_to_air: true,
    is_join: false,
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::failure::lower_failure),
    input_ports: input_ports,
    output_ports: output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    // No per-node structural rule. Failure IS Rhai-bearing
    // (`errorResultMapping` expressions) — those are syntax+ref-checked via
    // `nodes::guard_rhai_sources` in `validate_guards`, not here. The token is
    // forwarded unchanged (Failure is a process-level marker, not a transform).
    validate: None,
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_passthrough),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Single anonymous Json pass-through input — the variant carries no
    // node-specific input shape; matches the central
    // `WorkflowNodeData::input_ports` arm.
    vec![Port::empty_input()]
}

fn output_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    vec![Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: vec![],
    }]
}

fn yjs_encode(
    txn: &mut yrs::TransactionMut<'_>,
    config: &yrs::MapRef,
    data: &WorkflowNodeData,
) {
    use yrs::Map;
    let WorkflowNodeData::Failure {
        failure_message,
        error_result_mapping,
        ..
    } = data
    else {
        unreachable!("failure::yjs_encode on non-Failure variant");
    };
    if let Some(m) = failure_message {
        config.insert(txn, "failureMessage", m.clone());
    }
    // Omit when empty — the variant's `#[serde(skip_serializing_if = "Vec::is_empty")]`
    // mirrors this on the JSON wire side; preserves graph→Y.Doc→graph
    // round-trip parity in the publish path.
    if !error_result_mapping.is_empty() {
        let erm_val = serde_json::to_value(error_result_mapping)
            .unwrap_or(serde_json::Value::Array(vec![]));
        config.insert(txn, "errorResultMapping", json_value_to_any(&erm_val));
    }
}
