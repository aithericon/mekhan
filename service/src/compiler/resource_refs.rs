//! Phase B.6 — `WorkflowGraph.resources` validator + scope helpers.
//!
//! Three responsibilities:
//!
//! 1. [`validate_resource_refs`] — called from `compile_to_air`. Every
//!    `alias -> type_name` pair in `graph.resources` must:
//!    - Reference a type registered in `aithericon_resources::registry`.
//!    - Have an `alias` that doesn't collide with any step `slug` or with
//!      a reserved control-token field (`_instance_id`, `_template_id`, …).
//!
//! 2. [`is_resource_alias`] — exposed for the borrow/python_refs scanner
//!    (B.8). When the Python ref extractor finds `<head>.<attr>`, it asks
//!    this helper whether `head` is a workflow-declared resource alias.
//!    If yes, the borrow planner emits a `ResourceEnvelope` borrow instead
//!    of (or in addition to) the producer-slug path.
//!
//! 3. [`resource_alias_scope`] — the set of names workflow-level scope
//!    merging treats as in-scope. Same role as the slug set for producer
//!    refs; rolled into the merged identifier scope from `validate.rs`.

use std::collections::BTreeSet;

use crate::compiler::error::CompileError;
use crate::models::template::WorkflowGraph;

/// Reserved control-token / system-field names. Any token seeded by
/// `parameterize_air` carries these; they are also the names Rhai logic
/// expects to find on the inbound control token. A resource alias that
/// collides with one of these would either shadow a system field (silent
/// data loss) or produce a name-resolution ambiguity at runtime.
///
/// Kept in lockstep with `parameterize_air` — the system fields injected
/// into every Start token live in `service/src/petri/instance.rs` around
/// line 169. When that list changes, this set must change with it.
const CONTROL_TOKEN_NAMES: &[&str] = &[
    "_instance_id",
    "_template_id",
    "_template_version",
    "_created_at",
    "_created_by",
];

/// Validate `graph.resources` against the registry, the graph's slug
/// space, and the reserved control-token vocabulary.
///
/// Called from `compile_to_air` alongside the existing
/// `validate_*` passes. Empty `resources` map is a no-op.
pub(crate) fn validate_resource_refs(graph: &WorkflowGraph) -> Result<(), CompileError> {
    if graph.resources.is_empty() {
        return Ok(());
    }

    // Build the slug set once. We mirror the algorithm `slug_index` uses
    // for the *explicit-slug pass* only — collision-suffixing of derived
    // slugs isn't deterministic against future graph mutations and would
    // produce false positives ("alias `db` collides with `db_2`" — but `db_2`
    // was only invented because some other node had a derived `db`).
    // Explicit slugs ARE stable wire identifiers; those are the ones an
    // alias can genuinely shadow.
    let mut explicit_slugs: BTreeSet<String> = BTreeSet::new();
    for n in &graph.nodes {
        let has_explicit = n
            .slug
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if has_explicit {
            // `n.slug()` sanitizes the same way `slug_index` does, so the
            // comparison is apples-to-apples even with weird user input.
            explicit_slugs.insert(n.slug());
        }
    }

    for (alias, type_name) in &graph.resources {
        if aithericon_resources::registry::lookup(type_name).is_none() {
            return Err(CompileError::ResourceTypeUnknown {
                alias: alias.clone(),
                type_name: type_name.clone(),
            });
        }
        if explicit_slugs.contains(alias) {
            return Err(CompileError::ResourceAliasCollidesWithSlug {
                alias: alias.clone(),
            });
        }
        if CONTROL_TOKEN_NAMES.contains(&alias.as_str()) {
            return Err(CompileError::ResourceAliasCollidesWithToken {
                alias: alias.clone(),
            });
        }
    }

    Ok(())
}

/// Cheap O(log n) lookup — "is `head` a declared resource alias on this
/// graph?". Used by the Python borrow planner (B.8) to discriminate
/// `<head>.<attr>` accesses between producer-slug refs (existing arm) and
/// resource-envelope refs (new arm).
pub(crate) fn is_resource_alias(graph: &WorkflowGraph, head: &str) -> bool {
    graph.resources.contains_key(head)
}

/// Snapshot of every alias declared in `graph.resources`. Merged into the
/// guard / Python scope set by `validate.rs` so identifier resolution
/// sees resources alongside slugs and tokens.
pub(crate) fn resource_alias_scope(graph: &WorkflowGraph) -> BTreeSet<String> {
    graph.resources.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{
        Port, Position, WorkflowEdge, WorkflowNode, WorkflowNodeData,
    };
    use std::collections::BTreeMap;

    fn minimal_graph_with_resources(resources: BTreeMap<String, String>) -> WorkflowGraph {
        WorkflowGraph {
            nodes: vec![
                WorkflowNode {
                    id: "n_start".to_string(),
                    node_type: "start".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: Port::empty_input(),
                        process_name: None,
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "n_end".to_string(),
                    node_type: "end".to_string(),
                    slug: None,
                    position: Position { x: 100.0, y: 0.0 },
                    data: WorkflowNodeData::End {
                        label: "End".to_string(),
                        description: None,
                        terminal: crate::models::template::default_terminal_port(),
                        result_mapping: Vec::new(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
            ],
            edges: vec![WorkflowEdge {
                id: "e1".to_string(),
                source: "n_start".to_string(),
                target: "n_end".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            }],
            viewport: None,
            instance_concurrency: Default::default(),
            resources,
        }
    }

    #[test]
    fn empty_resources_is_ok() {
        let g = minimal_graph_with_resources(BTreeMap::new());
        validate_resource_refs(&g).expect("empty resources must validate");
    }

    #[test]
    fn known_type_validates() {
        let mut r = BTreeMap::new();
        r.insert("db".to_string(), "postgres".to_string());
        r.insert("ai".to_string(), "openai".to_string());
        let g = minimal_graph_with_resources(r);
        validate_resource_refs(&g).expect("postgres + openai are registered");
    }

    #[test]
    fn unknown_type_errors() {
        let mut r = BTreeMap::new();
        r.insert("db".to_string(), "not_a_real_type".to_string());
        let g = minimal_graph_with_resources(r);
        match validate_resource_refs(&g) {
            Err(CompileError::ResourceTypeUnknown { alias, type_name }) => {
                assert_eq!(alias, "db");
                assert_eq!(type_name, "not_a_real_type");
            }
            other => panic!("expected ResourceTypeUnknown, got {other:?}"),
        }
    }

    #[test]
    fn alias_collides_with_explicit_slug_errors() {
        // Set up a graph where node `n_step` has explicit slug `db`, then try
        // to declare a resource alias `db: postgres`. Should error.
        let mut g = minimal_graph_with_resources({
            let mut r = BTreeMap::new();
            r.insert("db".to_string(), "postgres".to_string());
            r
        });
        g.nodes.push(WorkflowNode {
            id: "n_step".to_string(),
            node_type: "automated_step".to_string(),
            slug: Some("db".to_string()),
            position: Position { x: 50.0, y: 0.0 },
            data: WorkflowNodeData::AutomatedStep {
                label: "DB".to_string(),
                description: None,
                execution_spec: crate::models::template::ExecutionSpecConfig {
                    backend_type: crate::models::template::ExecutionBackendType::Python,
                    config: serde_json::json!({}),
                    entrypoint: None,
                },
                input: Port::empty_input(),
                output: crate::models::template::default_automated_output_port(),
                retry_policy: Default::default(),
                deployment_model: Default::default(),
            },
            parent_id: None,
            width: None,
            height: None,
        });

        match validate_resource_refs(&g) {
            Err(CompileError::ResourceAliasCollidesWithSlug { alias }) => {
                assert_eq!(alias, "db");
            }
            other => panic!("expected ResourceAliasCollidesWithSlug, got {other:?}"),
        }
    }

    #[test]
    fn alias_collides_with_control_token_errors() {
        let mut r = BTreeMap::new();
        r.insert("_instance_id".to_string(), "postgres".to_string());
        let g = minimal_graph_with_resources(r);
        match validate_resource_refs(&g) {
            Err(CompileError::ResourceAliasCollidesWithToken { alias }) => {
                assert_eq!(alias, "_instance_id");
            }
            other => panic!("expected ResourceAliasCollidesWithToken, got {other:?}"),
        }
    }

    #[test]
    fn is_resource_alias_works() {
        let mut r = BTreeMap::new();
        r.insert("db".to_string(), "postgres".to_string());
        let g = minimal_graph_with_resources(r);
        assert!(is_resource_alias(&g, "db"));
        assert!(!is_resource_alias(&g, "review"));
    }
}
