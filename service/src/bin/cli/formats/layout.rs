use std::collections::HashMap;

use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};

use mekhan_service::models::template::{Position, WorkflowGraph};

const X_CENTER: f64 = 250.0;
const Y_START: f64 = 100.0;
const Y_SPACING: f64 = 150.0;
const X_BRANCH_OFFSET: f64 = 250.0;

/// Auto-layout nodes using topological sort and vertical layering.
pub fn auto_layout(graph: &mut WorkflowGraph) {
    if graph.nodes.is_empty() {
        return;
    }

    // Build petgraph
    let mut pg = DiGraph::<&str, ()>::new();
    let mut idx_map: HashMap<&str, NodeIndex> = HashMap::new();

    for node in &graph.nodes {
        let idx = pg.add_node(node.id.as_str());
        idx_map.insert(node.id.as_str(), idx);
    }

    for edge in &graph.edges {
        if let (Some(&src), Some(&tgt)) = (
            idx_map.get(edge.source.as_str()),
            idx_map.get(edge.target.as_str()),
        ) {
            pg.add_edge(src, tgt, ());
        }
    }

    // Topological sort
    let sorted = match toposort(&pg, None) {
        Ok(s) => s,
        Err(_) => {
            // Cycle (loop nodes) — fall back to insertion order
            for (i, node) in graph.nodes.iter_mut().enumerate() {
                node.position = Position {
                    x: X_CENTER,
                    y: Y_START + (i as f64) * Y_SPACING,
                };
            }
            return;
        }
    };

    // Assign layers via longest-path
    let mut layer: HashMap<&str, usize> = HashMap::new();
    for &idx in &sorted {
        let node_id = pg[idx];
        let max_parent = pg
            .neighbors_directed(idx, petgraph::Direction::Incoming)
            .filter_map(|parent_idx| layer.get(pg[parent_idx]))
            .max()
            .copied();

        let my_layer = match max_parent {
            Some(l) => l + 1,
            None => 0,
        };
        layer.insert(node_id, my_layer);
    }

    // Group nodes by layer
    let mut layers: HashMap<usize, Vec<String>> = HashMap::new();
    for (&id, &l) in &layer {
        layers.entry(l).or_default().push(id.to_string());
    }

    // Compute positions into a map first (avoids borrow conflict)
    let mut positions: HashMap<String, Position> = HashMap::new();
    for (&l, nodes_in_layer) in &layers {
        let count = nodes_in_layer.len();
        for (i, node_id) in nodes_in_layer.iter().enumerate() {
            let x = if count == 1 {
                X_CENTER
            } else {
                X_CENTER - ((count as f64 - 1.0) / 2.0) * X_BRANCH_OFFSET
                    + (i as f64) * X_BRANCH_OFFSET
            };
            let y = Y_START + (l as f64) * Y_SPACING;
            positions.insert(node_id.clone(), Position { x, y });
        }
    }

    // Apply positions
    for node in &mut graph.nodes {
        if let Some(pos) = positions.remove(&node.id) {
            node.position = pos;
        }
    }
}
