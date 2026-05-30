//! AutomatedStep ↔ workspace-resource borrow planner.
//!
//! Same scanner as [`super::automated_step::automated_step_borrow_plan`],
//! but discriminates against the workspace [`KnownResources`] rather
//! than the slug index — the borrow goes to a publish-time-resolved
//! resource envelope, not an upstream parked producer.

use crate::compiler::error::CompileError;
use crate::models::template::{ExecutionBackendType, WorkflowGraph, WorkflowNodeData};

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
        // Build the (backend_type, config) pair the scanner reads. Agents
        // are projected through the shared
        // `models::template::agent_to_llm_config` so the same scan rules
        // (`resource_alias_paths`, future `ref_scanner` overlays) apply
        // verbatim. AutomatedStep uses its native config.
        let (backend_type, config_owned, config_ref, entrypoint): (
            ExecutionBackendType,
            Option<serde_json::Value>,
            Option<&serde_json::Value>,
            Option<&str>,
        ) = match &node.data {
            WorkflowNodeData::AutomatedStep { execution_spec, .. } => (
                execution_spec.backend_type,
                None,
                Some(&execution_spec.config),
                execution_spec.entrypoint.as_deref(),
            ),
            WorkflowNodeData::Agent {
                model,
                system_prompt,
                user_prompt,
                response_format,
                images,
                ..
            } => (
                ExecutionBackendType::Llm,
                Some(crate::models::template::agent_to_llm_config(
                    model,
                    system_prompt.as_deref(),
                    user_prompt,
                    response_format.as_ref(),
                    images,
                    &[],
                )),
                None,
                None,
            ),
            _ => continue,
        };
        let config: &serde_json::Value =
            config_ref.unwrap_or_else(|| config_owned.as_ref().unwrap());

        let ctx = ScanCtx {
            config,
            node_id: &node.id,
            inline_sources,
            entrypoint,
        };
        let heads = collect_resource_heads(&ctx, backend_type);

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

// ─── BorrowSource impl ──────────────────────────────────────────────────────

use crate::compiler::borrow::shape::{Borrow, BorrowResolution};
use crate::compiler::borrow::source::{BorrowSource, PlanCtx};

pub(crate) struct ResourceSource;

impl BorrowSource for ResourceSource {
    fn name(&self) -> &'static str {
        "resource"
    }
    fn scan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<Borrow>, CompileError> {
        let mut out = Vec::new();
        for b in automated_step_resource_borrow_plan(
            ctx.graph,
            ctx.inline_sources,
            ctx.known_resources,
        )? {
            // `producer_node` is set to `__resources__/<name>` as a sentinel:
            // it identifies the borrow source on inspection but is never
            // consumed by `wire_read_arc` (the `ResourceEnvelope` apply arm
            // skips it). Matches the legacy hand-chain in `collect_borrows`.
            out.push(Borrow {
                consumer_node_id: b.consumer_node_id,
                producer_node: format!("__resources__/{}", b.name),
                slug: b.name.clone(),
                resolution: BorrowResolution::ResourceEnvelope {
                    name: b.name,
                    resource_id: b.resource_id,
                    type_name: b.type_name,
                    latest_version: b.latest_version,
                },
            });
        }
        Ok(out)
    }
}
