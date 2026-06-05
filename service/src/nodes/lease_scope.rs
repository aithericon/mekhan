//! `LeaseScope` node declaration.
//!
//! A container that HOLDS one `datacenter` allocation across its whole interior
//! region (acquire on enter / release on exit). Decouples "hold an allocation"
//! from "loop": any `Scheduled { Submit }` step nested inside — directly or
//! through an intervening plain `Loop` — runs ON the held alloc by containment
//! (no per-step `run_on_lease` flag; see `enclosing_leased_scope_slug`).
//!
//! Children attach via the same `body_in`/`body_out` interior handles as Loop
//! (`parent_id == lease_scope.id`); the perimeter `in`/`out` handles connect to
//! the outer flow. The held grant is parked write-once at `p_<id>_data` under a
//! `lease` key, borrowable downstream as `<scope_slug>.lease.<field>`.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static LEASE_SCOPE_DECL: NodeDecl = NodeDecl {
    wire_name: "lease_scope",
    display_label: "Lease Scope",
    description: Some(
        "Container that holds ONE datacenter allocation for the duration of its \
         body (acquire on enter / release on exit). Any Scheduled step inside \
         runs on the held allocation by containment — compose a Loop inside for \
         warm iteration, or sequential steps for a warm pipeline.",
    ),
    kind: NodeKind::LeaseScope,
    lowers_to_air: true,
    is_join: false,
    // LeaseScope's lowering parks the held lease via
    // `publish_interface().data_port = Some(...)` (see
    // `compiler/lower/lease_scope.rs`). Authors reference `<scope_slug>.lease.*`
    // from body/downstream blocks; the borrow planner synthesises a read-arc
    // through the parked envelope.
    parks_data_envelope: true,
    lower: Some(crate::compiler::lower::lease_scope::lower_lease_scope),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    validate: Some(crate::compiler::validate::validate_lease_scope),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_lease_scope),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // LeaseScope accepts the outer `in` and a `body_out` handle from its body
    // children. Both are Json pass-throughs — identical to Loop's handle set.
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
    // LeaseScope exposes its outer `out` plus a `body_in` handle that feeds body
    // children. Body children's outgoing edges back into the scope carry
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
    let WorkflowNodeData::LeaseScope {
        lease,
        requirements,
        ..
    } = data
    else {
        unreachable!("lease_scope::yjs_encode on non-LeaseScope variant");
    };
    // Persist the REQUIRED capacity lease binding through Yjs, or it is silently
    // dropped on publish's `doc_to_graph` reconstruction (mirrors the Loop-lease
    // persistence rationale). Key `lease` matches the serde field name so
    // `doc_to_graph`'s generic config-merge + `from_value` round-trips it back
    // into the struct.
    let lease_val = serde_json::to_value(lease).unwrap_or(serde_json::Value::Null);
    config.insert(txn, "lease", json_value_to_any(&lease_val));
    // Persist the optional presence-placement Requirements the same way (the
    // scope picks WHICH runner to hold). Omitted when `None` so a datacenter
    // lease's config stays clean and `from_value` defaults it back to `None`.
    if let Some(req) = requirements {
        let req_val = serde_json::to_value(req).unwrap_or(serde_json::Value::Null);
        config.insert(txn, "requirements", json_value_to_any(&req_val));
    }
}
