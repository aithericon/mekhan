//! `Scope` node declaration. A container that compiles to a `ScenarioGroup`
//! — no places/transitions of its own; children are tagged via the
//! centralised scope-tagging pass in `compile.rs`.
//!
//! Scope has no node-specific config fields (just `label` + `description`,
//! which are stored on the surrounding `WorkflowNode` envelope), so
//! `yjs_encode` is a no-op — matching the empty arm in
//! `yjs/doc_ops.rs::write_node_config` which previously combined Scope with
//! ParallelSplit under `{}`.

use crate::compiler::interface::NodeKind;
use crate::models::template::{Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};

pub(crate) static SCOPE_DECL: NodeDecl = NodeDecl {
    wire_name: "scope",
    display_label: "Scope",
    description: Some(
        "Container node that compiles to a ScenarioGroup. Children are tagged \
         with the scope's group_id; the scope itself emits no places or \
         transitions.",
    ),
    kind: NodeKind::Scope,
    lowers_to_air: true,
    is_join: false,
    // Scope is a structural boundary (ScenarioGroup), not a data producer.
    // No `p_{id}_data` is parked; downstream borrows resolve through the
    // children's own envelopes.
    parks_data_envelope: false,
    lower: Some(crate::compiler::lower::scope::lower_scope),
    input_ports: input_ports,
    output_ports: output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Single anonymous Json pass-through input — Scope routes the token to
    // its children unchanged.
    vec![Port::empty_input()]
}

fn output_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Single `out` port, empty fields (pass-through). The scope's *boundary*
    // port editor lands separately.
    vec![Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: vec![],
    }]
}

fn yjs_encode(
    _txn: &mut yrs::TransactionMut<'_>,
    _config: &yrs::MapRef,
    data: &WorkflowNodeData,
) {
    // No node-specific config — Scope carries only `label` + `description`,
    // which the WorkflowNode envelope encodes outside this function. The
    // legacy arm in `yjs/doc_ops.rs::write_node_config` for Scope (combined
    // with ParallelSplit under `{}`) is a no-op; mirror it here.
    debug_assert!(
        matches!(data, WorkflowNodeData::Scope { .. }),
        "scope::yjs_encode on non-Scope variant",
    );
}
