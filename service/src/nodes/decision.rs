//! `Decision` node declaration. Output ports are **derived from `conditions`**
//! — one per branch (id = `BranchCondition.edge_id`, label = branch label) —
//! plus an optional `default` catch-all when `default_branch` is set. Branch
//! ports carry empty `fields` (Phase 4 pass-through), so downstream type-checking
//! flows through unchanged.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static DECISION_DECL: NodeDecl = NodeDecl {
    wire_name: "decision",
    display_label: "Decision",
    description: Some(
        "Route the token down at most one of several branches based on Rhai guard \
         expressions evaluated against the inbound payload. Switch/case fallthrough \
         semantics: branch i fires only when its guard holds AND every \
         higher-precedence guard does not.",
    ),
    kind: NodeKind::Decision,
    lowers_to_air: true,
    is_join: false,
    // Decision is control-flow only — it routes the token down one branch but
    // does not park a write-once business envelope. Downstream nodes still
    // borrow via the inbound producer's `<slug>.<field>`, not via the
    // Decision's own id.
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::decision::lower_decision),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    validate: Some(crate::compiler::validate::validate_decision),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_passthrough),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Single anonymous Json pass-through input — Decision routes the inbound
    // token unchanged down one branch.
    vec![Port::empty_input()]
}

fn output_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // Derived: one port per condition (id = edge_id, label = branch label),
    // plus a `default` port when `default_branch` is set. Branch ports have
    // empty `fields` (Phase 4 pass-through), so downstream type-checking
    // flows through unchanged.
    let WorkflowNodeData::Decision {
        conditions,
        default_branch,
        ..
    } = data
    else {
        unreachable!("decision::output_ports on non-Decision variant");
    };
    let mut out: Vec<Port> = conditions
        .iter()
        .map(|c| Port {
            id: c.edge_id.clone(),
            label: c.label.clone(),
            fields: vec![],
        })
        .collect();
    if let Some(default_id) = default_branch {
        out.push(Port {
            id: default_id.clone(),
            label: "Default".to_string(),
            fields: vec![],
        });
    }
    out
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::Decision {
        conditions,
        default_branch,
        ..
    } = data
    else {
        unreachable!("decision::yjs_encode on non-Decision variant");
    };
    let conds_val = serde_json::to_value(conditions).unwrap_or(serde_json::Value::Array(vec![]));
    config.insert(txn, "conditions", json_value_to_any(&conds_val));
    if let Some(db) = default_branch {
        config.insert(txn, "defaultBranch", db.clone());
    }
}
