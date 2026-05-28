//! Petgraph adapter over a [`WorkflowGraph`] plus the topological sort used
//! to drive node lowering.

use crate::compiler::error::CompileError;
use crate::models::template::{WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData};
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use std::collections::HashMap;

/// Wraps petgraph directed graphs for the workflow.
///
/// Two graphs share the same `NodeIndex` values:
/// - `full`: all edges (for wiring and reachability queries)
/// - `dag`: back-edges removed (for topological sort and cycle detection)
///   — both explicit `loop_back` edges and any edge into a `body_out`
///   handle, which is the body-return arc of a Loop/Timeout container.
pub(crate) struct WorkflowDiGraph<'a> {
    pub(crate) full: DiGraph<&'a WorkflowNode, &'a WorkflowEdge>,
    pub(crate) dag: DiGraph<&'a WorkflowNode, &'a WorkflowEdge>,
    pub(crate) indices: HashMap<&'a str, NodeIndex>,
    pub(crate) start: NodeIndex,
}

impl<'a> WorkflowDiGraph<'a> {
    pub(crate) fn build(graph: &'a WorkflowGraph) -> Result<Self, CompileError> {
        let mut full = DiGraph::new();
        let mut dag = DiGraph::new();
        let mut indices = HashMap::new();
        let mut start = None;

        for node in &graph.nodes {
            let fi = full.add_node(node);
            let di = dag.add_node(node);
            debug_assert_eq!(fi, di);
            indices.insert(node.id.as_str(), fi);
            if matches!(node.data, WorkflowNodeData::Start { .. }) {
                start = Some(fi);
            }
        }

        let start = start.ok_or_else(|| {
            CompileError::Validation("expected exactly one Start node, found 0".into())
        })?;

        for edge in &graph.edges {
            let &src = indices.get(edge.source.as_str()).ok_or_else(|| {
                CompileError::Validation(format!(
                    "edge '{}' references unknown source node '{}'",
                    edge.id, edge.source
                ))
            })?;
            let &tgt = indices.get(edge.target.as_str()).ok_or_else(|| {
                CompileError::Validation(format!(
                    "edge '{}' references unknown target node '{}'",
                    edge.id, edge.target
                ))
            })?;
            full.add_edge(src, tgt, edge);
            // A body-return arc is a back-edge regardless of how it was
            // authored: the JSON demos tag it `loop_back`, but an edge drawn
            // in the editor onto a Loop/Timeout `body_out` handle arrives as
            // a plain `sequence`. Either form closes the body cycle, so both
            // are excluded from the DAG used for topo-sort + cycle detection.
            let is_back_edge = edge.edge_type == "loop_back"
                || edge.target_handle.as_deref() == Some("body_out");
            if !is_back_edge {
                dag.add_edge(src, tgt, edge);
            }
        }

        Ok(Self {
            full,
            dag,
            indices,
            start,
        })
    }

    pub(crate) fn node(&self, id: &str) -> &'a WorkflowNode {
        self.full.node_weight(self.indices[id]).unwrap()
    }

    /// Outgoing edges in original insertion order.
    pub(crate) fn outgoing(&self, id: &str) -> Vec<&'a WorkflowEdge> {
        let idx = self.indices[id];
        let mut edges: Vec<_> = self
            .full
            .edges_directed(idx, Direction::Outgoing)
            .map(|e| *e.weight())
            .collect();
        edges.reverse(); // petgraph iterates newest-first; restore insertion order
        edges
    }

    /// Incoming edges in original insertion order.
    pub(crate) fn incoming(&self, id: &str) -> Vec<&'a WorkflowEdge> {
        let idx = self.indices[id];
        let mut edges: Vec<_> = self
            .full
            .edges_directed(idx, Direction::Incoming)
            .map(|e| *e.weight())
            .collect();
        edges.reverse();
        edges
    }
}

pub(crate) fn topo_order(wg: &WorkflowDiGraph) -> Result<Vec<NodeIndex>, CompileError> {
    toposort(&wg.dag, None).map_err(|cycle| {
        let node = *wg.dag.node_weight(cycle.node_id()).unwrap();
        CompileError::Compilation(format!("cycle detected at node '{}'", node.id))
    })
}
