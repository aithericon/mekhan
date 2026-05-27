//! `Delay` node declaration. Single input, single output. Schedules a timer
//! for a Rhai-evaluated number of milliseconds, then forwards the token. The
//! `durationMsExpr` can reference upstream `<slug>.<field>` envelopes; the
//! borrow planner read-arc-synthesizes those at compile time.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};

pub(crate) static DELAY_DECL: NodeDecl = NodeDecl {
    wire_name: "delay",
    display_label: "Delay",
    description: Some(
        "Pause for a Rhai-evaluated number of milliseconds, then forward the token.",
    ),
    kind: NodeKind::Delay,
    lowers_to_air: true,
    is_join: false,
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::delay::lower_delay),
    input_ports: input_ports,
    output_ports: output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
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
    let WorkflowNodeData::Delay {
        duration_ms_expr, ..
    } = data
    else {
        unreachable!("delay::yjs_encode on non-Delay variant");
    };
    config.insert(txn, "durationMsExpr", duration_ms_expr.clone());
}
