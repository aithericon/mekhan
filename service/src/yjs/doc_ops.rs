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

        nodes.push(WorkflowNode {
            id: node_id.to_string(),
            node_type,
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

    Ok(WorkflowGraph {
        nodes,
        edges,
        viewport,
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
    }
    doc
}

/// Write type-specific WorkflowNodeData fields into a yrs config MapRef.
pub fn write_node_config(
    txn: &mut yrs::TransactionMut,
    config: &yrs::MapRef,
    data: &WorkflowNodeData,
) {
    match data {
        WorkflowNodeData::Start { initial, process_name, .. } => {
            let initial_val =
                serde_json::to_value(initial).unwrap_or(serde_json::Value::Object(Default::default()));
            config.insert(txn, "initial", json_value_to_any(&initial_val));
            // Opt-in per-instance process name template. Persist it so the
            // graph→Y.Doc seed path (createTemplate) and publish's Y.Doc
            // reconstruction don't silently drop it.
            if let Some(pn) = process_name.as_deref().filter(|s| !s.is_empty()) {
                config.insert(txn, "processName", pn.to_string());
            }
        }
        WorkflowNodeData::End { .. } => {}
        WorkflowNodeData::HumanTask {
            task_title,
            instructions_mdsvex,
            steps,
            ..
        } => {
            config.insert(txn, "taskTitle", task_title.clone());
            if let Some(inst) = instructions_mdsvex {
                config.insert(txn, "instructionsMdsvex", inst.clone());
            }
            let steps_val =
                serde_json::to_value(steps).unwrap_or(serde_json::Value::Array(vec![]));
            config.insert(txn, "steps", json_value_to_any(&steps_val));
        }
        WorkflowNodeData::AutomatedStep {
            execution_spec, ..
        } => {
            let spec_val = serde_json::to_value(execution_spec).unwrap_or_default();
            config.insert(txn, "executionSpec", json_value_to_any(&spec_val));
        }
        WorkflowNodeData::Decision {
            conditions,
            default_branch,
            ..
        } => {
            let conds_val =
                serde_json::to_value(conditions).unwrap_or(serde_json::Value::Array(vec![]));
            config.insert(txn, "conditions", json_value_to_any(&conds_val));
            if let Some(db) = default_branch {
                config.insert(txn, "defaultBranch", db.clone());
            }
        }
        WorkflowNodeData::ParallelSplit { .. }
        | WorkflowNodeData::ParallelJoin { .. }
        | WorkflowNodeData::Scope { .. } => {}
        WorkflowNodeData::Loop {
            max_iterations,
            loop_condition,
            ..
        } => {
            config.insert(txn, "maxIterations", *max_iterations as f64);
            config.insert(txn, "loopCondition", loop_condition.clone());
        }
        WorkflowNodeData::Trigger {
            source,
            concurrency,
            payload_mapping,
            enabled,
            ..
        } => {
            let source_val = serde_json::to_value(source).unwrap_or_default();
            config.insert(txn, "source", json_value_to_any(&source_val));
            let concurrency_val = serde_json::to_value(concurrency).unwrap_or_default();
            config.insert(txn, "concurrency", json_value_to_any(&concurrency_val));
            let mapping_val =
                serde_json::to_value(payload_mapping).unwrap_or(serde_json::Value::Array(vec![]));
            config.insert(txn, "payloadMapping", json_value_to_any(&mapping_val));
            config.insert(txn, "enabled", *enabled);
        }
    }
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
                    position: Position { x: 100.0, y: 200.0 },
                    data: WorkflowNodeData::End {
                        label: "Done".to_string(),
                        description: None,
                        terminal: Port {
                            id: "in".to_string(),
                            label: "Terminal".to_string(),
                            fields: vec![],
                        },
                    },
                    parent_id: Some("scope1".to_string()),
                    width: None,
                    height: None,
                },
            ],
            edges: vec![],
            viewport: None,
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
                viewport: None,
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
