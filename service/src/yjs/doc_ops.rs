use std::collections::HashMap;
use std::sync::Arc;

use yrs::types::map::MapPrelim;
use yrs::types::text::TextPrelim;
use yrs::{Any, Array, Doc, GetString, Map, ReadTxn, Transact, WriteTxn};

use crate::models::template::*;
use crate::yjs::persistence::{json_value_to_any, yrs_value_to_json};

/// Reconstruct a WorkflowGraph from a Y.Doc.
///
/// Reads the Y.Map("nodes"), Y.Array("edges"), and Y.Map("viewport") from the doc
/// and deserializes them into the WorkflowGraph struct.
pub fn doc_to_graph(doc: &Doc) -> Result<WorkflowGraph, String> {
    let txn = doc.transact();

    // -- nodes --
    let nodes_map = txn
        .get_map("nodes")
        .ok_or_else(|| "Y.Doc has no 'nodes' map".to_string())?;

    let mut nodes = Vec::new();
    for (node_id, value) in nodes_map.iter(&txn) {
        let node_map = match value {
            yrs::Out::YMap(m) => m,
            _ => continue,
        };

        let node_type = match node_map.get(&txn, "type") {
            Some(yrs::Out::Any(Any::String(s))) => s.to_string(),
            _ => continue,
        };

        let label = match node_map.get(&txn, "label") {
            Some(yrs::Out::Any(Any::String(s))) => s.to_string(),
            _ => String::new(),
        };

        let description = match node_map.get(&txn, "description") {
            Some(yrs::Out::Any(Any::String(s))) => Some(s.to_string()),
            _ => None,
        };

        let position = match node_map.get(&txn, "position") {
            Some(yrs::Out::Any(Any::Map(ref m))) => {
                let x = m
                    .get("x")
                    .and_then(|v| match v {
                        Any::Number(n) => Some(*n),
                        _ => None,
                    })
                    .unwrap_or(0.0);
                let y = m
                    .get("y")
                    .and_then(|v| match v {
                        Any::Number(n) => Some(*n),
                        _ => None,
                    })
                    .unwrap_or(0.0);
                Position { x, y }
            }
            _ => Position { x: 0.0, y: 0.0 },
        };

        // Build a flat JSON object for serde deserialization of WorkflowNodeData.
        // Merge type, label, description and all config entries at the same level.
        let mut data_json = serde_json::Map::new();
        data_json.insert(
            "type".to_string(),
            serde_json::Value::String(node_type.clone()),
        );
        data_json.insert("label".to_string(), serde_json::Value::String(label));
        if let Some(ref desc) = description {
            data_json.insert(
                "description".to_string(),
                serde_json::Value::String(desc.clone()),
            );
        }

        // Merge config fields into the top-level data object
        if let Some(yrs::Out::YMap(config_map)) = node_map.get(&txn, "config") {
            for (key, val) in config_map.iter(&txn) {
                data_json.insert(key.to_string(), yrs_value_to_json(&val, &txn));
            }
        }

        let data: WorkflowNodeData =
            serde_json::from_value(serde_json::Value::Object(data_json))
                .map_err(|e| format!("deserialize node data for {}: {}", node_id, e))?;

        let slug = match node_map.get(&txn, "slug") {
            Some(yrs::Out::Any(Any::String(s))) => Some(s.to_string()),
            _ => None,
        };
        let parent_id = match node_map.get(&txn, "parentId") {
            Some(yrs::Out::Any(Any::String(s))) => Some(s.to_string()),
            _ => None,
        };
        let width = match node_map.get(&txn, "width") {
            Some(yrs::Out::Any(Any::Number(n))) => Some(n),
            _ => None,
        };
        let height = match node_map.get(&txn, "height") {
            Some(yrs::Out::Any(Any::Number(n))) => Some(n),
            _ => None,
        };
        // `toolMeta` was removed from WorkflowNode — agent tool naming now
        // derives from the node's own `data.label` / `data.description`.
        // Old YDocs may still carry the field; we just ignore it on read.

        nodes.push(WorkflowNode {
            id: node_id.to_string(),
            node_type,
            slug,
            position,
            data,
            parent_id,
            width,
            height,
        });
    }

    // -- edges --
    let mut edges = Vec::new();
    if let Some(edges_arr) = txn.get_array("edges") {
        for value in edges_arr.iter(&txn) {
            if let yrs::Out::Any(Any::Map(ref m)) = value {
                let get_str = |key: &str| -> Option<String> {
                    match m.get(key) {
                        Some(Any::String(s)) => Some(s.to_string()),
                        _ => None,
                    }
                };

                edges.push(WorkflowEdge {
                    id: get_str("id").unwrap_or_default(),
                    source: get_str("source").unwrap_or_default(),
                    target: get_str("target").unwrap_or_default(),
                    source_handle: get_str("sourceHandle"),
                    target_handle: get_str("targetHandle"),
                    label: get_str("label"),
                    edge_type: get_str("type").unwrap_or_else(|| "sequence".to_string()),
                });
            }
        }
    }

    // -- viewport --
    let viewport = txn.get_map("viewport").and_then(|vp| {
        let x = match vp.get(&txn, "x") {
            Some(yrs::Out::Any(Any::Number(n))) => n,
            _ => return None,
        };
        let y = match vp.get(&txn, "y") {
            Some(yrs::Out::Any(Any::Number(n))) => n,
            _ => return None,
        };
        let zoom = match vp.get(&txn, "zoom") {
            Some(yrs::Out::Any(Any::Number(n))) => n,
            _ => return None,
        };
        Some(Viewport { x, y, zoom })
    });

    // -- instance_concurrency: read the top-level Y.Map written by
    // graph_to_doc_with_files. Absent → default (Unlimited).
    let instance_concurrency = txn
        .get_map("instanceConcurrency")
        .and_then(|m| {
            let mut obj = serde_json::Map::new();
            for (k, v) in m.iter(&txn) {
                obj.insert(k.to_string(), yrs_value_to_json(&v, &txn));
            }
            if obj.is_empty() {
                return None;
            }
            serde_json::from_value::<crate::models::template::InstanceConcurrencyPolicy>(
                serde_json::Value::Object(obj),
            )
            .ok()
        })
        .unwrap_or_default();

    Ok(WorkflowGraph {
        nodes,
        edges,
        viewport,
        instance_concurrency,
        // YJS read path does not yet carry workflow-level `definitions`
        // (no editor surface; see `compiler::schema_refs`). Templates that
        // need definitions are loaded from JSON-on-disk via the demo
        // seeder — that path uses serde and populates the field correctly.
        definitions: Default::default(),
    })
}

/// Extract file contents from a Y.Doc.
///
/// Returns a map of `node_id -> { filename -> content }`.
/// Reads `nodes[nodeId].files` Y.Map entries, where each file is a Y.Text.
pub fn extract_files_from_doc(doc: &Doc) -> HashMap<String, HashMap<String, String>> {
    let txn = doc.transact();
    let mut result: HashMap<String, HashMap<String, String>> = HashMap::new();

    let Some(nodes_map) = txn.get_map("nodes") else {
        return result;
    };

    for (node_id, value) in nodes_map.iter(&txn) {
        let yrs::Out::YMap(node_map) = value else {
            continue;
        };

        let Some(yrs::Out::YMap(files_map)) = node_map.get(&txn, "files") else {
            continue;
        };

        let mut node_files: HashMap<String, String> = HashMap::new();
        for (filename, file_val) in files_map.iter(&txn) {
            if let yrs::Out::YText(text_ref) = file_val {
                let content = text_ref.get_string(&txn);
                if !content.is_empty() {
                    node_files.insert(filename.to_string(), content);
                }
            }
        }

        if !node_files.is_empty() {
            result.insert(node_id.to_string(), node_files);
        }
    }

    result
}

/// Initialize a Y.Doc from a WorkflowGraph with no attached files.
///
/// Creates the Y.Map("nodes"), Y.Array("edges"), and Y.Map("viewport") structure
/// and returns the encoded update bytes.
pub fn graph_to_doc(graph: &WorkflowGraph) -> Doc {
    graph_to_doc_with_files(graph, &HashMap::new())
}

/// Initialize a Y.Doc from a WorkflowGraph, seeding each node's `files` Y.Map
/// with the provided contents (filename → Y.Text). Used by `create_template`
/// so seed templates (showcase, GitOps imports) can ship ready-to-publish
/// scripts without a second round-trip.
pub fn graph_to_doc_with_files(
    graph: &WorkflowGraph,
    files: &HashMap<String, HashMap<String, String>>,
) -> Doc {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();

        // -- nodes: Y.Map("nodes") with nested Y.Maps per node --
        let nodes_map = txn.get_or_insert_map("nodes");
        for node in &graph.nodes {
            let empty: MapPrelim = std::iter::empty::<(&str, Any)>().collect();
            let node_map = nodes_map.insert(&mut txn, node.id.as_str(), empty);

            node_map.insert(&mut txn, "type", node.node_type.clone());
            node_map.insert(&mut txn, "label", node.data.label().to_string());

            if let Some(desc) = node.data.description() {
                node_map.insert(&mut txn, "description", desc.to_string());
            }

            // Author-facing `<slug>.<field>` namespace. Round-tripped through
            // Y.Doc so seed paths (new_version fork, demo seed, GitOps apply)
            // don't drop the user-set slug and silently rename downstream
            // borrow refs to the placeholder derived from the node id.
            if let Some(ref s) = node.slug {
                node_map.insert(&mut txn, "slug", s.clone());
            }

            // position as Any::Map (plain object, not a Y.Map)
            let pos: HashMap<String, Any> = HashMap::from([
                ("x".to_string(), Any::Number(node.position.x)),
                ("y".to_string(), Any::Number(node.position.y)),
            ]);
            node_map.insert(&mut txn, "position", Any::from(pos));

            // parent_id, width, height (for scope support)
            if let Some(ref pid) = node.parent_id {
                node_map.insert(&mut txn, "parentId", pid.clone());
            }
            if let Some(w) = node.width {
                node_map.insert(&mut txn, "width", w);
            }
            if let Some(h) = node.height {
                node_map.insert(&mut txn, "height", h);
            }
            // `toolMeta` was dropped — tool naming for agent-bound tools
            // derives from the node's own `data.label` / `data.description`
            // now. The frontend Y.Doc binding also stops writing the field;
            // old docs may still contain it (read-side ignores stale entries).

            // config as nested Y.Map
            let config_empty: MapPrelim = std::iter::empty::<(&str, Any)>().collect();
            let config_map = node_map.insert(&mut txn, "config", config_empty);
            write_node_config(&mut txn, &config_map, &node.data);

            // files: Y.Map whose entries are Y.Text (matches frontend binding).
            // Seeds from the caller-provided map; nodes with no entry get an
            // empty files map.
            let files_empty: MapPrelim = std::iter::empty::<(&str, Any)>().collect();
            let files_map = node_map.insert(&mut txn, "files", files_empty);
            if let Some(node_files) = files.get(&node.id) {
                for (filename, content) in node_files {
                    files_map.insert(&mut txn, filename.as_str(), TextPrelim::new(content));
                }
            }
        }

        // -- edges: Y.Array("edges") with Any::Map objects --
        let edges_arr = txn.get_or_insert_array("edges");
        for edge in &graph.edges {
            let mut edge_map: HashMap<String, Any> = HashMap::new();
            edge_map.insert(
                "id".to_string(),
                Any::String(Arc::from(edge.id.as_str())),
            );
            edge_map.insert(
                "source".to_string(),
                Any::String(Arc::from(edge.source.as_str())),
            );
            edge_map.insert(
                "target".to_string(),
                Any::String(Arc::from(edge.target.as_str())),
            );
            edge_map.insert(
                "type".to_string(),
                Any::String(Arc::from(edge.edge_type.as_str())),
            );
            if let Some(ref sh) = edge.source_handle {
                edge_map.insert(
                    "sourceHandle".to_string(),
                    Any::String(Arc::from(sh.as_str())),
                );
            }
            if let Some(ref th) = edge.target_handle {
                edge_map.insert(
                    "targetHandle".to_string(),
                    Any::String(Arc::from(th.as_str())),
                );
            }
            if let Some(ref label) = edge.label {
                edge_map.insert(
                    "label".to_string(),
                    Any::String(Arc::from(label.as_str())),
                );
            }
            edges_arr.push_back(&mut txn, Any::from(edge_map));
        }

        // -- viewport: Y.Map("viewport") --
        if let Some(ref vp) = graph.viewport {
            let vp_map = txn.get_or_insert_map("viewport");
            vp_map.insert(&mut txn, "x", vp.x);
            vp_map.insert(&mut txn, "y", vp.y);
            vp_map.insert(&mut txn, "zoom", vp.zoom);
        }

        // -- instance_concurrency: top-level Y.Map ----------------------
        // Round-trips the template-level policy so publish (which reads the
        // graph back via doc_to_graph) doesn't silently downgrade
        // `SingleActiveCoalesce` to the `Unlimited` default. Stored under
        // `instanceConcurrency` (camelCase to match the frontend's existing
        // Y.Map key convention). Default elided so legacy docs keep parsing.
        if !matches!(
            graph.instance_concurrency,
            crate::models::template::InstanceConcurrencyPolicy::Unlimited
        ) {
            let ic_val = serde_json::to_value(graph.instance_concurrency)
                .unwrap_or(serde_json::Value::Object(Default::default()));
            let ic_map = txn.get_or_insert_map("instanceConcurrency");
            // Write each key from the tagged enum object directly into the
            // map (e.g. `mode: "single_active_coalesce"`) so future variants
            // with additional fields round-trip without code changes here.
            if let serde_json::Value::Object(obj) = ic_val {
                for (k, v) in obj {
                    ic_map.insert(&mut txn, k.as_str(), json_value_to_any(&v));
                }
            }
        }
    }
    doc
}

/// Write type-specific WorkflowNodeData fields into a yrs config MapRef.
/// Routes to the registry's per-variant `yjs_encode` fn pointer; the actual
/// per-field write lives in `service/src/nodes/<variant>.rs`.
pub fn write_node_config(
    txn: &mut yrs::TransactionMut,
    config: &yrs::MapRef,
    data: &WorkflowNodeData,
) {
    let decl = crate::nodes::lookup_by_variant(data)
        .expect("every WorkflowNodeData variant is registered in crate::nodes::NODES");
    (decl.yjs_encode)(txn, config, data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_node_ydoc_roundtrip() {
        let graph = WorkflowGraph {
            nodes: vec![
                WorkflowNode {
                    id: "scope1".to_string(),
                    node_type: "scope".to_string(),
                    slug: None,
                    position: Position { x: 50.0, y: 100.0 },
                    data: WorkflowNodeData::Scope {
                        label: "My Group".to_string(),
                        description: Some("container".to_string()),
                    },
                    parent_id: None,
                    width: Some(500.0),
                    height: Some(300.0),
                },
                WorkflowNode {
                    id: "child1".to_string(),
                    node_type: "end".to_string(),
                    slug: None,
                    position: Position { x: 100.0, y: 200.0 },
                    data: WorkflowNodeData::End {
                        label: "Done".to_string(),
                        description: None,
                        terminal: Port {
                            id: "in".to_string(),
                            label: "Terminal".to_string(),
                            fields: vec![],
                        },
                        result_mapping: Vec::new(),
                    },
                    parent_id: Some("scope1".to_string()),
                    width: None,
                    height: None,
                },
            ],
            edges: vec![],
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
        };

        let doc = graph_to_doc(&graph);
        let roundtripped = doc_to_graph(&doc).expect("should parse Y.Doc");

        // Scope node
        let scope = roundtripped.nodes.iter().find(|n| n.id == "scope1").unwrap();
        assert_eq!(scope.node_type, "scope");
        assert_eq!(scope.data.label(), "My Group");
        assert_eq!(scope.data.description(), Some("container"));
        assert_eq!(scope.parent_id, None);
        assert_eq!(scope.width, Some(500.0));
        assert_eq!(scope.height, Some(300.0));

        // Child node with parent_id
        let child = roundtripped.nodes.iter().find(|n| n.id == "child1").unwrap();
        assert_eq!(child.parent_id, Some("scope1".to_string()));
        assert_eq!(child.width, None);
        assert_eq!(child.height, None);
    }

    #[test]
    fn default_graph_ydoc_roundtrip() {
        let graph = WorkflowGraph::default_graph();
        let doc = graph_to_doc(&graph);
        let roundtripped = doc_to_graph(&doc).expect("should parse Y.Doc");

        assert_eq!(roundtripped.nodes.len(), 2);
        assert_eq!(roundtripped.edges.len(), 1);

        let start = roundtripped.nodes.iter().find(|n| n.node_type == "start").unwrap();
        assert_eq!(start.parent_id, None);
        assert_eq!(start.width, None);
    }

    #[test]
    fn start_process_name_survives_ydoc_roundtrip() {
        fn start_with(process_name: Option<&str>) -> WorkflowGraph {
            WorkflowGraph {
                nodes: vec![WorkflowNode {
                    id: "s".to_string(),
                    node_type: "start".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: Port {
                            id: "in".to_string(),
                            label: "Input".to_string(),
                            fields: vec![],
                        },
                        process_name: process_name.map(str::to_string),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                }],
                edges: vec![],
                viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
            }
        }

        // Set → preserved through graph→Y.Doc→graph (the publish path).
        let rt = doc_to_graph(&graph_to_doc(&start_with(Some("Invoice {{ invoice_id }}"))))
            .expect("parse Y.Doc");
        match &rt.nodes[0].data {
            WorkflowNodeData::Start { process_name, .. } => {
                assert_eq!(process_name.as_deref(), Some("Invoice {{ invoice_id }}"));
            }
            other => panic!("expected Start, got {other:?}"),
        }

        // None → stays None (opt-out: no stray key written/read back).
        let rt_none =
            doc_to_graph(&graph_to_doc(&start_with(None))).expect("parse Y.Doc");
        match &rt_none.nodes[0].data {
            WorkflowNodeData::Start { process_name, .. } => {
                assert_eq!(process_name.as_deref(), None);
            }
            other => panic!("expected Start, got {other:?}"),
        }
    }

    /// Locks in that AutomatedStep `output` (and `input`) survive a Y.Doc
    /// round-trip. Pre-fix the seeder wrote a graph with output ports but
    /// the Y.Doc init dropped them, so the editor's port panel rendered
    /// "No declared output fields" against a seeded demo whose disk
    /// fixture had them set. Catches the silent-default-collapse class of
    /// regression where a node-data field is `#[serde(default)]` and gets
    /// quietly omitted from the Y.Doc seed.
    #[test]
    fn automated_step_input_output_survive_ydoc_roundtrip() {
        use crate::models::template::{
            DeploymentModel, ExecutionBackendType, ExecutionSpecConfig, FieldKind, Port,
            PortField, RetryPolicy, WorkflowEdge, WorkflowNode,
        };

        let output_port = Port {
            id: "out".to_string(),
            label: "Out".to_string(),
            fields: vec![
                PortField {
                    name: "vendor".to_string(),
                    label: "Vendor".to_string(),
                    kind: FieldKind::Text,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                },
                PortField {
                    name: "amount".to_string(),
                    label: "Amount".to_string(),
                    kind: FieldKind::Number,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                },
            ],
        };

        let graph = WorkflowGraph {
            nodes: vec![WorkflowNode {
                id: "extract".to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: Position { x: 0.0, y: 0.0 },
                data: WorkflowNodeData::AutomatedStep {
                    label: "Extract".to_string(),
                    description: None,
                    execution_spec: ExecutionSpecConfig {
                        backend_type: ExecutionBackendType::Python,
                        entrypoint: Some("main.py".to_string()),
                        config: serde_json::json!({"python": "python3"}),
                    },
                    input: Port::empty_input(),
                    output: output_port.clone(),
                    retry_policy: RetryPolicy::default(),
                    deployment_model: DeploymentModel::default(),
                    resource_pool: None,
                },
                parent_id: None,
                width: None,
                height: None,
            }],
            edges: Vec::<WorkflowEdge>::new(),
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
        };

        let rt = doc_to_graph(&graph_to_doc(&graph)).expect("parse Y.Doc");
        match &rt.nodes[0].data {
            WorkflowNodeData::AutomatedStep { output, .. } => {
                assert_eq!(
                    output.fields.len(),
                    2,
                    "output.fields must survive Y.Doc round-trip"
                );
                let names: Vec<&str> = output.fields.iter().map(|f| f.name.as_str()).collect();
                assert_eq!(names, vec!["vendor", "amount"]);
            }
            other => panic!("expected AutomatedStep, got {other:?}"),
        }
    }

    /// Pre-fix the slug write side was missing, so `new_version` (and any
    /// other graph→Y.Doc seed) silently dropped the user-set slug. The
    /// reconstructed draft then opened with every node falling back to the
    /// placeholder derived from the node id, silently breaking every
    /// `<slug>.<field>` borrow ref authored against the published version.
    #[test]
    fn node_slug_survives_ydoc_roundtrip() {
        let graph = WorkflowGraph {
            nodes: vec![
                WorkflowNode {
                    id: "n_with_slug".to_string(),
                    node_type: "start".to_string(),
                    slug: Some("review_step".to_string()),
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: Port {
                            id: "in".to_string(),
                            label: "Input".to_string(),
                            fields: vec![],
                        },
                        process_name: None,
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "n_no_slug".to_string(),
                    node_type: "end".to_string(),
                    slug: None,
                    position: Position { x: 100.0, y: 0.0 },
                    data: WorkflowNodeData::End {
                        label: "End".to_string(),
                        description: None,
                        terminal: Port {
                            id: "in".to_string(),
                            label: "Terminal".to_string(),
                            fields: vec![],
                        },
                        result_mapping: Vec::new(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
            ],
            edges: vec![],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
        };

        let rt = doc_to_graph(&graph_to_doc(&graph)).expect("parse Y.Doc");
        let with_slug = rt.nodes.iter().find(|n| n.id == "n_with_slug").unwrap();
        assert_eq!(with_slug.slug.as_deref(), Some("review_step"));
        let no_slug = rt.nodes.iter().find(|n| n.id == "n_no_slug").unwrap();
        assert_eq!(no_slug.slug, None);
    }

    /// Verifies inline files at template creation make it into the Y.Doc as
    /// real Y.Text entries — the path the showcase relies on.
    #[test]
    fn graph_to_doc_with_files_seeds_y_text_entries() {
        let graph = WorkflowGraph::default_graph();
        let mut files: HashMap<String, HashMap<String, String>> = HashMap::new();
        files.insert(
            "start".to_string(),
            HashMap::from([(
                "main.py".to_string(),
                "print('seeded')\n".to_string(),
            )]),
        );

        let doc = graph_to_doc_with_files(&graph, &files);
        let extracted = extract_files_from_doc(&doc);

        let start_files = extracted.get("start").expect("start node should have files");
        assert_eq!(
            start_files.get("main.py").map(String::as_str),
            Some("print('seeded')\n")
        );
    }
}
