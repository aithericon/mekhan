//! Per-node lowering: each workflow node type expands into its Petri
//! places/transitions via the [`NodeLowering`] trait. [`expand_node`] is the
//! thin dispatch; the real work lives in one `lower_*` function per variant.

// Re-exported with `pub(super)` so per-variant submodules can `use super::*`
// and pick up the full lowering toolbelt (types, helpers, SDK glue) without
// maintaining their own import headers. This is the single import authority
// for the whole `lower/` module tree.
pub(super) use crate::compiler::compile::SubWorkflowAir;
pub(super) use crate::compiler::error::CompileError;
pub(super) use crate::compiler::interface::{InterfaceRegistry, NodeInterface, OutputKey};
pub(super) use crate::compiler::rhai_gen::{
    build_join_merge_logic_full, build_join_passthrough_logic, build_merge_logic,
    build_retry_topology, interpolate_to_rhai_expr, json_to_rhai_literal, rhai_str_escape,
    with_pluck_prelude,
};
pub(super) use crate::compiler::token_shape::YIELD_LOGIC;
pub(super) use crate::compiler::well_known;
pub(super) use crate::models::template::ToolErrorPolicy;
pub(super) use crate::models::template::{
    ContextStrategy, DeploymentModel, ExecutionBackendType, FieldMapping, JoinMode,
    PhaseUpdateStatus, Port, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
pub(super) use aithericon_executor_domain::InputSource;
pub(super) use aithericon_sdk::components::executor_lifecycle::{
    executor_lifecycle, ExecutorBridges,
};
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
/// map via `CompileOptions::inline_sources` to `compile_to_air_with_options`
/// so the planner still has something to scan. Callers using `Raw` can use
/// the derive-from-files plain `compile_to_air` entry point.
pub type NodeFiles = HashMap<String, HashMap<String, InputSource>>;

/// Wrap inline `node_id → filename → content` into a [`NodeFiles`]
/// emitting `InputSource::Raw` for every entry. Right for the stateless
/// preview (`POST /api/v1/compile`) and compiler tests.
///
/// **Don't use for publish.** Every `Raw` entry gets embedded inline in
/// the per-execution job spec dispatched over NATS; on workflows with
/// many or sizeable code files that blows the message budget. Use
/// [`node_files_storage_path`] instead and pass the inline source map
/// via `CompileOptions::inline_sources` to `compile_to_air_with_options`
/// so the borrow planner can still scan.
pub fn node_files_inline(inline: &HashMap<String, HashMap<String, String>>) -> NodeFiles {
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
/// `CompileOptions::inline_sources` to `compile_to_air_with_options` so the
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
                    let path = format!("templates/{template_id}/v{version}/{node_id}/{filename}");
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
    /// The agent's error place — `Some` only when the agent's error handle is
    /// wired to a downstream handler. `None` (unwired) makes the per-tool
    /// collect-bubble path crash the net (Rhai `throw`) instead of parking a
    /// dead-end error token. Mirrors the `Option<PlaceHandle>` panic/Result
    /// model in `lower_automated_step`.
    pub(crate) p_error: Option<PlaceHandle<DynamicToken>>,
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
    /// Per-Timeout body-cancellation fan-outs deferred to a post-pass.
    /// `apply_timeout_cancel_fanouts` walks each entry's body_child_ids,
    /// reads their `NodeInterface.cancellable` slot, and synthesizes a
    /// drain transition + matching `<kind>_cancel` effect for every
    /// cancellable child.
    pub(crate) timeout_cancel_fanouts: Vec<crate::compiler::lower::timeout::TimeoutCancelFanout>,
    /// Typed-lease definitions a registry-resolved pooled AutomatedStep needs
    /// in the AIR `definitions` map: `(def_name, json_schema)` where `def_name`
    /// is `Lease__<backend>`. The SDK `Context` has no public definition-register
    /// hook (only token-typed `register_schema`), so the lowering records the
    /// pair here and `compile_to_air` drains it into `scenario.definitions`
    /// after `ctx.build()`. Deduplicated on insert by the drain (same backend
    /// across N pooled nodes ⇒ one entry).
    pub(crate) lease_definitions: Vec<(String, serde_json::Value)>,
    /// Grant-inbox places to type with a `Lease__<backend>` ref: `(place_id,
    /// def_name)`. Drained alongside `lease_definitions` after build so the
    /// engine `SchemaRegistry` validates the routed grant reply IS the typed
    /// lease. Kept separate from `lease_definitions` because place typing is a
    /// post-build scenario mutation, not a definitions insert.
    pub(crate) lease_inbox_schemas: Vec<(String, String)>,
}

/// Tracks which places are the input/output interface of each expanded node.
pub(crate) struct NodePorts {
    /// The place where tokens enter this node block.
    pub(crate) input_place: PlaceHandle<DynamicToken>,
    /// The place(s) where tokens leave this node block.
    /// For decision nodes, there are multiple outputs keyed by edge_id.
    pub(crate) output_places: Vec<(Option<String>, PlaceHandle<DynamicToken>)>,
    /// For Join nodes: maps incoming edge_id -> input place.
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
    /// The whole workflow graph. Lowerings need it to inspect neighbour kinds
    /// (e.g. `lower_automated_step` checks whether its `parent_id` names a Map
    /// to choose the full-token `park_outputs` over the slim `split_outputs`
    /// for a map body terminal). Reuses `token_shape::is_*_node`.
    pub(crate) graph: &'a WorkflowGraph,
    pub(crate) outgoing_edges: &'a [&'a WorkflowEdge],
    pub(crate) incoming_edges: &'a [&'a WorkflowEdge],
    /// Container children — nodes whose `parent_id == self.node.id`. Empty for
    /// non-container nodes and for empty containers. Used by `lower_loop` to
    /// reject empty Loops; other lowering paths ignore it today (Scope has its
    /// own group-based traversal).
    pub(crate) children: &'a [&'a WorkflowNode],
    /// Agent tool targets — nodes reachable from this node via an outgoing
    /// edge with `source_handle == "tools"`. Empty for non-Agent nodes and
    /// for agents with no tools wired. Replaces the previous "any child node
    /// with `tool_meta`" discovery (which required dragging the tool node
    /// onto the agent to set `parent_id`); tools are now first-class graph
    /// nodes connected by edges, not visually nested children. The orchestrator
    /// builds the index once via `agent_tools_by_id` and passes the slice in.
    pub(crate) agent_tools: &'a [&'a WorkflowNode],
    /// True when THIS node is the target of some agent's `tools`-handled edge
    /// (i.e. it is used as an agent tool). A tool child has no authored `error`
    /// outgoing edge, so without this flag `error_path_wired` would be false and
    /// the child would lower a dead-end-throw failure path that crashes the
    /// agent. Lowerings that can be used as tools (SubWorkflow, AutomatedStep)
    /// OR this into their `error_handled` gate so the child mints a `p_error`
    /// output port the agent's collect-error wiring consumes. Default false for
    /// every non-tool node — all existing flows are unaffected.
    pub(crate) is_agent_tool: bool,
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
    /// Workspace-resource manifest the publish handler resolved
    /// (`discover_known_resources`). A pooled AutomatedStep reads its
    /// `Inline.capacity.alias` out of here to learn `{resource_id, kind}` and
    /// bridge to the `concurrency_limit`'s backing pool net. Empty for tests / previews
    /// that don't resolve resources — a pooled step then fails with
    /// `WorkspaceResourceUnknown` (no well-known-global fallback any more).
    pub(crate) known_resources: &'a crate::compiler::resource_refs::KnownResources,
    /// Per-node container execution spec, keyed by node id. A scheduled
    /// AutomatedStep reads its own entry to bake an Apptainer/Singularity
    /// `.sif` into the engine's Slurm allocator (native execution when absent).
    /// Empty for tests / previews that don't resolve container images — AIR is
    /// then byte-identical to the no-container case.
    pub(crate) container_specs:
        &'a std::collections::HashMap<String, crate::compiler::compile::CompilerContainerSpec>,
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
            // Single source of truth for the default key shape; the publish-time
            // upload (`ArtifactStore::upload_node_config`) mints the same key so
            // the compile-time Rhai literal and the actual blob path agree.
            None => {
                crate::s3::ArtifactStore::node_config_key(self.template_id, self.version, node_id)
            }
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
        let kind = crate::nodes::lookup_by_variant(&self.node.data)
            .expect("every WorkflowNodeData variant is registered in crate::nodes::NODES")
            .kind;
        let mut iface = NodeInterface::new(id.clone(), kind);
        if let Some(ports) = self.ports.get(&id) {
            iface.entry = Some(ports.input_place.id().to_string());
            for (handle, place) in &ports.input_handles {
                iface
                    .named_inputs
                    .insert(handle.clone(), place.id().to_string());
            }
            for (edge_id, place) in &ports.input_places {
                iface
                    .named_inputs
                    .insert(edge_id.clone(), place.id().to_string());
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

/// Thin dispatch retained as the lowering entry point used by the orchestrator.
#[allow(clippy::too_many_arguments)]
pub(crate) fn expand_node<'a>(
    node: &'a WorkflowNode,
    graph: &'a WorkflowGraph,
    outgoing_edges: &'a [&'a WorkflowEdge],
    incoming_edges: &'a [&'a WorkflowEdge],
    children: &'a [&'a WorkflowNode],
    agent_tools: &'a [&'a WorkflowNode],
    is_agent_tool: bool,
    ctx: &mut Context,
    ports: &mut HashMap<String, NodePorts>,
    fixups: &mut PostProcess,
    node_files: &'a HashMap<String, InputSource>,
    sub_air: &'a SubWorkflowAir,
    interfaces: &mut InterfaceRegistry,
    definitions: &'a std::collections::BTreeMap<String, serde_json::Value>,
    node_configs: &mut HashMap<String, serde_json::Value>,
    config_storage: ConfigStorage<'a>,
    known_resources: &'a crate::compiler::resource_refs::KnownResources,
    container_specs: &'a std::collections::HashMap<
        String,
        crate::compiler::compile::CompilerContainerSpec,
    >,
) -> Result<(), CompileError> {
    let mut cx = LoweringCtx {
        node,
        graph,
        outgoing_edges,
        incoming_edges,
        children,
        agent_tools,
        is_agent_tool,
        ctx,
        ports,
        fixups,
        node_files,
        sub_air,
        interfaces,
        definitions,
        node_configs,
        config_storage,
        known_resources,
        container_specs,
    };
    let decl = crate::nodes::lookup_by_variant(&node.data)
        .expect("every WorkflowNodeData variant is registered in crate::nodes::NODES");
    match decl.lower {
        Some(lf) => lf(&mut cx)?,
        None if !decl.lowers_to_air => {}
        None => {
            return Err(CompileError::Compilation(format!(
                "registry bug: variant `{}` declares lowers_to_air=true but has no `lower` fn",
                decl.wire_name
            )));
        }
    }
    // Protocol enforcement: every lowering that participates in AIR MUST
    // call `cx.publish_interface()` exactly once. The dispatcher hard-errors
    // if it didn't — there is no auto-derive fallback (by design; see
    // `service/src/compiler/interface.rs`).
    if decl.lowers_to_air && !cx.interfaces.contains_key(&node.id) {
        return Err(CompileError::Compilation(format!(
            "internal: lower_* for node '{}' ({:?}) did not publish an interface — \
             every lowering must call `cx.publish_interface()` before returning",
            node.id, node.data
        )));
    }
    Ok(())
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
pub(crate) mod agent;
pub(crate) mod automated_step;
pub(crate) mod channels;
pub(crate) mod decision;
pub(crate) mod delay;
pub(crate) mod end;
pub(crate) mod failure;
pub(crate) mod gather;
pub(crate) mod human_task;
pub(crate) mod join;
pub(crate) mod lease_bridge;
pub(crate) mod lease_scope;
pub(crate) mod loop_;
pub(crate) mod map;
pub(crate) mod parallel_split;
pub(crate) mod phase_update;
pub(crate) mod progress_update;
pub(crate) mod scope;
pub(crate) mod start;
pub(crate) mod subworkflow;
pub(crate) mod timeout;

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
        let key = serde_json::to_string(&m.target_field).unwrap_or_else(|_| "\"\"".to_string());
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

/// True when `node` is the TERMINAL of a Map body — the child whose edge enters
/// the parent Map's `body_out` handle. Such a node must fork its FULL completed
/// envelope (park data AND forward the whole token via `park_outputs`) so the
/// Map's `t_<map>_collect` can read `body.detail.outputs.<resultVar>` plus the
/// preserved `__map_idx`/`__map_id` correlation leaves; the slim `split_outputs`
/// control token carries neither. Single source of truth shared by every body
/// kind that can sit at a Map terminal (AutomatedStep inline, Agent full-loop,
/// SubWorkflow). Loop body terminals deliberately do NOT call this — a Loop
/// reads its body output via the parked `<body>.<field>` borrow once per
/// iteration, with no K-fan-out correlation.
///
/// True when this node's failure/error handle is WIRED to a downstream
/// handler — i.e. some outgoing edge carries `source_handle == "error"`. This
/// is the Rust `Result::Err`-is-handled predicate: a wired handle means the
/// error token routes to a handler and the net continues; an unwired handle
/// means a permanent failure must crash the net (a panic that unwinds to the
/// top → `NetFailed`) rather than strand a token in a dead-end error place.
///
/// `outgoing_edges` are the edges whose `source == node_id` (see
/// `graph::outgoing`), so `e.source == node_id` always holds here — we only
/// inspect the handle.
pub(super) fn error_path_wired(outgoing_edges: &[&WorkflowEdge]) -> bool {
    outgoing_edges
        .iter()
        .any(|e| e.source_handle.as_deref() == Some("error"))
}

/// True when `node` is the TERMINAL of a **Map** body — the child whose edge
/// enters the Map's `body_out` handle. The Map runs a body block per element and
/// gathers the results by `__map_id`, so a terminal child must fork its FULL
/// completed envelope (`park_outputs`) — the slim `split_outputs` control token
/// carries neither `detail.outputs.<resultVar>` nor the `__map_idx`/`__map_id`
/// correlation leaves. Single source of truth shared by every body kind that can
/// sit at such a terminal (AutomatedStep inline, SubWorkflow).
///
/// `outgoing_edges` are the edges whose `source == node_id` (see `graph::outgoing`).
pub(super) fn is_map_body_terminal(
    graph: &WorkflowGraph,
    parent_id: Option<&str>,
    outgoing_edges: &[&WorkflowEdge],
) -> bool {
    parent_id.is_some_and(|pid| {
        crate::compiler::token_shape::is_map_node(graph, pid)
            && outgoing_edges
                .iter()
                .any(|e| e.target == pid && e.target_handle.as_deref() == Some("body_out"))
    })
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
    interfaces: &InterfaceRegistry,
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
            // Stages a `role: tool` message as the `pending` delta with the
            // child's output payload. The transcript itself lives off-token:
            // the next prepare_call ships `pending` as a staged input and the
            // worker folds it into the cumulative blob. State stays inside the
            // agent — the workflow token (which the child may have stripped)
            // is irrelevant once we have the tool result.
            //
            // CRITICAL — read the child's PARKED data, not its control token.
            // Any parked producer (SubWorkflow, AutomatedStep) splits its
            // output via `split_outputs`/`YIELD_LOGIC`: the real business
            // payload is parked write-once in `p_<child>_data`, while the
            // DEFAULT output place carries only the slim control token
            // (`_`-prefixed leaves + `task_id`/`status`). Feeding the model
            // the control token hands it an empty tool result, so it never
            // sees the child's actual return (e.g. a looked-up order id) and
            // can't chain to the next tool. We therefore READ-ARC the parked
            // data place (published on `interface.data_port`) for `result`,
            // and still CONSUME the control token as the "child done" firing
            // trigger (its presence gates the transition + clears it for any
            // re-dispatch). Non-parking children (no `data_port`) keep the
            // old behaviour: their default output IS the payload.
            let child_data_port = interfaces
                .get(&entry.child_id)
                .and_then(|i| i.data_port.clone());
            match (child_default_out, child_data_port) {
                (Some(child_out), Some(data_place_id)) => {
                    let p_data: PlaceHandle<DynamicToken> = PlaceHandle::external(data_place_id);
                    ctx.transition(
                        format!("t_{agent_id}_collect_{tn}"),
                        format!("{agent_label} - Collect {tn}"),
                    )
                    .auto_input("ctrl", &child_out)
                    .read_input("result", &p_data)
                    .auto_input("state", &wiring.p_state_in_tool)
                    .auto_output("state", &wiring.p_state)
                    .logic_rhai(r#"let s = state; s.pending = [#{ role: "tool", tool_call_id: s.pending_tool_call_id, content: result }]; s.message_count = s.message_count + 1; #{ state: s }"#.to_string())
                    .done();
                }
                (Some(child_out), None) => {
                    ctx.transition(
                        format!("t_{agent_id}_collect_{tn}"),
                        format!("{agent_label} - Collect {tn}"),
                    )
                    .auto_input("result", &child_out)
                    .auto_input("state", &wiring.p_state_in_tool)
                    .auto_output("state", &wiring.p_state)
                    .logic_rhai(r#"let s = state; s.pending = [#{ role: "tool", tool_call_id: s.pending_tool_call_id, content: result }]; s.message_count = s.message_count + 1; #{ state: s }"#.to_string())
                    .done();
                }
                (None, _) => {}
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
                            r#"let s = state; let inner = if type_of(err) == "map" {{ if "error" in err {{ err.error }} else if "err" in err {{ err.err }} else {{ err }} }} else {{ err }}; let msg = if type_of(inner) == "map" {{ if "message" in inner {{ inner.message }} else if "reason" in inner {{ inner.reason }} else {{ "tool error" }} }} else if type_of(inner) == "string" {{ inner }} else {{ "tool error" }}; s.pending = [#{{ role: "tool", tool_call_id: s.pending_tool_call_id, content: "tool '{tn}' failed: " + msg }}]; s.message_count = s.message_count + 1; #{{ state: s }}"#
                        ))
                        .done();
                    }
                    ToolErrorPolicy::Bubble => match &wiring.p_error {
                        // Wired: surface the tool failure on the agent's error
                        // handle (today's behavior, byte-identical).
                        Some(p_error) => {
                            ctx.transition(
                                format!("t_{agent_id}_collect_{tn}_bubble"),
                                format!("{agent_label} - Collect {tn} (error → bubble)"),
                            )
                            .auto_input("err", &child_err)
                            .auto_input("state", &wiring.p_state_in_tool)
                            .auto_output("error", p_error)
                            .logic_rhai("#{ error: err }".to_string())
                            .done();
                        }
                        // Unwired: the bubbled tool failure has no handler — crash
                        // the net. Still consume `{err, state}` so nothing strands,
                        // then `throw` (permanent ScriptError → NetFailed).
                        None => {
                            let msg = format!(
                                "agent '{agent_id}' tool '{tn}' failed (bubble) and no error handler is wired"
                            );
                            ctx.transition(
                                format!("t_{agent_id}_collect_{tn}_bubble"),
                                format!("{agent_label} - Collect {tn} (error → crash net)"),
                            )
                            .auto_input("err", &child_err)
                            .auto_input("state", &wiring.p_state_in_tool)
                            .logic_rhai(format!("throw \"{}\"", rhai_str_escape(&msg)))
                            .done();
                        }
                    },
                }
            }
        }
    }
    Ok(())
}

/// Drain queued Timeout body-cancellation fan-outs. For each Timeout, walk
/// its `body_child_ids` and consult `interfaces[child].cancellable` — if
/// populated, synthesize a per-child drain transition + matching
/// `<kind>_cancel` effect transition so the in-flight resource is reclaimed
/// when the Timeout's timer wins the race.
///
/// Race correctness: each drain reads the cancel_pulse (non-consuming) and
/// consumes the child's in-flight token. Two consumers race for that token
/// — the child's normal "complete" path AND this drain. Whichever wins
/// blocks the other.
///
/// Non-cancellable body children (Decision, ParallelSplit, Failure, ...
/// nodes whose `cancellable` field is `None`) are silently skipped — the
/// race-winner transitions on the Timeout still cut their downstream paths.
pub(crate) fn apply_timeout_cancel_fanouts(
    ctx: &mut Context,
    interfaces: &InterfaceRegistry,
    fanouts: &[crate::compiler::lower::timeout::TimeoutCancelFanout],
) -> Result<(), CompileError> {
    use crate::compiler::interface::CancelKind;

    for fan in fanouts {
        for child_id in &fan.body_child_ids {
            let iface = match interfaces.get(child_id) {
                Some(i) => i,
                None => continue, // child wasn't lowered (e.g. Trigger) — skip
            };
            let Some(spec) = iface.cancellable.as_ref() else {
                continue;
            };

            let timeout_id = &fan.timeout_id;
            let timeout_label = &fan.timeout_label;
            let cancel_pulse = &fan.cancel_pulse;
            let effect_errors = &fan.effect_errors;
            let corr = &spec.correlation_field;

            // The drain places: one per body child. These hold the cancel
            // effect's input token (a typed `<Kind>CancelInput`-shaped map
            // synthesized by the drain transition's Rhai).
            let p_drain_input: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{timeout_id}_drain_{child_id}_input"),
                format!("{timeout_label} - Cancel {child_id} (request)"),
            );
            let p_drain_done: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{timeout_id}_drain_{child_id}_done"),
                format!("{timeout_label} - Cancel {child_id} (acked)"),
            );

            // The in-flight place we're draining lives elsewhere in the net —
            // synthesize a typed handle here by id alone. Use DynamicToken so
            // the consume arc accepts whatever shape the child parked.
            let in_flight: PlaceHandle<DynamicToken> = PlaceHandle::external(spec.place_id.clone());

            // Build the per-kind cancel-input shape AND pick the engine
            // effect descriptor to fire afterwards.
            let (cancel_shape_rhai, descriptor) = match spec.kind {
                CancelKind::Human => {
                    // human_cancel needs task_id + place. The place defaults
                    // to the child's signal place (`p_{child}_signal`),
                    // which is where the child's response would have been
                    // delivered.
                    let signal_place = format!("p_{child_id}_signal");
                    (
                        format!(
                            "#{{ task: #{{ task_id: token.{corr}, place: \"{signal_place}\" }} }}"
                        ),
                        &petri_domain::effects::HUMAN_CANCEL,
                    )
                }
                CancelKind::Executor => (
                    format!("#{{ job: #{{ execution_id: token.{corr} }} }}"),
                    &petri_domain::effects::EXECUTOR_CANCEL,
                ),
                CancelKind::Scheduler => (
                    format!("#{{ job: #{{ scheduler_job_id: token.{corr} }} }}"),
                    &petri_domain::effects::SCHEDULER_CANCEL,
                ),
                CancelKind::Timer => {
                    // timer_cancel takes both correlation_id + target_place_id.
                    let extra = spec
                        .extra_field
                        .as_deref()
                        .unwrap_or("target_place_id");
                    (
                        format!(
                            "#{{ timer: #{{ timer_correlation_id: token.{corr}, target_place_id: token.{extra} }} }}"
                        ),
                        &petri_domain::effects::TIMER_CANCEL,
                    )
                }
                CancelKind::SubWorkflow => (
                    format!(
                        "#{{ cancel: #{{ child_net_id: token.{corr}, reason: \"parent_timeout\" }} }}"
                    ),
                    &petri_domain::effects::SUBWORKFLOW_CANCEL,
                ),
            };

            // Drain transition: read-arc the cancel_pulse (non-consuming
            // gate so the pulse can fan out to many drains), consume the
            // child's in-flight token (racing with the child's own
            // success transition — only one wins), emit the cancel
            // effect input. The Rhai output port name matches the effect
            // descriptor's default_input_port: "task" for human_cancel,
            // "job" for executor/scheduler, "timer" for timer_cancel,
            // "cancel" for subworkflow_cancel.
            ctx.transition(
                format!("t_{timeout_id}_drain_{child_id}"),
                format!("{timeout_label} - Drain {child_id} on timeout"),
            )
            .auto_input("token", &in_flight)
            .read_input("pulse", cancel_pulse)
            .auto_output(descriptor.default_input_port, &p_drain_input)
            .logic_rhai(cancel_shape_rhai)
            .done();

            // Cancel effect transition: fire the matching <kind>_cancel
            // handler. Errors drain to the Timeout's effect_errors place.
            ctx.transition(
                format!("t_{timeout_id}_drain_{child_id}_effect"),
                format!("{timeout_label} - Cancel Effect {child_id}"),
            )
            .auto_input(descriptor.default_input_port, &p_drain_input)
            .auto_output(descriptor.default_output_port, &p_drain_done)
            .error_output(effect_errors)
            .builtin_effect(descriptor);
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
