//! Shared prelude for every borrow planner.

use std::collections::BTreeMap;

use crate::compiler::error::CompileError;
use crate::compiler::graph::{topo_order, WorkflowDiGraph};
use crate::compiler::token_shape::{slug_index, topo_pos, SlugIndex};
use crate::models::template::WorkflowGraph;

/// Shared prelude for every borrow planner: the directed workflow graph, its
/// topological order, the slug→node-id resolver, and the per-node topo
/// position used by `resolve_ref` / `resolve_backend_ref` to verify the
/// producer is strictly upstream of the consumer.
///
/// Built once via [`BorrowContext::build`]; each planner (guard read-arc,
/// AutomatedStep / resource / HumanTask / LLM / Kreuzberg) consumes the same
/// shape, so the four-line `wg + order + pos + slugs` recipe lives here
/// rather than copy-pasted into every planner head.
pub(crate) struct BorrowContext<'a> {
    pub(crate) wg: WorkflowDiGraph<'a>,
    pub(crate) order: Vec<petgraph::graph::NodeIndex>,
    pub(crate) pos: BTreeMap<String, usize>,
    pub(crate) slugs: SlugIndex,
}

impl<'a> BorrowContext<'a> {
    pub(crate) fn build(graph: &'a WorkflowGraph) -> Result<Self, CompileError> {
        let wg = WorkflowDiGraph::build(graph)?;
        let order = topo_order(&wg)?;
        let pos = topo_pos(&order, &wg);
        let slugs = slug_index(graph)?;
        Ok(Self {
            wg,
            order,
            pos,
            slugs,
        })
    }
}
