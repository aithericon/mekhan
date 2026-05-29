//! Orchestrator: drives the build/validate/lower/wire pipeline that turns a
//! [`WorkflowGraph`] into AIR JSON. The heavy lifting lives in the sibling
//! `error`/`graph`/`validate`/`lower`/`wire`/`rhai_gen`/`pyio` modules.

use crate::compiler::graph::{topo_order, WorkflowDiGraph};
use crate::compiler::interface::InterfaceRegistry;
use crate::compiler::lower::{expand_node, ConfigStorage, NodeFiles, NodePorts, PostProcess};
use crate::compiler::resource_refs::KnownResources;
use crate::compiler::validate::{
    validate, validate_edges_typed, validate_guards, validate_maps, validate_repeaters,
    validate_schema_refs, validate_triggers,
};
use crate::compiler::wire::{apply_merges, resolve_aliases, wire_edge};
use crate::compiler::CompileError;
use crate::models::template::{Port, WorkflowGraph, WorkflowNode, WorkflowNodeData};
use aithericon_executor_domain::InputSource;
use aithericon_sdk::scenario::{ScenarioDefinition, ScenarioGroup};
use aithericon_sdk::Context;
use petgraph::graph::NodeIndex;
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
                InputSource::Inline {
                    value: Value::String(s),
                } => {
                    inner.insert(name.clone(), s.clone());
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
pub(super) fn replace_word_boundary(haystack: &str, needle: &str, repl: &str) -> Option<String> {
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

/// Idempotently add a `d_<producer>` input port + read-arc to `t` against the
/// producer's parked data place. Returns the `d_<producer>` variable name on
/// success (with hyphens → underscores), or `None` when the producer didn't
/// publish a `data_port` — caller skips that borrow silently. Centralises the
/// four-times-repeated read-arc wiring across the borrow phases (guards / c2
/// Python / c3 HumanTask / c4-c5 backend-config).
///
/// `allow_under_consume_arc` controls the idempotency check: `true` only
/// blocks a new read arc when an *existing* read arc to the same place is
/// present (c2 / c3 / c4 / c5 — the consumer's data-binding transition never
/// has pre-existing arcs against producer data places); `false` blocks on
/// **any** arc, read or consume (guards — Loop's `lower_loop` pre-wires its
/// continue/exit transitions with consume arcs against the counter place,
/// and a duplicate read arc next to a consume arc breaks the engine's
/// binding-resolution rules).
pub(super) fn wire_read_arc(
    t: &mut aithericon_sdk::scenario::ScenarioTransition,
    producer_node: &str,
    interfaces: &InterfaceRegistry,
    allow_under_consume_arc: bool,
) -> Option<String> {
    use crate::compiler::token_shape::{data_def_name, def_ref};
    use aithericon_sdk::scenario::{ScenarioArc, ScenarioPort};

    let var = format!("d_{}", producer_node.replace('-', "_"));
    let data_place = interfaces
        .get(producer_node)
        .and_then(|i| i.data_port.clone())?;

    if !t.input_ports.iter().any(|p| p.name == var) {
        t.input_ports.push(ScenarioPort {
            name: var.clone(),
            schema_ref: Some(def_ref(&data_def_name(producer_node))),
            cardinality: "single".to_string(),
        });
    }
    let arc_blocks = if allow_under_consume_arc {
        t.inputs.iter().any(|a| a.place == data_place && a.read)
    } else {
        t.inputs.iter().any(|a| a.place == data_place)
    };
    if !arc_blocks {
        t.inputs.push(ScenarioArc {
            place: data_place,
            port: var.clone(),
            weight: 1,
            read: true,
            count_from: None,
            correlate_on: None,
        });
    }
    Some(var)
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
    /// The child's Start node `initial` Port — its user-declared input
    /// contract. When this child is referenced as an agent tool, these
    /// fields become the LLM-facing `input_schema`. Empty fields ⇒ the
    /// agent falls back to a permissive object schema. Extracted from the
    /// child's high-level graph at resolution time (`resolve_subworkflow_air`).
    pub input_contract: Port,
    /// The child's **output contract** — a `Result` Port whose fields are the
    /// union of every End node's `result_mapping` target field (Json-typed),
    /// i.e. exactly what the child returns as `exit_code.value`. Derived (with
    /// [`input_contract`](Self::input_contract)) via
    /// [`crate::compiler::derive_child_io`]. The publish path reconciles this
    /// onto the SubWorkflow node's `output` port so the join, `output_ports`,
    /// and the borrow resolver all read the true contract; the editor reads it
    /// via the `io-contract` endpoint. Empty fields ⇒ opaque pass-through.
    pub output_contract: Port,
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
    let known = KnownResources::new();
    let scenario = compile_to_scenario_with_inline_sources(
        graph,
        name,
        description,
        files,
        &inline,
        &SubWorkflowAir::new(),
        &known,
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
    let known = KnownResources::new();
    let scenario = compile_to_scenario_with_inline_sources(
        graph,
        name,
        description,
        files,
        inline_sources,
        sub_air,
        &known,
    )?;
    serde_json::to_value(&scenario)
        .map_err(|e| CompileError::Compilation(format!("failed to serialize scenario: {e}")))
}

/// Publish-path entry returning both AIR JSON and the per-node interface
/// registry JSON. The publish handler persists `interface_json` alongside
/// `air_json` so a parent's `SubWorkflow` resolver can read it back without
/// re-deriving boundary places from string conventions. See
/// `service/src/process/publish.rs::resolve_subworkflow_air`.
pub fn compile_to_air_with_subworkflows_and_interfaces(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    sub_air: &SubWorkflowAir,
    known_resources: &KnownResources,
) -> Result<(Value, Value), CompileError> {
    let (air, iface, _) = compile_to_air_with_subworkflows_interfaces_and_configs(
        graph,
        name,
        description,
        files,
        inline_sources,
        sub_air,
        known_resources,
        ConfigStorage::ephemeral(),
    )?;
    Ok((air, iface))
}

/// Publish-path entry that also returns the per-node static config blobs the
/// caller uploads to S3 (keyed by node id, value is the resolved JSON the
/// compiler would previously have inlined into the Rhai prepare-transition).
/// Pass [`ConfigStorage::ephemeral`] for previews / tests that don't upload.
pub fn compile_to_air_with_subworkflows_interfaces_and_configs(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    sub_air: &SubWorkflowAir,
    known_resources: &KnownResources,
    config_storage: ConfigStorage<'_>,
) -> Result<(Value, Value, HashMap<String, serde_json::Value>), CompileError> {
    let (scenario, interfaces, node_configs) = compile_to_scenario_and_interfaces_with_configs(
        graph,
        name,
        description,
        files,
        inline_sources,
        sub_air,
        known_resources,
        config_storage,
    )?;
    let air = serde_json::to_value(&scenario)
        .map_err(|e| CompileError::Compilation(format!("failed to serialize scenario: {e}")))?;
    let iface = serde_json::to_value(&interfaces)
        .map_err(|e| CompileError::Compilation(format!("failed to serialize interfaces: {e}")))?;
    Ok((air, iface, node_configs))
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
    let known = KnownResources::new();
    compile_to_scenario_with_inline_sources(
        graph,
        name,
        description,
        files,
        &inline,
        sub_air,
        &known,
    )
}

/// Internal entry that decouples the executor-side `files` (which may
/// carry `StoragePath` for runtime efficiency) from the compile-time
/// `inline_sources` (which the borrow planner needs as plain text).
pub(crate) fn compile_to_scenario_with_inline_sources(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    sub_air: &SubWorkflowAir,
    known_resources: &KnownResources,
) -> Result<ScenarioDefinition, CompileError> {
    let (scenario, _interfaces) = compile_to_scenario_and_interfaces(
        graph,
        name,
        description,
        files,
        inline_sources,
        sub_air,
        known_resources,
    )?;
    Ok(scenario)
}

/// Prototype entry: compile and return both the scenario AND the per-node
/// interface registry. The registry is alias-rewritten post-merge so place
/// ids are stable; consumers (publish-side SubWorkflow resolution, future
/// scope/borrow consumers) read it directly instead of pattern-matching on
/// place id conventions.
///
/// This is the seam that publish persists alongside `air_json` (sidecar
/// `interface_json`), so a parent compile that embeds this template via a
/// `SubWorkflow` node reads the child's interface verbatim — no scanning,
/// no `place_type == "terminal" && !id.contains('/')` filtering.
pub(crate) fn compile_to_scenario_and_interfaces(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    sub_air: &SubWorkflowAir,
    known_resources: &KnownResources,
) -> Result<(ScenarioDefinition, InterfaceRegistry), CompileError> {
    let (scenario, interfaces, _node_configs) = compile_to_scenario_and_interfaces_with_configs(
        graph,
        name,
        description,
        files,
        inline_sources,
        sub_air,
        known_resources,
        ConfigStorage::ephemeral(),
    )?;
    Ok((scenario, interfaces))
}

/// Like [`compile_to_scenario_and_interfaces`] but also returns the per-node
/// static config blobs the publish layer uploads to S3 (keyed by node id).
/// The publish entry point uses this variant; tests / preview that don't
/// upload pass [`ConfigStorage::ephemeral`] and discard the third element.
///
/// `config_storage` controls the storage key the Rhai literal embeds — see
/// [`ConfigStorage`]. `known_resources` carries the workspace-resource
/// manifest collected by the publish handler (see `discover_known_resources`);
/// tests / preview / analyze pass an empty map.
pub(crate) fn compile_to_scenario_and_interfaces_with_configs(
    graph: &WorkflowGraph,
    name: &str,
    description: &str,
    files: &NodeFiles,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    sub_air: &SubWorkflowAir,
    known_resources: &KnownResources,
    config_storage: ConfigStorage<'_>,
) -> Result<
    (
        ScenarioDefinition,
        InterfaceRegistry,
        HashMap<String, serde_json::Value>,
    ),
    CompileError,
> {
    // 1. Build directed graph
    let wg = WorkflowDiGraph::build(graph)?;

    // 2. Pre-lowering validations (edges, guards, triggers, resources,
    //    schema refs, repeaters). See `run_validations` for the per-phase
    //    rationale.
    run_validations(graph, &wg, known_resources)?;

    // 3. Topological sort (on DAG — loop_back edges excluded)
    let sorted = topo_order(&wg)?;

    // 4. Lower every node in topological order. Owns the per-node state
    //    (`ctx`, `node_ports`, `fixups`, `interfaces`, `node_configs`)
    //    that subsequent phases read from.
    let mut ctx = Context::new(name).description(description);
    let mut node_ports: HashMap<String, NodePorts> = HashMap::new();
    let mut fixups = PostProcess::default();
    let mut interfaces: InterfaceRegistry = HashMap::new();
    let mut node_configs: HashMap<String, serde_json::Value> = HashMap::new();
    lower_nodes_topologically(
        graph,
        &wg,
        &sorted,
        files,
        sub_air,
        config_storage,
        &mut ctx,
        &mut node_ports,
        &mut fixups,
        &mut interfaces,
        &mut node_configs,
    )?;

    // 4b. Drain queued agent → tool-child wiring. Runs after every
    //     `expand_node` so each tool child's NodePorts is in `node_ports`
    //     but before edge wiring so the new invoke/collect transitions
    //     participate in any downstream merges.
    crate::compiler::lower::apply_agent_tool_wirings(
        &mut ctx,
        &node_ports,
        &fixups.agent_tool_wirings,
    )?;

    // 4c. Drain queued Timeout body-cancellation fan-outs. Runs after every
    //     `expand_node` so each body child's `NodeInterface.cancellable`
    //     slot is populated, and before edge wiring so the synthesized
    //     drain transitions participate in any downstream merges.
    crate::compiler::lower::apply_timeout_cancel_fanouts(
        &mut ctx,
        &interfaces,
        &fixups.timeout_cancel_fanouts,
    )?;

    // 5. Wire edges (may record merges instead of creating transitions)
    for edge in &graph.edges {
        wire_edge(edge, &node_ports, &wg, &mut ctx, &mut fixups)?;
    }

    let mut scenario = ctx.build();

    // 6. Resolve place aliases from merges
    let alias = resolve_aliases(&fixups.merges);

    // 6a. Sub-graph ownership derivation (PROTOTYPE).
    //
    //     Walk the pre-merge scenario once and credit every place/transition
    //     to its owning node via the existing `p_{id}_*` / `t_{id}_*` naming
    //     convention. This is the one (and only) place the prefix match
    //     survives — concentrated, audited, and run once. After this every
    //     consumer reads `interface.owned_places` / `interface.owned_transitions`
    //     directly.
    //
    //     Then alias-rewrite every place id in every interface so downstream
    //     passes see post-merge ids: `entry`, `named_inputs`, `outputs`,
    //     `data_port`, `workflow_terminals`, `owned_places` are all stable.
    //     This is the analog of compile.rs's old terminal-id alias resolution
    //     (step 7), generalised — and the structural fix for the
    //     `e5ed9fc` / `674408e` SubWorkflow terminal-filter leak: consumers
    //     never have to re-derive collapse-stable ids.
    derive_node_ownership(&scenario, &mut interfaces);
    populate_borrowed_paths(graph, inline_sources, &mut interfaces);
    for iface in interfaces.values_mut() {
        iface.rewrite_places(&alias);
    }

    // 7. Terminal place_type fixup (reads `interfaces.workflow_terminals`).
    apply_terminal_place_types(&mut scenario, &interfaces);

    // 8. Apply group fixups (declared groups + scope-child group tagging).
    apply_group_fixups(&mut scenario, &fixups, &interfaces);

    // 9. Apply place merges (rewrite arcs, remove dead places)
    apply_merges(&mut scenario, &alias);

    // 10. Control/data foundation: register typed `#/definitions/*` for the
    //     parked data + control tokens, schema the split places/ports, and
    //     synthesize read-arcs (the compiler-as-borrow-checker) so every
    //     Decision/Loop guard physically `&`-borrows the parked data place
    //     that owns the field it references. Runs post-merge: place ids
    //     final. Reads exclusively from `interfaces.data_port`.
    apply_control_data_foundation(
        graph,
        &mut scenario,
        &interfaces,
        inline_sources,
        known_resources,
        &mut node_configs,
    )?;

    Ok((scenario, interfaces, node_configs))
}

/// Pre-lowering validation pipeline. Runs the seven typed validators in
/// the order downstream phases depend on:
///
/// - `validate` — structural sanity (nodes, edges, parent_id references).
/// - `validate_edges_typed` (Phase 2) — every edge carries an explicit
///   `target_handle`, and source/target ports type-match (empty target =
///   Json pass-through, otherwise exact field-name + kind match).
/// - `validate_guards` (Phase 3) — every Decision/Loop guard parses as
///   Rhai and every `<upstream>.<field>` ref resolves against the
///   topological scope at that node.
/// - `validate_triggers` (Phase 5a) — Trigger nodes connect via a single
///   outgoing edge; `payload_mapping` entries reference real target-port
///   fields and parse as Rhai.
/// - `validate_resource_refs` — every workspace-known resource references
///   a registered ResourceType; the name is unique against step slugs +
///   reserved control-token vocabulary. Empty `known_resources` (tests,
///   analyze, preview) short-circuits; only the publish path scans the
///   workspace.
/// - `validate_schema_refs` — every `{"$ref": "#/definitions/<name>"}` in
///   any `executionSpec.config` resolves cleanly. Surfaces as
///   `SchemaRefUnresolved` with offending node id + JSON pointer.
/// - `validate_repeaters` (Feature B) — each HumanTask Repeater's
///   `<slug>.<field>[*]…` ref is well-formed, the producer's array shape
///   matches, and the `output_slug` is valid. MUST run after
///   `validate_guards` (relies on per-node shapes) and before lowering.
/// - `validate_maps` (Map node) — each Map's `itemsRef` resolves to an array
///   on a known parked producer. Needs the per-node shapes (`analyze`) like
///   `validate_repeaters`, but runs BEFORE `validate_guards` so its precise
///   `MapItemsRef*` errors win over the guard pass's generic `GuardUnresolved`
///   (which would otherwise fire first on the itemsRef read-arc).
fn run_validations(
    graph: &WorkflowGraph,
    wg: &WorkflowDiGraph<'_>,
    known_resources: &KnownResources,
) -> Result<(), CompileError> {
    validate(graph, wg)?;
    validate_edges_typed(graph)?;
    // `validate_maps` runs BEFORE `validate_guards`: the guard read-arc plan
    // already scans each Map's `itemsRef` (a synthesized read-arc into the
    // producer's parked place) and would surface an unknown-slug `itemsRef` as
    // a generic `GuardUnresolved`. Running the Map-specific pass first yields
    // the precise `MapItemsRefUnresolved` / `MapItemsRefNotArray` (the latter
    // is NOT caught by the guard pass at all — it resolves any field, array or
    // not). It still needs the per-node shapes from `analyze`, available here.
    validate_maps(graph)?;
    validate_guards(graph, wg)?;
    validate_triggers(graph)?;
    crate::compiler::resource_refs::validate_resource_refs(known_resources, graph)?;
    validate_schema_refs(graph)?;
    validate_repeaters(graph)?;
    Ok(())
}

/// Lower every workflow node in topological order. Pre-indexes
/// `scope_groups` (child → parent-scope group_id) and `children_by_parent`
/// (container → child nodes) once, then dispatches to `expand_node` for
/// each topologically-sorted node. Side-effects flow through the `&mut`
/// params; downstream phases (wire / merge / control-data) read from
/// `interfaces` + `fixups` + `node_configs`.
#[allow(clippy::too_many_arguments)]
fn lower_nodes_topologically<'a>(
    graph: &'a WorkflowGraph,
    wg: &WorkflowDiGraph<'a>,
    sorted: &[NodeIndex],
    files: &'a NodeFiles,
    sub_air: &'a SubWorkflowAir,
    config_storage: ConfigStorage<'a>,
    ctx: &mut Context,
    node_ports: &mut HashMap<String, NodePorts>,
    fixups: &mut PostProcess,
    interfaces: &mut InterfaceRegistry,
    node_configs: &mut HashMap<String, serde_json::Value>,
) -> Result<(), CompileError> {
    // Pre-populate scope_groups: map child node_id → parent scope's group_id.
    for node in &graph.nodes {
        if let Some(ref pid) = node.parent_id {
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

    // Pre-index container children: parent_id → [child nodes]. Cheap O(n)
    // pass consumed by `lower_loop` (to reject empty Loops); ignored by
    // other lowerings today. Most lookups return an empty slice.
    let mut children_by_parent: HashMap<&str, Vec<&WorkflowNode>> = HashMap::new();
    for node in &graph.nodes {
        if let Some(ref pid) = node.parent_id {
            children_by_parent
                .entry(pid.as_str())
                .or_default()
                .push(node);
        }
    }

    // Pre-index agent tool targets: agent_id → [tool nodes]. A tool is a
    // node reached from an agent via an edge with `source_handle == "tools"`.
    // The agent compiler reads this slice to mint per-tool dispatch/collect
    // transitions; `wire_edge` skips `tools`-handled edges so they don't get
    // wired as regular sequence arcs. Missing target node ids are caught by
    // the standard graph-validation pass before we get here.
    let node_by_id: HashMap<&str, &WorkflowNode> =
        graph.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut agent_tools_by_id: HashMap<&str, Vec<&WorkflowNode>> = HashMap::new();
    for edge in &graph.edges {
        if edge.source_handle.as_deref() == Some("tools") {
            if let Some(&target) = node_by_id.get(edge.target.as_str()) {
                agent_tools_by_id
                    .entry(edge.source.as_str())
                    .or_default()
                    .push(target);
            }
        }
    }

    let empty_files: HashMap<String, InputSource> = HashMap::new();
    let empty_children: Vec<&WorkflowNode> = Vec::new();
    for ni in sorted {
        let node = *wg.full.node_weight(*ni).unwrap();
        let outgoing = wg.outgoing(&node.id);
        let incoming = wg.incoming(&node.id);
        let node_files = files.get(&node.id).unwrap_or(&empty_files);
        let children = children_by_parent
            .get(node.id.as_str())
            .unwrap_or(&empty_children);
        let agent_tools = agent_tools_by_id
            .get(node.id.as_str())
            .unwrap_or(&empty_children);
        expand_node(
            node,
            graph,
            &outgoing,
            &incoming,
            children,
            agent_tools,
            ctx,
            node_ports,
            fixups,
            node_files,
            sub_air,
            interfaces,
            &graph.definitions,
            node_configs,
            config_storage,
        )?;
    }
    Ok(())
}

/// Tag every place whose id is in `interfaces.workflow_terminals` as a
/// terminal. Per the interface contract (see `interface.rs`), `lower_end`
/// is the only lowering that populates `workflow_terminals` — there is no
/// other path. Runs after `iface.rewrite_places` so the place ids are
/// already post-alias-rewrite.
fn apply_terminal_place_types(
    scenario: &mut ScenarioDefinition,
    interfaces: &InterfaceRegistry,
) {
    let resolved_terminal_ids: std::collections::HashSet<&str> = interfaces
        .values()
        .flat_map(|i| i.workflow_terminals.iter().map(String::as_str))
        .collect();
    for place in &mut scenario.places {
        if resolved_terminal_ids.contains(place.id.as_str()) {
            place.place_type = "terminal".to_string();
        }
    }
}

/// Apply group-related post-processing: (a) push every declared
/// `ScenarioGroup` from `fixups.groups` into the scenario, then (b) tag
/// every place/transition owned by a scope-child node with its parent
/// scope's `group_id`. Step (b) walks `interfaces.owned_*` instead of
/// matching `p_{id}_*` / `t_{id}_*` prefixes — robust to nested scopes +
/// future renames inside `lower_*`. Skips places/transitions already
/// tagged so explicit per-lowering groups win.
fn apply_group_fixups(
    scenario: &mut ScenarioDefinition,
    fixups: &PostProcess,
    interfaces: &InterfaceRegistry,
) {
    for (group_id, group_name, parent_id) in &fixups.groups {
        scenario.groups.push(ScenarioGroup {
            id: group_id.clone(),
            name: group_name.clone(),
            parent_id: parent_id.clone(),
            metadata: None,
        });
    }

    for (node_id, group_id) in &fixups.scope_groups {
        let Some(iface) = interfaces.get(node_id) else {
            continue;
        };
        let owned_p: std::collections::HashSet<&str> =
            iface.owned_places.iter().map(String::as_str).collect();
        let owned_t: std::collections::HashSet<&str> =
            iface.owned_transitions.iter().map(String::as_str).collect();
        for place in &mut scenario.places {
            if owned_p.contains(place.id.as_str()) && place.group_id.is_none() {
                place.group_id = Some(group_id.clone());
            }
        }
        for transition in &mut scenario.transitions {
            if owned_t.contains(transition.id.as_str()) && transition.group_id.is_none() {
                transition.group_id = Some(group_id.clone());
            }
        }
    }
}

/// Walk the (pre-merge) scenario and credit every place to the owning node by
/// matching `p_{node_id}_*` / `t_{node_id}_*` prefixes. **The only place this
/// prefix match still lives.** Longest-prefix-wins so id-prefix collisions
/// like `"lp"` vs `"lp_inner"` don't misattribute.
fn derive_node_ownership(
    scenario: &ScenarioDefinition,
    interfaces: &mut InterfaceRegistry,
) {
    let mut by_len: Vec<String> = interfaces.keys().cloned().collect();
    by_len.sort_by_key(|b| std::cmp::Reverse(b.len()));

    // Match a Petri id back to its owning workflow node. Two naming
    // conventions are in play:
    //
    //   - `lower.rs` emits underscore-style ids directly (`t_{node}_role`,
    //     `p_{node}_role`). Decision, the wire-edge transitions, the top
    //     `to_output` / `to_error` of AutomatedStep, all look like this.
    //   - The SDK's `ctx.scoped_prefix(id, …)` joins nested ids with `/`
    //     (e.g. `extract/prepare`, `extract/t_accepted`, `extract/submitted`).
    //     The AutomatedStep `prepare` transition (which carries the
    //     `<slug>.<field>` read-arcs) and the entire `executor_lifecycle`
    //     sub-net live here. No `p_`/`t_` prefix is emitted on nested ids.
    //
    // Without the slash-form match the step-execution projector silently
    // skips owned-transition fires for AutomatedStep, so `inputs` is never
    // captured on the step row even though the read-arcs fire normally
    // at runtime.
    fn match_owner<'a>(id: &str, kind: char, by_len: &'a [String]) -> Option<&'a str> {
        for node_id in by_len {
            let prefix = match kind {
                'p' => format!("p_{node_id}_"),
                _ => format!("t_{node_id}_"),
            };
            if id.starts_with(&prefix) {
                return Some(node_id.as_str());
            }
            if id == &prefix[..prefix.len() - 1] {
                // Bare `p_{id}` / `t_{id}` (no trailing role) — counts too.
                return Some(node_id.as_str());
            }
            // SDK-style slash-nested id from `ctx.scoped_prefix(node_id, …)`.
            let slash_prefix = format!("{node_id}/");
            if id.starts_with(&slash_prefix) {
                return Some(node_id.as_str());
            }
        }
        None
    }

    for place in &scenario.places {
        if let Some(owner) = match_owner(&place.id, 'p', &by_len) {
            if let Some(iface) = interfaces.get_mut(owner) {
                if !iface.owned_places.iter().any(|p| p == &place.id) {
                    iface.owned_places.push(place.id.clone());
                }
            }
        }
    }
    for transition in &scenario.transitions {
        if let Some(owner) = match_owner(&transition.id, 't', &by_len) {
            if let Some(iface) = interfaces.get_mut(owner) {
                if !iface.owned_transitions.iter().any(|t| t == &transition.id) {
                    iface.owned_transitions.push(transition.id.clone());
                }
            }
        }
    }

    // Wire-edge transitions (`t_edge_<edge_id>`) belong to no node by the
    // prefix rule above — their ids are keyed on the edge, not either
    // endpoint. But for HumanTask consumers, `build_human_task_injection_logic`
    // runs on the wire transition (see `service/src/compiler/wire.rs:17-21`),
    // and `apply_control_data_foundation`'s (c3) phase synthesizes the
    // `<slug>.<field>` read-arcs on it. The read-arc payloads are exactly
    // the HumanTask's inputs, so credit the transition to whichever node's
    // entry the wire produces into. That puts the read_tokens on the right
    // step-execution row when the projector folds the event log.
    let mut wire_edge_owners: Vec<(String, String)> = Vec::new();
    for transition in &scenario.transitions {
        if !transition.id.starts_with("t_edge_") {
            continue;
        }
        for arc in &transition.outputs {
            for (node_id, iface) in interfaces.iter() {
                if iface.entry.as_deref() == Some(&arc.place) {
                    wire_edge_owners.push((transition.id.clone(), node_id.clone()));
                }
            }
        }
    }
    for (tid, owner) in wire_edge_owners {
        if let Some(iface) = interfaces.get_mut(&owner) {
            if !iface.owned_transitions.iter().any(|t| t == &tid) {
                iface.owned_transitions.push(tid);
            }
        }
    }
}

/// Author-visible borrow surface. For each node that authors
/// `<slug>.<attr>` references — Python AutomatedSteps (Python source) and
/// HumanTasks (`{{ }}` placeholders in title/instructions/step blocks) —
/// resolve every reference's `slug` to its upstream `producer_node_id` and
/// record the `attr` field name. The frontend's step drawer reads this to
/// show *what the step actually read* from each upstream envelope (the
/// runtime input is the whole envelope; this narrows it to the fields the
/// author asked for).
///
/// Stored on `NodeInterface.borrowed_paths` and shipped to the frontend
/// as part of `interface_json`. Decision/Loop guards are not yet covered
/// (they use a different scanner; their `<slug>.<field>` refs already
/// surface in the picker via `reachable_scope`).
fn populate_borrowed_paths(
    graph: &crate::models::template::WorkflowGraph,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    interfaces: &mut InterfaceRegistry,
) {
    use crate::compiler::human_task_refs::extract_human_task_refs;
    use crate::compiler::token_shape::slug_index;
    use crate::models::template::WorkflowNodeData;
    use std::collections::{BTreeMap, BTreeSet};

    let Ok(slugs) = slug_index(graph) else {
        return;
    };

    for node in &graph.nodes {
        // `BTreeSet` dedupes per (producer, attr); we then materialize to
        // the persisted `Vec<String>` shape.
        let mut paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        match &node.data {
            WorkflowNodeData::HumanTask { .. } => {
                for r in extract_human_task_refs(node) {
                    let Some(prod_id) = slugs.node_for(&r.head) else {
                        continue;
                    };
                    if prod_id == node.id {
                        continue;
                    }
                    paths
                        .entry(prod_id.to_string())
                        .or_default()
                        .insert(r.attr);
                }
            }
            WorkflowNodeData::AutomatedStep { execution_spec, .. } => {
                // Borrowed-path discovery runs through the backend's
                // registered `ref_scanner`. Every backend in `BACKENDS`
                // either provides a scanner (Python/SMTP/LLM/Kreuzberg/
                // FileOps) or doesn't need one (Process/Docker/HTTP/
                // CatalogueQuery — no `<slug>.<field>` surfaces, so
                // `ref_scanner: None`).
                let Some(decl) = crate::backends::lookup(execution_spec.backend_type) else {
                    continue;
                };
                let Some(scanner) = decl.ref_scanner else {
                    continue;
                };
                let ctx = crate::backends::ScanCtx {
                    config: &execution_spec.config,
                    node_id: &node.id,
                    inline_sources,
                    entrypoint: execution_spec.entrypoint.as_deref(),
                };
                for r in scanner(&ctx) {
                    let Some(prod_id) = slugs.node_for(&r.head) else {
                        continue;
                    };
                    if prod_id == node.id {
                        continue;
                    }
                    paths
                        .entry(prod_id.to_string())
                        .or_default()
                        .insert(r.attr);
                }
            }
            _ => continue,
        }

        if paths.is_empty() {
            continue;
        }
        if let Some(iface) = interfaces.get_mut(&node.id) {
            iface.borrowed_paths = paths
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().collect()))
                .collect();
        }
    }
}

/// Post-merge foundation phase. See call site (step 10). Reads exclusively
/// from `interfaces` (specifically `data_port`) — never from string-shape
/// derivations or `fixups`. Per the interface contract, every node that
/// parks a borrow-reachable envelope MUST publish `data_port` from its
/// `lower_*`.
fn apply_control_data_foundation(
    graph: &crate::models::template::WorkflowGraph,
    scenario: &mut aithericon_sdk::scenario::ScenarioDefinition,
    interfaces: &InterfaceRegistry,
    inline_sources: &HashMap<String, HashMap<String, String>>,
    known_resources: &KnownResources,
    node_configs: &mut HashMap<String, serde_json::Value>,
) -> Result<(), CompileError> {
    let report = crate::compiler::token_shape::analyze(graph)?;

    // Parked-producer nodes: those whose interface published a `data_port`.
    let parked: Vec<(&str, &str)> = interfaces
        .iter()
        .filter_map(|(id, iface)| iface.data_port.as_deref().map(|p| (id.as_str(), p)))
        .collect();

    stage_typed_definitions(scenario, &report, &parked);
    schema_split_places_and_yield(scenario, &parked);

    // Plan every borrow phase in one shot. The unified `Borrow` shape
    // (`compiler::borrow`) collapses the five formerly-separate phases —
    // Decision/Loop guards, Python AutomatedStep `<slug>.<field>`,
    // HumanTask `{{<slug>.<field>}}` placeholders, LLM and Kreuzberg
    // `{{<slug>.<field>}}` config refs — into one `Vec<Borrow>`. The
    // scanners (Python AST, HumanTask string walker, JSON-config
    // walker, Rhai AST guard walker) stay per-surface; the rewrite
    // dispatch is unified inside `apply_borrows`.
    let unified_borrows =
        crate::compiler::borrow::collect_borrows(graph, inline_sources, known_resources)?;

    validate_python_output_fields(graph, &unified_borrows)?;

    // Apply every borrow's rewrite + read-arc wiring. Handles guards,
    // Python envelope staging, HumanTask substring rewriting, and LLM /
    // Kreuzberg per-field staging in one pass. The backend arm also
    // rewrites placeholders inside `node_configs` (the parked static
    // config blobs) so the executor's `{{input:NAME}}` /
    // `{{input_path:NAME}}` resolver finds the rewritten form when it
    // downloads the blob and hands it to the backend.
    crate::compiler::borrow::apply_borrows(scenario, interfaces, unified_borrows, node_configs);

    align_decision_deadends(scenario, graph);
    fill_missing_definitions(scenario);
    hoist_join_any_data(scenario, graph, interfaces);

    Ok(())
}

/// (a) Typed definitions for every parked producer's data + control
/// token. Data = the producer's full output shape (enforced); control =
/// an open object (small, dynamic `_loop_*` keys). Also seeds the
/// `DynamicToken` definition every effect transition references.
fn stage_typed_definitions(
    scenario: &mut aithericon_sdk::scenario::ScenarioDefinition,
    report: &crate::compiler::token_shape::ShapeReport,
    parked: &[(&str, &str)],
) {
    use crate::compiler::token_shape::{ctrl_def_name, data_def_name, dynamic_token_definition};

    let (dyn_name, dyn_schema) = dynamic_token_definition();
    scenario.definitions.entry(dyn_name).or_insert(dyn_schema);
    for (node_id, _) in parked {
        if let Some(shape) = report.node_out.get(*node_id) {
            scenario
                .definitions
                .insert(data_def_name(node_id), shape.to_json_schema());
        }
        scenario.definitions.insert(
            ctrl_def_name(node_id),
            serde_json::json!({ "type": "object", "additionalProperties": true }),
        );
    }
}

/// (b) Schema the split places (parked data + ctrl) and the yield
/// transition's output ports for every parked producer. Read by the
/// runtime `SchemaRegistry`; without these refs the engine treats the
/// places as untyped pass-throughs.
fn schema_split_places_and_yield(
    scenario: &mut aithericon_sdk::scenario::ScenarioDefinition,
    parked: &[(&str, &str)],
) {
    use crate::compiler::token_shape::{ctrl_def_name, data_def_name, def_ref};

    for (node_id, data_place) in parked {
        let data_ref = def_ref(&data_def_name(node_id));
        let ctrl_ref = def_ref(&ctrl_def_name(node_id));
        let ctrl_place = format!("p_{node_id}_ctrl");
        for p in &mut scenario.places {
            if p.id == *data_place {
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
}

/// (c2-pre) Validate declared `output.fields` on every Python
/// AutomatedStep against (a) reserved runner globals and (b) slugs this
/// node actually borrows. The runner sweeps declared output names from
/// `globals()` after exec(); without these guards, a field named `token`
/// would shadow the inbound control token, and a field colliding with a
/// borrowed upstream slug would silently re-export the input as output.
/// Mirror of runner.rs `_RESERVED_GLOBALS` (executor-backend) — keep
/// both lists in sync when adding new injected globals.
fn validate_python_output_fields(
    graph: &crate::models::template::WorkflowGraph,
    unified_borrows: &[crate::compiler::borrow::Borrow],
) -> Result<(), CompileError> {
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
    let mut python_borrows_by_consumer: std::collections::HashMap<
        &str,
        Vec<&crate::compiler::borrow::Borrow>,
    > = std::collections::HashMap::new();
    for b in unified_borrows {
        if matches!(
            b.resolution,
            crate::compiler::borrow::BorrowResolution::PythonEnvelope
        ) {
            python_borrows_by_consumer
                .entry(b.consumer_node_id.as_str())
                .or_default()
                .push(b);
        }
    }
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
            if let Some(borrows) = python_borrows_by_consumer.get(node.id.as_str()) {
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
    Ok(())
}

/// (c-deadend) Decision deadend enabling-time alignment. The Decision
/// lowering emits one transition per branch + a default + an unguarded
/// `t_<dec>_deadend` whose intent is "fire only when nothing else
/// could." That priority intent breaks under the engine's selection
/// rule (`evaluation::select_next_transition`): step 1 is *earliest
/// enabling time wins*, and enabling time is the max `created_at` of all
/// *consumed + read* tokens on the binding. Because deadend reads only
/// the control-token place while branches/default also read the parked
/// `p_<producer>_data`, deadend can end up with an *earlier* enabling
/// time when the data token happens to be created after the ctrl token
/// (a non-deterministic micro-race inside the producer's yield: the
/// two are emitted from the same logic block but their `created_at`
/// stamps depend on hash iteration order). Step 1 wins outright, so
/// deadend fires even when a branch guard is true — caught live as
/// 03-decision-routing failing for `score=40` but passing for `score=10`.
///
/// Fix: mirror the read-arcs (and corresponding `input_ports`) that the
/// borrow read-arc synthesis added to a deadend's siblings onto the
/// deadend itself. The deadend's guard/logic stays unchanged (it still
/// `throw`s); the extra read-arcs only change its enabling time, so it
/// now ties with the branches/default on step 1 and loses on step 2
/// (specificity / `input_count`). Deadend's `priority(0)` is preserved
/// as the final tiebreak when read-arcs alone don't disambiguate.
fn align_decision_deadends(
    scenario: &mut aithericon_sdk::scenario::ScenarioDefinition,
    graph: &crate::models::template::WorkflowGraph,
) {
    use aithericon_sdk::scenario::{ScenarioArc, ScenarioPort};

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
                        count_from: None,
                        correlate_on: None,
                    });
                }
            }
        }
    }
}

/// (d) Safety net: any pre-existing schema ref (effect tokens,
/// `DynamicToken`) not in `definitions` gets a permissive `{}` so the
/// runtime `SchemaRegistry` resolves every ref (unresolvable refs *fail*).
fn fill_missing_definitions(scenario: &mut aithericon_sdk::scenario::ScenarioDefinition) {
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
}

/// (j) Join Any-mode data-pass-through. The `lower_join` Any branch
/// emits `#{ output: in_X, data: in_X }` so the parked data place of
/// the join carries the slim *control* token that arrived from the
/// upstream extractor — not the upstream's actual fields. A downstream
/// `<join_slug>.<field>` borrow (e.g. `extraction.fields` in the
/// doc-pipeline persist step) then hits the Python runner with an
/// envelope that has no `fields` key → `AttributeError:
/// '_AccessibleDict' object has no attribute 'fields'`.
///
/// Fix here, post-merge: for every Join Any node, walk its
/// `t_<id>_join_<i>` branch transitions. For each, resolve the upstream
/// node id (the source of the edge that this branch consumes via
/// `p_<id>_in_<i>`), wire a read-arc into the upstream's parked data
/// place, and rewrite the Rhai to use `data: <hoisted-upstream-data>`.
/// Hoisting matches the borrow planner's `producer_field_access_hoist`
/// (AutomatedStep parks under `detail.outputs`, HumanTask under `data`,
/// others flat), so the join's parked envelope mirrors the upstream
/// producer's flat field shape — exactly what `<slug>.<field>`
/// borrowers expect.
fn hoist_join_any_data(
    scenario: &mut aithericon_sdk::scenario::ScenarioDefinition,
    graph: &crate::models::template::WorkflowGraph,
    interfaces: &InterfaceRegistry,
) {
    use crate::models::template::JoinMode;

    for node in &graph.nodes {
        let WorkflowNodeData::Join { mode, .. } = &node.data else {
            continue;
        };
        if !matches!(mode, JoinMode::Any) {
            continue;
        }
        // Branch transitions are named t_{join_id}_join_{i}; each consumes
        // exactly one input place. The join's `p_{join}_in_{i}` place was
        // collapsed to the upstream's CTRL place by `apply_merges`, so
        // post-merge the consume arc points directly at
        // `p_<upstream>_ctrl`. We use that to identify the upstream
        // producer per branch — no need to walk the original edge list.
        let branch_prefix = format!("t_{}_join_", node.id);
        let mut rewrites: Vec<(String, String)> = Vec::new();
        let mut wires: Vec<(String, String)> = Vec::new(); // (transition_id, upstream_node)
        for t in &scenario.transitions {
            if !t.id.starts_with(&branch_prefix) {
                continue;
            }
            // Branch index is the suffix after `_join_` — also the
            // Rhai port name `in_<idx>`.
            let i_suffix = &t.id[branch_prefix.len()..];
            let Ok(idx) = i_suffix.parse::<usize>() else {
                continue;
            };
            // Identify upstream from the consume arc's place id, which
            // post-merge looks like `p_<upstream>_ctrl`. Falls through
            // gracefully (no rewrite) if a branch doesn't match the
            // expected shape — better to leave that branch as-is than
            // to misattribute.
            let Some(consume) = t
                .inputs
                .iter()
                .find(|a| !a.read && a.place.starts_with("p_") && a.place.ends_with("_ctrl"))
            else {
                continue;
            };
            let upstream_id = &consume.place[2..consume.place.len() - "_ctrl".len()];
            if interfaces
                .get(upstream_id)
                .and_then(|i| i.data_port.as_deref())
                .is_none()
            {
                continue;
            }
            let var = format!("d_{}", upstream_id.replace('-', "_"));
            // Hoist into the upstream's flat field namespace so a
            // downstream borrow of `<join_slug>.<field>` resolves
            // against `<field>` in the parked envelope (matches the
            // standard producer hoist: AutomatedStep → detail.outputs).
            let hoist: &[&str] = interfaces
                .get(upstream_id)
                .map(|i| i.kind.hoist_path())
                .unwrap_or(&[]);
            let pluck_segs: Vec<String> = hoist.iter().map(|s| format!("\"{s}\"")).collect();
            let data_expr = if pluck_segs.is_empty() {
                var.clone()
            } else {
                format!("__pluck({var}, [{}])", pluck_segs.join(", "))
            };
            let port_name = format!("in_{idx}");
            let new_logic = format!("#{{ output: {port_name}, data: {data_expr} }}");
            rewrites.push((t.id.clone(), new_logic));
            wires.push((t.id.clone(), upstream_id.to_string()));
        }
        // Apply: wire read-arcs and rewrite the logic. wire_read_arc
        // returns the var name (already factored into `data_expr` above).
        for (tid, upstream) in &wires {
            for t in &mut scenario.transitions {
                if &t.id != tid {
                    continue;
                }
                let _ = wire_read_arc(t, upstream, interfaces, false);
            }
        }
        for (tid, new_logic) in rewrites {
            // Preserve __pluck helper if any of the rewritten data
            // expressions reference it.
            let final_src = if new_logic.contains("__pluck(") {
                format!("{}{}", crate::compiler::rhai_gen::PLUCK_HELPER, new_logic)
            } else {
                new_logic
            };
            for t in &mut scenario.transitions {
                if t.id != tid {
                    continue;
                }
                t.logic = aithericon_sdk::scenario::TransitionLogic::Rhai {
                    source: final_src.clone(),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::pyio::generate_py_io_files;
    use crate::compiler::rhai_gen::{
        build_human_task_injection_logic, build_join_merge_logic_full, interpolate_to_rhai_expr,
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
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let places = air["places"].as_array().unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // Start forks (`park_outputs`): p_s_ready (seed) + p_s_data (write-once
        // parked copy) + p_s_main (forwarded — End's `p_e_done` merges into it).
        // End then mints its own terminal (`p_e_terminal`) + a `t_e_complete`
        // forwarder so the workflow exit is End-owned rather than the upstream's
        // ctrl-merge survivor. = 4 places (p_s_ready/p_s_data/p_s_main/p_e_terminal),
        // 2 transitions (t_s_park, t_e_complete).
        assert_eq!(places.len(), 4);
        assert_eq!(transitions.len(), 2);

        // End's own terminal carries the workflow-exit tag; p_s_main is just
        // the intermediate the forwarder consumes from. With typed ports, initial
        // tokens are NOT seeded at compile time — `parameterize_air` seeds them
        // at instance creation.
        let term = places.iter().find(|p| p["id"] == "p_e_terminal").unwrap();
        assert_eq!(term["type"], "terminal");
        let main_place = places.iter().find(|p| p["id"] == "p_s_main").unwrap();
        assert_ne!(main_place["type"], "terminal");
    }

    /// Prototype proof: the per-node interface registry is alias-stable on
    /// the exact graph the `e5ed9fc`/`674408e` SubWorkflow terminal-filter
    /// leak hit — a trivial Start→End child where End's terminal
    /// (`p_e_done`) collapses into Start's `p_s_main`. The End's interface
    /// entry stays consistent: `workflow_terminals` post-rewrite is
    /// `[p_s_main]`, NOT `[p_e_done]` (dead) or empty. That's what
    /// `publish.rs::resolve_subworkflow_air` reads as the spawn-callable
    /// reply source — no `place_type` peek, no slash-exclusion. See
    /// `service/src/compiler/interface.rs` for the registry shape.
    #[test]
    fn interface_registry_is_alias_collapse_stable() {
        use crate::compiler::interface::NodeKind;
        let graph = WorkflowGraph {
            nodes: vec![start_node("s"), end_node("e")],
            edges: vec![edge("e1", "s", "e")],
            viewport: None,
            instance_concurrency: Default::default(), definitions: Default::default(),
        };
        let files: NodeFiles = std::collections::HashMap::new();
        let inline: HashMap<String, HashMap<String, String>> = std::collections::HashMap::new();
        let (_, registry) = compile_to_scenario_and_interfaces(
            &graph,
            "test",
            "desc",
            &files,
            &inline,
            &SubWorkflowAir::new(),
            &crate::compiler::resource_refs::KnownResources::new(),
        )
        .unwrap();

        // Start: alive, kind, entry place stable, owned places include the
        // post-collapse survivor (`p_s_main`) — `p_s_ready`, `p_s_data`,
        // and `p_s_main` are the three Start-emitted places.
        let s = registry.get("s").expect("Start interface present");
        assert_eq!(s.kind, NodeKind::Start);
        assert_eq!(s.entry.as_deref(), Some("p_s_ready"));
        assert!(
            s.owned_places.iter().any(|p| p == "p_s_main"),
            "Start should own its collapsed-survivor output place: {:?}",
            s.owned_places
        );

        // End: alive, kind, workflow_terminals stable across alias rewrites.
        // The current shape: End mints its OWN `p_e_terminal` place (with a
        // `t_e_complete` forwarder) so the workflow exit is anchored on a
        // place End emits — independent of whether the upstream's `p_*_ctrl`
        // place collapsed onto `p_e_done` via the pass-through merge. This
        // closes a premature-termination class: when an Agent/AutomatedStep/
        // HumanTask feeds a bare End (no result_mapping, no Start-registered
        // process), the engine would otherwise tag the upstream's slim
        // control place `p_<upstream>_ctrl` as terminal and complete the
        // net the instant the upstream yielded, with no End-side projection.
        let e = registry.get("e").expect("End interface present");
        assert_eq!(e.kind, NodeKind::End);
        assert_eq!(
            e.workflow_terminals,
            vec!["p_e_terminal".to_string()],
            "End must own its own workflow terminal (was {:?})",
            e.workflow_terminals,
        );
        // The terminal id is NOT a slash-separated SDK lifecycle place (the
        // `!id.contains('/')` filter the original SubWorkflow reply-resolver
        // used still passes). The interface registry remains the single
        // source of truth — SubWorkflow's reply-resolver reads
        // `workflow_terminals` directly without name-pattern filtering.
        assert!(
            !e.workflow_terminals[0].contains('/'),
            "Terminal must be a workflow-exit, not an SDK lifecycle marker",
        );
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
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let places = air["places"].as_array().unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // HumanTask creates 5 places (input, active, signal, output, errors)
        // + the HT foundation split adds parked-data + slim-control = 7.
        // Start now forks too: p_s_ready + p_s_data + p_s_main = 3 → 10.
        // End mints its own terminal (`p_e_terminal`) so the workflow exit
        // is End-owned rather than the HumanTask's `p_ht_ctrl` survivor → 11.
        assert_eq!(places.len(), 11);

        // request + finalize + 1 injection edge (s->ht) + the HT yield
        // transition + Start's t_s_park + End's t_e_complete forwarder = 6
        // (ht->e merged into the control place).
        assert_eq!(transitions.len(), 6);
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
                        default_branch: Some("default".to_string()),
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
                edge_with_handle("edefault", "d", "e2", "default"),
            ],
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
        };

        let result = compile_to_air(&graph, "test", "desc", &std::collections::HashMap::new());
        assert!(result.is_ok(), "compile failed: {:?}", result.err());

        let air = result.unwrap();
        let transitions = air["transitions"].as_array().unwrap();

        // Start's t_s_park + 1 branch + 1 default + the always-emitted
        // dead-end (unroutable token -> observable net error) + one
        // `t_e*_complete` forwarder per bare End (e1, e2) = 6. The 3
        // pass-through edge transitions (s->d, d->e1, d->e2) are merged.
        assert_eq!(transitions.len(), 6);

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

    /// Regression: a graph with non-Default outputs (Decision's per-edge
    /// branches → `OutputKey::Edge("econd1")`, etc.) used to fail to
    /// serialize the interface registry to JSON because the derived
    /// serde repr of `OutputKey::Edge(...)` was an object, not a string,
    /// and `BTreeMap` requires string-shaped keys for JSON serialization.
    /// Surfaced live as `failed to serialize interfaces: key must be a
    /// string` during demo 03/04/05/invoice-processing seeding. The custom
    /// flat-string serde impl (`"default"` | `"edge:<id>"` | `"named:<id>"`)
    /// closes it.
    #[test]
    fn interface_registry_serializes_multi_output_nodes() {
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
                            guard: "true".to_string(),
                        }],
                        default_branch: Some("default".to_string()),
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
                edge_with_handle("edefault", "d", "e2", "default"),
            ],
            viewport: None,
            instance_concurrency: Default::default(), definitions: Default::default(),
        };
        let files: NodeFiles = std::collections::HashMap::new();
        let inline: HashMap<String, HashMap<String, String>> = std::collections::HashMap::new();
        let (_scn, registry) = compile_to_scenario_and_interfaces(
            &graph,
            "test",
            "desc",
            &files,
            &inline,
            &SubWorkflowAir::new(),
            &crate::compiler::resource_refs::KnownResources::new(),
        )
        .expect("compile");

        // The Decision node has Edge("econd1") and Edge("edefault") outputs —
        // both non-Default OutputKey variants. Serialization must succeed.
        let json = serde_json::to_value(&registry).expect("registry must serialize");
        let d_outputs = &json["d"]["outputs"];
        assert!(
            d_outputs.is_object(),
            "decision outputs should be an object (string keys): {d_outputs}"
        );
        let keys: Vec<&str> = d_outputs
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert!(
            keys.iter().any(|k| k.starts_with("edge:")),
            "expected at least one `edge:` key in {keys:?}",
        );

        // Round-trip: deserialize back and confirm the structural identity.
        let back: crate::compiler::interface::InterfaceRegistry =
            serde_json::from_value(json).expect("registry must round-trip");
        assert!(back.contains_key("d"));
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
        let shallow = build_join_merge_logic_full(&ports, MergeStrategy::ShallowLastWins, false);
        let deep = build_join_merge_logic_full(&ports, MergeStrategy::DeepMerge, false);
        // One branch never merges — both strategies collapse to pass-through.
        assert_eq!(shallow, "#{ output: in_0 }");
        assert_eq!(deep, "#{ output: in_0 }");
    }

    #[test]
    fn test_join_merge_strategies_differ() {
        let ports = vec!["in_0".to_string(), "in_1".to_string()];
        let shallow = build_join_merge_logic_full(&ports, MergeStrategy::ShallowLastWins, false);
        let deep = build_join_merge_logic_full(&ports, MergeStrategy::DeepMerge, false);

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
        let deep = build_join_merge_logic_full(&ports, MergeStrategy::DeepMerge, false);
        // Folds in arrival order so the last branch wins on scalar collisions.
        let i1 = deep.find("__deep_merge(result, in_1)").unwrap();
        let i2 = deep.find("__deep_merge(result, in_2)").unwrap();
        assert!(i1 < i2, "in_1 must be folded before in_2");
    }

    /// `Join { mode: Any }` with three incoming branches must lower into THREE
    /// transitions (one per branch), each consuming a dedicated `p_<id>_in_<i>`
    /// place and depositing into the *same* output and parked data places.
    /// This is the canonical petri-net XOR-join: any branch firing fires the
    /// join, no AND-wait. We also assert the parked data place
    /// (`p_merge_data`) is the published `data_port` so downstream
    /// `<slug>.<field>` borrows resolve through it.
    #[test]
    fn test_join_any_mode_emits_one_transition_per_branch() {
        // Graph: start -> {b0, b1, b2 (passthrough automated_steps)} -> merge (join any) -> end
        let start = start_node("start");
        let mk_step = |id: &str| WorkflowNode {
            id: id.to_string(),
            node_type: "automated_step".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::AutomatedStep {
                label: format!("step {id}"),
                description: None,
                execution_spec: ExecutionSpecConfig {
                    backend_type: ExecutionBackendType::Docker,
                    entrypoint: None,
                    config: serde_json::json!({"image": "alpine:latest"}),
                },
                input: Port::empty_input(),
                output: default_output_port(ExecutionBackendType::Docker),
                retry_policy: RetryPolicy {
                    max_retries: 0,
                    backoff: BackoffKind::Immediate,
                    base_delay_ms: 0,
                },
                deployment_model: Default::default(),
            },
            parent_id: None,
            width: None,
            height: None,
        };

        let join_node = WorkflowNode {
            id: "merge".to_string(),
            node_type: "join".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Join {
                label: "Merge".to_string(),
                description: None,
                mode: JoinMode::Any,
                merge_strategy: None,
                output: Port {
                    id: "out".to_string(),
                    label: "Output".to_string(),
                    fields: vec![],
                },
            },
            parent_id: None,
            width: None,
            height: None,
        };

        let graph = WorkflowGraph {
            nodes: vec![
                start,
                mk_step("b0"),
                mk_step("b1"),
                mk_step("b2"),
                join_node,
                end_node("done"),
            ],
            edges: vec![
                edge("e_s_b0", "start", "b0"),
                edge("e_s_b1", "start", "b1"),
                edge("e_s_b2", "start", "b2"),
                edge("e_b0_m", "b0", "merge"),
                edge("e_b1_m", "b1", "merge"),
                edge("e_b2_m", "b2", "merge"),
                edge("e_m_done", "merge", "done"),
            ],
            viewport: None,
            instance_concurrency: Default::default(), definitions: Default::default(),
        };

        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("Join { mode: Any } must compile");
        let s = air.to_string();

        // Three branch transitions, one per incoming edge.
        assert!(
            s.contains("t_merge_join_0") && s.contains("t_merge_join_1") && s.contains("t_merge_join_2"),
            "expected three per-branch transitions, got: {s}"
        );
        // No single AND-fire transition — that's the All-mode shape.
        assert!(
            !s.contains("\"t_merge_join\""),
            "Any-mode must not emit the All-mode aggregator transition"
        );

        // Each branch transition must deposit into the SHARED output + data
        // places. That's the XOR-join's defining property — N → 1.
        for i in 0..3 {
            assert!(
                s.contains(&format!("t_merge_join_{i}")),
                "missing branch {i} transition"
            );
        }
        assert!(s.contains("p_merge_output"), "missing shared output place");
        assert!(s.contains("p_merge_data"), "missing shared parked data place");
    }

    /// `Join { mode: All }` with two branches must lower into a single AND-fire
    /// transition consuming both input places and staging the merged token in
    /// the parked `p_<id>_data` place so downstream `<slug>.<field>` borrows
    /// resolve.
    #[test]
    fn test_join_all_mode_emits_single_and_fire() {
        let join_node = WorkflowNode {
            id: "j".to_string(),
            node_type: "join".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Join {
                label: "J".to_string(),
                description: None,
                mode: JoinMode::All,
                merge_strategy: Some(MergeStrategy::ShallowLastWins),
                output: Port {
                    id: "out".to_string(),
                    label: "Output".to_string(),
                    fields: vec![],
                },
            },
            parent_id: None,
            width: None,
            height: None,
        };
        let mk_step = |id: &str| WorkflowNode {
            id: id.to_string(),
            node_type: "automated_step".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::AutomatedStep {
                label: id.to_string(),
                description: None,
                execution_spec: ExecutionSpecConfig {
                    backend_type: ExecutionBackendType::Docker,
                    entrypoint: None,
                    config: serde_json::json!({"image": "alpine:latest"}),
                },
                input: Port::empty_input(),
                output: default_output_port(ExecutionBackendType::Docker),
                retry_policy: RetryPolicy {
                    max_retries: 0,
                    backoff: BackoffKind::Immediate,
                    base_delay_ms: 0,
                },
                deployment_model: Default::default(),
            },
            parent_id: None,
            width: None,
            height: None,
        };
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                mk_step("a"),
                mk_step("b"),
                join_node,
                end_node("e"),
            ],
            edges: vec![
                edge("e_s_a", "s", "a"),
                edge("e_s_b", "s", "b"),
                edge("e_a_j", "a", "j"),
                edge("e_b_j", "b", "j"),
                edge("e_j_e", "j", "e"),
            ],
            viewport: None,
            instance_concurrency: Default::default(), definitions: Default::default(),
        };

        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("Join { mode: All } must compile");
        let s = air.to_string();

        // Single AND-fire transition, not N branch transitions.
        assert!(s.contains("t_j_join"), "All-mode must keep the single aggregator transition");
        assert!(!s.contains("t_j_join_0"), "All-mode must not emit per-branch transitions");

        // Merged token still drops at the shared output AND the parked data
        // place (so `<slug>.<field>` borrows can resolve through `p_j_data`).
        assert!(s.contains("p_j_output"));
        assert!(s.contains("p_j_data"));
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
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
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

    /// SMTP AutomatedStep that references `{{ intake.email }}` /
    /// `{{ intake.name }}` in its Tera templates emits a scenario with the
    /// same `(read-arc, d_<producer>, job_inputs.push)` triplet Python uses
    /// — proving the placeholder scanner is wired identically into the
    /// borrow planner. The discriminator vs Python: SMTP's template
    /// sources live inline on `execution_spec.config`, not in node files.
    #[test]
    fn smtp_step_with_template_refs_wires_into_scenario() {
        use serde_json::json;
        use std::collections::HashMap;
        use aithericon_executor_domain::InputSource;

        // Start → intake (HumanTask, slug "intake") → send (SMTP) → end.
        let graph_json = json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"intake","type":"human_task","slug":"intake",
             "position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Intake","taskTitle":"Intake",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"name","label":"Name","kind":"text","required":true}},
                       {"type":"input","field":{"name":"email","label":"Email","kind":"text","required":true}}
                     ]}]}},
            {"id":"send","type":"automated_step","slug":"send",
             "position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Send",
                     "executionSpec":{
                       "backendType":"smtp",
                       "config":{
                         "to":["{{ intake.email }}"],
                         "subject":{"label":"subject.tera","source":"Welcome, {{ intake.name }}!"},
                         "body_text":{"label":"body.txt.tera","source":"Hi {{ intake.name }}.\n"},
                         "resource_alias":"mail"
                       }
                     },
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"intake","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"intake","target":"send","targetHandle":"in","type":"sequence"},
            {"id":"e3","source":"send","target":"end","targetHandle":"in","type":"sequence"}
          ]
        });
        let graph: WorkflowGraph =
            serde_json::from_value(graph_json).expect("graph deser");

        // SMTP doesn't read node files for templates — they're inline on
        // the config — but the compile API requires a `files` map. Empty
        // works because there are no entrypoints to stage.
        let files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();

        let scenario = crate::compiler::compile_to_scenario(
            &graph,
            "smtp-borrow-test",
            "test",
            &files,
            &crate::compiler::SubWorkflowAir::new(),
        )
        .expect("compile smtp graph with template borrows");

        let prepare = scenario
            .transitions
            .iter()
            .find(|t| t.id == "send/prepare")
            .expect("prepare transition for SMTP step");

        // (1) The read-arc port + arc landed on the prepare transition.
        assert!(
            prepare.input_ports.iter().any(|p| p.name == "d_intake"),
            "d_intake input port missing on prepare; got: {:?}",
            prepare.input_ports
        );
        assert!(
            prepare
                .inputs
                .iter()
                .any(|a| a.place == "p_intake_data" && a.read),
            "read-arc into p_intake_data missing; got: {:?}",
            prepare.inputs
        );

        // (2) The prepare Rhai stages `intake.json` so the runner's Tera
        // context picker sees it under the slug name. Same convention as
        // Python's _AccessibleDict.
        match &prepare.logic {
            aithericon_sdk::scenario::TransitionLogic::Rhai { source } => {
                assert!(
                    !source.contains("__BORROWED_INPUTS__"),
                    "marker should be substituted; source: {source}"
                );
                assert!(
                    source.contains(r#""name": "intake.json""#),
                    "prepare must stage intake.json; source: {source}"
                );
                assert!(
                    source.contains("backend"),
                    "spec must carry a backend discriminator; source: {source}"
                );
                assert!(
                    source.contains(r#""smtp""#),
                    "backend discriminator must be 'smtp'; source: {source}"
                );
            }
            other => panic!("expected Rhai logic, got {other:?}"),
        }
    }

    /// SMTP AutomatedStep with a resource binding (`resource_alias: "mail"`,
    /// templates referencing `{{ mail.from_address }}`) compiles into an
    /// AIR whose prepare transition stages `mail.json` from the
    /// `__resources` map. Distinct from the upstream-producer-borrow test
    /// above: this one uses the publish-path entry point that accepts
    /// `KnownResources`, which is what the live `apply_template` calls.
    /// Without this test the production failure mode
    /// ("resource 'mail' not staged as <alias>.json") slipped past CI.
    #[test]
    fn smtp_step_with_resource_alias_stages_resource_envelope() {
        use crate::compiler::resource_refs::{KnownResource, KnownResources};
        use serde_json::json;
        use std::collections::HashMap;
        use uuid::Uuid;

        // Same graph shape as the earlier SMTP test, with the template
        // also referencing the resource's public field so the borrow
        // plan has both `intake` (producer) and `mail` (resource) to hit.
        let graph_json = json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"intake","type":"human_task","slug":"intake",
             "position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Intake","taskTitle":"Intake",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"name","label":"Name","kind":"text","required":true}},
                       {"type":"input","field":{"name":"email","label":"Email","kind":"text","required":true}}
                     ]}]}},
            {"id":"send","type":"automated_step","slug":"send",
             "position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Send",
                     "executionSpec":{
                       "backendType":"smtp",
                       "config":{
                         "to":["{{ intake.email }}"],
                         "subject":{"label":"subject.tera","source":"Welcome, {{ intake.name }}!"},
                         "body_text":{"label":"body.txt.tera","source":"From {{ mail.from_address }}\n"},
                         "resource_alias":"mail"
                       }
                     },
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"intake","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"intake","target":"send","targetHandle":"in","type":"sequence"},
            {"id":"e3","source":"send","target":"end","targetHandle":"in","type":"sequence"}
          ]
        });
        let graph: WorkflowGraph =
            serde_json::from_value(graph_json).expect("graph deser");

        let files: HashMap<String, HashMap<String, InputSource>> = HashMap::new();
        let inline_sources: HashMap<String, HashMap<String, String>> = HashMap::new();

        // KnownResources is what the publish handler's
        // `discover_known_resources` produces: head ID → workspace
        // resource pin. Without `mail` in this map, the borrow plan
        // silently emits nothing — exactly the production failure.
        let mut known_resources = KnownResources::new();
        known_resources.insert(
            "mail".to_string(),
            KnownResource {
                id: Uuid::new_v4(),
                type_name: "smtp".to_string(),
                latest_version: 1,
            },
        );

        let (air, _iface) = crate::compiler::compile_to_air_with_subworkflows_and_interfaces(
            &graph,
            "smtp-resource-test",
            "test",
            &files,
            &inline_sources,
            &crate::compiler::SubWorkflowAir::new(),
            &known_resources,
        )
        .expect("compile must succeed with known_resources");

        // Look at the send/prepare transition specifically. If the resource
        // borrow was emitted, its Rhai source contains the `mail.json`
        // job_inputs.push snippet.
        let transitions = air.get("transitions").and_then(|t| t.as_array()).expect("transitions array");
        let send_prepare = transitions
            .iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("send/prepare"))
            .expect("send/prepare transition exists");
        let logic_node = send_prepare.get("logic").expect("send/prepare has logic field");
        // Two possible shapes: { "Rhai": {"source": "..."} } (utoipa-tagged)
        // or { "type": "rhai", "source": "..." } (serde flat-tag). Match either.
        let logic = logic_node
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .and_then(|s| s.as_str())
            .or_else(|| logic_node.get("source").and_then(|s| s.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "send/prepare logic source not findable; raw shape:\n{}",
                    serde_json::to_string_pretty(logic_node).unwrap()
                )
            });

        assert!(
            logic.contains("mail.json"),
            "send/prepare transition must stage mail.json from __resources;\n\
             actual logic source:\n{logic}"
        );
        assert!(
            logic.contains("__resources[\"mail\"]") || logic.contains("__resources['mail']"),
            "send/prepare logic must read from __resources[\"mail\"]; got:\n{logic}"
        );
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
            .parse_json(d_review_json.to_string(), true)
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

    /// The step-execution projector keys input/output attribution off the
    /// node interface's `owned_transitions` list. AutomatedStep's `prepare`
    /// transition (which holds the `<slug>.<field>` read-arcs) lives under
    /// the SDK's `ctx.scoped_prefix({id}, …)`, so its actual id is
    /// `{id}/prepare` — slash-separated, not the `t_{id}_prepare`
    /// underscore form. The HumanTask `t_edge_<edge_id>` wire transition
    /// (which `build_human_task_injection_logic` runs on and is rewritten
    /// with read-arcs by phase (c3)) is keyed on the edge id and has no
    /// node prefix at all. Both used to slip past `derive_node_ownership`,
    /// leaving the projector unable to credit their read_tokens to the
    /// step row — so the workflow-projection drawer showed empty Inputs
    /// for HumanTask and AutomatedStep even when the run consumed inputs
    /// correctly.
    #[test]
    fn ownership_credits_scoped_prepare_and_wire_edge_to_consumer_node() {
        use aithericon_executor_domain::InputSource;
        use serde_json::json;
        use std::collections::HashMap;

        // Same invoice-shaped graph as `human_task_borrow_stages_…` —
        // a HumanTask that the AutomatedStep borrows from.
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

        let inline_sources: HashMap<String, HashMap<String, String>> = HashMap::new();
        let (_scenario, interfaces) = compile_to_scenario_and_interfaces(
            &graph,
            "ownership-test",
            "test",
            &files,
            &inline_sources,
            &crate::compiler::SubWorkflowAir::new(),
            &crate::compiler::resource_refs::KnownResources::new(),
        )
        .expect("compile");

        let extract = interfaces.get("extract").expect("extract interface");
        assert!(
            extract
                .owned_transitions
                .iter()
                .any(|t| t == "extract/prepare"),
            "extract should own its slash-nested `prepare` transition; got: {:?}",
            extract.owned_transitions
        );

        let review = interfaces.get("review").expect("review interface");
        assert!(
            review.owned_transitions.iter().any(|t| t.starts_with("t_edge_")),
            "review should own the `t_edge_*` wire transition that feeds its entry place (the wire holds the `<slug>.<field>` read-arcs synthesized for HumanTask injection); got: {:?}",
            review.owned_transitions
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
            instance_concurrency: Default::default(), definitions: Default::default(),
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
            viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
        };
        assert!(
            compile_to_air(&graph, "t", "d", &std::collections::HashMap::new()).is_ok(),
            "step without an error edge must still compile"
        );
    }

    /// `lower_engine_effect` parity: a CatalogueQuery AutomatedStep
    /// (Phase 2.e — first non-executor backend) must lower to a Petri
    /// transition that fires the engine's `catalogue_lookup` builtin
    /// effect, NOT an executor job. The registry-first dispatch in
    /// `lower_automated_step` reads the handler ID from the backend
    /// decl's `DispatchMode::EngineEffect { handler }` — this test
    /// covers the legacy → registry refactor and serves as a guardrail
    /// for future engine-effect backends. Asserts the AIR contains the
    /// effect handler invocation and the canonical `q_build`/`lookup`
    /// transition pair, with the query token re-serialized through
    /// `CatalogueQueryConfig`.
    #[test]
    fn catalogue_query_lowers_via_engine_effect() {
        let cq_node = WorkflowNode {
            id: "q".to_string(),
            node_type: "automated_step".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 50.0 },
            data: WorkflowNodeData::AutomatedStep {
                label: "Lookup".to_string(),
                description: None,
                execution_spec: ExecutionSpecConfig {
                    backend_type: ExecutionBackendType::CatalogueQuery,
                    entrypoint: None,
                    config: serde_json::json!({
                        "category": "invoice",
                        "limit": 10,
                    }),
                },
                input: Port::empty_input(),
                output: default_output_port(ExecutionBackendType::CatalogueQuery),
                retry_policy: RetryPolicy::default(),
                deployment_model: Default::default(),
            },
            parent_id: None,
            width: None,
            height: None,
        };
        let graph = WorkflowGraph {
            nodes: vec![start_node("s"), cq_node, end_node("e")],
            edges: vec![edge("e1", "s", "q"), edge("e2", "q", "e")],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
        };

        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("catalogue_query graph compiles");
        let transitions = air["transitions"].as_array().unwrap();

        // The lookup transition fires the catalogue_lookup builtin effect.
        let lookup = transitions
            .iter()
            .find(|t| t["id"] == "t_q_lookup")
            .expect("t_q_lookup transition emitted by lower_engine_effect");
        let blob = serde_json::to_string(lookup).unwrap();
        assert!(
            blob.contains("catalogue_lookup"),
            "lookup transition must invoke the catalogue_lookup effect handler: {blob}"
        );

        // The q_build transition stages the validated query token. We
        // re-serialize CatalogueQueryConfig in validate, which strips
        // `None` options via `skip_serializing_if` — so neither `page`
        // nor `filters` should appear in the inlined Rhai literal.
        let q_build = transitions
            .iter()
            .find(|t| t["id"] == "t_q_q_build")
            .expect("t_q_q_build transition emitted by lower_engine_effect");
        let logic = q_build["logic"]["source"].as_str().unwrap_or_default();
        assert!(
            logic.contains("\"category\""),
            "q_build logic must inline the category field: {logic}"
        );
        assert!(
            logic.contains("\"limit\""),
            "q_build logic must inline the limit field: {logic}"
        );
        assert!(
            !logic.contains("\"page\""),
            "stripped None options must not appear in the token literal: {logic}"
        );

        // Intermediate / output places use the descriptor's port names.
        // (Input/output places may be alias-collapsed by the compile
        // pipeline; the `p_q_query` intermediate sits between the two
        // engine-effect transitions and is never collapsed.)
        let places = air["places"].as_array().unwrap();
        let place_ids: Vec<&str> = places
            .iter()
            .map(|p| p["id"].as_str().unwrap_or_default())
            .collect();
        assert!(
            place_ids.contains(&"p_q_query"),
            "missing engine-effect intermediate place p_q_query (places: {place_ids:?})"
        );

        // CRITICAL: catalogue_query must NOT lower to an executor
        // submit. The legacy executor lifecycle would emit `submitted`
        // / `completed` / `failed` lifecycle places on a `q/`-prefixed
        // scope; the engine-effect path emits none of them.
        for tid in [
            "t_q/submitted",
            "t_q/completed",
            "t_q/failed",
            "t_q/exec_submit",
        ] {
            assert!(
                !transitions.iter().any(|t| t["id"] == tid),
                "engine-effect lowering must NOT emit executor lifecycle transition {tid}"
            );
        }
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
            instance_concurrency: Default::default(), definitions: Default::default(),
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
            instance_concurrency: Default::default(), definitions: Default::default(),
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
            instance_concurrency: Default::default(), definitions: Default::default(),
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
    /// LLM AutomatedStep with `{{<slug>.<field>}}` borrows in its prompt
    /// gets rewritten by c4: per-borrow `job_inputs.push` against the
    /// producer's read-arc'd envelope + the prompt's placeholder strings
    /// rewritten to `{{input:__borrow_*}}` form the resolver knows.
    fn llm_prompt_borrow_rewrites_placeholders_and_stages_field() {
        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"review","type":"human_task","slug":"review","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"Review","taskTitle":"R",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"vendor_name","label":"V","kind":"text","required":true}}
                     ]}]}},
            {"id":"classify","type":"automated_step","slug":"classify","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Classify",
                     "executionSpec":{"backendType":"llm","config":{
                        "provider":"openai","model":"gpt-4o-mini",
                        "prompt":"Vendor: {{ review.vendor_name }} — classify"
                     }},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"review","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"review","target":"classify","targetHandle":"in","type":"sequence"},
            {"id":"e3","source":"classify","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }"#;
        let graph: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        let (scenario, _interfaces, node_configs) =
            super::compile_to_scenario_and_interfaces_with_configs(
                &graph,
                "llm-borrow-test",
                "",
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                &crate::compiler::SubWorkflowAir::new(),
                &crate::compiler::resource_refs::KnownResources::new(),
                ConfigStorage::ephemeral(),
            )
            .expect("compile llm-borrow graph");

        let prepare = scenario
            .transitions
            .iter()
            .find(|t| t.id == "classify/prepare")
            .expect("classify prepare transition exists");

        let source = match &prepare.logic {
            aithericon_sdk::scenario::TransitionLogic::Rhai { source } => source,
            other => panic!("expected Rhai logic, got {other:?}"),
        };

        // (1) The c4 phase replaced the BORROW_MARKER with a job_inputs.push.
        assert!(
            !source.contains("__BORROWED_INPUTS__"),
            "marker should be substituted; source: {source}"
        );
        assert!(
            source.contains(r#""name": "__borrow_review__vendor_name""#),
            "prepare must stage __borrow_review__vendor_name; source: {source}"
        );

        // (2) The original placeholder `{{ review.vendor_name }}` in the
        //     embedded config got rewritten in the side-channel blob the
        //     publish layer uploads. The Rhai source now only carries the
        //     `config_ref` — the actual rewritten string lives in
        //     `node_configs["classify"]`.
        let cfg = node_configs
            .get("classify")
            .expect("classify must have parked config");
        let cfg_str = cfg.to_string();
        assert!(
            cfg_str.contains("{{input:__borrow_review__vendor_name}}"),
            "side-channel prompt must rewrite to {{input:__borrow_*}}; got: {cfg_str}"
        );
        assert!(
            !cfg_str.contains("{{ review.vendor_name }}"),
            "the original slug.field placeholder should be gone from side-channel; got: {cfg_str}"
        );
        // And the Rhai source must NOT carry the rewritten form either —
        // the only inline placeholder left is the executor-resolver call
        // against the staged file, which lives in the parked blob.
        assert!(
            !source.contains("{{input:__borrow_review__vendor_name}}"),
            "rewritten placeholder must not appear in Rhai (it's in the side-channel now): {source}"
        );

        // (3) The read-arc port + arc landed on prepare.
        assert!(
            prepare.input_ports.iter().any(|p| p.name == "d_review"),
            "d_review input port missing; got: {:?}",
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
    }

    /// Kreuzberg with an upstream file ref stages StoragePath against the
    /// HumanTask's parked FileRef.url, and the `file:` placeholder in the
    /// config becomes `{{input_path:__borrow_*}}` for the resolver.
    #[test]
    fn kreuzberg_upstream_file_ref_rewrites_to_input_path() {
        let json = r#"{
          "nodes":[
            {"id":"start","type":"start","slug":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"uploader","type":"human_task","slug":"uploader","position":{"x":0,"y":0},
             "data":{"type":"human_task","label":"U","taskTitle":"U",
                     "steps":[{"id":"s","title":"S","blocks":[
                       {"type":"input","field":{"name":"pdf","label":"P","kind":"file","required":true}}
                     ]}]}},
            {"id":"ocr","type":"automated_step","slug":"ocr","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"OCR",
                     "executionSpec":{"backendType":"kreuzberg","config":{"file":"{{ uploader.pdf }}"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"inline"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"uploader","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"uploader","target":"ocr","targetHandle":"in","type":"sequence"},
            {"id":"e3","source":"ocr","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }"#;
        let graph: WorkflowGraph = serde_json::from_str(json).expect("deser graph");
        let (scenario, _interfaces, node_configs) =
            super::compile_to_scenario_and_interfaces_with_configs(
                &graph,
                "kz-borrow-test",
                "",
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                &crate::compiler::SubWorkflowAir::new(),
                &crate::compiler::resource_refs::KnownResources::new(),
                ConfigStorage::ephemeral(),
            )
            .expect("compile kreuzberg-borrow graph");

        let prepare = scenario
            .transitions
            .iter()
            .find(|t| t.id == "ocr/prepare")
            .expect("ocr prepare transition exists");
        let source = match &prepare.logic {
            aithericon_sdk::scenario::TransitionLogic::Rhai { source } => source,
            other => panic!("expected Rhai logic, got {other:?}"),
        };

        // (1) Per-borrow stage with type=storage_path because the field is FieldKind::File.
        assert!(
            source.contains(r#""type": "storage_path""#),
            "File-kind producer must stage StoragePath; source: {source}"
        );
        assert!(
            source.contains(r#""name": "__borrow_uploader__pdf""#),
            "Kreuzberg must stage __borrow_uploader__pdf; source: {source}"
        );

        // (2) Config rewrite: `{{ uploader.pdf }}` → `{{input_path:__borrow_uploader__pdf}}`
        //     now lands in the parked side-channel blob (the publish
        //     uploader writes it to S3) instead of the Rhai literal.
        let cfg = node_configs
            .get("ocr")
            .expect("ocr must have parked config");
        let cfg_str = cfg.to_string();
        assert!(
            cfg_str.contains("{{input_path:__borrow_uploader__pdf}}"),
            "side-channel `file:` must rewrite to {{input_path:...}}; got: {cfg_str}"
        );
        assert!(
            !cfg_str.contains("{{ uploader.pdf }}"),
            "the original placeholder should be gone from side-channel; got: {cfg_str}"
        );

        // (3) Read-arc landed.
        assert!(
            prepare.input_ports.iter().any(|p| p.name == "d_uploader"),
            "d_uploader input port missing; got: {:?}",
            prepare.input_ports
        );
    }

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
            instance_concurrency: Default::default(), definitions: Default::default(),
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
                input_contract: Port::empty_input(),
                output_contract: Port::empty_input(),
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
                        default_branch: Some("default".to_string()),
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
                    source_handle: Some("default".to_string()),
                    target_handle: Some("in".to_string()),
                    label: None,
                    edge_type: "conditional".to_string(),
                },
            ],
            viewport: None,
            instance_concurrency: Default::default(), definitions: Default::default(),
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

    /// End-to-end proof that `WorkflowGraph.definitions` is inlined at
    /// lowering: an `automated_step` whose LLM `response_format.schema` is a
    /// `$ref` to a workflow-scoped definition is compiled to AIR that
    /// contains the resolved schema and no surviving `"$ref"` token.
    #[test]
    fn lowering_inlines_workflow_definitions_into_llm_config() {
        let mut definitions = std::collections::BTreeMap::new();
        definitions.insert(
            "ExtractionFields".to_string(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "fields": { "type": "array", "items": { "type": "object" } }
                }
            }),
        );
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                WorkflowNode {
                    id: "extract".to_string(),
                    node_type: "automated_step".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::AutomatedStep {
                        label: "Extract".to_string(),
                        description: None,
                        execution_spec: ExecutionSpecConfig {
                            backend_type: ExecutionBackendType::Llm,
                            entrypoint: None,
                            config: serde_json::json!({
                                "provider": "openai",
                                "model": "mlx-community/Qwen3.5-9B-MLX-4bit",
                                "base_url": "http://localhost:8000",
                                "prompt": "extract",
                                "response_format": {
                                    "type": "json_schema",
                                    "schema": { "$ref": "#/definitions/ExtractionFields" }
                                }
                            }),
                        },
                        input: Port::empty_input(),
                        output: default_output_port(ExecutionBackendType::Llm),
                        retry_policy: RetryPolicy::default(),
                        deployment_model: Default::default(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                end_node("e"),
            ],
            edges: vec![edge("e1", "s", "extract"), edge("e2", "extract", "e")],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions,
        };
        let air = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect("compile with schema ref should succeed");
        let s = air.to_string();
        // The AIR naturally contains many `$ref` tokens — every schemars-
        // generated petri-domain type schema uses internal `#/definitions/...`
        // refs for its type registry (Ctrl__, Data__, DynamicToken, …). Those
        // are NOT what this test is about. What we care about: the *specific*
        // workflow-level definition name we authored must not survive
        // anywhere as a `$ref` payload — every consumer that referenced
        // `ExtractionFields` should now carry the inlined object literal.
        assert!(
            !s.contains("#/definitions/ExtractionFields"),
            "workflow-level $ref to ExtractionFields must be inlined; got: {s}"
        );
        assert!(
            !s.contains("ExtractionFields"),
            "definition name should not appear anywhere in lowered AIR; got: {s}"
        );
    }

    /// Unknown `$ref` surfaces a `SchemaRefUnresolved` compile error tagged
    /// with the offending node id.
    #[test]
    fn lowering_unknown_schema_ref_errors_with_node_id() {
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                WorkflowNode {
                    id: "extract".to_string(),
                    node_type: "automated_step".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::AutomatedStep {
                        label: "Extract".to_string(),
                        description: None,
                        execution_spec: ExecutionSpecConfig {
                            backend_type: ExecutionBackendType::Llm,
                            entrypoint: None,
                            config: serde_json::json!({
                                "provider": "openai",
                                "model": "x",
                                "prompt": "p",
                                "response_format": {
                                    "type": "json_schema",
                                    "schema": { "$ref": "#/definitions/Missing" }
                                }
                            }),
                        },
                        input: Port::empty_input(),
                        output: default_output_port(ExecutionBackendType::Llm),
                        retry_policy: RetryPolicy::default(),
                        deployment_model: Default::default(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                end_node("e"),
            ],
            edges: vec![edge("e1", "s", "extract"), edge("e2", "extract", "e")],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: std::collections::BTreeMap::new(),
        };
        let err = compile_to_air(&graph, "t", "d", &std::collections::HashMap::new())
            .expect_err("unknown $ref must fail compilation");
        match err {
            CompileError::SchemaRefUnresolved { node_id, message, .. } => {
                assert_eq!(node_id, "extract");
                assert!(
                    message.contains("Missing"),
                    "error message should mention the unknown definition name: {message}"
                );
            }
            other => panic!("expected SchemaRefUnresolved, got {other:?}"),
        }
    }

    /// Regression: an LLM `AutomatedStep` carrying a deeply-nested
    /// `response_format.schema` used to blow Rhai's expression-complexity
    /// limit when the compiler inlined the full schema into the prepare
    /// transition's Rhai literal. The offload to `config_ref` + S3 means
    /// the Rhai script is now a fixed-size envelope referencing the blob
    /// by storage key — independent of schema depth.
    ///
    /// Asserts:
    ///   1. compile succeeds (no Rhai panic);
    ///   2. the lowered prepare-transition Rhai is small (well under the
    ///      old multi-KB inline literal) and carries `config_ref` instead
    ///      of `config`;
    ///   3. the side-channel `node_configs` carries the resolved blob with
    ///      the expanded schema (so publish actually has something to
    ///      upload).
    #[test]
    fn deeply_nested_llm_config_lowers_via_config_ref_not_literal() {
        let mut definitions = std::collections::BTreeMap::new();
        // Deliberately deep + array-heavy — emulates the failing
        // `demos/document-pipeline-v1` `ExtractionFields` shape that
        // tripped the Rhai parser before this fix.
        definitions.insert(
            "ExtractionFields".to_string(),
            serde_json::json!({
                "type": "object",
                "required": ["fields"],
                "properties": {
                    "fields": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["key", "value", "confidence", "citations"],
                            "properties": {
                                "key": { "type": "string" },
                                "value": { "type": "string" },
                                "unit": { "type": ["string", "null"] },
                                "reference_range": { "type": ["string", "null"] },
                                "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                                "citations": {
                                    "type": "array",
                                    "minItems": 1,
                                    "items": {
                                        "type": "object",
                                        "required": ["kind", "supporting_text"],
                                        "properties": {
                                            "kind": { "type": "string", "enum": ["ocr_span"] },
                                            "supporting_text": { "type": "string" },
                                            "confidence": { "type": "number", "minimum": 0, "maximum": 1 }
                                        },
                                        "additionalProperties": false
                                    }
                                }
                            },
                            "additionalProperties": false
                        }
                    }
                },
                "additionalProperties": false
            }),
        );
        let graph = WorkflowGraph {
            nodes: vec![
                start_node("s"),
                WorkflowNode {
                    id: "extract".to_string(),
                    node_type: "automated_step".to_string(),
                    slug: None,
                    position: Position { x: 0.0, y: 0.0 },
                    data: WorkflowNodeData::AutomatedStep {
                        label: "Extract".to_string(),
                        description: None,
                        execution_spec: ExecutionSpecConfig {
                            backend_type: ExecutionBackendType::Llm,
                            entrypoint: None,
                            config: serde_json::json!({
                                "provider": "openai",
                                "model": "x",
                                "prompt": "extract",
                                "response_format": {
                                    "type": "json_schema",
                                    "schema": { "$ref": "#/definitions/ExtractionFields" }
                                }
                            }),
                        },
                        input: Port::empty_input(),
                        output: default_output_port(ExecutionBackendType::Llm),
                        retry_policy: RetryPolicy::default(),
                        deployment_model: Default::default(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                end_node("e"),
            ],
            edges: vec![edge("e1", "s", "extract"), edge("e2", "extract", "e")],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions,
        };
        let template_id = uuid::Uuid::new_v4();
        let config_storage = ConfigStorage { template_id, version: 1, key_fn: None };
        let (scenario, _interfaces, node_configs) =
            compile_to_scenario_and_interfaces_with_configs(
                &graph,
                "t",
                "d",
                &std::collections::HashMap::new(),
                &std::collections::HashMap::new(),
                &crate::compiler::SubWorkflowAir::new(),
                &crate::compiler::resource_refs::KnownResources::new(),
                config_storage,
            )
            .expect("compile must succeed even with deeply-nested response_format schema");

        // 1. Side-channel carries the resolved (`$ref`-inlined) config.
        let cfg = node_configs
            .get("extract")
            .expect("extract node config must be parked in side-channel");
        let cfg_str = cfg.to_string();
        assert!(
            cfg_str.contains("ocr_span"),
            "side-channel blob must carry the inlined schema details: {cfg_str}"
        );
        assert!(
            !cfg_str.contains("\"$ref\""),
            "$ref must have been inlined before parking: {cfg_str}"
        );

        // 2. The prepare-transition Rhai is now a tiny envelope.
        let prepare = scenario
            .transitions
            .iter()
            .find(|t| t.id == "extract/prepare")
            .expect("extract/prepare transition must exist");
        let logic = match &prepare.logic {
            aithericon_sdk::scenario::TransitionLogic::Rhai { source } => source.clone(),
            other => panic!("expected rhai logic, got {other:?}"),
        };
        assert!(
            logic.contains("config_ref"),
            "prepare Rhai must carry `config_ref`, got: {logic}"
        );
        assert!(
            !logic.contains("ocr_span"),
            "prepare Rhai must NOT inline the schema content; got: {logic}"
        );
        assert!(
            logic.contains(&template_id.to_string()),
            "prepare Rhai must embed the template_id-scoped storage key; got: {logic}"
        );
        assert!(
            logic.len() < 4096,
            "prepare Rhai must be small (< 4KB) after offload; got len={} script={logic}",
            logic.len()
        );
    }
}
