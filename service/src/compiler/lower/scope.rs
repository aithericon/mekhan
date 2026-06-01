//! `WorkflowNodeData::Scope` lowering. Compiles to a `ScenarioGroup`; no
//! places/transitions of its own. Children are compiled as normal nodes and
//! tagged with the scope's group_id via the centralised scope-tagging pass
//! in `compile.rs`.

use super::*;

pub(crate) fn lower_scope(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Scope { label, .. } = &cx.node.data else {
        unreachable!("lower_scope on non-Scope node")
    };
    // Scope compiles to a ScenarioGroup. No places/transitions of its own —
    // children are compiled as normal nodes and tagged with this group's ID
    // via the centralised scope-tagging pass in `compile.rs`.
    let group_id = format!("grp_{id}");
    let parent_group = cx.fixups.scope_groups.get(id).cloned();
    cx.fixups
        .groups
        .push((group_id, label.clone(), parent_group));
    // Protocol: publish the interface even though Scope has no boundary
    // places (kind alone marks its presence; ownership is filled centrally).
    cx.publish_interface();
    Ok(())
}
