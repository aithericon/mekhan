//! `Loop` node declaration.
//!
//! Parks the iteration counter as `<slug>` in `p_{id}_data` so it survives
//! the AutomatedStep envelope strip (the workflow token is fair game inside
//! the body). Body children attach via `sourceHandle: "body_in"` /
//! `targetHandle: "body_out"`.
//!
//! Per `project_loop_composition_gaps`: `lower_loop` emits a
//! `t_*_body_noop` passthrough so the loop's `p_body_in` doesn't dead-end
//! when authors compose Loop with other blocks. Body authoring is a future
//! refactor; the registry layer doesn't change that contract.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};

pub(crate) static LOOP_DECL: NodeDecl = NodeDecl {
    wire_name: "loop",
    display_label: "Loop",
    description: Some(
        "Iterates a body block until `loop_condition` evaluates true or \
         `max_iterations` is reached. Parks the iteration counter and any \
         author-declared loop state in `p_<id>_data` for downstream borrows.",
    ),
    kind: NodeKind::Loop,
    lowers_to_air: true,
    is_join: false,
    // Loop's lowering parks data via `publish_interface().data_port = Some(...)`
    // (see `compiler/lower/loop_.rs:143`). Authors can reference
    // `<loop_slug>.<field>` from downstream blocks; the borrow planner
    // synthesises a read-arc through the parked envelope.
    parks_data_envelope: true,
    lower: Some(crate::compiler::lower::loop_::lower_loop),
    input_ports: input_ports,
    output_ports: output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Loop accepts the outer `in` and a `body_out` handle from its body
    // children. Both are Json pass-throughs. Mirrors the existing arm in
    // `models/template.rs::input_ports`.
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
    // Loop exposes its outer `out` plus a `body_in` handle that feeds body
    // children. Body children's outgoing edges back into the loop carry
    // `targetHandle: "body_out"` (declared in `input_ports`).
    vec![
        Port {
            id: "out".to_string(),
            label: "Output".to_string(),
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
    let WorkflowNodeData::Loop {
        max_iterations,
        loop_condition,
        ..
    } = data
    else {
        unreachable!("loop_::yjs_encode on non-Loop variant");
    };
    config.insert(txn, "maxIterations", *max_iterations as f64);
    config.insert(txn, "loopCondition", loop_condition.clone());
}
