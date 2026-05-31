use std::collections::HashMap;

use anyhow::Result;
use yrs::{Array, Doc, GetString, Map, Text, Transact, WriteTxn};

use mekhan_service::models::template::WorkflowGraph;
use mekhan_service::yjs::doc_ops;

/// Read a Y.Doc into a graph + file map.
///
/// Combines the shared `doc_to_graph()` with file extraction.
#[allow(clippy::type_complexity)]
pub fn read_doc(doc: &Doc) -> Result<(WorkflowGraph, HashMap<String, HashMap<String, String>>)> {
    let graph = doc_ops::doc_to_graph(doc).map_err(|e| anyhow::anyhow!(e))?;
    let files = doc_ops::extract_files_from_doc(doc);
    Ok((graph, files))
}

/// Apply a complete graph + files to a Y.Doc.
///
/// This writes the full graph structure and file contents.
/// Used during push to update the remote doc with local changes.
pub fn apply_graph_and_files(
    doc: &Doc,
    graph: &WorkflowGraph,
    files: &HashMap<String, HashMap<String, String>>,
) -> Result<()> {
    // Build a fresh doc from the graph, then merge it
    // For push, we mutate the existing doc in-place
    let mut txn = doc.transact_mut();

    // Clear and rewrite edges
    let edges_arr = txn.get_or_insert_array("edges");
    let len = edges_arr.len(&txn);
    if len > 0 {
        edges_arr.remove_range(&mut txn, 0, len);
    }
    for edge in &graph.edges {
        let mut edge_map: HashMap<String, yrs::Any> = HashMap::new();
        edge_map.insert(
            "id".to_string(),
            yrs::Any::String(std::sync::Arc::from(edge.id.as_str())),
        );
        edge_map.insert(
            "source".to_string(),
            yrs::Any::String(std::sync::Arc::from(edge.source.as_str())),
        );
        edge_map.insert(
            "target".to_string(),
            yrs::Any::String(std::sync::Arc::from(edge.target.as_str())),
        );
        edge_map.insert(
            "type".to_string(),
            yrs::Any::String(std::sync::Arc::from(edge.edge_type.as_str())),
        );
        if let Some(ref sh) = edge.source_handle {
            edge_map.insert(
                "sourceHandle".to_string(),
                yrs::Any::String(std::sync::Arc::from(sh.as_str())),
            );
        }
        if let Some(ref label) = edge.label {
            edge_map.insert(
                "label".to_string(),
                yrs::Any::String(std::sync::Arc::from(label.as_str())),
            );
        }
        edges_arr.push_back(&mut txn, yrs::Any::from(edge_map));
    }

    // Update nodes: add new, update existing, remove deleted
    let nodes_map = txn.get_or_insert_map("nodes");

    // Collect existing node IDs to detect deletions
    let existing_ids: Vec<String> = nodes_map.iter(&txn).map(|(id, _)| id.to_string()).collect();

    let graph_ids: std::collections::HashSet<&str> =
        graph.nodes.iter().map(|n| n.id.as_str()).collect();

    // Remove nodes not in the local graph
    for id in &existing_ids {
        if !graph_ids.contains(id.as_str()) {
            nodes_map.remove(&mut txn, id);
        }
    }

    // Add/update nodes
    for node in &graph.nodes {
        // Check if node exists
        let node_map = if nodes_map.get(&txn, &node.id).is_some() {
            // Get existing node map
            match nodes_map.get(&txn, &node.id) {
                Some(yrs::Out::YMap(m)) => m,
                _ => continue,
            }
        } else {
            // Create new node
            let empty: yrs::types::map::MapPrelim =
                std::iter::empty::<(&str, yrs::Any)>().collect();
            nodes_map.insert(&mut txn, node.id.as_str(), empty)
        };

        node_map.insert(&mut txn, "type", node.node_type.clone());
        node_map.insert(&mut txn, "label", node.data.label().to_string());

        if let Some(desc) = node.data.description() {
            node_map.insert(&mut txn, "description", desc.to_string());
        }

        // position
        let pos: HashMap<String, yrs::Any> = HashMap::from([
            ("x".to_string(), yrs::Any::Number(node.position.x)),
            ("y".to_string(), yrs::Any::Number(node.position.y)),
        ]);
        node_map.insert(&mut txn, "position", yrs::Any::from(pos));

        // config
        // Remove old config and recreate
        node_map.remove(&mut txn, "config");
        let config_empty: yrs::types::map::MapPrelim =
            std::iter::empty::<(&str, yrs::Any)>().collect();
        let config_map = node_map.insert(&mut txn, "config", config_empty);
        doc_ops::write_node_config(&mut txn, &config_map, &node.data);
    }

    // Write files
    for (node_id, node_files) in files {
        let Some(yrs::Out::YMap(node_map)) = nodes_map.get(&txn, node_id.as_str()) else {
            continue;
        };

        // Ensure files map exists
        let files_map = match node_map.get(&txn, "files") {
            Some(yrs::Out::YMap(m)) => m,
            _ => {
                let empty: yrs::types::map::MapPrelim =
                    std::iter::empty::<(&str, yrs::Any)>().collect();
                node_map.insert(&mut txn, "files", empty)
            }
        };

        // Collect existing file names to detect deletions
        let existing_files: Vec<String> = files_map
            .iter(&txn)
            .map(|(name, _)| name.to_string())
            .collect();

        // Remove files not in local
        for name in &existing_files {
            if !node_files.contains_key(name) {
                files_map.remove(&mut txn, name);
            }
        }

        // Add/update files
        for (filename, content) in node_files {
            // Check if file Y.Text exists
            match files_map.get(&txn, filename.as_str()) {
                Some(yrs::Out::YText(text_ref)) => {
                    // Update existing Y.Text: clear and rewrite
                    let old_len = text_ref.get_string(&txn).len() as u32;
                    if old_len > 0 {
                        text_ref.remove_range(&mut txn, 0, old_len);
                    }
                    text_ref.insert(&mut txn, 0, content);
                }
                _ => {
                    // Create new Y.Text
                    let text = yrs::TextPrelim::new(content.as_str());
                    files_map.insert(&mut txn, filename.as_str(), text);
                }
            }
        }
    }

    // Also handle file deletion for nodes that have files in remote but not in local
    for node_id in &existing_ids {
        if !files.contains_key(node_id) {
            // Check if this node has files in the doc
            if let Some(yrs::Out::YMap(node_map)) = nodes_map.get(&txn, node_id.as_str()) {
                if let Some(yrs::Out::YMap(files_map)) = node_map.get(&txn, "files") {
                    let file_names: Vec<String> = files_map
                        .iter(&txn)
                        .map(|(name, _)| name.to_string())
                        .collect();
                    for name in file_names {
                        files_map.remove(&mut txn, &name);
                    }
                }
            }
        }
    }

    // Update viewport
    let vp_map = txn.get_or_insert_map("viewport");
    if let Some(ref vp) = graph.viewport {
        vp_map.insert(&mut txn, "x", vp.x);
        vp_map.insert(&mut txn, "y", vp.y);
        vp_map.insert(&mut txn, "zoom", vp.zoom);
    }

    Ok(())
}
