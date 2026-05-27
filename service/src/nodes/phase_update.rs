//! `PhaseUpdate` node declaration. The trivial migration: fixed pass-through
//! ports, no derived shape, simple Y.Doc encode, single lower fn.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static PHASE_UPDATE_DECL: NodeDecl = NodeDecl {
    wire_name: "phase_update",
    display_label: "Phase Update",
    description: Some(
        "Mark a named HPI phase as running/completed/failed/skipped. \
         No-op outside a named process.",
    ),
    kind: NodeKind::PhaseUpdate,
    lowers_to_air: true,
    is_join: false,
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::phase_update::lower_phase_update),
    input_ports: input_ports,
    output_ports: output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Single anonymous Json pass-through input — matches the central
    // `WorkflowNodeData::input_ports` arm. The variant carries no
    // node-specific input shape.
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
    let WorkflowNodeData::PhaseUpdate {
        phase_name,
        status,
        message,
        ..
    } = data
    else {
        unreachable!("phase_update::yjs_encode on non-PhaseUpdate variant");
    };
    config.insert(txn, "phaseName", phase_name.clone());
    let status_val = serde_json::to_value(status).unwrap_or_default();
    config.insert(txn, "status", json_value_to_any(&status_val));
    if let Some(m) = message {
        config.insert(txn, "message", m.clone());
    }
}
