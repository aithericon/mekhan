//! Multi-cluster scheduler selection — the compiler resolution chain.
//!
//! A `Scheduled`/leased step names its cluster through a three-rung fallback
//! chain (`docs/16-multi-cluster-scheduling.md` §6):
//!
//! ```text
//! effective_cluster(step) =
//!       node.scheduler                  // DeploymentModel::Scheduled.scheduler / Loop.lease.scheduler
//!    ?? template.default_scheduler       // WorkflowGraph.default_scheduler
//!    ?? workspace.default_datacenter     // workspaces.default_datacenter_resource_id → alias
//!    ?? CompileError::SchedulerUnresolved // hard error, no implicit fallback
//! ```
//!
//! Resolution happens ONCE in publish, BEFORE `discover_known_resources`, by
//! rewriting the graph so each Scheduled/leased node carries its *resolved*
//! alias in its own `scheduler` / `lease.scheduler` field. Downstream — both
//! `discover_known_resources` (collection + workspace lookup) and the compiler
//! lowering (`resolve_binding` in `lower/automated_step.rs` + `lower/loop_.rs`)
//! — then read the already-resolved alias from the node data. A single
//! resolution site means collection and lowering cannot drift.
//!
//! Back-compat for the env-global submit path: a `Scheduled { operation:
//! Submit, scheduler: None }` with no template/workspace default stays `None`
//! (today's env-global scheduler-net) — it is framed as "the dev-bootstrap
//! cluster", never truly unresolved while `SLURM_*`/`NOMAD_*` env is set, so
//! `just dev scheduler-up` keeps working. Only `operation: Lease` (and a
//! `Loop.lease`) — which REQUIRE a concrete cluster — hard-error when the chain
//! bottoms out.

use crate::compiler::error::CompileError;
use crate::models::template::{
    DeploymentModel, ScheduledOperation, WorkflowGraph, WorkflowNodeData,
};

/// Resolve the multi-cluster selection chain over a graph, returning a rewritten
/// clone with each Scheduled/leased node's effective cluster alias stamped into
/// its own `scheduler` / `lease.scheduler` field.
///
/// `workspace_default` is the workspace's `default_datacenter` resource already
/// mapped to its alias (path) — `None` when the workspace has no default. The
/// template default is read off `graph.default_scheduler`.
///
/// Errors (one `CompileError::SchedulerUnresolved` per offending node) when a
/// `Lease` step or a `Loop.lease` resolves to nothing through the chain. A
/// `Submit` step with no resolution stays `scheduler: None` (the env-global /
/// dev-bootstrap path) — see the module docs.
pub fn resolve_scheduler_defaults(
    graph: &WorkflowGraph,
    workspace_default: Option<&str>,
) -> Result<WorkflowGraph, Vec<CompileError>> {
    let template_default = graph.default_scheduler.as_deref().and_then(non_blank);
    let workspace_default = workspace_default.and_then(non_blank);

    // The fallback alias the two defaults provide (template wins over workspace).
    let default_alias: Option<&str> = template_default.or(workspace_default);

    let mut out = graph.clone();
    let mut errors: Vec<CompileError> = Vec::new();

    for node in &mut out.nodes {
        match &mut node.data {
            // ── Per-step Scheduled (submit or lease) ──────────────────────
            WorkflowNodeData::AutomatedStep {
                deployment_model:
                    DeploymentModel::Scheduled {
                        scheduler,
                        operation,
                        ..
                    },
                ..
            } => {
                let node_scheduler = scheduler.as_deref().and_then(non_blank);
                if node_scheduler.is_some() {
                    // Explicit node-level alias — already the effective cluster.
                    // Normalize away a blank-but-present string just in case.
                    *scheduler = node_scheduler.map(str::to_string);
                    continue;
                }
                // No node-level alias: inherit the default.
                match default_alias {
                    Some(alias) => {
                        *scheduler = Some(alias.to_string());
                    }
                    None => {
                        // No default. A `Lease` REQUIRES a concrete cluster, so
                        // it is unresolved. A `Submit` stays the env-global /
                        // dev-bootstrap path (None) — never errors here.
                        if *operation == ScheduledOperation::Lease {
                            errors.push(CompileError::SchedulerUnresolved {
                                node_id: node.id.clone(),
                            });
                        }
                    }
                }
            }
            // ── Loop-scoped lease ─────────────────────────────────────────
            WorkflowNodeData::Loop {
                lease: Some(binding),
                ..
            } => {
                let node_scheduler = non_blank(&binding.scheduler);
                if node_scheduler.is_some() {
                    binding.scheduler = node_scheduler.unwrap().to_string();
                    continue;
                }
                // A loop lease REQUIRES a concrete cluster — inherit the default
                // or hard-error.
                match default_alias {
                    Some(alias) => binding.scheduler = alias.to_string(),
                    None => errors.push(CompileError::SchedulerUnresolved {
                        node_id: node.id.clone(),
                    }),
                }
            }
            _ => {}
        }
    }

    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// `Some(trimmed)` for a non-blank string, `None` for empty/whitespace.
fn non_blank(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::WorkflowNode;

    /// Build a `Scheduled` AutomatedStep node from the wire shape (robust to the
    /// struct's many fields — we only care about `deploymentModel.scheduler` +
    /// `operation`).
    fn scheduled_node(
        id: &str,
        scheduler: Option<&str>,
        operation: ScheduledOperation,
    ) -> WorkflowNode {
        let op = match operation {
            ScheduledOperation::Submit => "submit",
            ScheduledOperation::Lease => "lease",
        };
        let mut dm = serde_json::json!({
            "mode": "scheduled",
            "operation": op,
            "jobTemplate": "jt",
        });
        if let Some(s) = scheduler {
            dm["scheduler"] = serde_json::Value::String(s.to_string());
        }
        serde_json::from_value(serde_json::json!({
            "id": id,
            "type": "automated_step",
            "slug": id,
            "position": { "x": 0.0, "y": 0.0 },
            "data": {
                "type": "automated_step",
                "label": "Step",
                "executionSpec": { "backendType": "docker", "config": { "image": "alpine:latest" } },
                "deploymentModel": dm,
            }
        }))
        .expect("scheduled node fixture")
    }

    /// A leased Loop. `lease_scheduler == None` => a `LeaseBinding` with a
    /// BLANK `scheduler` (i.e. a leased loop that names no cluster — the rung
    /// that should inherit a default or hard-error), distinct from a loop with
    /// no lease at all.
    fn loop_node(id: &str, lease_scheduler: Option<&str>) -> WorkflowNode {
        let data = serde_json::json!({
            "type": "loop",
            "label": "Loop",
            "maxIterations": 3,
            "loopCondition": "true",
            "lease": { "scheduler": lease_scheduler.unwrap_or("") },
        });
        serde_json::from_value(serde_json::json!({
            "id": id,
            "type": "loop",
            "slug": id,
            "position": { "x": 0.0, "y": 0.0 },
            "data": data,
        }))
        .expect("loop node fixture")
    }

    fn graph(nodes: Vec<WorkflowNode>, template_default: Option<&str>) -> WorkflowGraph {
        WorkflowGraph {
            nodes,
            edges: vec![],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: template_default.map(str::to_string),
        }
    }

    fn node_scheduler<'a>(g: &'a WorkflowGraph, id: &str) -> Option<&'a str> {
        g.nodes.iter().find(|n| n.id == id).and_then(|n| match &n.data {
            WorkflowNodeData::AutomatedStep {
                deployment_model: DeploymentModel::Scheduled { scheduler, .. },
                ..
            } => scheduler.as_deref(),
            WorkflowNodeData::Loop {
                lease: Some(b), ..
            } => Some(b.scheduler.as_str()),
            _ => None,
        })
    }

    // node.scheduler set → it wins; template/workspace defaults ignored.
    #[test]
    fn node_level_alias_wins() {
        let g = graph(
            vec![scheduled_node("a", Some("node_dc"), ScheduledOperation::Lease)],
            Some("tmpl_dc"),
        );
        let out = resolve_scheduler_defaults(&g, Some("ws_dc")).unwrap();
        assert_eq!(node_scheduler(&out, "a"), Some("node_dc"));
    }

    // node omits scheduler → the template default fills in.
    #[test]
    fn template_default_used_when_node_omits() {
        let g = graph(
            vec![scheduled_node("a", None, ScheduledOperation::Lease)],
            Some("tmpl_dc"),
        );
        let out = resolve_scheduler_defaults(&g, Some("ws_dc")).unwrap();
        assert_eq!(node_scheduler(&out, "a"), Some("tmpl_dc"));
    }

    // node + template omit → the workspace default fills in.
    #[test]
    fn workspace_default_used_when_node_and_template_omit() {
        let g = graph(
            vec![scheduled_node("a", None, ScheduledOperation::Lease)],
            None,
        );
        let out = resolve_scheduler_defaults(&g, Some("ws_dc")).unwrap();
        assert_eq!(node_scheduler(&out, "a"), Some("ws_dc"));
    }

    // Lease + all rungs absent → SchedulerUnresolved.
    #[test]
    fn lease_all_absent_is_unresolved() {
        let g = graph(
            vec![scheduled_node("a", None, ScheduledOperation::Lease)],
            None,
        );
        let errs = resolve_scheduler_defaults(&g, None).unwrap_err();
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            CompileError::SchedulerUnresolved { node_id } => assert_eq!(node_id, "a"),
            other => panic!("expected SchedulerUnresolved, got {other:?}"),
        }
        assert_eq!(errs[0].kind(), "scheduler_unresolved");
        assert_eq!(errs[0].node_id(), Some("a"));
    }

    // Submit + all rungs absent → NOT an error; stays None (env-global /
    // dev-bootstrap path) so `just dev scheduler-up` keeps working.
    #[test]
    fn submit_all_absent_stays_env_global() {
        let g = graph(
            vec![scheduled_node("a", None, ScheduledOperation::Submit)],
            None,
        );
        let out = resolve_scheduler_defaults(&g, None).unwrap();
        assert_eq!(node_scheduler(&out, "a"), None);
    }

    // Loop lease omits scheduler → inherits the template default; absent
    // everywhere → unresolved.
    #[test]
    fn loop_lease_inherits_default_else_unresolved() {
        let inherit = graph(vec![loop_node("lp", None)], Some("tmpl_dc"));
        let out = resolve_scheduler_defaults(&inherit, None).unwrap();
        assert_eq!(node_scheduler(&out, "lp"), Some("tmpl_dc"));

        let bare = graph(vec![loop_node("lp", None)], None);
        let errs = resolve_scheduler_defaults(&bare, None).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].node_id(), Some("lp"));
    }

    // Template default beats the workspace default when both present.
    #[test]
    fn template_beats_workspace() {
        let g = graph(
            vec![scheduled_node("a", None, ScheduledOperation::Lease)],
            Some("tmpl_dc"),
        );
        let out = resolve_scheduler_defaults(&g, Some("ws_dc")).unwrap();
        assert_eq!(node_scheduler(&out, "a"), Some("tmpl_dc"));
    }
}
