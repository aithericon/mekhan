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
use crate::yjs::persistence::json_value_to_any;

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
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    validate: Some(crate::compiler::validate::validate_loop),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_loop),
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

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::Loop {
        max_iterations,
        loop_condition,
        accumulators,
        lease,
        ..
    } = data
    else {
        unreachable!("loop_::yjs_encode on non-Loop variant");
    };
    config.insert(txn, "maxIterations", *max_iterations as f64);
    config.insert(txn, "loopCondition", loop_condition.clone());
    // Accumulators serialize as a yrs array of maps {var, init, mergeExpr}
    // (camelCase via the `LoopAccumulator` serde rename), mirroring how
    // `decision::yjs_encode` encodes its `conditions` array.
    if !accumulators.is_empty() {
        let accs_val =
            serde_json::to_value(accumulators).unwrap_or(serde_json::Value::Array(vec![]));
        config.insert(txn, "accumulators", json_value_to_any(&accs_val));
    }
    // L3 loop-scoped lease (datacenter alias + optional request). MUST be
    // persisted through Yjs or it is silently dropped on publish's
    // `doc_to_graph` reconstruction (the `..` previously ate it) — offline
    // `compile_to_air` keeps it directly, which is why `compiler_e2e` passed
    // but the live published instance lowered as a plain (lease-less) loop.
    // Key `lease` matches the serde field name so `doc_to_graph`'s generic
    // config-merge + `from_value::<WorkflowNodeData>` round-trips it back.
    if let Some(lease) = lease {
        let lease_val = serde_json::to_value(lease).unwrap_or(serde_json::Value::Null);
        config.insert(txn, "lease", json_value_to_any(&lease_val));
    }
}
