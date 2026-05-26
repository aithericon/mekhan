//! Workspace-Resource discriminator + scope helper.
//!
//! After the alias-layer drop, workflows reference resources by their
//! workspace-scoped name directly: Python code writes `local_pg.host`, and
//! the compiler resolves `local_pg` against the workspace's resource list
//! at publish time. The caller (publish handler) source-scans Python
//! entrypoints with `extract_python_refs`, intersects the head identifiers
//! with the workspace's published resources, and hands the compiler a
//! [`KnownResources`] map naming exactly the resources this graph touches.
//!
//! Three responsibilities:
//!
//! 1. [`validate_resource_refs`] — called from `compile_to_air`. Every entry
//!    in `known` must:
//!    - Reference a type registered in `aithericon_resources::registry`.
//!    - Have a name that doesn't collide with a step `slug` or a reserved
//!      control-token field (`_instance_id`, `_template_id`, …).
//!
//! 2. [`is_resource_name`] — exposed for the borrow/python_refs scanner.
//!    When the Python ref extractor finds `<head>.<attr>`, this discriminates
//!    between producer-slug refs (existing path) and resource refs (the
//!    `ResourceEnvelope` arm).
//!
//! 3. [`resource_name_scope`] — the set of names workflow-level scope merging
//!    treats as in-scope. Same role as the slug set for producer refs;
//!    rolled into the merged identifier scope from `validate.rs`.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use crate::compiler::error::CompileError;
use crate::models::template::WorkflowGraph;

/// One resource the workspace exposes that this workflow's Python source
/// references. Built by the publish handler from the workspace's resources
/// list (filtered by source-scan) and threaded into the compile entry points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownResource {
    /// Stable workspace-scoped resource id. Persisted in the AIR so renames
    /// don't break already-published workflows; deletes do (intentionally).
    pub id: Uuid,
    /// Resource type name (`postgres`, `openai`, …) — must be registered in
    /// `aithericon_resources::registry`.
    pub type_name: String,
    /// Latest published version at publish time. The compiler pins to this
    /// version in the AIR so post-publish rotations don't silently change
    /// what an already-running workflow sees.
    pub latest_version: i32,
}

/// Per-workspace resource map handed to the compiler at publish time.
/// Keyed by resource name (the `<head>` in `<head>.<field>` Python source
/// references). `BTreeMap` so iteration / serialization order is stable —
/// the compiler emits splice snippets in this order and stable order keeps
/// the AIR diff-friendly.
pub type KnownResources = BTreeMap<String, KnownResource>;

/// Reserved control-token / system-field names. Any token seeded by
/// `parameterize_air` carries these; they are also the names Rhai logic
/// expects to find on the inbound control token. A resource name that
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

/// Validate `known` against the registry, the graph's slug space, and the
/// reserved control-token vocabulary.
///
/// Called from `compile_to_air` alongside the existing `validate_*` passes.
/// Empty `known` map is a no-op.
pub(crate) fn validate_resource_refs(
    known: &KnownResources,
    graph: &WorkflowGraph,
) -> Result<(), CompileError> {
    if known.is_empty() {
        return Ok(());
    }

    // Build the slug set once. We mirror the algorithm `slug_index` uses for
    // the *explicit-slug pass* only — collision-suffixing of derived slugs
    // isn't deterministic against future graph mutations and would produce
    // false positives ("resource `db` collides with `db_2`" — but `db_2` was
    // only invented because some other node had a derived `db`). Explicit
    // slugs ARE stable wire identifiers; those are the ones a resource
    // reference can genuinely shadow.
    let mut explicit_slugs: BTreeSet<String> = BTreeSet::new();
    for n in &graph.nodes {
        let has_explicit = n
            .slug
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if has_explicit {
            explicit_slugs.insert(n.slug());
        }
    }

    for (name, info) in known {
        if aithericon_resources::registry::lookup(&info.type_name).is_none() {
            return Err(CompileError::ResourceTypeUnknown {
                alias: name.clone(),
                type_name: info.type_name.clone(),
            });
        }
        if explicit_slugs.contains(name) {
            return Err(CompileError::ResourceAliasCollidesWithSlug { alias: name.clone() });
        }
        if CONTROL_TOKEN_NAMES.contains(&name.as_str()) {
            return Err(CompileError::ResourceAliasCollidesWithToken { alias: name.clone() });
        }
    }

    Ok(())
}

/// Cheap O(log n) lookup — "is `head` a known workspace-resource name?".
/// Used by the Python borrow planner to discriminate `<head>.<attr>` accesses
/// between producer-slug refs (existing arm) and resource-envelope refs.
pub(crate) fn is_resource_name(known: &KnownResources, head: &str) -> bool {
    known.contains_key(head)
}

/// Snapshot of every known resource name. Merged into the guard / Python
/// scope set by `validate.rs` so identifier resolution sees resources
/// alongside slugs and tokens.
pub(crate) fn resource_name_scope(known: &KnownResources) -> BTreeSet<String> {
    known.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{
        Port, Position, WorkflowEdge, WorkflowNode, WorkflowNodeData,
    };

    fn minimal_graph() -> WorkflowGraph {
        WorkflowGraph {
            definitions: Default::default(),
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
                    tool_meta: None,
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
                    tool_meta: None,
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
        }
    }

    fn known(entries: &[(&str, &str)]) -> KnownResources {
        let mut k = KnownResources::new();
        for (name, type_name) in entries {
            k.insert(
                (*name).to_string(),
                KnownResource {
                    id: Uuid::new_v4(),
                    type_name: (*type_name).to_string(),
                    latest_version: 1,
                },
            );
        }
        k
    }

    #[test]
    fn empty_known_is_ok() {
        let g = minimal_graph();
        validate_resource_refs(&KnownResources::new(), &g).expect("empty must validate");
    }

    #[test]
    fn known_types_validate() {
        let g = minimal_graph();
        let k = known(&[("local_pg", "postgres"), ("openai_prod", "openai")]);
        validate_resource_refs(&k, &g).expect("postgres + openai are registered");
    }

    #[test]
    fn unknown_type_errors() {
        let g = minimal_graph();
        let k = known(&[("local_pg", "not_a_real_type")]);
        match validate_resource_refs(&k, &g) {
            Err(CompileError::ResourceTypeUnknown { alias, type_name }) => {
                assert_eq!(alias, "local_pg");
                assert_eq!(type_name, "not_a_real_type");
            }
            other => panic!("expected ResourceTypeUnknown, got {other:?}"),
        }
    }

    #[test]
    fn name_collides_with_explicit_slug_errors() {
        let mut g = minimal_graph();
        g.nodes.push(WorkflowNode {
            id: "n_step".to_string(),
            node_type: "automated_step".to_string(),
            slug: Some("local_pg".to_string()),
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
            tool_meta: None,
        });

        let k = known(&[("local_pg", "postgres")]);
        match validate_resource_refs(&k, &g) {
            Err(CompileError::ResourceAliasCollidesWithSlug { alias }) => {
                assert_eq!(alias, "local_pg");
            }
            other => panic!("expected ResourceAliasCollidesWithSlug, got {other:?}"),
        }
    }

    #[test]
    fn name_collides_with_control_token_errors() {
        let g = minimal_graph();
        let k = known(&[("_instance_id", "postgres")]);
        match validate_resource_refs(&k, &g) {
            Err(CompileError::ResourceAliasCollidesWithToken { alias }) => {
                assert_eq!(alias, "_instance_id");
            }
            other => panic!("expected ResourceAliasCollidesWithToken, got {other:?}"),
        }
    }

    #[test]
    fn is_resource_name_works() {
        let k = known(&[("local_pg", "postgres")]);
        assert!(is_resource_name(&k, "local_pg"));
        assert!(!is_resource_name(&k, "review"));
    }
}
