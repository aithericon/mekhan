//! `ProgressUpdate` node declaration. Pass-through control node that sets the
//! owning HPI process's progress fraction (optional message + step counts).
//! Compiles to a shape transition + `process_progress` builtin effect.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNode, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};

pub(crate) static PROGRESS_UPDATE_DECL: NodeDecl = NodeDecl {
    wire_name: "progress_update",
    display_label: "Progress Update",
    description: Some(
        "Record the owning HPI process's overall progress fraction (0.0–1.0) \
         with an optional message and step counts. No-op outside a named process.",
    ),
    kind: NodeKind::ProgressUpdate,
    lowers_to_air: true,
    is_join: false,
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::progress_update::lower_progress_update),
    input_ports: input_ports,
    output_ports: output_ports,
    yjs_encode: yjs_encode as YjsEncodeFn,
};

fn input_ports(_node: &WorkflowNode) -> Vec<Port> {
    // Single anonymous Json pass-through input — the variant carries no
    // node-specific input shape; matches the central
    // `WorkflowNodeData::input_ports` arm.
    vec![Port::empty_input()]
}

fn output_ports(_node: &WorkflowNode) -> Vec<Port> {
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
    let WorkflowNodeData::ProgressUpdate {
        fraction,
        message,
        current_step,
        total_steps,
        ..
    } = data
    else {
        unreachable!("progress_update::yjs_encode on non-ProgressUpdate variant");
    };
    config.insert(txn, "fraction", *fraction);
    if let Some(m) = message {
        config.insert(txn, "message", m.clone());
    }
    // Cast to f64 — Y.Map's `insert` accepts the JS-Number-equivalent
    // primitive (`yrs::Any::Number`); i64 is lossy past 2^53 but the step
    // counters are tiny in practice.
    if let Some(cs) = current_step {
        config.insert(txn, "currentStep", *cs as f64);
    }
    if let Some(ts) = total_steps {
        config.insert(txn, "totalSteps", *ts as f64);
    }
}
