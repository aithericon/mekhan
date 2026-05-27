//! AutomatedStep ↔ workspace-resource borrow planner.
//!
//! Same scanner as [`super::automated_step::automated_step_borrow_plan`],
//! but discriminates against the workspace [`KnownResources`] rather
//! than the slug index — the borrow goes to a publish-time-resolved
//! resource envelope, not an upstream parked producer.

use crate::compiler::error::CompileError;
use crate::models::template::{WorkflowGraph, WorkflowNodeData};

/// One resolved Python `<name>.<attr>` access where `<name>` is a known
/// workspace resource. Direct sibling of `AutomatedStepDataBorrow` — same
/// scanner input, but the head doesn't resolve to a producer slug; it
/// resolves to a workspace resource the caller (publish handler)
/// discovered before invoking the compiler.
///
/// Unlike `AutomatedStepDataBorrow`, there is **no upstream producer**:
/// the resource envelope is materialized at publish time by the resolver
/// and spliced into the AIR. The apply step for this borrow emits a
/// `job_inputs.push` snippet that reads from the spliced `__resources` Rhai
/// map; it does NOT call `wire_read_arc`.
///
/// One borrow per `(consumer, name)` pair regardless of how many fields
/// the Python source reads off the name — the runner stages the whole
/// envelope as `<name>.json` and the Python `AccessibleDict` exposes the
/// fields client-side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutomatedStepResourceBorrow {
    /// Python AutomatedStep that authors the borrow.
    pub consumer_node_id: String,
    /// Workspace-known resource name (`local_pg` in `local_pg.host`). Also
    /// the staged filename stem (`local_pg.json`) and the Python global.
    pub name: String,
    /// Pinned resource_id — rename-safe across publishes.
    pub resource_id: uuid::Uuid,
    /// Resource type name (`postgres`, `openai`, …). Carried through to
    /// downstream consumers (`.pyi` generation, telemetry).
    pub type_name: String,
    /// Latest version at publish time.
    pub latest_version: i32,
}

/// Scan every Python `AutomatedStep`'s entrypoint for `<name>.<attr>`
/// accesses whose `<name>` matches an entry in `known`. Returns one
/// [`AutomatedStepResourceBorrow`] per `(consumer, name)` pair.
///
/// Same lexical scanner as `automated_step_borrow_plan`; the discrimination
/// happens via [`crate::compiler::resource_refs::is_resource_name`] rather
/// than the slug index. A `<head>.<attr>` access where the head matches
/// *both* a slug and a known resource is impossible because
/// `validate_resource_refs` rejects name/slug collisions at compile time
/// — see [`CompileError::ResourceAliasCollidesWithSlug`].
pub(crate) fn automated_step_resource_borrow_plan(
    graph: &WorkflowGraph,
    inline_sources: &std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    known: &crate::compiler::resource_refs::KnownResources,
) -> Result<Vec<AutomatedStepResourceBorrow>, CompileError> {
    use crate::backends::ScanCtx;
    use crate::compiler::resource_binding::collect_resource_heads;

    if known.is_empty() {
        return Ok(Vec::new());
    }

    let mut out: Vec<AutomatedStepResourceBorrow> = Vec::new();
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();

    for node in &graph.nodes {
        let WorkflowNodeData::AutomatedStep { execution_spec, .. } = &node.data else {
            continue;
        };

        let ctx = ScanCtx {
            config: &execution_spec.config,
            node_id: &node.id,
            inline_sources,
            entrypoint: execution_spec.entrypoint.as_deref(),
        };
        let heads = collect_resource_heads(&ctx, execution_spec.backend_type);

        for head in heads {
            let Some(info) = known.get(&head) else {
                continue;
            };
            let key = (node.id.clone(), head.clone());
            if !seen.insert(key) {
                continue;
            }
            out.push(AutomatedStepResourceBorrow {
                consumer_node_id: node.id.clone(),
                name: head,
                resource_id: info.id,
                type_name: info.type_name.clone(),
                latest_version: info.latest_version,
            });
        }
    }
    Ok(out)
}
