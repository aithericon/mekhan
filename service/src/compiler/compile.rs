//! Orchestrator: drives the build/validate/lower/wire pipeline that turns a
//! [`WorkflowGraph`] into AIR JSON. The heavy lifting lives in the sibling
//! `error`/`graph`/`validate`/`lower`/`wire`/`rhai_gen`/`pyio` modules.

use crate::compiler::graph::{topo_order, WorkflowDiGraph};
use crate::compiler::lower::{expand_node, NodeFiles, NodePorts, PostProcess};
use crate::compiler::validate::{
    validate, validate_edges_typed, validate_guards, validate_triggers,
};
use crate::compiler::wire::{apply_merges, resolve_aliases, wire_edge};
use crate::compiler::CompileError;
use crate::models::template::{WorkflowGraph, WorkflowNodeData};
use aithericon_executor_domain::InputSource;
use aithericon_sdk::scenario::ScenarioGroup;
use aithericon_sdk::Context;
use serde_json::Value;
use std::collections::HashMap;

/// Compile a WorkflowGraph to AIR JSON.
pub fn compile_to_air(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
) -> Result<Value, CompileError> {
    // 1. Build directed graph
    let wg = WorkflowDiGraph::build(graph)?;

    // 2. Validate
    validate(graph, &wg)?;

    // 2b. Typed-ports edge validation (Phase 2). Every edge must carry an
    //     explicit `target_handle` (Phase 2 hard-require) and the resolved
    //     source/target ports must type-match (empty target port = Json
    //     pass-through, otherwise exact field-name + kind match).
    validate_edges_typed(graph)?;

    // 2c. Typed-ports guard validation (Phase 3). Every Decision/Loop guard
    //     parses as Rhai and every `<upstream>.<field>` reference resolves
    //     against the topological scope at that node.
    validate_guards(graph, &wg)?;

    // 2d. Trigger node validation (Phase 5a). Trigger nodes connect to the
    //     workflow via a single outgoing edge; payload_mapping entries must
    //     reference real target-port fields and parse as Rhai.
    validate_triggers(graph)?;

    // 3. Topological sort (on DAG — loop_back edges excluded)
    let sorted = topo_order(&wg)?;

    // 4. Expand nodes
    let mut ctx = Context::new(name).description(description);
    let mut node_ports: HashMap<String, NodePorts> = HashMap::new();
    let mut fixups = PostProcess::default();

    // Pre-populate scope_groups: map child node_id → parent scope's group_id
    for node in &graph.nodes {
        if let Some(ref pid) = node.parent_id {
            // Only map if the parent is actually a scope node
            if graph
                .nodes
                .iter()
                .any(|n| n.id == *pid && matches!(n.data, WorkflowNodeData::Scope { .. }))
            {
                fixups
                    .scope_groups
                    .insert(node.id.clone(), format!("grp_{}", pid));
            }
        }
    }

    let empty_files: HashMap<String, InputSource> = HashMap::new();
    for ni in &sorted {
        let node = *wg.full.node_weight(*ni).unwrap();
        let outgoing = wg.outgoing(&node.id);
        let incoming = wg.incoming(&node.id);
        let node_files = files.get(&node.id).unwrap_or(&empty_files);
        expand_node(
            node,
            &outgoing,
            &incoming,
            &mut ctx,
            &mut node_ports,
            &mut fixups,
            node_files,
        )?;
    }

    // 5. Wire edges (may record merges instead of creating transitions)
    for edge in &graph.edges {
        wire_edge(edge, &node_ports, &wg, &mut ctx, &mut fixups)?;
    }

    let mut scenario = ctx.build();

    // 6. Resolve place aliases from merges
    let alias = resolve_aliases(&fixups.merges);

    // 7. Resolve terminal place IDs through aliases, then apply fixups
    let resolved_terminal_ids: Vec<String> = fixups
        .terminal_place_ids
        .iter()
        .map(|id| alias.get(id).cloned().unwrap_or_else(|| id.clone()))
        .collect();
    for place in &mut scenario.places {
        if resolved_terminal_ids.contains(&place.id) {
            place.place_type = "terminal".to_string();
        }
    }

    // 8. Apply group fixups
    for (group_id, group_name, parent_id) in &fixups.groups {
        scenario.groups.push(ScenarioGroup {
            id: group_id.clone(),
            name: group_name.clone(),
            parent_id: parent_id.clone(),
            metadata: None,
        });
    }

    // 8b. Tag places/transitions of scope children with their group_id
    for (node_id, group_id) in &fixups.scope_groups {
        let prefix = format!("p_{}_", node_id);
        let t_prefix = format!("t_{}_", node_id);
        for place in &mut scenario.places {
            if place.id.starts_with(&prefix) && place.group_id.is_none() {
                place.group_id = Some(group_id.clone());
            }
        }
        for transition in &mut scenario.transitions {
            if transition.id.starts_with(&t_prefix) && transition.group_id.is_none() {
                transition.group_id = Some(group_id.clone());
            }
        }
    }

    // 9. Apply place merges (rewrite arcs, remove dead places)
    apply_merges(&mut scenario, &alias);

    // 10. Control/data foundation: register typed `#/definitions/*` for the
    //     parked data + control tokens, schema the split places/ports, and
    //     synthesize read-arcs (the compiler-as-borrow-checker) so every
    //     Decision/Loop guard physically `&`-borrows the parked data place
    //     that owns the field it references. Runs post-merge: place ids final.
    apply_control_data_foundation(graph, &mut scenario, &fixups)?;

    let air_value = serde_json::to_value(&scenario)
        .map_err(|e| CompileError::Compilation(format!("failed to serialize scenario: {e}")))?;

    Ok(air_value)
}

/// Post-merge foundation phase. See call site (step 10).
fn apply_control_data_foundation(
    graph: &crate::models::template::WorkflowGraph,
    scenario: &mut aithericon_sdk::scenario::ScenarioDefinition,
    fixups: &PostProcess,
) -> Result<(), CompileError> {
    use crate::compiler::token_shape::{
        analyze, ctrl_def_name, data_def_name, def_ref, dynamic_token_definition,
        guard_readarc_plan,
    };
    use aithericon_sdk::scenario::{ScenarioArc, ScenarioPort, TransitionGuard, TransitionLogic};

    let report = analyze(graph)?;

    // (a) Typed definitions for every split node's parked data + control
    //     token. Data = the producer's full output shape (enforced);
    //     control = an open object (small, dynamic `_loop_*` keys).
    let (dyn_name, dyn_schema) = dynamic_token_definition();
    scenario.definitions.entry(dyn_name).or_insert(dyn_schema);
    for node_id in fixups.data_places.keys() {
        if let Some(shape) = report.node_out.get(node_id) {
            scenario
                .definitions
                .insert(data_def_name(node_id), shape.to_json_schema());
        }
        scenario.definitions.insert(
            ctrl_def_name(node_id),
            serde_json::json!({ "type": "object", "additionalProperties": true }),
        );
    }

    // (b) Schema the split places + the yield transition's output ports.
    for (node_id, data_place) in &fixups.data_places {
        let data_ref = def_ref(&data_def_name(node_id));
        let ctrl_ref = def_ref(&ctrl_def_name(node_id));
        let ctrl_place = format!("p_{node_id}_ctrl");
        for p in &mut scenario.places {
            if &p.id == data_place {
                p.token_schema = Some(data_ref.clone());
            } else if p.id == ctrl_place {
                p.token_schema = Some(ctrl_ref.clone());
            }
        }
        let yield_id = format!("t_{node_id}_yield");
        for t in &mut scenario.transitions {
            if t.id != yield_id {
                continue;
            }
            for port in &mut t.output_ports {
                if port.name == "data" {
                    port.schema_ref = Some(data_ref.clone());
                } else if port.name == "ctrl" {
                    port.schema_ref = Some(ctrl_ref.clone());
                }
            }
        }
    }

    // (c) Read-arc synthesis: lower each logical `input.<path>` reference to a
    //     physical `&`-borrow of the owning parked data place, rebinding it in
    //     the consuming transition's guard AND/OR logic. Decision/Loop hold
    //     the reference in `guard`; End/Failure result-mapping expressions
    //     (added on main) hold it in `logic` — both are covered.
    for b in guard_readarc_plan(graph)? {
        let data_place = format!("p_{}_data", b.producer_node);
        let var = format!("d_{}", b.producer_node.replace('-', "_"));
        let new_ref = format!("{var}.{}", b.producer_path);
        let schema_ref = def_ref(&data_def_name(&b.producer_node));
        let t_prefix = format!("t_{}_", b.consumer_node_id);

        for t in &mut scenario.transitions {
            if !t.id.starts_with(&t_prefix) {
                continue;
            }
            let guard_src = match &t.guard {
                Some(TransitionGuard::Rhai { source }) => Some(source.clone()),
                _ => None,
            };
            let logic_src = match &t.logic {
                TransitionLogic::Rhai { source } => Some(source.clone()),
                _ => None,
            };
            let in_guard = guard_src
                .as_deref()
                .map(|s| s.contains(&b.referenced))
                .unwrap_or(false);
            let in_logic = logic_src
                .as_deref()
                .map(|s| s.contains(&b.referenced))
                .unwrap_or(false);
            if !in_guard && !in_logic {
                continue;
            }
            if !t.input_ports.iter().any(|p| p.name == var) {
                t.input_ports.push(ScenarioPort {
                    name: var.clone(),
                    schema_ref: Some(schema_ref.clone()),
                    cardinality: "single".to_string(),
                });
            }
            if !t.inputs.iter().any(|a| a.place == data_place && a.read) {
                t.inputs.push(ScenarioArc {
                    place: data_place.clone(),
                    port: var.clone(),
                    weight: 1,
                    read: true,
                });
            }
            if in_guard {
                if let Some(s) = guard_src {
                    t.guard = Some(TransitionGuard::Rhai {
                        source: s.replace(&b.referenced, &new_ref),
                    });
                }
            }
            if in_logic {
                if let Some(s) = logic_src {
                    t.logic = TransitionLogic::Rhai {
                        source: s.replace(&b.referenced, &new_ref),
                    };
                }
            }
        }
    }

    // (d) Safety net: any pre-existing schema ref (effect tokens, DynamicToken)
    //     not in `definitions` gets a permissive `{}` so the runtime
    //     `SchemaRegistry` resolves every ref (unresolvable refs *fail*).
    let mut referenced: Vec<String> = Vec::new();
    for p in &scenario.places {
        if let Some(s) = &p.token_schema {
            referenced.push(s.clone());
        }
    }
    for t in &scenario.transitions {
        for port in t.input_ports.iter().chain(t.output_ports.iter()) {
            if let Some(s) = &port.schema_ref {
                referenced.push(s.clone());
            }
        }
    }
    for r in referenced {
        if let Some(name) = r.strip_prefix("#/definitions/") {
            scenario
                .definitions
                .entry(name.to_string())
                .or_insert(serde_json::json!({}));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::pyio::generate_py_io_files;
    use crate::compiler::rhai_gen::{
        build_human_task_injection_logic, build_join_merge_logic, interpolate_to_rhai_expr,
        json_to_rhai_literal, placeholder_to_accessor, PLUCK_HELPER,
    };
    use crate::models::template::*;

    #[test]
    fn placeholder_paths_validate() {
        assert_eq!(
            placeholder_to_accessor("invoice_file.url").as_deref(),
            Some("__pluck(input, [\"invoice_file\", \"url\"])")
        );
        assert_eq!(
            placeholder_to_accessor("  items[0].amount  ").as_deref(),
            Some("__pluck(input, [\"items\", 0, \"amount\"])")
        );
        assert_eq!(
            placeholder_to_accessor("invoice_id").as_deref(),
            Some("__pluck(input, [\"invoice_id\"])")
        );
        // Rejected: arbitrary Rhai / unsafe content stays literal.
        assert_eq!(placeholder_to_accessor("a + b").as_deref(), None);
        assert_eq!(placeholder_to_accessor("system(\"rm\")").as_deref(), None);
        assert_eq!(placeholder_to_accessor("1abc").as_deref(), None);
        assert_eq!(placeholder_to_accessor("").as_deref(), None);
        assert_eq!(placeholder_to_accessor("a[]").as_deref(), None);
    }

    #[test]
    fn interpolation_preserves_static_strings() {
        // No placeholder → byte-identical to json_to_rhai_literal.
        let s = "Plain \"quoted\" text\nwith newline";
        assert_eq!(
            interpolate_to_rhai_expr(s),
            json_to_rhai_literal(&Value::String(s.to_string()))
        );
        // Unbalanced / invalid braces are kept literal, not interpolated.
        assert_eq!(interpolate_to_rhai_expr("{{ a + b }}"), "\"{{ a + b }}\"");
        assert_eq!(interpolate_to_rhai_expr("a {{ unclosed"), "\"a {{ unclosed\"");
    }

    #[test]
    fn interpolation_builds_concat_expr() {
        assert_eq!(
            interpolate_to_rhai_expr("{{ invoice_file.url }}"),
            "(\"\" + (__pluck(input, [\"invoice_file\", \"url\"])))"
        );
        assert_eq!(
            interpolate_to_rhai_expr("Invoice {{ invoice_id }} ready"),
            "(\"\" + \"Invoice \" + (__pluck(input, [\"invoice_id\"])) + \" ready\")"
        );
    }

    /// Regression: the exact scenario that wedged a live net. A
    /// `{{ invoice_file.url }}` placeholder where `invoice_file` is a bare
    /// string (not an upload object) must degrade to an empty string, never
    /// raise a hard Rhai error (which a pure edge transition would retry
    /// forever).
    #[test]
    fn interpolation_is_null_safe_on_non_map_field() {
        let engine = rhai::Engine::new();
        let expr = interpolate_to_rhai_expr("img: {{ invoice_file.url }}");

        // invoice_file is a string -> .url is a hard error without __pluck.
        let s: String = engine
            .eval::<String>(&format!(
                "{PLUCK_HELPER}let input = #{{ invoice_file: \"example\" }}; {expr}"
            ))
            .expect("must not hard-error on a string-typed field");
        assert_eq!(s, "img: ");

        // Missing entirely -> still empty, no error.
        let s2: String = engine
            .eval::<String>(&format!("{PLUCK_HELPER}let input = #{{}}; {expr}"))
            .expect("must not hard-error on a missing field");
        assert_eq!(s2, "img: ");

        // Proper upload object -> the value resolves.
        let s3: String = engine
            .eval::<String>(&format!(
                "{PLUCK_HELPER}let input = #{{ invoice_file: #{{ url: \"http://x/y\" }} }}; {expr}"
            ))
            .expect("resolves");
        assert_eq!(s3, "img: http://x/y");
    }

    #[test]
    fn human_task_injection_interpolates_token() {
        let node = WorkflowNode {
            id: "review".to_string(),
            node_type: "human_task".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::HumanTask {
                label: "Review".to_string(),
                description: None,
                task_title: "Invoice {{ invoice_id }}".to_string(),
                instructions_mdsvex: Some("See {{ invoice_file.filename }}".to_string()),
                steps: vec![TaskStepConfig {
                    id: "s1".to_string(),
                    title: "Doc".to_string(),
                    description_mdsvex: None,
                    blocks: vec![TaskBlockConfig::Mdsvex {
                        content: "![invoice]({{ invoice_file.url }})".to_string(),
                    }],
                }],
            },
            parent_id: None,
            width: None,
            height: None,
        };

        let logic = build_human_task_injection_logic(&node);
        // Null-safe accessor + helper prelude (it has interpolations).
        assert!(logic.starts_with("fn __pluck("), "helper prelude missing: {logic}");
        assert!(
            logic.contains("d.title = (\"\" + \"Invoice \" + (__pluck(input, [\"invoice_id\"])))"),
            "title not interpolated: {logic}"
        );
        assert!(
            logic.contains("(__pluck(input, [\"invoice_file\", \"filename\"]))"),
            "instructions not interpolated: {logic}"
        );
        assert!(
            logic.contains("(__pluck(input, [\"invoice_file\", \"url\"]))"),
            "step block string not interpolated: {logic}"
        );
        // Static block keys remain plain literals.
        assert!(logic.contains("\"type\": \"mdsvex\""), "block shape changed: {logic}");
    }

    fn start_node(id: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
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
        }
    }

    fn end_node(id: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "end".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 100.0 },
            data: WorkflowNodeData::End {
                label: "End".to_string(),
                description: None,
                terminal: crate::models::template::default_terminal_port(),
                result_mapping: Vec::new(),
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn edge(id: &str, source: &str, target: &str) -> WorkflowEdge {
        WorkflowEdge {
            id: id.to_string(),
            source: source.to_string(),
            target: target.to_string(),
            source_handle: None,
            target_handle: Some("in".to_string()),
            label: None,
            edge_type: "sequence".to_string(),
        }
    }

    #[test]
    fn test_start_to_end() {
        let graph = WorkflowGraph {
            nodes: vec![start_node("s"), end_node("e")],
            edges: vec![edge("e1", "s", "e")],
            viewport: None,
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let places = air["places"].as_array().unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // End place merged into Start place = 1 place, 0 transitions
        assert_eq!(places.len(), 1);
        assert_eq!(transitions.len(), 0);

        // Start place absorbs terminal type. With typed ports, initial tokens
        // are NOT seeded at compile time — `parameterize_air` seeds them at
        // instance creation. Just verify the place is terminal-typed here.
        let start_place = places.iter().find(|p| p["id"] == "p_s_ready").unwrap();
        assert_eq!(start_place["type"], "terminal");
    }

    #[test]
    fn start_process_name_emits_rhai_and_process_start() {
        let mut s = start_node("s");
        if let WorkflowNodeData::Start {
            ref mut process_name,
            ..
        } = s.data
        {
            *process_name = Some("Invoice {{ invoice_id }}".to_string());
        }
        let graph = WorkflowGraph {
            nodes: vec![s, end_node("e")],
            edges: vec![edge("e1", "s", "e")],
            viewport: None,
        };

        let air = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new())
            .expect("compile ok");
        let transitions = air["transitions"].as_array().unwrap();

        // 1. Rhai name-derivation transition with the interpolated accessor.
        let proc_name = transitions
            .iter()
            .find(|t| t["id"] == "t_s_proc_name")
            .expect("t_s_proc_name transition");
        let logic = proc_name["logic"]["source"].as_str().unwrap_or_default();
        assert!(
            logic.contains(
                r#"d._process_name = ("" + "Invoice " + (__pluck(input, ["invoice_id"])))"#
            ),
            "name expr not interpolated: {logic}"
        );
        assert!(logic.starts_with("fn __pluck("), "helper prelude missing: {logic}");

        // 2. process_start effect transition, name resolved from the token.
        let proc_start = transitions
            .iter()
            .find(|t| t["id"] == "t_s_proc_start")
            .expect("t_s_proc_start transition");
        let ps = serde_json::to_string(proc_start).unwrap();
        assert!(ps.contains("process_start"), "not a process_start effect: {ps}");
        assert!(ps.contains("\"name_field\""), "missing name_field: {ps}");
        assert!(ps.contains("_process_name"), "missing _process_name: {ps}");
        assert!(ps.contains("forward_ports"), "missing forward_ports: {ps}");

        // Pipeline places exist; the seeded place id is unchanged.
        let places = air["places"].as_array().unwrap();
        for pid in ["p_s_ready", "p_s_named", "p_s_ready_out", "p_s_process"] {
            assert!(places.iter().any(|p| p["id"] == pid), "missing place {pid}");
        }
    }

    #[test]
    fn end_completes_process_when_start_registers() {
        let mut s = start_node("s");
        if let WorkflowNodeData::Start {
            ref mut process_name,
            ..
        } = s.data
        {
            *process_name = Some("Invoice {{ invoice_id }}".to_string());
        }
        let graph = WorkflowGraph {
            nodes: vec![s, end_node("e")],
            edges: vec![edge("e1", "s", "e")],
            viewport: None,
        };

        let air = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new())
            .expect("compile ok");
        let transitions = air["transitions"].as_array().unwrap();

        // End emits a `process_complete` effect that read-arcs the Start's
        // parked ProcessStarted token (`p_s_process`, non-consuming).
        let proc_complete = transitions
            .iter()
            .find(|t| t["id"] == "t_e_proc_complete")
            .expect("t_e_proc_complete transition");
        let pc = serde_json::to_string(proc_complete).unwrap();
        assert!(pc.contains("process_complete"), "not a process_complete effect: {pc}");
        assert!(pc.contains("\"read\":true"), "process token must be read-arc: {pc}");
        assert!(pc.contains("p_s_process"), "must read the Start's process place: {pc}");
        assert!(pc.contains("\"completed\""), "missing completed output port: {pc}");

        // The terminal moves to `p_e_completed` (post-completion sink).
        let places = air["places"].as_array().unwrap();
        let completed = places
            .iter()
            .find(|p| p["id"] == "p_e_completed")
            .expect("p_e_completed place");
        assert_eq!(completed["type"], "terminal");
    }

    #[test]
    fn end_stays_bare_terminal_without_process() {
        // No `process_name` on the Start → no process registered → the End
        // must NOT emit a `process_complete` effect (opt-in preserved).
        let graph = WorkflowGraph {
            nodes: vec![start_node("s"), end_node("e")],
            edges: vec![edge("e1", "s", "e")],
            viewport: None,
        };

        let air = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new())
            .expect("compile ok");
        let transitions = air["transitions"].as_array().unwrap();
        assert!(
            !transitions.iter().any(|t| t["id"] == "t_e_proc_complete"),
            "End must not complete a process when none was registered"
        );
    }

    #[test]
    fn test_start_edge_with_cosmetic_source_handle() {
        // Repro: the editor renders a Start's source handle with the
        // `initial` port id ("in" for a default Start), so an edge drawn
        // from Start serializes `source_handle: "in"`. Start's only output
        // is a pass-through place (`None`-keyed); the cosmetic handle must
        // fall back to it instead of failing "no output place for
        // source_handle 'in'".
        let graph = WorkflowGraph {
            nodes: vec![start_node("s"), end_node("e")],
            edges: vec![edge_with_handle("e1", "s", "e", "in")],
            viewport: None,
        };
        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(
            result.is_ok(),
            "cosmetic source_handle should fall back to pass-through place: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_human_task_expands() {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                WorkflowNode {
                    id: "ht".to_string(),
                    node_type: "human_task".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 50.0 },
                    data: WorkflowNodeData::HumanTask {
                        label: "Review".to_string(),
                        description: None,
                        task_title: "Review Task".to_string(),
                        instructions_mdsvex: Some("Please review".to_string()),
                        steps: vec![],
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                end_node("e"),
            ],
            edges: vec![edge("e1", "s", "ht"), edge("e2", "ht", "e")],
            viewport: None,
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let places = air["places"].as_array().unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // HumanTask creates 5 places (input, active, signal, output, errors)
        // + Start place = 6, + the control/data foundation split adds the
        // write-once parked data place and the slim control place = 8.
        assert_eq!(places.len(), 8);

        // request + finalize + 1 injection edge (s->ht) + the yield/park
        // transition = 4 (ht->e edge merged into the control place).
        assert_eq!(transitions.len(), 4);
    }

    #[test]
    fn test_decision_creates_branches() {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                WorkflowNode {
                    id: "d".to_string(),
                    node_type: "decision".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 50.0 },
                    data: WorkflowNodeData::Decision {
                        label: "Route".to_string(),
                        description: None,
                        conditions: vec![BranchCondition {
                            edge_id: "cond1".to_string(),
                            label: "Yes".to_string(),
                            // Constant guard — this test verifies that a Decision
                            // produces a branch transition with *some* guard, not the
                            // semantics of the guard. Phase 3 scope validation rejects
                            // unqualified `input.X`, so we use `true` here.
                            guard: "true".to_string(),
                        }],
                        default_branch: Some("default1".to_string()),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                end_node("e1"),
                end_node_with_id("e2"),
            ],
            edges: vec![
                edge("e0", "s", "d"),
                edge_with_handle("econd1", "d", "e1", "cond1"),
                edge_with_handle("edefault", "d", "e2", "default1"),
            ],
            viewport: None,
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // 1 branch + 1 default = 2 (3 pass-through edge transitions merged)
        assert_eq!(transitions.len(), 2);

        // Verify the branch has a guard
        let branch = transitions.iter().find(|t| t["id"] == "t_d_branch_0").unwrap();
        assert!(branch.get("guard").is_some());
    }

    fn end_node_with_id(id: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "end".to_string(),
            slug: None,
            position: Position { x: 100.0, y: 100.0 },
            data: WorkflowNodeData::End {
                label: format!("End {id}"),
                description: None,
                terminal: crate::models::template::default_terminal_port(),
                result_mapping: Vec::new(),
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn edge_with_handle(id: &str, source: &str, target: &str, handle: &str) -> WorkflowEdge {
        WorkflowEdge {
            id: id.to_string(),
            source: source.to_string(),
            target: target.to_string(),
            source_handle: Some(handle.to_string()),
            target_handle: Some("in".to_string()),
            label: None,
            edge_type: "sequence".to_string(),
        }
    }

    #[test]
    fn test_full_showcase_graph_compiles() {
        // Use the default graph to verify basic compilation works
        let graph = WorkflowGraph::default_graph();
        let result = compile_to_air(
            &graph,
            "showcase",
            "A test workflow",
            &std::collections::HashMap::new(),
        );
        assert!(result.is_ok(), "showcase compile failed: {:?}", result.err());
    }

    #[test]
    fn test_join_merge_single_input_is_passthrough() {
        let ports = vec!["in_0".to_string()];
        let shallow = build_join_merge_logic(&ports, MergeStrategy::ShallowLastWins);
        let deep = build_join_merge_logic(&ports, MergeStrategy::DeepMerge);
        // One branch never merges — both strategies collapse to pass-through.
        assert_eq!(shallow, "#{ output: in_0 }");
        assert_eq!(deep, "#{ output: in_0 }");
    }

    #[test]
    fn test_join_merge_strategies_differ() {
        let ports = vec!["in_0".to_string(), "in_1".to_string()];
        let shallow = build_join_merge_logic(&ports, MergeStrategy::ShallowLastWins);
        let deep = build_join_merge_logic(&ports, MergeStrategy::DeepMerge);

        assert_ne!(shallow, deep, "strategies must emit different Rhai");

        // ShallowLastWins: top-level key copy, no recursion helper, and crucially
        // no unregistered `merge_maps` call (the old latent bug).
        assert!(shallow.contains("for k in in_1.keys()"));
        assert!(shallow.contains("result[k] = in_1[k];"));
        assert!(!shallow.contains("merge_maps"));
        assert!(!shallow.contains("__deep_merge"));

        // DeepMerge: defines and folds through the recursive helper.
        assert!(deep.contains("fn __deep_merge(a, b)"));
        assert!(deep.contains("result = __deep_merge(result, in_1);"));
        assert!(deep.trim_end().ends_with("#{ output: result }"));
    }

    #[test]
    fn test_join_merge_three_inputs_fold_left() {
        let ports = vec!["in_0".to_string(), "in_1".to_string(), "in_2".to_string()];
        let deep = build_join_merge_logic(&ports, MergeStrategy::DeepMerge);
        // Folds in arrival order so the last branch wins on scalar collisions.
        let i1 = deep.find("__deep_merge(result, in_1)").unwrap();
        let i2 = deep.find("__deep_merge(result, in_2)").unwrap();
        assert!(i1 < i2, "in_1 must be folded before in_2");
    }

    fn automated_step_with_retry(id: &str, policy: RetryPolicy) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "automated_step".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 50.0 },
            data: WorkflowNodeData::AutomatedStep {
                label: "Run".to_string(),
                description: None,
                execution_spec: ExecutionSpecConfig {
                    backend_type: ExecutionBackendType::Docker,
                    entrypoint: None,
                    config: serde_json::json!({"image": "alpine:latest"}),
                },
                input: Port::empty_input(),
                output: default_output_port(ExecutionBackendType::Docker),
                retry_policy: policy,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn compile_retry_graph(policy: RetryPolicy) -> String {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                automated_step_with_retry("a", policy),
                end_node("e"),
            ],
            edges: vec![edge("e1", "s", "a"), edge("e2", "a", "e")],
            viewport: None,
        };
        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("retry graph should compile");
        air.to_string()
    }

    #[test]
    fn test_retry_immediate_no_timer() {
        let s = compile_retry_graph(RetryPolicy {
            max_retries: 2,
            backoff: BackoffKind::Immediate,
            base_delay_ms: 0,
        });
        // Immediate path: a direct Retry transition, no timer transitions.
        assert!(s.contains("\"Retry\""), "missing immediate Retry transition");
        assert!(!s.contains("Retry (arm timer)"), "immediate must not arm a timer");
        assert!(!s.contains("Retry (schedule)"));
        assert!(s.contains("Retries Exhausted"), "missing exhausted→error path");
        assert!(s.contains("f.retries < f.max_retries"));
        assert!(s.contains("f.retries >= f.max_retries"));
    }

    #[test]
    fn test_retry_exponential_emits_timer() {
        let s = compile_retry_graph(RetryPolicy {
            max_retries: 3,
            backoff: BackoffKind::Exponential,
            base_delay_ms: 1000,
        });
        assert!(s.contains("Retry (arm timer)"), "missing timer-arm transition");
        assert!(s.contains("Retry (schedule)"), "missing timer schedule effect");
        assert!(s.contains("Retry (re-dispatch)"), "missing timer re-dispatch");
        assert!(s.contains("Retries Exhausted"));
        // Exponential delay = base << attempt.
        assert!(s.contains("1000 << f.retries"), "expected exponential delay expr");
    }

    #[test]
    fn test_retry_prepare_uses_configured_max_retries() {
        let s = compile_retry_graph(RetryPolicy {
            max_retries: 5,
            backoff: BackoffKind::Immediate,
            base_delay_ms: 0,
        });
        // Prepare seeds the configured ceiling, not the old hardcoded 3.
        assert!(
            s.contains("d.max_retries = 5"),
            "prepare must use the configured max_retries"
        );
        assert!(
            !s.contains("d.max_retries = 3"),
            "the hardcoded max_retries=3 must be gone"
        );
    }

    #[test]
    fn test_generate_py_io_files_pair_and_no_duplicate_loader() {
        use crate::models::template::FieldKind;
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("vendor".to_string(), FieldKind::Text);
        fields.insert("amount".to_string(), FieldKind::Number);
        fields.insert("ok".to_string(), FieldKind::Bool);
        // Non-identifier: dropped from the typed surface, still item-accessible.
        fields.insert("bad-name".to_string(), FieldKind::Text);

        let files = generate_py_io_files(&fields);
        let map: std::collections::HashMap<_, _> = files.iter().cloned().collect();

        let stub = &map["_aithericon_io.pyi"];
        assert!(stub.contains("class Token(dict):"));
        assert!(stub.contains("vendor: Optional[str]"));
        assert!(stub.contains("amount: Optional[float]"));
        assert!(stub.contains("ok: Optional[bool]"));
        assert!(stub.contains("def load_input() -> Token: ..."));
        // Unsafe identifier is not a typed attribute.
        assert!(!stub.contains("bad-name"));

        let runtime = &map["_aithericon_io.py"];
        assert!(runtime.contains("import aithericon"));
        assert!(runtime.contains("return aithericon.token()"));
        // The shape lives in the SDK only — the runtime must not reimplement a
        // multi-file/dataclass loader (just the degraded SDK-absent read).
        assert!(!runtime.contains("dataclass"));
        assert!(!runtime.contains("Input"));

        // Pass-through node: still a valid stub, no field decls.
        let empty = generate_py_io_files(&std::collections::BTreeMap::new());
        let empty_map: std::collections::HashMap<_, _> = empty.iter().cloned().collect();
        assert!(empty_map["_aithericon_io.pyi"].contains("class Token(dict): ..."));
        assert!(empty_map["_aithericon_io.py"].contains("aithericon.token()"));
    }

    #[test]
    fn test_automated_step_error_edge_wires() {
        // An edge drawn from the automated step's "error" handle must resolve
        // (it would previously fail "no output place for source_handle
        // 'error'"). Success path goes to e1, failure path to e2.
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                automated_step_with_retry("a", RetryPolicy::default()),
                end_node("e1"),
                end_node_with_id("e2"),
            ],
            edges: vec![
                edge("e0", "s", "a"),
                edge("esucc", "a", "e1"),
                edge_with_handle("eerr", "a", "e2", "error"),
            ],
            viewport: None,
        };
        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("error-handle edge should wire");
        let s = air.to_string();
        // The error place must feed the error-handler branch.
        assert!(s.contains("p_a_error"), "error output place missing");
    }

    #[test]
    fn test_automated_step_without_error_edge_still_compiles() {
        // Default (no error edge): p_a_error has no consumer — the prior
        // dead-end-on-failure behaviour is preserved, compilation succeeds.
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                automated_step_with_retry("a", RetryPolicy::default()),
                end_node("e"),
            ],
            edges: vec![edge("e0", "s", "a"), edge("e1", "a", "e")],
            viewport: None,
        };
        assert!(
            compile_to_air(&graph, "t", "d", &std::collections::HashMap::new()).is_ok(),
            "step without an error edge must still compile"
        );
    }
}
