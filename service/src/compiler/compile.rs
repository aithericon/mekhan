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
use crate::models::template::{WorkflowGraph, WorkflowNode, WorkflowNodeData};
use aithericon_executor_domain::InputSource;
use aithericon_sdk::scenario::{ScenarioDefinition, ScenarioGroup};
use aithericon_sdk::Context;
use serde_json::Value;
use std::collections::HashMap;

/// Extract inline Python source text from a [`NodeFiles`] for the
/// borrow planner. Callers whose `files` already carry `InputSource::Raw`
/// (preview, stateless compile, most tests) get a complete map for free;
/// callers using `InputSource::StoragePath` (publish path) should call
/// the `*_with_inline_sources` entry points and pass the inline source
/// map directly. Skips `StoragePath` and `Url` silently.
fn derive_inline_sources(
    files: &NodeFiles,
) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (node_id, node_files) in files {
        let mut inner: HashMap<String, String> = HashMap::new();
        for (name, source) in node_files {
            match source {
                InputSource::Raw { content } => {
                    inner.insert(name.clone(), content.clone());
                }
                InputSource::Inline { value } => {
                    if let Value::String(s) = value {
                        inner.insert(name.clone(), s.clone());
                    }
                }
                _ => {}
            }
        }
        if !inner.is_empty() {
            out.insert(node_id.clone(), inner);
        }
    }
    out
}

/// Word-boundary-aware substring replace. Returns `Some(rewritten)` if at
/// least one match was rewritten; `None` if no matches were found (so callers
/// can skip the allocation when nothing changes).
///
/// Used by the Loop alias rewrite path: a naïve `str::replace("lp.iteration",
/// "input.lp.iteration")` would double-rewrite the engine-injected portion of
/// `t_<id>_continue` / `t_<id>_exit`'s guard, since the substring
/// `lp.iteration` appears inside `input.lp.iteration` too. This helper only
/// matches when the character immediately before the needle isn't an
/// identifier-continuation byte (alphanumeric or `_`) and isn't `.` — so
/// `(lp.iteration < 3)` rewrites cleanly while `input.lp.iteration` stays
/// untouched. Tail boundary is unconstrained — the needle ends in either an
/// identifier (`.iteration`) or a closing dot; what comes next doesn't matter
/// for safe matching.
fn replace_word_boundary(haystack: &str, needle: &str, repl: &str) -> Option<String> {
    if needle.is_empty() || !haystack.contains(needle) {
        return None;
    }
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0;
    let mut any = false;
    while i + needle_bytes.len() <= bytes.len() {
        if &bytes[i..i + needle_bytes.len()] == needle_bytes {
            let prev_ok = if i == 0 {
                true
            } else {
                let p = bytes[i - 1];
                !(p.is_ascii_alphanumeric() || p == b'_' || p == b'.')
            };
            if prev_ok {
                out.push_str(repl);
                i += needle_bytes.len();
                any = true;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    if i < bytes.len() {
        out.push_str(&haystack[i..]);
    }
    if any {
        Some(out)
    } else {
        None
    }
}

/// A child template, fully compiled + made spawn-callable, resolved at the
/// *parent's* publish time and frozen into the parent. Keyed by the parent's
/// `SubWorkflow` node id in [`SubWorkflowAir`]. `lower_subworkflow` embeds
/// [`air`](Self::air) into the `spawn_net` effect config; the callable
/// contract guarantees the child exposes the fixed boundary places `inbox`
/// (bridge_in), `reply_out` (bridge_reply), `fail_out` (bridge_out_param).
#[derive(Clone, Debug)]
pub struct ResolvedChild {
    /// Fully compiled + made-callable child scenario AIR, ready to embed.
    pub air: Value,
    /// Concrete child version this resolved to (provenance / pin freeze).
    pub resolved_version: i32,
    /// Stable child template id (spawn label / provenance).
    pub template_id: String,
}

/// Per-`SubWorkflow`-node resolved child AIR. Empty for every compile path
/// that has no sub-workflows (preview/tests use the back-compat wrapper); the
/// publish/preview handlers populate it after recursively compiling +
/// `make_child_subable`-ing each referenced child template.
pub type SubWorkflowAir = HashMap<String, ResolvedChild>;

/// Compile a WorkflowGraph to AIR JSON. Back-compat wrapper: no sub-workflow
/// resolution (a graph containing a `SubWorkflow` node compiles to an
/// `Unresolved` error here — callers that support sub-workflows use
/// [`compile_to_air_with_subworkflows`]).
///
/// Derives the Python-source map for the borrow planner from any `Raw`
/// entries in `files`. Callers that pass `StoragePath` (publish path)
/// should use [`compile_to_air_with_subworkflows_inline`] and provide
/// the inline source map explicitly — otherwise the borrow planner
/// can't scan source and silently emits no `<slug>.json` staging.
pub fn compile_to_air(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
) -> Result<Value, CompileError> {
    let inline = derive_inline_sources(files);
    let scenario = compile_to_scenario_with_inline_sources(
        graph,
        name,
        description,
        files,
        &inline,
        &SubWorkflowAir::new(),
    )?;
    serde_json::to_value(&scenario)
        .map_err(|e| CompileError::Compilation(format!("failed to serialize scenario: {e}")))
}

/// Like [`compile_to_air`] but with pre-resolved child sub-workflow AIR
/// (built by the publish/preview handlers, frozen at parent publish time).
pub fn compile_to_air_with_subworkflows(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
    sub_air: &SubWorkflowAir,
) -> Result<Value, CompileError> {
    let inline = derive_inline_sources(files);
    let scenario = compile_to_scenario_with_inline_sources(
        graph,
        name,
        description,
        files,
        &inline,
        sub_air,
    )?;
    serde_json::to_value(&scenario)
        .map_err(|e| CompileError::Compilation(format!("failed to serialize scenario: {e}")))
}

/// Publish-path entry: `files` may carry `InputSource::StoragePath` for
/// scaling (per-job-dispatch NATS payload stays small), and the
/// `inline_sources` map carries the Python source the borrow planner
/// needs to detect `<slug>.<field>` accesses. The two are decoupled —
/// the executor stages whatever `files` says; the planner scans whatever
/// `inline_sources` says.
pub fn compile_to_air_with_subworkflows_inline(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    sub_air: &SubWorkflowAir,
) -> Result<Value, CompileError> {
    let scenario = compile_to_scenario_with_inline_sources(
        graph,
        name,
        description,
        files,
        inline_sources,
        sub_air,
    )?;
    serde_json::to_value(&scenario)
        .map_err(|e| CompileError::Compilation(format!("failed to serialize scenario: {e}")))
}

/// Run the full build/validate/lower/wire pipeline and return the typed
/// [`ScenarioDefinition`] *before* JSON serialization. Recursive child
/// compilation (publish-time pin resolution) needs the typed scenario so it
/// can be made spawn-callable, hence this is the real entry point and the
/// `*_to_air` functions are thin serializing wrappers.
pub fn compile_to_scenario(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
    sub_air: &SubWorkflowAir,
) -> Result<ScenarioDefinition, CompileError> {
    let inline = derive_inline_sources(files);
    compile_to_scenario_with_inline_sources(graph, name, description, files, &inline, sub_air)
}

/// Internal entry that decouples the executor-side `files` (which may
/// carry `StoragePath` for runtime efficiency) from the compile-time
/// `inline_sources` (which the borrow planner needs as plain text).
pub fn compile_to_scenario_with_inline_sources(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    sub_air: &SubWorkflowAir,
) -> Result<ScenarioDefinition, CompileError> {
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

    // Pre-index container children: node_id -> [child nodes]. Cheap O(n)
    // pass; consumed by `lower_loop` to reject empty Loops, ignored by other
    // lowerings today. Keyed by parent id (not every node lives in a
    // container, so most lookups return an empty slice).
    let mut children_by_parent: HashMap<&str, Vec<&WorkflowNode>> = HashMap::new();
    for node in &graph.nodes {
        if let Some(ref pid) = node.parent_id {
            children_by_parent
                .entry(pid.as_str())
                .or_default()
                .push(node);
        }
    }

    let empty_files: HashMap<String, InputSource> = HashMap::new();
    let empty_children: Vec<&WorkflowNode> = Vec::new();
    for ni in &sorted {
        let node = *wg.full.node_weight(*ni).unwrap();
        let outgoing = wg.outgoing(&node.id);
        let incoming = wg.incoming(&node.id);
        let node_files = files.get(&node.id).unwrap_or(&empty_files);
        let children = children_by_parent
            .get(node.id.as_str())
            .unwrap_or(&empty_children);
        expand_node(
            node,
            &outgoing,
            &incoming,
            children,
            &mut ctx,
            &mut node_ports,
            &mut fixups,
            node_files,
            sub_air,
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
    apply_control_data_foundation(graph, &mut scenario, &fixups, inline_sources)?;

    Ok(scenario)
}

/// Post-merge foundation phase. See call site (step 10).
fn apply_control_data_foundation(
    graph: &crate::models::template::WorkflowGraph,
    scenario: &mut aithericon_sdk::scenario::ScenarioDefinition,
    fixups: &PostProcess,
    inline_sources: &HashMap<String, HashMap<String, String>>,
) -> Result<(), CompileError> {
    use crate::compiler::token_shape::{
        analyze, automated_step_borrow_plan, ctrl_def_name, data_def_name, def_ref,
        dynamic_token_definition, guard_readarc_plan, human_task_borrow_plan,
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
            // Skip arc-add if ANY arc to this place exists, regardless of
            // read/consume direction. Loop's own continue/exit transitions
            // are pre-wired in `lower_loop`: continue consumes + reproduces
            // its counter (`read: false` arc), exit read-arcs it. The
            // synthesis pass would otherwise add a duplicate `read: true`
            // arc next to the consuming one, breaking the engine's binding
            // resolution.
            if !t.inputs.iter().any(|a| a.place == data_place) {
                t.inputs.push(ScenarioArc {
                    place: data_place.clone(),
                    port: var.clone(),
                    weight: 1,
                    read: true,
                });
            }
            // Word-boundary replace so the rewrite doesn't double-prefix
            // an already-rewritten reference. Loop's own continue/exit
            // guards/logic are pre-wired in `lower_loop` with a hard-coded
            // `d_<slug>.iteration` (when expanding the user `loop_condition`
            // through this pipeline). A naïve `str::replace("<slug>.",
            // "d_<slug>.")` would then turn `d_<slug>.iteration` into
            // `d_d_<slug>.iteration` because the inner `<slug>.iteration`
            // matches as a substring. The boundary check (prior byte is
            // not an identifier-continuation byte) stops that.
            if in_guard {
                if let Some(s) = guard_src {
                    if let Some(rewritten) =
                        replace_word_boundary(&s, &b.referenced, &new_ref)
                    {
                        t.guard = Some(TransitionGuard::Rhai { source: rewritten });
                    }
                }
            }
            if in_logic {
                if let Some(s) = logic_src {
                    if let Some(rewritten) =
                        replace_word_boundary(&s, &b.referenced, &new_ref)
                    {
                        t.logic = TransitionLogic::Rhai { source: rewritten };
                    }
                }
            }
        }
    }

    // (c-deadend) Decision deadend enabling-time alignment. The Decision
    //      lowering emits one transition per branch + a default + an unguarded
    //      `t_<dec>_deadend` whose intent is "fire only when nothing else
    //      could." That priority intent breaks under the engine's selection
    //      rule (evaluation::select_next_transition): step 1 is *earliest
    //      enabling time wins*, and enabling time is the max created_at of all
    //      *consumed + read* tokens on the binding. Because deadend reads only
    //      the control-token place while branches/default also read the parked
    //      `p_<producer>_data`, deadend can end up with an *earlier* enabling
    //      time when the data token happens to be created after the ctrl token
    //      (a non-deterministic micro-race inside the producer's yield: the
    //      two are emitted from the same logic block but their `created_at`
    //      stamps depend on hash iteration order). Step 1 wins outright, so
    //      deadend fires even when a branch guard is true — caught live as
    //      03-decision-routing failing for score=40 but passing for score=10.
    //
    //      Fix: mirror the read-arcs (and corresponding input_ports) that the
    //      (c) read-arc synthesis added to a deadend's siblings onto the
    //      deadend itself. The deadend's guard/logic stays unchanged (it still
    //      `throw`s); the extra read-arcs only change its enabling time, so
    //      it now ties with the branches/default on step 1 and loses on step 2
    //      (specificity / input_count). Deadend's `priority(0)` is preserved
    //      as the final tiebreak when read-arcs alone don't disambiguate.
    for node in &graph.nodes {
        if !matches!(node.data, WorkflowNodeData::Decision { .. }) {
            continue;
        }
        let deadend_id = format!("t_{}_deadend", node.id);
        let sibling_prefixes = [
            format!("t_{}_branch_", node.id),
            format!("t_{}_default", node.id),
        ];

        // Collect siblings' read-arcs (place_id, port_name, schema_ref).
        let mut sibling_reads: Vec<(String, String, Option<String>)> = Vec::new();
        for t in &scenario.transitions {
            if !sibling_prefixes.iter().any(|p| t.id.starts_with(p)) {
                continue;
            }
            for a in &t.inputs {
                if !a.read {
                    continue;
                }
                let schema_ref = t
                    .input_ports
                    .iter()
                    .find(|p| p.name == a.port)
                    .and_then(|p| p.schema_ref.clone());
                if !sibling_reads
                    .iter()
                    .any(|(pl, po, _)| pl == &a.place && po == &a.port)
                {
                    sibling_reads.push((a.place.clone(), a.port.clone(), schema_ref));
                }
            }
        }
        if sibling_reads.is_empty() {
            continue;
        }

        if let Some(deadend) = scenario
            .transitions
            .iter_mut()
            .find(|t| t.id == deadend_id)
        {
            for (place_id, port_name, schema_ref) in sibling_reads {
                if !deadend.input_ports.iter().any(|p| p.name == port_name) {
                    deadend.input_ports.push(ScenarioPort {
                        name: port_name.clone(),
                        schema_ref,
                        cardinality: "single".to_string(),
                    });
                }
                if !deadend
                    .inputs
                    .iter()
                    .any(|a| a.place == place_id && a.port == port_name && a.read)
                {
                    deadend.inputs.push(ScenarioArc {
                        place: place_id,
                        port: port_name,
                        weight: 1,
                        read: true,
                    });
                }
            }
        }
    }

    // (c2) Python AutomatedStep direct slug access: for every
    //      `<slug>.<field>` access in a Python step's source, stage the
    //      producer's parked envelope as `<slug>.json` alongside
    //      `input.json` (the runner exposes `<slug>` as a Python global so
    //      `review.invoice_amount` is a plain attribute lookup — no
    //      `token[...]` ceremony, no IPC). The borrow plan resolves the
    //      slug to the parked producer via the same machinery as guards.
    let borrows = automated_step_borrow_plan(graph, inline_sources)?;
    let mut by_consumer: std::collections::HashMap<String, Vec<_>> =
        std::collections::HashMap::new();
    for b in borrows {
        by_consumer
            .entry(b.consumer_node_id.clone())
            .or_default()
            .push(b);
    }

    // (c2-pre) Validate declared output.fields on every Python AutomatedStep
    //      against (a) reserved runner globals (b) slugs this node actually
    //      borrows. The runner sweeps declared output names from globals()
    //      after exec(); without these guards, a field named `token` would
    //      shadow the inbound control token, and a field colliding with a
    //      borrowed upstream slug would silently re-export the input as
    //      output. Mirror of runner.rs _RESERVED_GLOBALS (executor-backend) —
    //      keep both lists in sync when adding new injected globals.
    const PY_RESERVED_GLOBALS: &[&str] = &[
        "token",
        "input",
        "inputs",
        "set_output",
        "load_inputs",
        "log_info",
        "log_warn",
        "log_error",
        "log_debug",
        "log_metric",
        "log_artifact",
        "update_progress",
        "define_phases",
        "update_phase",
        "aithericon",
        "sys",
        "os",
        "json",
    ];
    for node in &graph.nodes {
        let WorkflowNodeData::AutomatedStep {
            execution_spec,
            output,
            ..
        } = &node.data
        else {
            continue;
        };
        if execution_spec.backend_type != crate::models::template::ExecutionBackendType::Python {
            continue;
        }
        for field in &output.fields {
            if PY_RESERVED_GLOBALS.contains(&field.name.as_str()) {
                return Err(CompileError::OutputFieldShadowsReserved {
                    node_id: node.id.clone(),
                    field_name: field.name.clone(),
                });
            }
            if let Some(borrows) = by_consumer.get(&node.id) {
                if let Some(clash) = borrows.iter().find(|b| b.slug == field.name) {
                    return Err(CompileError::OutputFieldShadowsInput {
                        node_id: node.id.clone(),
                        field_name: field.name.clone(),
                        upstream_slug: clash.slug.clone(),
                        upstream_node_id: clash.producer_node.clone(),
                    });
                }
            }
        }
    }

    const BORROW_MARKER: &str = "/*__BORROWED_INPUTS__*/";
    for (consumer_id, consumer_borrows) in &by_consumer {
        // Two prepare-transition ID conventions: the inline AutomatedStep
        // wraps `prepare` under `scoped_prefix({id})` so the id is
        // `{id}/prepare`; the scheduled lifecycle emits `t_{id}_prepare`
        // directly. Match either.
        let prepare_a = format!("{}/prepare", consumer_id);
        let prepare_b = format!("t_{}_prepare", consumer_id);
        for t in &mut scenario.transitions {
            if t.id != prepare_a && t.id != prepare_b {
                continue;
            }
            let mut pushes = String::new();
            for b in consumer_borrows {
                let var = format!("d_{}", b.producer_node.replace('-', "_"));
                let data_place = format!("p_{}_data", b.producer_node);
                if !t.input_ports.iter().any(|p| p.name == var) {
                    t.input_ports.push(ScenarioPort {
                        name: var.clone(),
                        schema_ref: Some(def_ref(&data_def_name(&b.producer_node))),
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

                // Hoist business fields from their nested envelope path up to
                // the top level so the Python runner's `<slug>.<field>` direct
                // access matches what the picker / `_aithericon_io.pyi` show.
                // The shape model surfaces e.g. `review.invoice_amount` to the
                // user even though the parked envelope nests it under `data`
                // (HumanTask) or `detail.outputs` (AutomatedStep) — Rhai
                // guards close that gap via `producer_path` rewriting; Python
                // source isn't rewritten, so the staged envelope must be the
                // flat form. Spread is "envelope first, business overlay
                // second", so business fields win on any collision with
                // envelope meta (e.g. a form field literally named
                // `task_id`).
                let producer = graph.nodes.iter().find(|n| n.id == b.producer_node);
                let hoist_path: &[&str] = match producer.map(|n| &n.data) {
                    Some(WorkflowNodeData::HumanTask { .. }) => &["data"],
                    Some(WorkflowNodeData::AutomatedStep { .. }) => &["detail", "outputs"],
                    _ => &[],
                };
                let value_expr = if hoist_path.is_empty() {
                    var.clone()
                } else {
                    let flat = format!("__flat_{}", b.producer_node.replace('-', "_"));
                    // Build `__flat_x` by copying the envelope (sans the hoist
                    // segment at the top) then overlaying the nested business
                    // map. We narrow `__h` segment-by-segment with `type_of` /
                    // `()`-guard so a missing intermediate key yields an
                    // empty overlay rather than a Rhai hard error.
                    pushes.push_str(&format!(
                        "let {flat} = #{{}}; \
                         for __k in {var}.keys() {{ \
                             if __k != \"{top}\" {{ {flat}[__k] = {var}[__k]; }} \
                         }} \
                         let __h_{pid} = {var}; ",
                        flat = flat,
                        var = var,
                        top = hoist_path[0],
                        pid = b.producer_node.replace('-', "_"),
                    ));
                    for seg in hoist_path {
                        pushes.push_str(&format!(
                            "__h_{pid} = if type_of(__h_{pid}) == \"map\" {{ __h_{pid}[\"{seg}\"] }} else {{ () }}; ",
                            pid = b.producer_node.replace('-', "_"),
                            seg = seg,
                        ));
                    }
                    pushes.push_str(&format!(
                        "if type_of(__h_{pid}) == \"map\" {{ \
                             for __k in __h_{pid}.keys() {{ {flat}[__k] = __h_{pid}[__k]; }} \
                         }} ",
                        pid = b.producer_node.replace('-', "_"),
                        flat = flat,
                    ));
                    flat
                };

                pushes.push_str(&format!(
                    r#"job_inputs.push(#{{ "name": "{}.json", "source": #{{ "type": "inline", "value": {} }} }}); "#,
                    b.slug, value_expr
                ));
            }
            if let TransitionLogic::Rhai { source } = &t.logic {
                let new_source = source.replace(BORROW_MARKER, &pushes);
                t.logic = TransitionLogic::Rhai { source: new_source };
            }
        }
    }
    // Strip leftover markers from prepare transitions that had no borrows.
    for t in &mut scenario.transitions {
        if let TransitionLogic::Rhai { source } = &t.logic {
            if source.contains(BORROW_MARKER) {
                let new_source = source.replace(BORROW_MARKER, "");
                t.logic = TransitionLogic::Rhai { source: new_source };
            }
        }
    }

    // (c3) HumanTask placeholder borrows: direct sibling of (c2) for
    //      AutomatedStep — same read-arc, same `d_<producer>` var, same
    //      schema ref. `build_human_task_injection_logic` emits the
    //      wire-edge transition's Rhai against `input` (the inbound slim
    //      control token) with `__pluck(input, ["<slug>", …])` for every
    //      slug-qualified placeholder; this phase rewrites those calls
    //      to `__pluck(d_<producer>, […])` so they resolve against the
    //      read-arc-bound parked envelope instead. One model.
    let ht_borrows = human_task_borrow_plan(graph)?;
    let mut ht_by_consumer: std::collections::HashMap<String, Vec<_>> =
        std::collections::HashMap::new();
    for b in ht_borrows {
        ht_by_consumer
            .entry(b.consumer_node_id.clone())
            .or_default()
            .push(b);
    }
    for (consumer_id, consumer_borrows) in &ht_by_consumer {
        // The wire-edge transition is the one whose output writes to the
        // HumanTask's `p_<id>_input`. Multi-inbound HumanTasks (rare —
        // typically only via ParallelJoin) have one such transition per
        // inbound edge; rewrite each independently.
        let input_place = format!("p_{}_input", consumer_id);
        for t in &mut scenario.transitions {
            if !t.outputs.iter().any(|a| a.place == input_place) {
                continue;
            }
            for b in consumer_borrows {
                let var = format!("d_{}", b.producer_node.replace('-', "_"));
                let data_place = format!("p_{}_data", b.producer_node);
                // Slug-specific needle: the trailing `, ` (comma+space)
                // is exactly what `interpolate_to_rhai_expr` emits
                // between segments via `segs.join(", ")`, so this
                // matches only the multi-segment case `{{ <slug>.<f> }}`
                // and never a same-prefix root-level field like
                // `__pluck(input, ["startle"])` (no trailing comma).
                let needle = format!(r#"__pluck(input, ["{}", "#, b.slug);
                let replacement = format!(r#"__pluck({var}, ["#);
                let source = match &t.logic {
                    TransitionLogic::Rhai { source } => source.clone(),
                    _ => continue,
                };
                if !source.contains(&needle) {
                    continue;
                }
                if !t.input_ports.iter().any(|p| p.name == var) {
                    t.input_ports.push(ScenarioPort {
                        name: var.clone(),
                        schema_ref: Some(def_ref(&data_def_name(&b.producer_node))),
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
                t.logic = TransitionLogic::Rhai {
                    source: source.replace(&needle, &replacement),
                };
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
            viewport: None, instance_concurrency: Default::default(),
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let places = air["places"].as_array().unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // Start now forks (`park_outputs`): p_s_ready (seed) + p_s_data
        // (write-once parked copy, never read here) + p_s_main (token
        // forwarded; End merges into it) = 3 places, 1 transition (t_s_park).
        assert_eq!(places.len(), 3);
        assert_eq!(transitions.len(), 1);

        // The forwarded place absorbs the terminal type (End merged into
        // p_s_main); the seed place stays a normal state place. With typed
        // ports, initial tokens are NOT seeded at compile time —
        // `parameterize_air` seeds them at instance creation.
        let main_place = places.iter().find(|p| p["id"] == "p_s_main").unwrap();
        assert_eq!(main_place["type"], "terminal");
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
            viewport: None, instance_concurrency: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(),
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let places = air["places"].as_array().unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // HumanTask creates 5 places (input, active, signal, output, errors)
        // + the HT foundation split adds parked-data + slim-control = 7.
        // Start now forks too: p_s_ready + p_s_data + p_s_main = 3 → 10.
        assert_eq!(places.len(), 10);

        // request + finalize + 1 injection edge (s->ht) + the HT yield
        // transition + Start's t_s_park = 5 (ht->e merged into the control
        // place).
        assert_eq!(transitions.len(), 5);
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
            viewport: None, instance_concurrency: Default::default(),
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // Start's t_s_park + 1 branch + 1 default + the always-emitted
        // dead-end (unroutable token -> observable net error) = 4. The 3
        // pass-through edge transitions (s->d, d->e1, d->e2) are merged.
        assert_eq!(transitions.len(), 4);

        // The branch fires on its own guard.
        let branch = transitions
            .iter()
            .find(|t| t["id"] == "t_d_branch_0")
            .unwrap();
        assert!(branch.get("guard").is_some());

        // The default fires only when no branch matched (guarded).
        let default = transitions
            .iter()
            .find(|t| t["id"] == "t_d_default")
            .unwrap();
        assert!(default.get("guard").is_some());

        // A token matching neither branch nor default dead-ends into an
        // explicit error instead of being silently stranded: unguarded,
        // lowest priority so it only wins when nothing else is enabled.
        let deadend = transitions
            .iter()
            .find(|t| t["id"] == "t_d_deadend")
            .unwrap();
        assert!(deadend.get("guard").is_none());
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
                deployment_model: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(),
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

        let empty_ns = std::collections::BTreeMap::new();
        let empty_out = std::collections::BTreeMap::new();
        let files = generate_py_io_files(&fields, &empty_ns, &empty_out);
        let map: std::collections::HashMap<_, _> = files.iter().cloned().collect();

        let stub = &map["_aithericon_io.pyi"];
        assert!(stub.contains("class Token(dict):"));
        assert!(stub.contains("vendor: Optional[str]"));
        assert!(stub.contains("amount: Optional[float]"));
        assert!(stub.contains("ok: Optional[bool]"));
        // Unsafe identifier is not a typed attribute.
        assert!(!stub.contains("bad-name"));

        // No runtime `.py` is generated — the SDK exports `aithericon.token()`
        // and the runner injects `token`/`input` as globals, so a per-node
        // delegate adds noise without adding capability.
        assert_eq!(files.len(), 1, "only `.pyi` is generated per node");
        assert!(!map.contains_key("_aithericon_io.py"));
        // And the stub MUST NOT advertise a `load_input()` import — there's
        // no runtime backing it. Authors use the global `token` / `input`
        // or `aithericon.token()`.
        assert!(
            !stub.contains("def load_input"),
            "load_input is no longer a public API; stub must not advertise it"
        );

        // Pass-through node: still a valid stub, no field decls.
        let empty = generate_py_io_files(
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
        );
        let empty_map: std::collections::HashMap<_, _> = empty.iter().cloned().collect();
        assert!(empty_map["_aithericon_io.pyi"].contains("class Token(dict): ..."));
    }

    /// Declared outputs surface as top-level annotations in the `.pyi`
    /// overlay so Pyright/Pylance treats `vendor = ...` as a typed write
    /// (the runner's post-exec sweep promotes that global to `vendor.json`).
    /// Unsafe identifiers (Python keyword / hyphenated name) get dropped
    /// from the typed surface like input fields do — the runtime sweep
    /// still works via `globals()[name]`, just no editor type-check.
    #[test]
    fn test_pyio_outputs_become_top_level_annotations() {
        use crate::models::template::FieldKind;
        let fields = std::collections::BTreeMap::new();
        let empty_ns = std::collections::BTreeMap::new();
        let mut outputs = std::collections::BTreeMap::new();
        outputs.insert("vendor".to_string(), FieldKind::Text);
        outputs.insert("amount".to_string(), FieldKind::Number);
        outputs.insert("extracted".to_string(), FieldKind::Bool);
        outputs.insert("blob".to_string(), FieldKind::Json);
        // Dropped from typed surface (keyword + hyphen), runtime still
        // sweeps via globals().
        outputs.insert("class".to_string(), FieldKind::Text);
        outputs.insert("bad-name".to_string(), FieldKind::Text);

        let files = generate_py_io_files(&fields, &empty_ns, &outputs);
        let map: std::collections::HashMap<_, _> = files.iter().cloned().collect();
        let stub = &map["_aithericon_io.pyi"];

        assert!(
            stub.contains("Declared outputs"),
            "outputs header missing: {stub}"
        );
        // py_type mapping: Text/Textarea/etc → str, Number → float,
        // Bool → bool, Json → Any.
        assert!(stub.contains("vendor: str"), "vendor str: {stub}");
        assert!(stub.contains("amount: float"), "amount float: {stub}");
        assert!(stub.contains("extracted: bool"), "extracted bool: {stub}");
        assert!(stub.contains("blob: Any"), "blob Any: {stub}");
        // Order matters: outputs must come AFTER the token/input block so
        // Pyright resolves the assignment site to the module-level annotation.
        let token_pos = stub.find("input: Token").expect("input: Token");
        let vendor_pos = stub.find("vendor: str").expect("vendor: str");
        assert!(
            vendor_pos > token_pos,
            "outputs must follow token/input block"
        );

        // Unsafe identifiers dropped from typed surface.
        assert!(!stub.contains("class: str"), "keyword leaked: {stub}");
        assert!(!stub.contains("bad-name"), "hyphen leaked: {stub}");

        // Empty outputs: no annotation block at all (no spurious header).
        let no_outputs = generate_py_io_files(
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::new(),
        );
        let no_map: std::collections::HashMap<_, _> = no_outputs.iter().cloned().collect();
        assert!(!no_map["_aithericon_io.pyi"].contains("Declared outputs"));
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
            viewport: None, instance_concurrency: Default::default(),
        };
        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("error-handle edge should wire");
        let s = air.to_string();
        // The error place must feed the error-handler branch.
        assert!(s.contains("p_a_error"), "error output place missing");
    }

    /// Python AutomatedStep that reads `<slug>.<field>` in its source
    /// emits a scenario with (1) a read-arc into the producer's parked
    /// data place, (2) a `d_<producer>` input port on the prepare
    /// transition, and (3) a `job_inputs.push(... "<slug>.json" ... d_<producer> ...)`
    /// snippet in the prepare Rhai source. That triplet is the complete
    /// runtime contract for the direct-slug-access model.
    #[test]
    fn python_step_direct_slug_access_wires_into_scenario() {
        use aithericon_executor_domain::InputSource;
        use serde_json::json;
        use std::collections::HashMap;

        // Start → review (HumanTask, slug "review") → extract (Python AS) → end.
        // Python source reads `review.invoice_amount` so the compiler must
        // synthesize a borrow into review's parked data place.
        let graph_json = json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"review","type":"human_task","slug":"review",
             "position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review","taskTitle":"Review",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"invoice_amount","label":"Amt","kind":"number","required":true}}
                     ]}]}},
            {"id":"extract","type":"automated_step","slug":"extract",
             "position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"review","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"review","target":"extract","targetHandle":"in","type":"sequence"},
            {"id":"e3","source":"extract","target":"end","targetHandle":"in","type":"sequence"}
          ]
        });
        let graph: WorkflowGraph =
            serde_json::from_value(graph_json).expect("graph deser");

        let mut step_files = HashMap::new();
        step_files.insert(
            "main.py".to_string(),
            InputSource::Raw {
                content: "amount = review.invoice_amount\nprint(amount)\n".to_string(),
            },
        );
        let mut files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
        files.insert("extract".to_string(), step_files);

        let scenario = crate::compiler::compile_to_scenario(
            &graph,
            "borrow-test",
            "test",
            &files,
            &crate::compiler::SubWorkflowAir::new(),
        )
        .expect("compile direct-slug graph");

        // (1) The prepare transition has been rewritten with the borrow.
        let prepare = scenario
            .transitions
            .iter()
            .find(|t| t.id == "extract/prepare")
            .expect("prepare transition exists");

        // (1a) Sentinel has been replaced; no marker remains.
        match &prepare.logic {
            aithericon_sdk::scenario::TransitionLogic::Rhai { source } => {
                assert!(
                    !source.contains("__BORROWED_INPUTS__"),
                    "marker should be substituted; source: {source}"
                );
                assert!(
                    source.contains(r#""name": "review.json""#),
                    "prepare must stage review.json; source: {source}"
                );
                assert!(
                    source.contains("d_review"),
                    "prepare must reference the d_review read-arc var; source: {source}"
                );
            }
            other => panic!("expected Rhai logic, got {other:?}"),
        }

        // (1b) The read-arc port + arc landed on the prepare transition.
        assert!(
            prepare.input_ports.iter().any(|p| p.name == "d_review"),
            "d_review input port missing on prepare; got: {:?}",
            prepare.input_ports
        );
        assert!(
            prepare
                .inputs
                .iter()
                .any(|a| a.place == "p_review_data" && a.read),
            "read-arc into p_review_data missing; got: {:?}",
            prepare.inputs
        );

        // (2) For HumanTask producers the staged value must be the
        //     `data`-hoisted form: `__flat_<producer>` is built and what's
        //     passed to `job_inputs.push`. The bare `d_review` envelope (with
        //     `data.invoice_amount` nested) would defeat direct slug access in
        //     Python — the picker promises `review.invoice_amount`, not
        //     `review.data.invoice_amount`, so the runtime envelope must match.
        match &prepare.logic {
            aithericon_sdk::scenario::TransitionLogic::Rhai { source } => {
                assert!(
                    source.contains("__flat_review"),
                    "HumanTask producer must be staged via __flat_<producer>; source: {source}"
                );
                assert!(
                    source.contains("__h_review = d_review")
                        && source.contains(r#"__h_review["data"]"#),
                    "hoist must descend from d_review into its `data` segment; source: {source}"
                );
                assert!(
                    source.contains(r#""value": __flat_review"#),
                    "job_inputs.push must reference __flat_review, not d_review; source: {source}"
                );
                assert!(
                    !source.contains(r#""value": d_review"#),
                    "job_inputs.push must NOT stage the bare d_review envelope; source: {source}"
                );
            }
            other => panic!("expected Rhai logic, got {other:?}"),
        }
    }

    /// Execute the rewritten prepare Rhai with a *production-shaped*
    /// HumanTask envelope (form fields nested under `.data`) and prove the
    /// resulting `job_inputs[1].source.value` is the *flat* dict the Python
    /// runner expects — every form field at the top level, envelope-meta
    /// keys preserved, form fields winning on name collision. This is the
    /// integration test the runner-template unit tests can't be: those use
    /// synthetic flat envelopes and so cannot catch this end-to-end gap.
    #[test]
    fn human_task_borrow_stages_flat_envelope_at_runtime() {
        use aithericon_executor_domain::InputSource;
        use serde_json::json;
        use std::collections::HashMap;

        // Reuse the same minimal graph as the wiring test. The Python
        // source's `review.invoice_amount` access drives the borrow.
        let graph_json = json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"review","type":"human_task","slug":"review",
             "position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review","taskTitle":"Review",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"vendor_name","label":"V","kind":"text","required":true}},
                       {"type":"input","field":{"name":"invoice_amount","label":"A","kind":"number","required":true}}
                     ]}]}},
            {"id":"extract","type":"automated_step","slug":"extract",
             "position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"review","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"review","target":"extract","targetHandle":"in","type":"sequence"},
            {"id":"e3","source":"extract","target":"end","targetHandle":"in","type":"sequence"}
          ]
        });
        let graph: WorkflowGraph = serde_json::from_value(graph_json).unwrap();

        let mut step_files = HashMap::new();
        step_files.insert(
            "main.py".to_string(),
            InputSource::Raw {
                content: "v = review.vendor_name\na = review.invoice_amount\n".to_string(),
            },
        );
        let mut files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
        files.insert("extract".to_string(), step_files);

        let scenario = crate::compiler::compile_to_scenario(
            &graph,
            "borrow-runtime",
            "test",
            &files,
            &crate::compiler::SubWorkflowAir::new(),
        )
        .expect("compile");

        let prepare = scenario
            .transitions
            .iter()
            .find(|t| t.id == "extract/prepare")
            .expect("prepare transition");
        let source = match &prepare.logic {
            aithericon_sdk::scenario::TransitionLogic::Rhai { source } => source.clone(),
            _ => panic!("expected Rhai logic"),
        };

        // Build a synthetic prepare-time scope: `input` is the slim control
        // token (what `let d = input;` reads in the real prepare), `d_review`
        // is the production-shaped HumanTask parked envelope — form fields
        // nested under `.data`, envelope meta (`status`, `task_id`, ...) at
        // the top level. This is the exact shape `t_review_yield`'s
        // `YIELD_LOGIC` parks (per `lower_human_task` + the engine's
        // `HumanTaskCompletion` injection).
        let mut engine = rhai::Engine::new();
        engine.set_max_expr_depths(256, 256);
        let mut scope = rhai::Scope::new();
        scope.push("input", rhai::Map::new());
        let d_review_json = json!({
            "task_id": "T-1",
            "status": "completed",
            "completed_at": "2026-05-23T00:00:00Z",
            "data": {
                "vendor_name": "ACME",
                "invoice_amount": 1234.5
            }
        });
        let d_review: rhai::Dynamic = engine
            .parse_json(&d_review_json.to_string(), true)
            .expect("d_review parse")
            .into();
        scope.push_dynamic("d_review", d_review);

        let result: rhai::Map = engine
            .eval_with_scope(&mut scope, &source)
            .expect("prepare Rhai must execute under the synthetic scope");

        // The prepare returns `#{ job: d }`. Find the staged `review.json`
        // and assert its `value` is the flat form.
        let job = result
            .get("job")
            .and_then(|v| v.clone().try_cast::<rhai::Map>())
            .expect("prepare must return #{ job: <map> }");
        let inputs = job
            .get("spec")
            .and_then(|v| v.clone().try_cast::<rhai::Map>())
            .and_then(|m| m.get("inputs").cloned())
            .and_then(|v| v.try_cast::<rhai::Array>())
            .expect("spec.inputs array");

        let review_entry = inputs
            .iter()
            .filter_map(|v| v.clone().try_cast::<rhai::Map>())
            .find(|m| {
                m.get("name")
                    .and_then(|v| v.clone().try_cast::<String>())
                    .as_deref()
                    == Some("review.json")
            })
            .expect("review.json must be staged");
        let staged_value: rhai::Map = review_entry
            .get("source")
            .and_then(|v| v.clone().try_cast::<rhai::Map>())
            .and_then(|m| m.get("value").cloned())
            .and_then(|v| v.try_cast::<rhai::Map>())
            .expect("review.json source.value must be a map");

        // Form fields hoisted to top level — the direct-slug-access promise.
        assert_eq!(
            staged_value
                .get("vendor_name")
                .and_then(|v| v.clone().try_cast::<String>())
                .as_deref(),
            Some("ACME"),
            "form field vendor_name must be at top level; got staged: {staged_value:?}"
        );
        assert_eq!(
            staged_value
                .get("invoice_amount")
                .and_then(|v| v.clone().try_cast::<f64>()),
            Some(1234.5),
            "form field invoice_amount must be at top level; got staged: {staged_value:?}"
        );

        // Envelope meta preserved (so `review.task_id` / `review.completed_at`
        // still work in Python source if anyone ever wants them).
        assert_eq!(
            staged_value
                .get("task_id")
                .and_then(|v| v.clone().try_cast::<String>())
                .as_deref(),
            Some("T-1"),
            "envelope task_id must remain; got staged: {staged_value:?}"
        );

        // Nested `.data` is GONE from the staged shape — leaving it as a
        // sibling would let stale `review.data.invoice_amount` keep working
        // alongside the canonical `review.invoice_amount`, drifting the two.
        assert!(
            !staged_value.contains_key("data"),
            "nested `data` must be removed after hoisting; got staged: {staged_value:?}"
        );
    }

    /// Non-Python AutomatedStep (e.g. Docker) must still have the marker
    /// stripped — leaving no residual `/*__BORROWED_INPUTS__*/` comment
    /// in the published scenario.
    #[test]
    fn non_python_prepare_has_marker_stripped() {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                automated_step_with_retry("a", RetryPolicy::default()),
                end_node("e"),
            ],
            edges: vec![edge("e0", "s", "a"), edge("e1", "a", "e")],
            viewport: None,
            instance_concurrency: Default::default(),
        };
        let scenario = crate::compiler::compile_to_scenario(
            &graph,
            "no-borrow",
            "test",
            &std::collections::HashMap::new(),
            &crate::compiler::SubWorkflowAir::new(),
        )
        .expect("compile");

        let prepare = scenario
            .transitions
            .iter()
            .find(|t| t.id == "a/prepare")
            .expect("prepare exists");
        if let aithericon_sdk::scenario::TransitionLogic::Rhai { source } = &prepare.logic {
            assert!(
                !source.contains("__BORROWED_INPUTS__"),
                "marker must be stripped even when no borrows; source: {source}"
            );
        }
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
            viewport: None, instance_concurrency: Default::default(),
        };
        assert!(
            compile_to_air(&graph, "t", "d", &std::collections::HashMap::new()).is_ok(),
            "step without an error edge must still compile"
        );
    }

    fn start_node_with_slug_and_field(id: &str, slug: &str, field: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "start".to_string(),
            slug: Some(slug.to_string()),
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Start {
                label: "Start".to_string(),
                description: None,
                initial: Port {
                    id: "in".to_string(),
                    label: "Initial".to_string(),
                    fields: vec![PortField {
                        name: field.to_string(),
                        label: field.to_string(),
                        kind: FieldKind::Text,
                        required: true,
                        options: None,
                        description: None,
                        accept: None,
                    }],
                },
                process_name: None,
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn human_task_with_title(id: &str, title: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "human_task".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 50.0 },
            data: WorkflowNodeData::HumanTask {
                label: "Task".to_string(),
                description: None,
                task_title: title.to_string(),
                instructions_mdsvex: None,
                steps: vec![],
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    /// End-to-end: a HumanTask placeholder `{{ <slug>.<field> }}` must
    /// land in the compiled AIR as a read-arc against the upstream
    /// parked place + a rewritten `__pluck(d_<producer>, [...])` call,
    /// not the pre-rewrite `__pluck(input, ["<slug>", ...])` form. One
    /// model — same shape as Python AutomatedStep's `<slug>.<field>`.
    #[test]
    fn human_task_slug_borrow_rewrites_pluck_and_adds_read_arc() {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node_with_slug_and_field("s", "start", "invoice_id"),
                human_task_with_title("ht", "Pay {{ start.invoice_id }}"),
                end_node("e"),
            ],
            edges: vec![edge("e1", "s", "ht"), edge("e2", "ht", "e")],
            viewport: None,
            instance_concurrency: Default::default(),
        };

        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("compile");
        let transitions = air["transitions"].as_array().unwrap();

        // Find the wire-edge transition writing to the HumanTask's input.
        let edge_t = transitions
            .iter()
            .find(|t| {
                t["outputs"]
                    .as_array()
                    .map(|arr| arr.iter().any(|a| a["place"] == "p_ht_input"))
                    .unwrap_or(false)
            })
            .expect("wire-edge transition writing to p_ht_input must exist");

        // (1) Rhai logic must have been rewritten away from `input` to
        //     the read-arc-bound `d_s` variable.
        let logic_src = edge_t["logic"]["source"].as_str().unwrap_or("");
        assert!(
            logic_src.contains(r#"__pluck(d_s, ["invoice_id"])"#),
            "expected rewritten pluck against d_s, got logic: {logic_src}"
        );
        assert!(
            !logic_src.contains(r#"__pluck(input, ["start", "#),
            "pre-rewrite `__pluck(input, [\"start\", …])` must be gone: {logic_src}"
        );

        // (2) A read-arc on `p_s_data` with port `d_s` must have been
        //     added — the borrow's physical realization.
        let inputs = edge_t["inputs"].as_array().expect("inputs array");
        let read_arc = inputs.iter().find(|a| a["place"] == "p_s_data");
        assert!(
            read_arc.is_some(),
            "expected read-arc to p_s_data; inputs: {inputs:?}"
        );
        let read_arc = read_arc.unwrap();
        assert_eq!(read_arc["read"], serde_json::Value::Bool(true));
        assert_eq!(read_arc["port"], "d_s");

        // (3) The corresponding input port must carry the schema ref —
        //     same shape as Python's borrow ports.
        let ports = edge_t["input_ports"].as_array().expect("input_ports");
        let d_s_port = ports
            .iter()
            .find(|p| p["name"] == "d_s")
            .expect("d_s input port");
        assert_eq!(d_s_port["schema_ref"], "#/definitions/Data__s");
    }

    /// An unknown head identifier (typo, or a legitimate root-level
    /// field on the slim control token) must NOT be rewritten — the
    /// existing slim-token pluck path stays in place and resolves
    /// at runtime against `input` directly.
    #[test]
    fn human_task_unknown_slug_left_alone() {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node_with_slug_and_field("s", "start", "invoice_id"),
                human_task_with_title("ht", "Hello {{ mystery.field }}"),
                end_node("e"),
            ],
            edges: vec![edge("e1", "s", "ht"), edge("e2", "ht", "e")],
            viewport: None,
            instance_concurrency: Default::default(),
        };

        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("compile");
        let transitions = air["transitions"].as_array().unwrap();
        let edge_t = transitions
            .iter()
            .find(|t| {
                t["outputs"]
                    .as_array()
                    .map(|arr| arr.iter().any(|a| a["place"] == "p_ht_input"))
                    .unwrap_or(false)
            })
            .expect("wire-edge transition");

        let logic_src = edge_t["logic"]["source"].as_str().unwrap_or("");
        // Unknown slug stays as a root-level pluck against `input`.
        assert!(
            logic_src.contains(r#"__pluck(input, ["mystery", "field"])"#),
            "unknown slug must remain a root-level pluck: {logic_src}"
        );

        // No spurious read-arc on the unknown producer.
        let inputs = edge_t["inputs"].as_array().expect("inputs array");
        assert!(
            !inputs.iter().any(|a| a["place"] == "p_mystery_data"),
            "no read-arc should be synthesized for an unknown slug"
        );
    }

    /// A bare root-level placeholder `{{ field }}` (no slug prefix)
    /// remains a slim-token pluck — these are not slug-namespaced
    /// borrows. Start fields stay at the root of `input` at the
    /// wire-edge, exactly as before. Regression guard for backward
    /// compatibility with templates authored against the legacy
    /// flat-token model.
    #[test]
    fn human_task_bare_field_placeholder_left_at_root() {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node_with_slug_and_field("s", "start", "invoice_id"),
                human_task_with_title("ht", "Pay {{ invoice_id }}"),
                end_node("e"),
            ],
            edges: vec![edge("e1", "s", "ht"), edge("e2", "ht", "e")],
            viewport: None,
            instance_concurrency: Default::default(),
        };

        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("compile");
        let transitions = air["transitions"].as_array().unwrap();
        let edge_t = transitions
            .iter()
            .find(|t| {
                t["outputs"]
                    .as_array()
                    .map(|arr| arr.iter().any(|a| a["place"] == "p_ht_input"))
                    .unwrap_or(false)
            })
            .expect("wire-edge transition");

        let logic_src = edge_t["logic"]["source"].as_str().unwrap_or("");
        assert!(
            logic_src.contains(r#"__pluck(input, ["invoice_id"])"#),
            "bare field placeholder must stay as root-of-input pluck: {logic_src}"
        );

        let inputs = edge_t["inputs"].as_array().expect("inputs array");
        assert!(
            !inputs.iter().any(|a| a.get("read") == Some(&serde_json::Value::Bool(true))),
            "no read-arc should be added for a non-slug placeholder; inputs: {inputs:?}"
        );
    }

    /// Declared output field literally named `token` collides with the
    /// inbound-token runner global. The post-exec sweep would either
    /// shadow `token` or surprise the author by re-emitting it; reject
    /// at compile so the editor pins the offending node before publish.
    /// Mirror guard: see `PY_RESERVED_GLOBALS` in apply_control_data_foundation.
    #[test]
    fn python_output_field_named_token_rejected_as_reserved() {
        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"extract","type":"automated_step","slug":"extract","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "output":{"id":"out","label":"Output","fields":[
                       {"name":"token","label":"Token","kind":"text","required":true}
                     ]},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"extract","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"extract","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }"#;
        let graph: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        // Validation requires the Python step's entrypoint file to exist —
        // stage an empty `main.py`. The guard runs after validation but
        // before lowering, so empty source is enough to reach it.
        let mut files: std::collections::HashMap<
            String,
            std::collections::HashMap<String, aithericon_executor_domain::InputSource>,
        > = std::collections::HashMap::new();
        let mut step = std::collections::HashMap::new();
        step.insert(
            "main.py".to_string(),
            aithericon_executor_domain::InputSource::Raw {
                content: String::new(),
            },
        );
        files.insert("extract".to_string(), step);

        let err = compile_to_air(&graph, "t", "d", &files)
            .expect_err("token-named output field must reject");
        match err {
            CompileError::OutputFieldShadowsReserved { node_id, field_name } => {
                assert_eq!(node_id, "extract");
                assert_eq!(field_name, "token");
            }
            other => panic!("expected OutputFieldShadowsReserved, got {other:?}"),
        }
    }

    /// Declared output field name matches an upstream slug the Python
    /// source actually borrows (`review.invoice_amount` borrows `review`,
    /// and `review` is then declared as an output field). Without the
    /// guard the input global would silently re-export as this step's
    /// output. The check uses the per-consumer borrow list so a slug
    /// not referenced in source is fine — only the actually-bound names
    /// collide.
    #[test]
    fn python_output_field_collides_with_borrowed_input_rejected() {
        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"review","type":"human_task","slug":"review","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review","taskTitle":"Review",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"invoice_amount","label":"Amt","kind":"number","required":true}}
                     ]}]}},
            {"id":"extract","type":"automated_step","slug":"extract","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "output":{"id":"out","label":"Output","fields":[
                       {"name":"review","label":"Review","kind":"json","required":true}
                     ]},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"review","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"review","target":"extract","targetHandle":"in","type":"sequence"},
            {"id":"e3","source":"extract","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }"#;
        let graph: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        // Stage `main.py` with a `review.invoice_amount` borrow — that puts
        // `review` in the per-consumer bound-globals set the guard checks
        // against the declared output field.
        let mut files: std::collections::HashMap<
            String,
            std::collections::HashMap<String, aithericon_executor_domain::InputSource>,
        > = std::collections::HashMap::new();
        let mut step = std::collections::HashMap::new();
        step.insert(
            "main.py".to_string(),
            aithericon_executor_domain::InputSource::Raw {
                content: "amount = review.invoice_amount\nprint(amount)\n".to_string(),
            },
        );
        files.insert("extract".to_string(), step);

        let err = compile_to_air(&graph, "t", "d", &files)
            .expect_err("output 'review' shadowing borrowed input must reject");
        match err {
            CompileError::OutputFieldShadowsInput {
                node_id,
                field_name,
                upstream_slug,
                upstream_node_id,
            } => {
                assert_eq!(node_id, "extract");
                assert_eq!(field_name, "review");
                assert_eq!(upstream_slug, "review");
                assert_eq!(upstream_node_id, "review");
            }
            other => panic!("expected OutputFieldShadowsInput, got {other:?}"),
        }
    }

    /// Lower.rs MUST emit the declared `output.fields` into the prepare
    /// transition's `d.spec.outputs` Rhai literal — name, required, and
    /// kind per entry. Without this the executor sees `outputs: []` at
    /// runtime, the runner template bakes an empty `_DECLARED_OUTPUTS`,
    /// and the entire P1+P3 implicit-output story is inert in
    /// production. Locks the wiring: kind serializes as the snake_case
    /// FieldKind string ("text", "number", "bool", "json"), required
    /// flag carried verbatim, non-Python backends keep `outputs: []`.
    #[test]
    fn python_automated_step_emits_declared_outputs_in_prepare_rhai() {
        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"extract","type":"automated_step","slug":"extract","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Extract",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "output":{"id":"out","label":"Output","fields":[
                       {"name":"vendor","label":"Vendor","kind":"text","required":true},
                       {"name":"amount","label":"Amount","kind":"number","required":false},
                       {"name":"extracted","label":"Extracted","kind":"bool","required":true}
                     ]},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"extract","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"extract","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }"#;
        let graph: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        let mut files: std::collections::HashMap<
            String,
            std::collections::HashMap<String, aithericon_executor_domain::InputSource>,
        > = std::collections::HashMap::new();
        let mut step = std::collections::HashMap::new();
        step.insert(
            "main.py".to_string(),
            aithericon_executor_domain::InputSource::Raw {
                content: String::new(),
            },
        );
        files.insert("extract".to_string(), step);

        let air = compile_to_air(&graph, "t", "d", &files).expect("compile");
        let transitions = air["transitions"].as_array().expect("transitions");
        let prepare = transitions
            .iter()
            .find(|t| {
                t["id"]
                    .as_str()
                    .map(|s| s == "extract/prepare" || s == "t_extract_prepare")
                    .unwrap_or(false)
            })
            .expect("prepare transition");
        let source = prepare["logic"]["source"]
            .as_str()
            .expect("prepare logic source");

        // The literal `"outputs": []` is the pre-feature default — its
        // presence here would mean the lower.rs wiring regressed and
        // declared outputs never reach the runner.
        assert!(
            !source.contains(r#""outputs": []"#),
            "outputs literal must NOT be empty when fields are declared: {source}"
        );
        // Each declared field appears in the Rhai array with name +
        // required + kind, in serde-snake_case form.
        for needle in [
            r#""name": "vendor""#,
            r#""kind": "text""#,
            r#""name": "amount""#,
            r#""kind": "number""#,
            r#""name": "extracted""#,
            r#""kind": "bool""#,
            r#""required": true"#,
            r#""required": false"#,
        ] {
            assert!(
                source.contains(needle),
                "prepare Rhai missing {needle:?}: {source}"
            );
        }
    }

    /// Sibling of the wiring test: non-Python backends keep the
    /// historical `outputs: []` since the runner sweep / strict
    /// validation only exists on the Python runner. Widening this to
    /// other backends needs their own validation path.
    #[test]
    fn non_python_automated_step_keeps_empty_outputs_in_prepare_rhai() {
        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"run","type":"automated_step","slug":"run","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Run",
                     "executionSpec":{"backendType":"docker","config":{"image":"alpine:latest"}},
                     "output":{"id":"out","label":"Output","fields":[
                       {"name":"stdout","label":"Stdout","kind":"textarea","required":false}
                     ]},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"run","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"run","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }"#;
        let graph: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("compile");
        let transitions = air["transitions"].as_array().expect("transitions");
        let prepare = transitions
            .iter()
            .find(|t| {
                t["id"]
                    .as_str()
                    .map(|s| s == "run/prepare" || s == "t_run_prepare")
                    .unwrap_or(false)
            })
            .expect("prepare transition");
        let source = prepare["logic"]["source"]
            .as_str()
            .expect("prepare logic source");
        assert!(
            source.contains(r#""outputs": []"#),
            "non-Python backends must keep outputs: [] for now: {source}"
        );
    }

    fn subworkflow_node_with_output(id: &str, child_template_id: uuid::Uuid) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "sub_workflow".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::SubWorkflow {
                label: "Call".to_string(),
                description: None,
                template_id: child_template_id,
                version_pin: crate::models::template::VersionPin::Latest,
                input_mapping: vec![],
                output: Port {
                    id: "out".to_string(),
                    label: "Out".to_string(),
                    fields: vec![PortField {
                        name: "greeting".to_string(),
                        label: "Greeting".to_string(),
                        kind: FieldKind::Text,
                        required: true,
                        options: None,
                        description: None,
                        accept: None,
                    }],
                },
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    fn end_node_with_mapping(id: &str, target: &str, expr: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.to_string(),
            node_type: "end".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::End {
                label: "Done".to_string(),
                description: None,
                terminal: Port {
                    id: "in".to_string(),
                    label: "Terminal".to_string(),
                    fields: vec![],
                },
                result_mapping: vec![crate::models::template::FieldMapping {
                    target_field: target.to_string(),
                    expression: expr.to_string(),
                }],
            },
            parent_id: None,
            width: None,
            height: None,
        }
    }

    /// SubWorkflow's declared output field accessed as `<slug>.<field>` in a
    /// downstream End mapping MUST resolve as a parked-producer borrow — read-
    /// arc on `p_<sub>_data` + Rhai rewrite to `d_<sub>.<field>`. Without this
    /// (regression caught live on 06-subworkflow): the End reads from the
    /// post-yield control token which carries only `_*`/task_id/status, so
    /// `input.<field>` returns null and the result is empty.
    ///
    /// Also asserts the new `t_<sub>_join` envelope-unwrap shape: the child's
    /// terminal reply token wraps the declared outputs under
    /// `exit_code.value.<field>` (End's result_shape stamp), so the join must
    /// unwrap before projecting the declared port — otherwise the parent
    /// downstream sees `null` despite the child returning the right value.
    #[test]
    fn subworkflow_slug_borrow_and_join_unwraps_exit_code() {
        let child_id = uuid::Uuid::new_v4();
        let parent = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                {
                    let mut n = subworkflow_node_with_output("sub", child_id);
                    n.slug = Some("sub".to_string());
                    n
                },
                end_node_with_mapping("e", "greeting", "sub.greeting"),
            ],
            edges: vec![edge("e0", "s", "sub"), edge("e1", "sub", "e")],
            viewport: None,
            instance_concurrency: Default::default(),
        };

        // SubWorkflow lowering only needs an opaque AIR Value to embed in the
        // spawn effect config; the child isn't recompiled here. A minimal
        // ScenarioDefinition-shaped JSON suffices.
        let mut sub_air = SubWorkflowAir::new();
        sub_air.insert(
            "sub".to_string(),
            ResolvedChild {
                air: serde_json::json!({
                    "name": "child-stub",
                    "places": [],
                    "transitions": [],
                    "groups": [],
                    "mock_adapters": [],
                    "definitions": {},
                    "requirements": [],
                }),
                resolved_version: 1,
                template_id: child_id.to_string(),
            },
        );

        let air = compile_to_air_with_subworkflows_inline(
            &parent,
            "test",
            "",
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &sub_air,
        )
        .expect("compile parent with sub_air");
        let transitions = air["transitions"].as_array().unwrap();

        // (1) Join logic unwraps exit_code.value before projecting declared
        //     output port. Without the unwrap, the parent receives the raw
        //     End-shaped envelope (where the field lives at depth-3, not
        //     top level).
        let join = transitions
            .iter()
            .find(|t| t["id"] == "t_sub_join")
            .expect("t_sub_join transition");
        let join_src = join["logic"]["source"].as_str().expect("join logic");
        assert!(
            join_src.contains("exit_code") && join_src.contains("value"),
            "join must unwrap reply.exit_code.value before projecting declared port: {join_src}"
        );
        assert!(
            join_src.contains(r#"__v["greeting"]"#),
            "join must read declared field from unwrapped __v, not raw reply: {join_src}"
        );

        // (2) End's `sub.greeting` mapping → read-arc on `p_sub_data` +
        //     `d_sub.greeting` rewrite. This is the SubWorkflow-as-parked-
        //     producer contract (is_parked_producer recognizes SubWorkflow).
        let end_shape = transitions
            .iter()
            .find(|t| t["id"] == "t_e_result_shape")
            .expect("t_e_result_shape transition");

        let end_src = end_shape["logic"]["source"].as_str().expect("end logic");
        assert!(
            end_src.contains("d_sub.greeting"),
            "End mapping must rewrite sub.greeting → d_sub.greeting: {end_src}"
        );

        let inputs = end_shape["inputs"].as_array().expect("end inputs");
        let read = inputs.iter().find(|a| a["place"] == "p_sub_data");
        assert!(
            read.is_some(),
            "End must take a read-arc on p_sub_data; inputs: {inputs:?}"
        );
        assert_eq!(read.unwrap()["read"], serde_json::Value::Bool(true));
        assert_eq!(read.unwrap()["port"], "d_sub");
    }

    /// Loop counter parked in `p_<loop>_data`: an AutomatedStep body of the
    /// loop must be able to read `<slug>.iteration` even though the executor
    /// envelope (executor_lifecycle.rs t_<step>_to_output) strips the
    /// workflow token down to `{ job_id, run, execution_id, detail, source,
    /// status }`. Pre-park-refactor the counter rode on the control token
    /// under `<slug>: { iteration: N }`, was dropped at the AutomatedStep
    /// envelope, and the loop's own continue/exit guards failed reading
    /// `input.<slug>.iteration` from the post-envelope body_out token.
    ///
    /// Asserts the full park-and-borrow pipeline end-to-end:
    ///   - `t_<loop>_enter` produces a `p_<loop>_data` token (`{iteration:0}`).
    ///   - `t_<loop>_continue` consumes + reproduces `p_<loop>_data`,
    ///     guards on `d_<slug>.iteration`.
    ///   - `t_<loop>_exit` read-arcs `p_<loop>_data` (so it stays parked
    ///     for post-loop consumers).
    ///   - The body's `t_<body>/prepare` gets a read-arc on `p_<loop>_data`
    ///     and a `<slug>.json` staging entry — promoted by the runner as a
    ///     Python global so `lp.iteration` is a plain attribute lookup.
    ///   - End mapping `<slug>.iteration` rewrites to `d_<slug>.iteration`
    ///     with a read-arc on `p_<loop>_data` (final-count visibility).
    #[test]
    fn loop_counter_parked_and_borrowed_into_automated_step_body() {
        use aithericon_executor_domain::InputSource;
        use serde_json::json;
        use std::collections::HashMap;

        let graph_json = json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"lp","type":"loop","slug":"lp","position":{"x":0,"y":0},
             "data":{"type":"loop","label":"Iterate",
                     "maxIterations": 10,
                     "loopCondition":"lp.iteration < 3"}},
            {"id":"tick","type":"automated_step","slug":"tick",
             "parentId":"lp",
             "position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Body",
                     "executionSpec":{"backendType":"python","entrypoint":"main.py","config":{"entrypoint":"main.py","python":"python3","sdk":true}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"},
                     "output":{"id":"out","label":"Tick","fields":[
                       {"name":"saw","label":"Iteration","kind":"number","required":true}
                     ]}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End",
                     "resultMapping":[
                       {"targetField":"final_count","expression":"lp.iteration"}
                     ]}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"lp","targetHandle":"in","type":"sequence"},
            {"id":"e_body_in","source":"lp","target":"tick","sourceHandle":"body_in","targetHandle":"in","type":"sequence"},
            {"id":"e_body_out","source":"tick","target":"lp","targetHandle":"body_out","type":"loop_back"},
            {"id":"e_lp_end","source":"lp","target":"end","targetHandle":"in","type":"sequence"}
          ]
        });
        let graph: WorkflowGraph =
            serde_json::from_value(graph_json).expect("graph deser");

        let mut step_files: HashMap<String, InputSource> = HashMap::new();
        step_files.insert(
            "main.py".to_string(),
            InputSource::Raw {
                content: "saw = lp.iteration\n".to_string(),
            },
        );
        let mut files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
        files.insert("tick".to_string(), step_files);

        let scenario = crate::compiler::compile_to_scenario(
            &graph,
            "loop-body-borrow",
            "",
            &files,
            &crate::compiler::SubWorkflowAir::new(),
        )
        .expect("compile loop+automated-step graph");

        // (1) Loop's parked-counter topology.
        let enter = scenario
            .transitions
            .iter()
            .find(|t| t.id == "t_lp_enter")
            .expect("t_lp_enter");
        assert!(
            enter
                .outputs
                .iter()
                .any(|a| a.place == "p_lp_data" && a.port == "data"),
            "enter must produce the parked counter; outputs: {:?}",
            enter.outputs
        );

        let cont = scenario
            .transitions
            .iter()
            .find(|t| t.id == "t_lp_continue")
            .expect("t_lp_continue");
        assert!(
            cont.inputs
                .iter()
                .any(|a| a.place == "p_lp_data" && a.port == "d_lp" && !a.read),
            "continue must CONSUME the counter (read=false) on port d_lp; inputs: {:?}",
            cont.inputs
        );
        assert!(
            cont.outputs
                .iter()
                .any(|a| a.place == "p_lp_data" && a.port == "data"),
            "continue must produce a fresh counter; outputs: {:?}",
            cont.outputs
        );
        match &cont.guard {
            Some(aithericon_sdk::scenario::TransitionGuard::Rhai { source }) => {
                assert!(
                    source.contains("d_lp.iteration"),
                    "continue guard must reference d_lp.iteration (rewritten): {source}"
                );
                assert!(
                    !source.contains("input.lp.iteration"),
                    "continue guard must NOT read iteration off the token (executor envelope strips it): {source}"
                );
            }
            other => panic!("continue must have a Rhai guard: {other:?}"),
        }

        let exit = scenario
            .transitions
            .iter()
            .find(|t| t.id == "t_lp_exit")
            .expect("t_lp_exit");
        assert!(
            exit.inputs
                .iter()
                .any(|a| a.place == "p_lp_data" && a.port == "d_lp" && a.read),
            "exit must READ-ARC the counter so it survives for post-loop consumers; inputs: {:?}",
            exit.inputs
        );

        // (2) Body's prepare staging — `lp.json` + read-arc into `p_lp_data`.
        let prepare = scenario
            .transitions
            .iter()
            .find(|t| t.id == "tick/prepare")
            .expect("tick/prepare");
        match &prepare.logic {
            aithericon_sdk::scenario::TransitionLogic::Rhai { source } => {
                assert!(
                    source.contains(r#""name": "lp.json""#),
                    "body prepare must stage lp.json (the parked counter envelope): {source}"
                );
                assert!(
                    source.contains("d_lp"),
                    "body prepare must reference d_lp read-arc var: {source}"
                );
            }
            other => panic!("expected Rhai logic on prepare, got {other:?}"),
        }
        assert!(
            prepare
                .inputs
                .iter()
                .any(|a| a.place == "p_lp_data" && a.read),
            "body prepare must read-arc p_lp_data; inputs: {:?}",
            prepare.inputs
        );

        // (3) End mapping reads the final counter through the same parked
        //     place — proves the counter survives post-loop too.
        let end_shape = scenario
            .transitions
            .iter()
            .find(|t| t.id == "t_end_result_shape")
            .expect("t_end_result_shape");
        match &end_shape.logic {
            aithericon_sdk::scenario::TransitionLogic::Rhai { source } => {
                assert!(
                    source.contains("d_lp.iteration"),
                    "End mapping must rewrite lp.iteration → d_lp.iteration: {source}"
                );
            }
            other => panic!("expected Rhai logic on end shape, got {other:?}"),
        }
        assert!(
            end_shape
                .inputs
                .iter()
                .any(|a| a.place == "p_lp_data" && a.read),
            "End must read-arc p_lp_data for the final iteration value; inputs: {:?}",
            end_shape.inputs
        );
    }

    /// `t_<dec>_deadend` must inherit the read-arcs that the (c) read-arc
    /// synthesis added to its sibling branch/default transitions. Without
    /// the mirror, deadend's enabling time (max created_at over only the
    /// control-token arc) can land *earlier* than the branches' (which
    /// includes the parked data place), and the engine's
    /// `select_next_transition` step-1 "earliest enabling time wins" rule
    /// fires deadend even when a branch guard is true. Regression caught
    /// live on 03-decision-routing: score=40 (`doubled >= 50 → true`)
    /// failing because deadend won the race over t_route_branch_0.
    #[test]
    fn decision_deadend_inherits_sibling_readarcs() {
        use crate::models::template::BranchCondition;
        let graph = WorkflowGraph {
            nodes: vec![
                start_node_with_slug_and_field("s", "start", "score"),
                WorkflowNode {
                    id: "dec".to_string(),
                    node_type: "decision".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::Decision {
                        label: "Route".to_string(),
                        description: None,
                        conditions: vec![BranchCondition {
                            edge_id: "hi".to_string(),
                            label: "High".to_string(),
                            guard: "start.score >= 50".to_string(),
                        }],
                        default_branch: Some("lo".to_string()),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                end_node_with_id("e_hi"),
                end_node_with_id("e_lo"),
            ],
            edges: vec![
                edge("e0", "s", "dec"),
                WorkflowEdge {
                    id: "e_hi".to_string(),
                    source: "dec".to_string(),
                    target: "e_hi".to_string(),
                    source_handle: Some("hi".to_string()),
                    target_handle: Some("in".to_string()),
                    label: None,
                    edge_type: "conditional".to_string(),
                },
                WorkflowEdge {
                    id: "e_lo".to_string(),
                    source: "dec".to_string(),
                    target: "e_lo".to_string(),
                    source_handle: Some("lo".to_string()),
                    target_handle: Some("in".to_string()),
                    label: None,
                    edge_type: "conditional".to_string(),
                },
            ],
            viewport: None,
            instance_concurrency: Default::default(),
        };
        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("compile");
        let transitions = air["transitions"].as_array().unwrap();

        let branch = transitions
            .iter()
            .find(|t| t["id"] == "t_dec_branch_0")
            .expect("t_dec_branch_0");
        let branch_data_arc = branch["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["place"] == "p_s_data")
            .expect("branch should have a read-arc to p_s_data");
        assert_eq!(branch_data_arc["read"], serde_json::Value::Bool(true));

        let deadend = transitions
            .iter()
            .find(|t| t["id"] == "t_dec_deadend")
            .expect("t_dec_deadend");
        let deadend_data_arc = deadend["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["place"] == "p_s_data");
        assert!(
            deadend_data_arc.is_some(),
            "deadend must mirror the branch's read-arc on p_s_data to align enabling time; inputs: {:?}",
            deadend["inputs"]
        );
        assert_eq!(deadend_data_arc.unwrap()["read"], serde_json::Value::Bool(true));
        // The mirrored read-arc must also bring along the input_port so the
        // engine can bind the schema'd token, matching the sibling's shape.
        let deadend_ports = deadend["input_ports"].as_array().unwrap();
        assert!(
            deadend_ports.iter().any(|p| p["name"] == "d_s"),
            "deadend must declare the mirrored input_port: {deadend_ports:?}"
        );
    }
}
