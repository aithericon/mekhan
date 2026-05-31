//! `Join` node declaration. **The only variant with `is_join: true`.**
//!
//! Join edges bypass the place-merge optimization in `compiler/wire.rs`:
//! each inbound edge gets its own named input place (`p_<id>_in_<edge_id>`),
//! and the join transition consumes every named input simultaneously (when
//! `mode == All`) or one-at-a-time (when `mode == Any`). The `is_join` flag
//! is read by `wire.rs::edge_target_place` and the can-merge gate at
//! `wire.rs:95` — currently via `matches!(WorkflowNodeData::Join { .. })`.
//! The legacy `matches!` check stays during PR2 coexistence; the registry
//! now declares the same fact so PR2's final cleanup pass can rewire
//! `wire.rs` to `decl.is_join` in one shot.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static JOIN_DECL: NodeDecl = NodeDecl {
    wire_name: "join",
    display_label: "Join",
    description: Some(
        "Unified converge primitive. `mode == All` waits for every incoming \
         branch and merges payloads per `merge_strategy`; `mode == Any` \
         fires per arriving token (XOR-join). Both modes park each branch's \
         inbound payload at `p_<id>_data`.",
    ),
    kind: NodeKind::Join,
    lowers_to_air: true,
    // ── The only variant where this is true. ──
    // `wire.rs` reads via `matches!(WorkflowNodeData::Join { .. })` today;
    // the legacy check coexists with this registry declaration until PR2's
    // final cleanup pass rewires `wire.rs` to consult `decl.is_join`.
    is_join: true,
    // Join parks each branch's inbound payload at `p_<id>_data`. The task
    // spec lists it as `parks_data_envelope: false` because the join itself
    // is a *control* converge — its parked data is the merged result, not
    // a per-author authoring surface the borrow planner scans. The borrow
    // planner reads against the declared `output` Port, not against the
    // raw parked envelope.
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::join::lower_join),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    validate: None,
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_passthrough),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Single anonymous Json pass-through input — matches the central
    // `WorkflowNodeData::input_ports` arm for control-flow blocks. The
    // actual per-edge named inbound places are minted by `wire.rs` (the
    // `is_join` carve-out); the declared shape stays a pass-through.
    vec![Port::empty_input()]
}

fn output_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // Join carries an explicit output Port describing the parked
    // `<slug>.<field>` shape downstream borrows can read.
    let WorkflowNodeData::Join { output, .. } = data else {
        unreachable!("join::output_ports on non-Join variant");
    };
    vec![output.clone()]
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::Join {
        mode,
        merge_strategy,
        output,
        ..
    } = data
    else {
        unreachable!("join::yjs_encode on non-Join variant");
    };
    let mode_val = serde_json::to_value(mode).unwrap_or_default();
    config.insert(txn, "mode", json_value_to_any(&mode_val));
    if let Some(ms) = merge_strategy {
        let ms_val = serde_json::to_value(ms).unwrap_or_default();
        config.insert(txn, "mergeStrategy", json_value_to_any(&ms_val));
    }
    let out_val = serde_json::to_value(output).unwrap_or_default();
    config.insert(txn, "output", json_value_to_any(&out_val));
}
