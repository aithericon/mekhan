//! Per-node lowering: each workflow node type expands into its Petri
//! places/transitions via the [`NodeLowering`] trait. [`expand_node`] is the
//! thin dispatch; the real work lives in one `lower_*` function per variant.

// Re-exported with `pub(super)` so per-variant submodules can `use super::*`
// and pick up the full lowering toolbelt (types, helpers, SDK glue) without
// maintaining their own import headers. This is the single import authority
// for the whole `lower/` module tree.
pub(super) use crate::compiler::compile::SubWorkflowAir;
pub(super) use crate::compiler::error::CompileError;
pub(super) use crate::compiler::interface::{InterfaceRegistry, NodeInterface, NodeKind, OutputKey};
pub(super) use crate::compiler::well_known;
pub(super) use crate::compiler::rhai_gen::{
    build_join_merge_logic, build_join_merge_logic_full, build_join_passthrough_logic,
    build_merge_logic, build_retry_topology, interpolate_to_rhai_expr, json_to_rhai_literal,
    rhai_str_escape, with_pluck_prelude,
};
pub(super) use crate::compiler::token_shape::YIELD_LOGIC;
pub(super) use crate::models::template::ToolErrorPolicy;
pub(super) use crate::models::template::{
    ContextStrategy, DeploymentModel, ExecutionBackendType, FieldMapping, JoinMode,
    PhaseUpdateStatus, Port, ResourceConfig, WorkflowEdge, WorkflowNode, WorkflowNodeData,
};
pub(super) use aithericon_executor_domain::InputSource;
pub(super) use aithericon_sdk::components::executor_lifecycle::{executor_lifecycle, ExecutorBridges};
pub(super) use aithericon_sdk::{
    effects, Context, DynamicToken, EffectError, ExecutorSubmitInput, HumanTaskAssigned,
    HumanTaskRequest, HumanTaskResponse, HumanTaskSubmit, PlaceHandle,
};
pub(super) use serde_json::json;
pub(super) use std::collections::HashMap;

/// Per-node, per-filename input source map. Two flavours coexist:
///
///   - `InputSource::Raw { content }` — inline source carried in the
///     AIR. Right for stateless preview (no S3 yet) and compiler tests.
///   - `InputSource::StoragePath { path, .. }` — S3 reference resolved
///     by the executor at stage time. Right for publish + apply, where
///     embedding every code file inline would blow the per-execution
///     NATS message budget on large workflows.
///
/// The borrow planner needs source TEXT to detect `<slug>.<field>`
/// access. Callers using `StoragePath` here must pass an inline source
/// map to `compile_to_air_with_subworkflows_inline` so the planner
/// still has something to scan. Callers using `Raw` can use the
/// derive-from-files plain `compile_to_air*` entry points.
pub type NodeFiles = HashMap<String, HashMap<String, InputSource>>;

/// Wrap inline `node_id → filename → content` into a [`NodeFiles`]
/// emitting `InputSource::Raw` for every entry. Right for the stateless
/// preview (`POST /api/v1/compile`) and compiler tests.
///
/// **Don't use for publish.** Every `Raw` entry gets embedded inline in
/// the per-execution job spec dispatched over NATS; on workflows with
/// many or sizeable code files that blows the message budget. Use
/// [`node_files_storage_path`] instead and pass the inline source map
/// to `compile_to_air_with_subworkflows_inline` so the borrow planner
/// can still scan.
pub fn node_files_inline(
    inline: &HashMap<String, HashMap<String, String>>,
) -> NodeFiles {
    inline
        .iter()
        .map(|(node_id, files)| {
            let sources = files
                .iter()
                .map(|(filename, content)| {
                    (
                        filename.clone(),
                        InputSource::Raw {
                            content: content.clone(),
                        },
                    )
                })
                .collect();
            (node_id.clone(), sources)
        })
        .collect()
}

/// Wrap a published-template's inline file map into a [`NodeFiles`]
/// keyed by S3 storage paths (`templates/{id}/v{n}/{node}/{filename}`),
/// matching the S3 layout written by
/// [`crate::process::publish::PublishService::upload_files`]. The
/// executor downloads the file at stage time, so per-job NATS payloads
/// stay small — the right primitive for publish + apply.
///
/// Pair with the original `ydoc_files` inline map passed as
/// `inline_sources` to `compile_to_air_with_subworkflows_inline` so the
/// borrow planner has source text to scan.
pub fn node_files_storage_path(
    template_id: uuid::Uuid,
    version: i32,
    ydoc_files: &HashMap<String, HashMap<String, String>>,
) -> NodeFiles {
    ydoc_files
        .iter()
        .map(|(node_id, files)| {
            let sources = files
                .keys()
                .map(|filename| {
                    let path =
                        format!("templates/{template_id}/v{version}/{node_id}/{filename}");
                    (
                        filename.clone(),
                        InputSource::StoragePath {
                            path,
                            storage: None,
                        },
                    )
                })
                .collect();
            (node_id.clone(), sources)
        })
        .collect()
}

/// Instruction to merge `dead` place into `survivor` place.
/// All references to `dead` become references to `survivor`, then `dead` is removed.
pub(crate) struct PlaceMerge {
    pub(crate) dead: String,
    pub(crate) survivor: String,
}

/// One agent's tool wiring deferred to the post-traversal fixup phase.
///
/// `lower_agent_loop` knows the agent's own places (state-in-tool, error,
/// per-tool dispatch) but tool children are lowered after the agent
/// itself in topological order — their `NodePorts` aren't in
/// `node_ports` yet when the agent runs. Queue this struct during
/// `lower_agent_loop`; `apply_agent_tool_wirings` drains it after the
/// topological loop, when every child's input/output places are minted.
pub(crate) struct AgentToolWiring {
    pub(crate) agent_id: String,
    pub(crate) agent_label: String,
    pub(crate) p_state: PlaceHandle<DynamicToken>,
    pub(crate) p_state_in_tool: PlaceHandle<DynamicToken>,
    pub(crate) p_error: PlaceHandle<DynamicToken>,
    pub(crate) tools: Vec<AgentToolEntry>,
}

/// Per-tool wiring data: the dispatch place the agent's route deposits
/// to, the child node id whose input/output places get bridged in, and
/// the error-policy that decides whether tool failures feed back into
/// the loop or bubble to the agent's error path.
pub(crate) struct AgentToolEntry {
    pub(crate) tool_name: String,
    pub(crate) child_id: String,
    pub(crate) dispatch_place: PlaceHandle<DynamicToken>,
    pub(crate) on_tool_error: ToolErrorPolicy,
}

/// Side-channel state that builds during lowering and is consumed by the
/// post-merge orchestration passes in `compile.rs`. Distinct from the
/// per-node interface registry (`InterfaceRegistry`): this holds *non*-
/// per-node bookkeeping (place merges, group declarations, scope-child
/// parentage, the process token shared between Start and End).
///
/// Workflow-exit terminal places and parked-data ports used to live here
/// too; both moved to `NodeInterface` as the canonical source of truth.
#[derive(Default)]
pub(crate) struct PostProcess {
    /// Groups to add: (id, name, parent_id).
    pub(crate) groups: Vec<(String, String, Option<String>)>,
    /// Pass-through edge merges: dead place → survivor place.
    pub(crate) merges: Vec<PlaceMerge>,
    /// Maps node_id → group_id for scope children.
    /// Used to tag places/transitions with the correct group after build().
    pub(crate) scope_groups: HashMap<String, String>,
    /// Set by the Start arm when the opt-in `process_name` registered an HPI
    /// process: the place holding the `ProcessStarted` token (`process_id`).
    /// End nodes read it (non-consuming) to wire a `process_complete` effect
    /// before their terminal place, so the process is marked complete. `None`
    /// = no process registered → End stays a bare terminal (unchanged).
    pub(crate) process_token_place: Option<PlaceHandle<DynamicToken>>,
    /// Agent → tool-child wiring deferred to after the topological pass
    /// (every tool child's NodePorts must be present in node_ports
    /// before the invoke/collect transitions can reference them).
    pub(crate) agent_tool_wirings: Vec<AgentToolWiring>,
}

/// Tracks which places are the input/output interface of each expanded node.
pub(crate) struct NodePorts {
    /// The place where tokens enter this node block.
    pub(crate) input_place: PlaceHandle<DynamicToken>,
    /// The place(s) where tokens leave this node block.
    /// For decision nodes, there are multiple outputs keyed by edge_id.
    pub(crate) output_places: Vec<(Option<String>, PlaceHandle<DynamicToken>)>,
    /// For ParallelJoin nodes: maps incoming edge_id -> input place.
    /// Empty for all other node types.
    pub(crate) input_places: HashMap<String, PlaceHandle<DynamicToken>>,
    /// Named inbound ports keyed by `target_handle`. wire.rs checks this before
    /// falling back to `input_place`. Used by Loop's `body_out` so a body
    /// child's outgoing edge with `targetHandle: "body_out"` routes to the
    /// loop's `p_body_out` rather than its main `p_input`. Empty for any node
    /// type without named inbound ports.
    pub(crate) input_handles: HashMap<String, PlaceHandle<DynamicToken>>,
}

/// Everything a single node's lowering needs: the shared build `ctx`, the
/// accumulating `ports`/`fixups` maps, plus the node-local view (its node,
/// incident edges, staged files).
pub(crate) struct LoweringCtx<'a, 'c> {
    pub(crate) node: &'a WorkflowNode,
    pub(crate) outgoing_edges: &'a [&'a WorkflowEdge],
    pub(crate) incoming_edges: &'a [&'a WorkflowEdge],
    /// Container children — nodes whose `parent_id == self.node.id`. Empty for
    /// non-container nodes and for empty containers. Used by `lower_loop` to
    /// reject empty Loops; other lowering paths ignore it today (Scope has its
    /// own group-based traversal).
    pub(crate) children: &'a [&'a WorkflowNode],
    pub(crate) ctx: &'c mut Context,
    pub(crate) ports: &'c mut HashMap<String, NodePorts>,
    pub(crate) fixups: &'c mut PostProcess,
    pub(crate) node_files: &'a HashMap<String, InputSource>,
    /// Pre-resolved child sub-workflow AIR, keyed by SubWorkflow node id.
    /// Empty unless the publish/preview path populated it.
    pub(crate) sub_air: &'a SubWorkflowAir,
    /// Per-node sub-graph interface registry. Every `lower_*` MUST call
    /// `publish_interface()` exactly once (except Trigger). See
    /// `service/src/compiler/interface.rs` for the protocol.
    pub(crate) interfaces: &'c mut InterfaceRegistry,
    /// Workflow-level reusable JSON-Schema fragments. `lower_automated_step`
    /// passes its node's `executionSpec.config` through
    /// `compiler::schema_refs::inline_refs` so backends never see a
    /// `{"$ref": "#/definitions/<name>"}`.
    pub(crate) definitions: &'a std::collections::BTreeMap<String, serde_json::Value>,
    /// Side-channel for static per-node configs the publish layer uploads to
    /// S3. Lower-paths that previously inlined a `{config: {…}}` Rhai
    /// literal now register the resolved JSON here (keyed by node id) and
    /// emit a tiny `{config_ref: {storage_path: …}}` Rhai literal instead.
    /// The Petri token stays small and the Rhai parser's
    /// expression-complexity limit no longer caps schema depth. See
    /// `lower_automated_step` for the emission site,
    /// `service/src/process/publish.rs` for the upload.
    pub(crate) node_configs: &'c mut HashMap<String, serde_json::Value>,
    /// Compile-time `(template_id, version)` used to mint the deterministic
    /// S3 key for every `node_configs` entry. Mirrors the executor-side
    /// `node-config.json` key the publish path uploads to (see
    /// `mekhan_service::s3::ArtifactStore::node_config_key`).
    pub(crate) config_storage: ConfigStorage<'a>,
}

/// Compile-time pointer set the lowering uses to mint the
/// `templates/{template_id}/v{version}/{node_id}/node-config.json` key for
/// every parked `node_configs` entry. The key has to be computable at
/// compile time so the Rhai literal can reference it before publish writes
/// the blob. Tests that don't care about real publish IDs pass
/// [`ConfigStorage::ephemeral`].
#[derive(Clone, Copy)]
pub struct ConfigStorage<'a> {
    pub template_id: uuid::Uuid,
    pub version: i32,
    /// Optional override for the key-computation function. None means use
    /// the standard `templates/{tid}/v{ver}/{node_id}/node-config.json`
    /// format. Reserved for future use (e.g., per-tenant prefixes).
    #[allow(clippy::type_complexity)]
    pub key_fn: Option<&'a (dyn Fn(uuid::Uuid, i32, &str) -> String + Sync)>,
}

impl<'a> ConfigStorage<'a> {
    /// Compute the S3 key for one node's static config blob.
    pub fn key(&self, node_id: &str) -> String {
        match self.key_fn {
            Some(f) => f(self.template_id, self.version, node_id),
            None => format!(
                "templates/{}/v{}/{}/node-config.json",
                self.template_id, self.version, node_id
            ),
        }
    }

    /// Compile-time-only storage tag with a synthetic template id. Right for
    /// compiler unit tests and the previewer where no publish is on the
    /// horizon — the lowered Rhai still embeds a `config_ref` (so the
    /// emission path is exercised) but no S3 upload happens.
    pub fn ephemeral() -> Self {
        Self {
            template_id: uuid::Uuid::nil(),
            version: 0,
            key_fn: None,
        }
    }
}

impl LoweringCtx<'_, '_> {
    /// Publish this node's interface to the registry. Derives `kind`,
    /// `entry`, `named_inputs`, and `outputs` from `ports[node_id]` (which
    /// the lowering already populated) and inserts the entry. Returns a
    /// mutable handle so the caller can extend with fields `ports` doesn't
    /// carry — `workflow_terminals` (End) or `data_port` (parked producers).
    ///
    /// Must be called exactly once per non-Trigger lower_*. The dispatcher
    /// hard-errors if no entry is published.
    pub(crate) fn publish_interface(&mut self) -> &mut NodeInterface {
        let id = self.node.id.clone();
        let kind = node_kind_of(self.node);
        let mut iface = NodeInterface::new(id.clone(), kind);
        if let Some(ports) = self.ports.get(&id) {
            iface.entry = Some(ports.input_place.id().to_string());
            for (handle, place) in &ports.input_handles {
                iface.named_inputs.insert(handle.clone(), place.id().to_string());
            }
            for (edge_id, place) in &ports.input_places {
                iface.named_inputs.insert(edge_id.clone(), place.id().to_string());
            }
            for (key, place) in &ports.output_places {
                let k = match key {
                    None => OutputKey::Default,
                    Some(s) => OutputKey::Edge(s.clone()),
                };
                iface.outputs.insert(k, place.id().to_string());
            }
        }
        self.interfaces.insert(id.clone(), iface);
        self.interfaces.get_mut(&id).expect("just inserted")
    }
}

/// Expand one workflow node into Petri structure.
pub(crate) trait NodeLowering {
    fn lower(&self, cx: &mut LoweringCtx) -> Result<(), CompileError>;
}

impl NodeLowering for WorkflowNode {
    fn lower(&self, cx: &mut LoweringCtx) -> Result<(), CompileError> {
        match &self.data {
            WorkflowNodeData::Start { .. } => start::lower_start(cx),
            WorkflowNodeData::End { .. } => end::lower_end(cx),
            WorkflowNodeData::HumanTask { .. } => human_task::lower_human_task(cx),
            WorkflowNodeData::AutomatedStep { .. } => automated_step::lower_automated_step(cx),
            WorkflowNodeData::Agent { .. } => agent::lower_agent(cx),
            WorkflowNodeData::Decision { .. } => decision::lower_decision(cx),
            WorkflowNodeData::ParallelSplit { .. } => parallel_split::lower_parallel_split(cx),
            WorkflowNodeData::ParallelJoin { .. } => parallel_join::lower_parallel_join(cx),
            WorkflowNodeData::Join { .. } => join::lower_join(cx),
            WorkflowNodeData::Loop { .. } => loop_::lower_loop(cx),
            WorkflowNodeData::Scope { .. } => scope::lower_scope(cx),
            WorkflowNodeData::PhaseUpdate { .. } => phase_update::lower_phase_update(cx),
            WorkflowNodeData::ProgressUpdate { .. } => progress_update::lower_progress_update(cx),
            WorkflowNodeData::Failure { .. } => failure::lower_failure(cx),
            WorkflowNodeData::SubWorkflow { .. } => subworkflow::lower_subworkflow(cx),
            WorkflowNodeData::Trigger { .. } => {
                // Trigger nodes are NOT compiled into AIR — they are a
                // pre-compile concern owned by the trigger dispatcher
                // (`service::triggers`). The trigger's outgoing edge is also
                // skipped during wire_edge.
                Ok(())
            }
        }
    }
}

/// Thin dispatch retained as the lowering entry point used by the orchestrator.
#[allow(clippy::too_many_arguments)]
pub(crate) fn expand_node<'a>(
    node: &'a WorkflowNode,
    outgoing_edges: &'a [&'a WorkflowEdge],
    incoming_edges: &'a [&'a WorkflowEdge],
    children: &'a [&'a WorkflowNode],
    ctx: &mut Context,
    ports: &mut HashMap<String, NodePorts>,
    fixups: &mut PostProcess,
    node_files: &'a HashMap<String, InputSource>,
    sub_air: &'a SubWorkflowAir,
    interfaces: &mut InterfaceRegistry,
    definitions: &'a std::collections::BTreeMap<String, serde_json::Value>,
    node_configs: &mut HashMap<String, serde_json::Value>,
    config_storage: ConfigStorage<'a>,
) -> Result<(), CompileError> {
    let mut cx = LoweringCtx {
        node,
        outgoing_edges,
        incoming_edges,
        children,
        ctx,
        ports,
        fixups,
        node_files,
        sub_air,
        interfaces,
        definitions,
        node_configs,
        config_storage,
    };
    node.lower(&mut cx)?;
    // Protocol enforcement: every non-Trigger lowering MUST call
    // `cx.publish_interface()` exactly once. The dispatcher hard-errors
    // if it didn't — there is no auto-derive fallback (by design; see
    // `service/src/compiler/interface.rs` for the contract).
    if !matches!(node.data, WorkflowNodeData::Trigger { .. })
        && !cx.interfaces.contains_key(&node.id)
    {
        return Err(CompileError::Compilation(format!(
            "internal: lower_* for node '{}' ({:?}) did not publish an interface — \
             every lowering must call `cx.publish_interface()` before returning",
            node.id, node.data
        )));
    }
    Ok(())
}

fn node_kind_of(node: &WorkflowNode) -> NodeKind {
    match &node.data {
        WorkflowNodeData::Start { .. } => NodeKind::Start,
        WorkflowNodeData::End { .. } => NodeKind::End,
        WorkflowNodeData::HumanTask { .. } => NodeKind::HumanTask,
        WorkflowNodeData::AutomatedStep { .. } => NodeKind::AutomatedStep,
        // PR 1: Agent's degenerate path lowers byte-identically to
        // AutomatedStep(Llm); publish the same interface kind so downstream
        // consumers don't have to special-case it. The follow-up loop
        // lowering will switch to a dedicated `NodeKind::Agent`.
        WorkflowNodeData::Agent { .. } => NodeKind::AutomatedStep,
        WorkflowNodeData::Decision { .. } => NodeKind::Decision,
        WorkflowNodeData::Loop { .. } => NodeKind::Loop,
        WorkflowNodeData::ParallelSplit { .. } => NodeKind::ParallelSplit,
        WorkflowNodeData::ParallelJoin { .. } => NodeKind::ParallelJoin,
        WorkflowNodeData::Join { .. } => NodeKind::Join,
        WorkflowNodeData::Scope { .. } => NodeKind::Scope,
        WorkflowNodeData::SubWorkflow { .. } => NodeKind::SubWorkflow,
        WorkflowNodeData::PhaseUpdate { .. } => NodeKind::PhaseUpdate,
        WorkflowNodeData::ProgressUpdate { .. } => NodeKind::ProgressUpdate,
        WorkflowNodeData::Failure { .. } => NodeKind::Failure,
        WorkflowNodeData::Trigger { .. } => NodeKind::Trigger,
    }
}

// ── Per-variant lowerings ───────────────────────────────────────────────
// One submodule per `WorkflowNodeData` variant (siblings sharing a single
// dispatch arm — `automated_step` includes Scheduled + EngineEffect; `agent`
// includes the degenerate + loop paths). Each module exports a single
// `pub(super) fn lower_<variant>(cx: &mut LoweringCtx) -> Result<...>` that
// the dispatcher above routes to. Shared mid-lowering helpers
// (`split_outputs`, `park_outputs`, `result_mapping_rhai`,
// `declared_outputs_rhai`) live below as `pub(super)` so every child can
// `use super::*` and pick them up uniformly.
pub(super) mod agent;
pub(super) mod automated_step;
pub(super) mod decision;
pub(super) mod end;
pub(super) mod failure;
pub(super) mod human_task;
pub(super) mod join;
pub(super) mod loop_;
pub(super) mod parallel_join;
pub(super) mod parallel_split;
pub(super) mod phase_update;
pub(super) mod progress_update;
pub(super) mod scope;
pub(super) mod start;
pub(super) mod subworkflow;
/// Build `(let-bindings, value-expr)` Rhai for a result-mapping list, mirroring
/// the PhaseUpdate "bind interpolations to shallow locals" recipe so the
/// envelope map literal stays within the debug-build Rhai expr-depth limit.
/// Empty list → `("", "()")` (Rhai unit, serializes to JSON `null`).
///
/// `expression` is raw author Rhai (same trust model as Trigger
/// `payload_mapping` / BranchCondition `guard`); the publish-time validator
/// (`validate::validate_guards`) parses each and resolves its `input.<field>`
/// refs against the node's inbound scope. `target_field` is emitted as a
/// JSON-escaped Rhai map key so any field name is injection-safe.
pub(super) fn result_mapping_rhai(mappings: &[FieldMapping]) -> (String, String) {
    if mappings.is_empty() {
        return (String::new(), "()".to_string());
    }
    let mut lets = String::new();
    let mut entries: Vec<String> = Vec::with_capacity(mappings.len());
    for (i, m) in mappings.iter().enumerate() {
        lets.push_str(&format!("let __rv{i} = ({}); ", m.expression));
        let key =
            serde_json::to_string(&m.target_field).unwrap_or_else(|_| "\"\"".to_string());
        entries.push(format!("{key}: __rv{i}"));
    }
    (lets, format!("#{{ {} }}", entries.join(", ")))
}

/// Foundation: split a data-yielding node's output into a write-once parked
/// **data** place + a slim **control** place, joined by a `t_{id}_yield`
/// transition. Generalizes the Start-parks-`ProcessStarted` precedent to
/// every HumanTask/AutomatedStep. Returns the parked-data place id (the
/// caller publishes on `interface.data_port`) and the control place (the
/// node's new downstream output). Schema refs are left as the default
/// permissive `DynamicToken`; the post-merge phase upgrades the data/ctrl
/// `token_schema` to the typed `#/definitions/*` and registers them.
pub(super) fn split_outputs(
    ctx: &mut Context,
    id: &str,
    label: &str,
    producer_out: &PlaceHandle<DynamicToken>,
) -> (String, PlaceHandle<DynamicToken>) {
    let p_data: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_data"),
        format!("{label} - Parked Data (write-once)"),
    );
    let p_ctrl: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_ctrl"), format!("{label} - Control Token"));
    ctx.transition(
        format!("t_{id}_yield"),
        format!("{label} - Yield (park data, forward control)"),
    )
    .auto_input("tok", producer_out)
    .auto_output("data", &p_data)
    .auto_output("ctrl", &p_ctrl)
    .logic(YIELD_LOGIC);
    (format!("p_{id}_data"), p_ctrl)
}

/// Foundation (Start variant): park a write-once copy of the producer's
/// output as `p_{id}_data` so downstream guards / result-mappings can borrow
/// `<slug>.<field>` via the same read-arc synthesis as `split_outputs` —
/// **without** slimming the forwarded token. Start is special: the very next
/// node still reads its inputs off the control token (human-task
/// `{{ invoice_id }}` interpolation is baked against the inbound token at
/// compile time), so the token must continue intact. We therefore *fork*
/// (`#{ data: d, main: d }`) rather than *split*. Returns the parked-data
/// place id (the caller publishes on `interface.data_port`) and the place
/// carrying the unchanged token onward.
pub(super) fn park_outputs(
    ctx: &mut Context,
    id: &str,
    label: &str,
    producer_out: &PlaceHandle<DynamicToken>,
) -> (String, PlaceHandle<DynamicToken>) {
    let p_data: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_data"),
        format!("{label} - Parked Data (write-once)"),
    );
    let p_main: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_main"), format!("{label} - Output"));
    ctx.transition(
        format!("t_{id}_park"),
        format!("{label} - Park Inputs (fork: park data, forward token)"),
    )
    .auto_input("tok", producer_out)
    .auto_output("data", &p_data)
    .auto_output("main", &p_main)
    .logic("let d = tok; #{ data: d, main: d }");
    (format!("p_{id}_data"), p_main)
}

/// Apply every queued [`AgentToolWiring`]: mint the per-tool invoke +
/// collect (+ optional collect_error / bubble) transitions that bridge
/// each agent's `p_dispatch_<tn>` and `p_state_in_tool` places to its
/// tool children's already-lowered input/output places. Runs once after
/// the topological lowering pass so every tool child's `NodePorts` is in
/// `node_ports`. Errors when a referenced child is missing — that's an
/// internal invariant break (the child wasn't lowered) rather than user
/// input, so the message names the agent + child for debugging.
pub(crate) fn apply_agent_tool_wirings(
    ctx: &mut Context,
    node_ports: &HashMap<String, NodePorts>,
    wirings: &[AgentToolWiring],
) -> Result<(), CompileError> {
    for wiring in wirings {
        for entry in &wiring.tools {
            let child_ports = node_ports.get(&entry.child_id).ok_or_else(|| {
                CompileError::Compilation(format!(
                    "agent '{}': tool child '{}' has no NodePorts (was it lowered?)",
                    wiring.agent_id, entry.child_id
                ))
            })?;
            let agent_id = &wiring.agent_id;
            let agent_label = &wiring.agent_label;
            let tn = &entry.tool_name;

            // t_invoke_<tn>: consume dispatch → child input. The token
            // we deposit is the tool call's args map (the LLM's
            // argument object). Children that expect a richer envelope
            // (e.g. HTTP backend's `{url, body}`) can be authored by
            // having the model emit those keys directly — v1 keeps the
            // shape literal.
            ctx.transition(
                format!("t_{agent_id}_invoke_{tn}"),
                format!("{agent_label} - Invoke {tn}"),
            )
            .auto_input("dispatch", &entry.dispatch_place)
            .auto_output("input", &child_ports.input_place)
            .logic_rhai("#{ input: dispatch.args }".to_string())
            .done();

            // Tool child's primary (default-keyed) output is the
            // success path; the `Some("error")` keyed output is the
            // failure path. Either may be absent depending on the
            // child's lowering — pass-through nodes with no error edge
            // skip the failure transitions entirely.
            let child_default_out = child_ports
                .output_places
                .iter()
                .find(|(k, _)| k.is_none())
                .map(|(_, p)| p.clone());
            let child_error_out = child_ports
                .output_places
                .iter()
                .find(|(k, _)| k.as_deref() == Some("error"))
                .map(|(_, p)| p.clone());

            // t_collect_<tn>: child success + state_in_tool → state.
            // Appends a `role: tool` message to history with the
            // child's output payload. State stays inside the agent —
            // the workflow token (which the child may have stripped)
            // is irrelevant once we have the tool result.
            if let Some(child_out) = child_default_out {
                ctx.transition(
                    format!("t_{agent_id}_collect_{tn}"),
                    format!("{agent_label} - Collect {tn}"),
                )
                .auto_input("result", &child_out)
                .auto_input("state", &wiring.p_state_in_tool)
                .auto_output("state", &wiring.p_state)
                .logic_rhai(format!(
                    r#"let s = state; s.history.push(#{{ role: "tool", tool_name: "{tn}", content: result }}); s.message_count = s.message_count + 1; #{{ state: s }}"#
                ))
                .done();
            }

            // Error path: Feedback re-enters the loop with a
            // synthesized failure message; Bubble drains state and
            // surfaces the failure on the agent's error output.
            if let Some(child_err) = child_error_out {
                match entry.on_tool_error {
                    ToolErrorPolicy::Feedback => {
                        ctx.transition(
                            format!("t_{agent_id}_collect_{tn}_error"),
                            format!("{agent_label} - Collect {tn} (error → feedback)"),
                        )
                        .auto_input("err", &child_err)
                        .auto_input("state", &wiring.p_state_in_tool)
                        .auto_output("state", &wiring.p_state)
                        .logic_rhai(format!(
                            r#"let s = state; let msg = if type_of(err) == "map" && "message" in err {{ err.message }} else {{ "tool error" }}; s.history.push(#{{ role: "tool", tool_name: "{tn}", content: "tool '{tn}' failed: " + msg, is_error: true }}); s.message_count = s.message_count + 1; #{{ state: s }}"#
                        ))
                        .done();
                    }
                    ToolErrorPolicy::Bubble => {
                        ctx.transition(
                            format!("t_{agent_id}_collect_{tn}_bubble"),
                            format!("{agent_label} - Collect {tn} (error → bubble)"),
                        )
                        .auto_input("err", &child_err)
                        .auto_input("state", &wiring.p_state_in_tool)
                        .auto_output("error", &wiring.p_error)
                        .logic_rhai("#{ error: err }".to_string())
                        .done();
                    }
                }
            }
        }
    }
    Ok(())
}

/// Serialize the declared `output.fields` as a Rhai array literal carrying
/// `(name, required, kind)` per entry, suitable for embedding into the
/// prepare transition's `d.spec.outputs` slot.
///
/// Enabled for the backends that consume declared outputs at runtime:
/// - **Python**: the runner sweeps `globals()` by declared name + validates
///   each value against `kind` (executor-backend::python).
/// - **Kreuzberg**: `build_single_outputs` emits kreuzberg's native
///   `ExtractionResult` shape 1:1 — `content`, `mime_type`, `metadata`,
///   `tables`, `detected_languages`, and optional `chunks`/`images`/`pages`/
///   `elements`/`djot_content`. Declarations must match these names; the
///   executor's required-output check fires on mismatch. No aliasing.
/// - **LLM**: when the response has a structured-JSON payload, the backend
///   unpacks each declared output by matching it to a top-level key; any
///   unmatched declaration falls back to the whole response_value
///   (executor-llm::backend). The structured-output path is the only way
///   to expose multiple typed fields from one LLM call.
///
/// Other backends (process, docker, http, file_ops, postgres, …) don't
/// auto-fill declared outputs; emitting names would force the executor's
/// `required`-output check to fail. Keep `[]` for them until they grow
/// their own auto-fill or output-validation path.
pub(super) fn declared_outputs_rhai(backend: ExecutionBackendType, output: &Port) -> String {
    let backend_consumes_declared = crate::backends::lookup(backend)
        .map(|d| d.consumes_declared_outputs)
        .unwrap_or(false);
    if !backend_consumes_declared || output.fields.is_empty() {
        return "[]".to_string();
    }
    let arr: Vec<serde_json::Value> = output
        .fields
        .iter()
        .map(|f| {
            // FieldKind serializes as snake_case (text, number, bool, json,
            // textarea, select, file, signature, timestamp). The runner side
            // maps unknown kind strings to "skip validation" so forward-compat
            // additions don't break existing deployments before they roll.
            let kind_str = serde_json::to_value(f.kind)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "json".to_string());
            serde_json::json!({
                "name": f.name,
                "required": f.required,
                "kind": kind_str,
            })
        })
        .collect();
    json_to_rhai_literal(&serde_json::Value::Array(arr))
}

