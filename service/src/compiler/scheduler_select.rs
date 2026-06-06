//! Multi-cluster scheduler selection — the compiler resolution chain.
//!
//! A `Scheduled`/leased step names its cluster through a three-rung fallback
//! chain (`docs/16-multi-cluster-scheduling.md` §6):
//!
//! ```text
//! effective_cluster(step) =
//!       node.scheduler                  // DeploymentModel::Scheduled.scheduler
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
//! Every `Scheduled` step (standalone or inside a `LeaseScope`)
//! now REQUIRES a concrete cluster — the legacy env-global scheduler fallback
//! fallback is retired.

use crate::compiler::error::CompileError;
use crate::models::template::{DeploymentModel, WorkflowGraph, WorkflowNodeData};

/// Resolve the multi-cluster selection chain over a graph, returning a rewritten
/// clone with each Scheduled/leased node's effective cluster alias stamped into
/// its own `scheduler` / `lease.scheduler` field.
///
/// `workspace_default` is the workspace's `default_datacenter` resource already
/// mapped to its alias (path) — `None` when the workspace has no default. The
/// template default is read off `graph.default_scheduler`.
///
/// Errors (one `CompileError::SchedulerUnresolved` per offending node) when a
/// `Scheduled` step (standalone or leased) resolves to
/// nothing through the chain.
pub fn resolve_scheduler_defaults(
    graph: &WorkflowGraph,
    workspace_default: Option<&str>,
) -> Result<WorkflowGraph, Vec<CompileError>> {
    let template_default = graph.default_scheduler.as_deref().and_then(non_blank);
    let workspace_default = workspace_default.and_then(non_blank);

    // The fallback alias the two defaults provide (template wins over workspace).
    let default_alias: Option<&str> = template_default.or(workspace_default);

    // A `Scheduled` body enclosed by a `LeaseScope` (at any depth — e.g.
    // `LeaseScope { Loop { body } }`) derives its datacenter from the scope's
    // held allocation BY CONTAINMENT; lowering resolves it through
    // `enclosing_leased_scope_slug`. Such a body needs NO node-level scheduler,
    // so it is exempt from the scheduler-required rule below. Precompute the set
    // off the immutable input graph (the parent walk needs whole-graph access,
    // which the `&mut out.nodes` loop cannot borrow).
    let lease_enclosed: std::collections::HashSet<&str> = graph
        .nodes
        .iter()
        .filter(|n| {
            crate::compiler::lower::automated_step::enclosing_leased_scope_slug(n, graph).is_some()
        })
        .map(|n| n.id.as_str())
        .collect();

    let mut out = graph.clone();
    let mut errors: Vec<CompileError> = Vec::new();

    for node in &mut out.nodes {
        match &mut node.data {
            // ── Per-step Scheduled (unifying on the lease path) ───────────
            WorkflowNodeData::AutomatedStep {
                deployment_model: DeploymentModel::Scheduled { scheduler, .. },
                ..
            } => {
                let node_scheduler = scheduler.as_deref().and_then(non_blank);
                if node_scheduler.is_some() {
                    // Explicit node-level alias — already the effective cluster.
                    // Normalize away a blank-but-present string just in case.
                    *scheduler = node_scheduler.map(str::to_string);
                    continue;
                }
                // Lease-enclosed body: its cluster is the enclosing LeaseScope's,
                // resolved by containment during lowering. Leave `scheduler`
                // unset — it must NOT be forced to name (or inherit) a cluster.
                if lease_enclosed.contains(node.id.as_str()) {
                    continue;
                }
                // No node-level alias: inherit the default.
                match default_alias {
                    Some(alias) => {
                        *scheduler = Some(alias.to_string());
                    }
                    None => {
                        // A `Scheduled` step now REQUIRES a concrete cluster —
                        // it can no longer fall back to a global scheduler.
                        errors.push(CompileError::SchedulerUnresolved {
                            node_id: node.id.clone(),
                        });
                    }
                }
            }
            // ── LeaseScope (docs/17) ──────────────────────────────────────────
            WorkflowNodeData::LeaseScope { lease, .. } => {
                // A LeaseScope's `pool` may name a `datacenter` OR a presence
                // `capacity`; an explicit alias always wins (and is REQUIRED by
                // `validate_lease_scope`). The default-datacenter fallback only
                // matters for the legacy blank-datacenter case.
                let node_pool = non_blank(&lease.pool);
                if let Some(alias) = node_pool {
                    lease.pool = alias.to_string();
                    continue;
                }
                match default_alias {
                    Some(alias) => lease.pool = alias.to_string(),
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
    /// struct's many fields — we only care about `deploymentModel.scheduler`).
    fn scheduled_node(id: &str, scheduler: Option<&str>) -> WorkflowNode {
        let mut dm = serde_json::json!({
            "mode": "scheduled",
            "jobTemplate": "jt",
        });
        if let Some(s) = scheduler {
            dm["scheduler"] = serde_json::json!(s);
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
        g.nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| match &n.data {
                WorkflowNodeData::AutomatedStep {
                    deployment_model: DeploymentModel::Scheduled { scheduler, .. },
                    ..
                } => scheduler.as_deref(),
                WorkflowNodeData::LeaseScope { lease, .. } => Some(lease.pool.as_str()),
                _ => None,
            })
    }

    // node.scheduler set → it wins; template/workspace defaults ignored.
    #[test]
    fn node_level_alias_wins() {
        let g = graph(vec![scheduled_node("a", Some("node_dc"))], Some("tmpl_dc"));
        let out = resolve_scheduler_defaults(&g, Some("ws_dc")).unwrap();
        assert_eq!(node_scheduler(&out, "a"), Some("node_dc"));
    }

    // node omits scheduler → the template default fills in.
    #[test]
    fn template_default_used_when_node_omits() {
        let g = graph(vec![scheduled_node("a", None)], Some("tmpl_dc"));
        let out = resolve_scheduler_defaults(&g, Some("ws_dc")).unwrap();
        assert_eq!(node_scheduler(&out, "a"), Some("tmpl_dc"));
    }

    // node + template omit → the workspace default fills in.
    #[test]
    fn workspace_default_used_when_node_and_template_omit() {
        let g = graph(vec![scheduled_node("a", None)], None);
        let out = resolve_scheduler_defaults(&g, Some("ws_dc")).unwrap();
        assert_eq!(node_scheduler(&out, "a"), Some("ws_dc"));
    }

    // Scheduled node + all rungs absent → SchedulerUnresolved (no longer stays None).
    #[test]
    fn scheduled_all_absent_is_scheduler_unresolved() {
        let g = graph(vec![scheduled_node("a", None)], None);
        let errs = resolve_scheduler_defaults(&g, None).expect_err("fully-unresolved must fail");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].kind(), "scheduler_unresolved");
        assert_eq!(errs[0].node_id(), Some("a"));
    }

    // Template default beats the workspace default when both present.
    #[test]
    fn template_beats_workspace() {
        let g = graph(vec![scheduled_node("a", None)], Some("tmpl_dc"));
        let out = resolve_scheduler_defaults(&g, Some("ws_dc")).unwrap();
        assert_eq!(node_scheduler(&out, "a"), Some("tmpl_dc"));
    }

    /// A `lease_scope` node carrying `lease.scheduler`.
    fn lease_scope_node(id: &str, scheduler: &str) -> WorkflowNode {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "type": "lease_scope",
            "slug": id,
            "position": { "x": 0.0, "y": 0.0 },
            "data": {
                "type": "lease_scope",
                "label": "Lease Scope",
                "lease": { "pool": scheduler },
            }
        }))
        .expect("lease_scope node fixture")
    }

    /// A `loop` node parented under `parent`.
    fn loop_node(id: &str, parent: &str) -> WorkflowNode {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "type": "loop",
            "slug": id,
            "position": { "x": 0.0, "y": 0.0 },
            "parentId": parent,
            "data": {
                "type": "loop",
                "label": "Loop",
                "maxIterations": 3,
                "loopCondition": "true",
                "accumulators": [],
            }
        }))
        .expect("loop node fixture")
    }

    /// A `scheduled_node` parented under `parent` (no node-level scheduler).
    fn scheduled_child(id: &str, parent: &str) -> WorkflowNode {
        let mut n = scheduled_node(id, None);
        n.parent_id = Some(parent.to_string());
        n
    }

    // A `Scheduled` body enclosed by a `LeaseScope` — even two levels deep,
    // through a `Loop` — needs NO scheduler and NO default: its cluster comes
    // from the enclosing scope by containment. It must compile (the publish-path
    // analogue of compiler_e2e's `scheduled_body_inside_lease_scope_*`), and the
    // body's `scheduler` must stay unset so lowering resolves it by containment.
    #[test]
    fn lease_enclosed_scheduled_body_needs_no_scheduler() {
        let g = graph(
            vec![
                lease_scope_node("scope", "dc"),
                loop_node("lp", "scope"),
                scheduled_child("body", "lp"),
            ],
            None, // no template default
        );
        let out = resolve_scheduler_defaults(&g, None).expect("lease-enclosed body must compile");
        assert_eq!(
            node_scheduler(&out, "body"),
            None,
            "lease-enclosed body's scheduler stays unset (resolved by containment)"
        );
        assert_eq!(node_scheduler(&out, "scope"), Some("dc"));
    }

    // The exemption is strictly containment-gated: a Scheduled body NOT enclosed
    // by a LeaseScope (a bare Loop parent) still hard-errors when unresolved.
    #[test]
    fn non_enclosed_scheduled_body_still_unresolved() {
        let g = graph(
            vec![
                loop_node("lp", "scope_missing"),
                scheduled_child("body", "lp"),
            ],
            None,
        );
        // `lp`'s parent does not exist → no enclosing LeaseScope → body unresolved.
        let errs = resolve_scheduler_defaults(&g, None).expect_err("non-enclosed must fail");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].kind(), "scheduler_unresolved");
        assert_eq!(errs[0].node_id(), Some("body"));
    }
}
