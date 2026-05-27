//! `HumanTask` node declaration. Output port is **derived** from the union of
//! every step's Input blocks — the registry holds the same logic the central
//! `WorkflowNodeData::output_ports` arm used.

use crate::compiler::interface::NodeKind;
use crate::models::template::{derive_human_task_output_port, Port, WorkflowNode, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static HUMAN_TASK_DECL: NodeDecl = NodeDecl {
    wire_name: "human_task",
    display_label: "Human Task",
    description: Some(
        "Multi-step interactive task assigned to a human. Parks form responses \
         as a write-once envelope downstream borrows can read via `<slug>.<field>`.",
    ),
    kind: NodeKind::HumanTask,
    lowers_to_air: true,
    is_join: false,
    parks_data_envelope: true,
    lower: Some(crate::compiler::lower::human_task::lower_human_task),
    input_ports: input_ports,
    output_ports: output_ports,
    yjs_encode: yjs_encode as YjsEncodeFn,
};

fn input_ports(_node: &WorkflowNode) -> Vec<Port> {
    // Single anonymous Json pass-through input — HumanTask routes the inbound
    // token straight to its form-rendering effect; per-step inputs are derived
    // from outputs not edge contracts.
    vec![Port::empty_input()]
}

fn output_ports(node: &WorkflowNode) -> Vec<Port> {
    // Derived single `out` port whose fields are the union of every Input
    // block's `TaskFieldConfig` across all steps (first-wins on duplicate
    // names). Matches the central arm in `WorkflowNodeData::output_ports`.
    let WorkflowNodeData::HumanTask { steps, .. } = &node.data else {
        unreachable!("human_task::output_ports on non-HumanTask variant");
    };
    vec![derive_human_task_output_port(steps)]
}

fn yjs_encode(
    txn: &mut yrs::TransactionMut<'_>,
    config: &yrs::MapRef,
    data: &WorkflowNodeData,
) {
    use yrs::Map;
    let WorkflowNodeData::HumanTask {
        task_title,
        instructions_mdsvex,
        steps,
        ..
    } = data
    else {
        unreachable!("human_task::yjs_encode on non-HumanTask variant");
    };
    config.insert(txn, "taskTitle", task_title.clone());
    if let Some(inst) = instructions_mdsvex {
        config.insert(txn, "instructionsMdsvex", inst.clone());
    }
    let steps_val = serde_json::to_value(steps).unwrap_or(serde_json::Value::Array(vec![]));
    config.insert(txn, "steps", json_value_to_any(&steps_val));
}
