use std::collections::{HashMap, HashSet};

use mekhan_service::models::template::WorkflowGraph;

#[derive(Debug)]
pub enum FileDiffKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug)]
pub struct FileDiff {
    pub node_id: String,
    pub filename: String,
    pub kind: FileDiffKind,
}

#[derive(Debug)]
pub enum NodeDiffKind {
    Added,
    Removed,
    Modified,
}

#[derive(Debug)]
pub struct NodeDiff {
    pub node_id: String,
    pub kind: NodeDiffKind,
}

#[derive(Debug)]
pub enum EdgeDiffKind {
    Added,
    Removed,
}

#[derive(Debug)]
pub struct EdgeDiff {
    pub edge_id: String,
    pub kind: EdgeDiffKind,
}

#[derive(Debug)]
pub struct DiffResult {
    pub files: Vec<FileDiff>,
    pub nodes: Vec<NodeDiff>,
    pub edges: Vec<EdgeDiff>,
}

impl DiffResult {
    pub fn has_changes(&self) -> bool {
        !self.files.is_empty() || !self.nodes.is_empty() || !self.edges.is_empty()
    }

    pub fn print_summary(&self) {
        if !self.has_changes() {
            println!("No changes.");
            return;
        }

        for diff in &self.nodes {
            let symbol = match diff.kind {
                NodeDiffKind::Added => "+",
                NodeDiffKind::Removed => "-",
                NodeDiffKind::Modified => "~",
            };
            println!("  {symbol} node: {}", diff.node_id);
        }

        for diff in &self.edges {
            let symbol = match diff.kind {
                EdgeDiffKind::Added => "+",
                EdgeDiffKind::Removed => "-",
            };
            println!("  {symbol} edge: {}", diff.edge_id);
        }

        for diff in &self.files {
            let symbol = match diff.kind {
                FileDiffKind::Added => "+",
                FileDiffKind::Modified => "~",
                FileDiffKind::Deleted => "-",
            };
            println!("  {symbol} file: {}/{}", diff.node_id, diff.filename);
        }

        let stats = format!(
            "{} node(s), {} edge(s), {} file(s) changed",
            self.nodes.len(),
            self.edges.len(),
            self.files.len()
        );
        println!("\n  {stats}");
    }
}

/// Compute the diff between local and remote state.
///
/// When `ignore_positions` is true, node position changes are excluded
/// from the diff (used for DSL formats that auto-generate positions).
pub fn compute_diff(
    local_graph: &WorkflowGraph,
    local_files: &HashMap<String, HashMap<String, String>>,
    remote_graph: &WorkflowGraph,
    remote_files: &HashMap<String, HashMap<String, String>>,
) -> DiffResult {
    compute_diff_inner(local_graph, local_files, remote_graph, remote_files, false)
}

pub fn compute_diff_ignoring_positions(
    local_graph: &WorkflowGraph,
    local_files: &HashMap<String, HashMap<String, String>>,
    remote_graph: &WorkflowGraph,
    remote_files: &HashMap<String, HashMap<String, String>>,
) -> DiffResult {
    compute_diff_inner(local_graph, local_files, remote_graph, remote_files, true)
}

fn compute_diff_inner(
    local_graph: &WorkflowGraph,
    local_files: &HashMap<String, HashMap<String, String>>,
    remote_graph: &WorkflowGraph,
    remote_files: &HashMap<String, HashMap<String, String>>,
    ignore_positions: bool,
) -> DiffResult {
    let mut node_diffs = Vec::new();
    let mut edge_diffs = Vec::new();
    let mut file_diffs = Vec::new();

    // -- Nodes --
    let local_node_ids: HashSet<&str> = local_graph.nodes.iter().map(|n| n.id.as_str()).collect();
    let remote_node_ids: HashSet<&str> = remote_graph.nodes.iter().map(|n| n.id.as_str()).collect();

    // Added nodes
    for id in local_node_ids.difference(&remote_node_ids) {
        node_diffs.push(NodeDiff {
            node_id: id.to_string(),
            kind: NodeDiffKind::Added,
        });
    }

    // Removed nodes
    for id in remote_node_ids.difference(&local_node_ids) {
        node_diffs.push(NodeDiff {
            node_id: id.to_string(),
            kind: NodeDiffKind::Removed,
        });
    }

    // Modified nodes (compare JSON serialization)
    for id in local_node_ids.intersection(&remote_node_ids) {
        let local_node = local_graph.nodes.iter().find(|n| n.id == *id).unwrap();
        let remote_node = remote_graph.nodes.iter().find(|n| n.id == *id).unwrap();

        let mut local_json = serde_json::to_value(local_node).unwrap_or_default();
        let mut remote_json = serde_json::to_value(remote_node).unwrap_or_default();

        if ignore_positions {
            local_json.as_object_mut().map(|m| m.remove("position"));
            remote_json.as_object_mut().map(|m| m.remove("position"));
        }

        if local_json != remote_json {
            node_diffs.push(NodeDiff {
                node_id: id.to_string(),
                kind: NodeDiffKind::Modified,
            });
        }
    }

    // -- Edges --
    let local_edge_ids: HashSet<&str> = local_graph.edges.iter().map(|e| e.id.as_str()).collect();
    let remote_edge_ids: HashSet<&str> = remote_graph.edges.iter().map(|e| e.id.as_str()).collect();

    for id in local_edge_ids.difference(&remote_edge_ids) {
        edge_diffs.push(EdgeDiff {
            edge_id: id.to_string(),
            kind: EdgeDiffKind::Added,
        });
    }

    for id in remote_edge_ids.difference(&local_edge_ids) {
        edge_diffs.push(EdgeDiff {
            edge_id: id.to_string(),
            kind: EdgeDiffKind::Removed,
        });
    }

    // -- Files --
    // Collect all node IDs that have files in either local or remote
    let all_file_nodes: HashSet<&str> = local_files
        .keys()
        .chain(remote_files.keys())
        .map(|s| s.as_str())
        .collect();

    for node_id in all_file_nodes {
        let local_node_files = local_files.get(node_id);
        let remote_node_files = remote_files.get(node_id);

        let local_names: HashSet<&str> = local_node_files
            .map(|f| f.keys().map(|s| s.as_str()).collect())
            .unwrap_or_default();
        let remote_names: HashSet<&str> = remote_node_files
            .map(|f| f.keys().map(|s| s.as_str()).collect())
            .unwrap_or_default();

        // Added files
        for name in local_names.difference(&remote_names) {
            file_diffs.push(FileDiff {
                node_id: node_id.to_string(),
                filename: name.to_string(),
                kind: FileDiffKind::Added,
            });
        }

        // Deleted files
        for name in remote_names.difference(&local_names) {
            file_diffs.push(FileDiff {
                node_id: node_id.to_string(),
                filename: name.to_string(),
                kind: FileDiffKind::Deleted,
            });
        }

        // Modified files
        for name in local_names.intersection(&remote_names) {
            let local_content = &local_node_files.unwrap()[*name];
            let remote_content = &remote_node_files.unwrap()[*name];
            if local_content != remote_content {
                file_diffs.push(FileDiff {
                    node_id: node_id.to_string(),
                    filename: name.to_string(),
                    kind: FileDiffKind::Modified,
                });
            }
        }
    }

    DiffResult {
        files: file_diffs,
        nodes: node_diffs,
        edges: edge_diffs,
    }
}
