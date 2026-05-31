//! `SubWorkflow` node declaration. Spawns a child net via the engine's
//! `spawn_net` machinery: request → spawn (child AIR embedded) → bridge_out
//! to spawned child, terminal reply on a `bridge_in` reply place. Sequential
//! call/return is the v1 contract.
//!
//! The decl's `lower` references **the top-level entry point only**
//! (`lower_subworkflow`). Other helpers in `compiler/subworkflow.rs` —
//! `make_child_callable`, child-AIR merge, etc. — are called from inside the
//! lower fn and the publish path; they're not re-routed through the
//! registry. Keeps the registry's role narrow: dispatch source-of-truth,
//! not a full re-export hub.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static SUB_WORKFLOW_DECL: NodeDecl = NodeDecl {
    wire_name: "sub_workflow",
    display_label: "Sub-workflow",
    description: Some(
        "Calls another published template as a child net and returns its \
         terminal result, correlated per invocation. The child AIR is \
         embedded at the parent's publish time (per `version_pin`).",
    ),
    kind: NodeKind::SubWorkflow,
    lowers_to_air: true,
    is_join: false,
    // SubWorkflow's lowering publishes `data_port` (see
    // `compiler/lower/subworkflow.rs:188`). The task spec lists this as
    // `parks_data_envelope: false` — the child workflow parks its own
    // envelope at its terminal places, while the SubWorkflow node here
    // simply routes the reply payload. Borrow planning happens against
    // the declared `output` Port shape, not against the parked node.
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::subworkflow::lower_subworkflow),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    validate: None,
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_sub_workflow),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // SubWorkflow accepts the single anonymous upstream token; its
    // `input_mapping` shapes it into the child Start input at compile
    // time, so the parent-side input port is a Json pass-through.
    vec![Port::empty_input()]
}

fn output_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // Declared child-result success output + an always-present "error"
    // output (child failure / spawn failure). Mirrors AutomatedStep; the
    // compiler maps "error" to `p_{id}_error`.
    let WorkflowNodeData::SubWorkflow { output, .. } = data else {
        unreachable!("sub_workflow::output_ports on non-SubWorkflow variant");
    };
    vec![
        output.clone(),
        Port {
            id: "error".to_string(),
            label: "On error".to_string(),
            fields: vec![],
        },
    ]
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::SubWorkflow {
        template_id,
        version_pin,
        input_mapping,
        output,
        ..
    } = data
    else {
        unreachable!("sub_workflow::yjs_encode on non-SubWorkflow variant");
    };
    config.insert(txn, "templateId", template_id.to_string());
    let vp_val = serde_json::to_value(version_pin).unwrap_or_default();
    config.insert(txn, "versionPin", json_value_to_any(&vp_val));
    if !input_mapping.is_empty() {
        let im_val =
            serde_json::to_value(input_mapping).unwrap_or(serde_json::Value::Array(vec![]));
        config.insert(txn, "inputMapping", json_value_to_any(&im_val));
    }
    let out_val = serde_json::to_value(output).unwrap_or_default();
    config.insert(txn, "output", json_value_to_any(&out_val));
}
