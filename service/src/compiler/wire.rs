//! Edge wiring: turning workflow edges into Petri arcs/transitions (or
//! recording a pass-through place merge), plus the post-build alias/merge
//! resolution that collapses those pass-throughs.

use crate::compiler::error::CompileError;
use crate::compiler::graph::WorkflowDiGraph;
use crate::compiler::lower::{NodePorts, PlaceMerge, PostProcess};
use crate::models::template::{WorkflowEdge, WorkflowNode};
use crate::nodes::lookup_by_variant;
use aithericon_sdk::scenario::ScenarioDefinition;
use aithericon_sdk::{Context, DynamicToken, PlaceHandle};
use std::collections::{HashMap, HashSet};

fn decl_of(node: &WorkflowNode) -> &'static crate::nodes::NodeDecl {
    lookup_by_variant(&node.data)
        .expect("every WorkflowNodeData variant is registered in crate::nodes::NODES")
}

pub(crate) fn wire_edge(
    edge: &WorkflowEdge,
    node_ports: &HashMap<String, NodePorts>,
    wg: &WorkflowDiGraph,
    ctx: &mut Context,
    fixups: &mut PostProcess,
) -> Result<(), CompileError> {
    // Edges from Trigger nodes are pre-compile dispatcher concerns — they
    // don't exist in AIR. Skip silently so the rest of the graph still wires
    // up. Trigger is the only variant with `lowers_to_air: false`.
    if !decl_of(wg.node(&edge.source)).lowers_to_air {
        return Ok(());
    }

    // Tools-handle edges are agent-loop bindings, not sequence arcs: the
    // orchestrator pre-indexes them into `cx.agent_tools` and `lower_agent`
    // mints the per-tool dispatch/invoke/collect transitions via the
    // `apply_agent_tool_wirings` fixup. Treating them as regular edges here
    // would (a) inject a stray `t_edge_*` pass-through between the agent's
    // ctrl place and the tool's input, breaking the loop topology, and
    // (b) double-count the tool's incoming-edge degree so the agent edge
    // couldn't merge with a real upstream caller. Skip silently.
    if edge.source_handle.as_deref() == Some("tools") {
        return Ok(());
    }

    let source_ports = node_ports.get(&edge.source).ok_or_else(|| {
        CompileError::Compilation(format!("no ports for source node '{}'", edge.source))
    })?;
    let target_ports = node_ports.get(&edge.target).ok_or_else(|| {
        CompileError::Compilation(format!("no ports for target node '{}'", edge.target))
    })?;

    let source_node = wg.node(&edge.source);
    let target_node = wg.node(&edge.target);
    let target_decl = decl_of(target_node);

    // Determine source output place
    let source_place = find_output_place(source_ports, edge)?;

    // Determine target input place
    let actual_target = if target_decl.is_join {
        target_ports
            .input_places
            .get(&edge.id)
            .cloned()
            .ok_or_else(|| {
                CompileError::Compilation(format!(
                    "join '{}' has no input place for edge '{}'",
                    edge.target, edge.id
                ))
            })?
    } else if let Some(handle) = edge.target_handle.as_deref() {
        // Named inbound port (e.g. Loop's `body_out`). Fall through to the
        // default `input_place` if the handle isn't a registered named port —
        // most nodes only declare an implicit "in" handle (cosmetic, satisfies
        // xyflow + the typed-ports invariant) and the compiler models that as
        // the single `input_place`. Mirror of the source-handle fallback in
        // `find_output_place`.
        target_ports
            .input_handles
            .get(handle)
            .cloned()
            .unwrap_or_else(|| target_ports.input_place.clone())
    } else {
        target_ports.input_place.clone()
    };

    let logic = target_decl.wiring_logic.map(|f| f(target_node));

    if let Some(script) = logic {
        // Real transformation — must keep this transition
        let edge_label = edge.label.clone().unwrap_or_else(|| {
            format!(
                "{} -> {}",
                source_node.data.label(),
                target_node.data.label()
            )
        });
        ctx.transition(format!("t_edge_{}", edge.id), &edge_label)
            .auto_input("input", &source_place)
            .auto_output("output", &actual_target)
            .logic_rhai(script)
            .done();
    } else {
        // Pure pass-through — try to merge places instead of creating a transition
        let can_merge = target_decl.is_join || wg.incoming(&edge.target).len() == 1;

        if can_merge {
            fixups.merges.push(PlaceMerge {
                dead: actual_target.id().to_string(),
                survivor: source_place.id().to_string(),
            });
        } else {
            // Multi-input non-join: keep pass-through transition
            let edge_label = edge.label.clone().unwrap_or_else(|| {
                format!(
                    "{} -> {}",
                    source_node.data.label(),
                    target_node.data.label()
                )
            });
            ctx.transition(format!("t_edge_{}", edge.id), &edge_label)
                .auto_input("input", &source_place)
                .auto_output("output", &actual_target)
                .logic_rhai("#{ output: input }")
                .done();
        }
    }

    Ok(())
}

fn find_output_place(
    ports: &NodePorts,
    edge: &WorkflowEdge,
) -> Result<PlaceHandle<DynamicToken>, CompileError> {
    // For decision nodes, find the output place matching the edge_id
    if let Some(ref handle) = edge.source_handle {
        for (edge_id, place) in &ports.output_places {
            if edge_id.as_deref() == Some(handle.as_str()) {
                return Ok(place.clone());
            }
        }
        // The handle named no registered output port. For single-output
        // nodes the source-handle id is *cosmetic*: the editor must emit
        // some handle id to satisfy xyflow + the typed-ports invariant
        // (e.g. a Start's `initial` port id "in", an AutomatedStep's success
        // port id), but the compiler models that node's only output as a
        // pass-through (`None`-keyed) place. Fall back to it rather than
        // failing. Genuine multi-output nodes (Decision branches,
        // ParallelSplit per-edge) register *only* `Some`-keyed places, so
        // they have no pass-through entry and an unmatched handle there
        // remains a hard error below.
        if let Some((_, place)) = ports.output_places.iter().find(|(eid, _)| eid.is_none()) {
            return Ok(place.clone());
        }
        return Err(CompileError::Compilation(format!(
            "no output place for source_handle '{}' on edge '{}'",
            handle, edge.id
        )));
    }

    // For parallel split nodes, find the output matching this edge id
    for (edge_id, place) in &ports.output_places {
        if edge_id.as_deref() == Some(edge.id.as_str()) {
            return Ok(place.clone());
        }
    }

    // Default: first output place with None edge_id, or first output place
    if let Some((_, place)) = ports.output_places.iter().find(|(eid, _)| eid.is_none()) {
        return Ok(place.clone());
    }

    if let Some((_, place)) = ports.output_places.first() {
        return Ok(place.clone());
    }

    Err(CompileError::Compilation(format!(
        "no output place for edge '{}'",
        edge.id
    )))
}

/// Build a dead→survivor alias map, resolving transitive chains.
pub(crate) fn resolve_aliases(merges: &[PlaceMerge]) -> HashMap<String, String> {
    let mut alias: HashMap<String, String> = HashMap::new();
    for m in merges {
        alias.insert(m.dead.clone(), m.survivor.clone());
    }
    // Resolve transitive chains: if A→B and B→C, make A→C
    let keys: Vec<String> = alias.keys().cloned().collect();
    for key in &keys {
        let mut target = alias[key].clone();
        let mut seen = HashSet::new();
        seen.insert(key.clone());
        while let Some(next) = alias.get(&target) {
            if seen.contains(next) {
                break;
            }
            seen.insert(target.clone());
            target = next.clone();
        }
        alias.insert(key.clone(), target);
    }
    alias
}

/// Rewrite all place references through the alias map, then remove dead places.
pub(crate) fn apply_merges(scenario: &mut ScenarioDefinition, alias: &HashMap<String, String>) {
    if alias.is_empty() {
        return;
    }
    let dead: HashSet<&String> = alias.keys().collect();

    // Rewrite arc place references in all transitions
    for t in &mut scenario.transitions {
        for arc in &mut t.inputs {
            if let Some(survivor) = alias.get(&arc.place) {
                arc.place = survivor.clone();
            }
        }
        for arc in &mut t.outputs {
            if let Some(survivor) = alias.get(&arc.place) {
                arc.place = survivor.clone();
            }
        }
    }

    // Transfer initial_tokens from dead places to survivors
    let mut tokens_to_move: HashMap<String, Vec<_>> = HashMap::new();
    for place in &scenario.places {
        if let Some(survivor) = alias.get(&place.id) {
            if !place.initial_tokens.is_empty() {
                tokens_to_move
                    .entry(survivor.clone())
                    .or_default()
                    .extend(place.initial_tokens.clone());
            }
        }
    }
    for place in &mut scenario.places {
        if let Some(tokens) = tokens_to_move.remove(&place.id) {
            place.initial_tokens.extend(tokens);
        }
    }

    // Remove dead places
    scenario.places.retain(|p| !dead.contains(&p.id));
}
