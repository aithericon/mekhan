#![allow(unused_imports)]

use std::collections::BTreeMap;

use serde_json::Value;

use crate::compiler::error::CompileError;
use crate::compiler::graph::{topo_order, WorkflowDiGraph};
use crate::models::template::{
    FieldKind, JoinMode, MergeStrategy, Port, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};

use super::*;
// ─── AIR integration + reporting ────────────────────────────────────────────

/// Replace `token_schema` on every AIR place we have a derived shape for.
/// Today those places carry `"#/definitions/DynamicToken"`; this swaps in the
/// structural shape so the contract is visible to the engine and the editor.
pub fn annotate_air(air: &mut Value, report: &ShapeReport) {
    let Some(places) = air.get_mut("places").and_then(|p| p.as_array_mut()) else {
        return;
    };
    for place in places {
        let Some(id) = place.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(shape) = report.place_schemas.get(id) {
            place["token_schema"] = Value::String(format!("inline:{shape}"));
        }
    }
}

/// Convenience wrapper: compile as usual, then annotate places with derived
/// shapes. `compile_to_air` itself is left untouched.
pub fn compile_to_air_with_shapes(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &crate::compiler::lower::NodeFiles,
) -> Result<(Value, ShapeReport), CompileError> {
    let mut air = crate::compiler::compile_to_air(graph, name, description, files)?;
    let report = analyze(graph)?;
    annotate_air(&mut air, &report);
    Ok((air, report))
}

impl ShapeReport {
    /// Human-readable demo dump.
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str("══ Derived token shape at each node's input place ══\n\n");
        for (nid, shape) in &self.node_in {
            s.push_str(&format!("● {nid}\n{}\n\n", shape.render(0)));
        }
        s.push_str("══ Editor scope surface (what the variable picker should show) ══\n\n");
        for (nid, entries) in &self.scopes {
            if entries.is_empty() {
                continue;
            }
            s.push_str(&format!("● {nid}\n"));
            for e in entries {
                s.push_str(&format!(
                    "    {} : {}   (from {} — {})\n",
                    e.path, e.ty, e.producer_label, e.note
                ));
            }
            s.push('\n');
        }
        s.push_str("══ Guard diagnostics (shape-aware) ══\n\n");
        if self.diagnostics.is_empty() {
            s.push_str("(none)\n");
        }
        for d in &self.diagnostics {
            match d {
                ShapeDiagnostic::DroppedUpstream {
                    node_label,
                    node_id,
                    guard,
                    referenced,
                    produced_by,
                    produced_label,
                    produced_path,
                    produced_ty,
                    dropped_by,
                    drop_reason,
                    fixes,
                } => {
                    s.push_str(&format!(
                        "✖ DROPPED     [{node_label} ({node_id})]\n  guard: {guard}\n  \
                         `{referenced}` is not present here.\n  produced by '{produced_label}' \
                         ({produced_by}) as `{produced_path}: {produced_ty}`\n  dropped at {}: \
                         {drop_reason}\n  fixes:\n",
                        dropped_by.as_deref().unwrap_or("(upstream)")
                    ));
                    for f in fixes {
                        s.push_str(&format!("    • {f}\n"));
                    }
                    s.push('\n');
                }
                ShapeDiagnostic::UnresolvedGuardPath {
                    node_label,
                    node_id,
                    guard,
                    referenced,
                } => {
                    s.push_str(&format!(
                        "✖ UNRESOLVED  [{node_label} ({node_id})]\n  guard: {guard}\n  \
                         `{referenced}` is produced by no upstream node.\n\n"
                    ));
                }
                ShapeDiagnostic::GuardTypeMismatch {
                    node_label,
                    node_id,
                    guard,
                    referenced,
                    found,
                    note,
                } => {
                    s.push_str(&format!(
                        "✖ TYPE        [{node_label} ({node_id})]\n  guard: {guard}\n  \
                         `{referenced}` is `{found}` but {note}.\n\n"
                    ));
                }
                ShapeDiagnostic::GraphIncomplete { message } => {
                    s.push_str(&format!("… GRAPH       not analyzable yet: {message}\n\n"));
                }
            }
        }
        s
    }
}
