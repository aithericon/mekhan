//! `Timeout` node declaration. Body container that races a wrapped subgraph
//! against a deadline. Three source handles (`out` = done, `timeout`,
//! `body_in`) plus the `body_out` inbound handle that body children's
//! completion edges target — same convention as `Loop`. On timer-win the
//! lowering's post-pass fans a cancel pulse out to every cancellable body
//! child (Human/Executor/Scheduler/Timer/SubWorkflow drains).

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};

pub(crate) static TIMEOUT_DECL: NodeDecl = NodeDecl {
    wire_name: "timeout",
    display_label: "Timeout",
    description: Some(
        "Race a body branch against a deadline; cancel in-flight body work on timer-win.",
    ),
    kind: NodeKind::Timeout,
    lowers_to_air: true,
    is_join: false,
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::timeout::lower_timeout),
    input_ports: input_ports,
    output_ports: output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    vec![
        Port::empty_input(),
        Port {
            id: "body_out".to_string(),
            label: "Body Out".to_string(),
            fields: vec![],
        },
    ]
}

fn output_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    vec![
        Port {
            id: "out".to_string(),
            label: "Done".to_string(),
            fields: vec![],
        },
        Port {
            id: "timeout".to_string(),
            label: "On timeout".to_string(),
            fields: vec![],
        },
        Port {
            id: "body_in".to_string(),
            label: "Body In".to_string(),
            fields: vec![],
        },
    ]
}

fn yjs_encode(
    txn: &mut yrs::TransactionMut<'_>,
    config: &yrs::MapRef,
    data: &WorkflowNodeData,
) {
    use yrs::Map;
    let WorkflowNodeData::Timeout {
        duration_ms_expr, ..
    } = data
    else {
        unreachable!("timeout::yjs_encode on non-Timeout variant");
    };
    config.insert(txn, "durationMsExpr", duration_ms_expr.clone());
}
