//! `HumanTask` node declaration. Output port is **derived** from the union of
//! every step's Input blocks — the registry holds the same logic the central
//! `WorkflowNodeData::output_ports` arm used.

use crate::compiler::interface::NodeKind;
use crate::compiler::rhai_gen::build_human_task_injection_logic;
use crate::models::template::{
    derive_human_task_output_port, task_step_list_json_schema, FieldKind, Port, PortField,
    WorkflowNodeData,
};
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
    // Only variant with wiring_logic: the inbound-edge transition binds each
    // step input's `{{ name }}` slot to the upstream token's field path before
    // the human-task effect fires.
    wiring_logic: Some(build_human_task_injection_logic),
    yjs_encode: yjs_encode as YjsEncodeFn,
    // The unmerged-fan-in warning (shared with AutomatedStep) — never errors,
    // just `tracing::warn!`s when this work node has >1 incoming edge.
    validate: Some(crate::compiler::validate::warn_unmerged_fan_in),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_human_task),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // A single permissive `steps` Json field carrying the full
    // `TaskStepConfig[]` JSON Schema. The schema is advisory for ordinary
    // sequence edges (the field is Json → permissive in `validate_edges_typed`
    // and `validate_token`), but when an agent calls this HumanTask as a tool,
    // `port_to_input_schema` surfaces the rich schema so the model produces a
    // valid dynamic-form block list to drive `steps_ref`.
    vec![Port {
        id: "in".to_string(),
        label: "Input".to_string(),
        fields: vec![PortField {
            name: "steps".to_string(),
            label: "Form steps".to_string(),
            kind: FieldKind::Json,
            required: false,
            options: None,
            description: Some("Dynamic form step/block list — see schema".to_string()),
            accept: None,
            schema: Some(task_step_list_json_schema()),
        }],
    }]
}

fn output_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // Derived single `out` port whose fields are the union of every Input
    // block's `TaskFieldConfig` across all steps (first-wins on duplicate
    // names). Matches the central arm in `WorkflowNodeData::output_ports`.
    let WorkflowNodeData::HumanTask {
        steps, steps_ref, ..
    } = data
    else {
        unreachable!("human_task::output_ports on non-HumanTask variant");
    };
    if steps_ref.is_some() {
        // Dynamic form: field names are unknown at compile time → opaque port.
        return vec![Port {
            id: "out".to_string(),
            label: "Output".to_string(),
            fields: vec![],
        }];
    }
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
        steps_ref,
        ..
    } = data
    else {
        unreachable!("human_task::yjs_encode on non-HumanTask variant");
    };
    config.insert(txn, "taskTitle", task_title.clone());
    if let Some(inst) = instructions_mdsvex {
        config.insert(txn, "instructionsMdsvex", inst.clone());
    }
    if let Some(sr) = steps_ref {
        config.insert(txn, "stepsRef", sr.clone());
    }
    let steps_val = serde_json::to_value(steps).unwrap_or(serde_json::Value::Array(vec![]));
    config.insert(txn, "steps", json_value_to_any(&steps_val));
}
