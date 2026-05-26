//! Per-node lowering: each workflow node type expands into its Petri
//! places/transitions via the [`NodeLowering`] trait. [`expand_node`] is the
//! thin dispatch; the real work lives in one `lower_*` function per variant.

use crate::compiler::compile::SubWorkflowAir;
use crate::compiler::error::CompileError;
use crate::compiler::interface::{InterfaceRegistry, NodeInterface, NodeKind, OutputKey};
use crate::compiler::well_known;
use crate::compiler::rhai_gen::{
    build_join_merge_logic, build_join_merge_logic_full, build_join_passthrough_logic,
    build_merge_logic, build_retry_topology, interpolate_to_rhai_expr, json_to_rhai_literal,
    rhai_str_escape, with_pluck_prelude,
};
use crate::compiler::token_shape::YIELD_LOGIC;
use crate::models::template::{
    DeploymentModel, ExecutionBackendType, FieldMapping, JoinMode, PhaseUpdateStatus, Port,
    ResourceConfig, WorkflowEdge, WorkflowNode, WorkflowNodeData,
};
use aithericon_executor_domain::InputSource;
use aithericon_sdk::components::executor_lifecycle::{executor_lifecycle, ExecutorBridges};
use aithericon_sdk::{
    effects, Context, DynamicToken, EffectError, ExecutorSubmitInput, HumanTaskAssigned,
    HumanTaskRequest, HumanTaskResponse, HumanTaskSubmit, PlaceHandle,
};
use serde_json::json;
use std::collections::HashMap;

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
/// preview (`POST /api/compile`) and compiler tests.
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
            WorkflowNodeData::Start { .. } => lower_start(cx),
            WorkflowNodeData::End { .. } => lower_end(cx),
            WorkflowNodeData::HumanTask { .. } => lower_human_task(cx),
            WorkflowNodeData::AutomatedStep { .. } => lower_automated_step(cx),
            WorkflowNodeData::Decision { .. } => lower_decision(cx),
            WorkflowNodeData::ParallelSplit { .. } => lower_parallel_split(cx),
            WorkflowNodeData::ParallelJoin { .. } => lower_parallel_join(cx),
            WorkflowNodeData::Join { .. } => lower_join(cx),
            WorkflowNodeData::Loop { .. } => lower_loop(cx),
            WorkflowNodeData::Scope { .. } => lower_scope(cx),
            WorkflowNodeData::PhaseUpdate { .. } => lower_phase_update(cx),
            WorkflowNodeData::ProgressUpdate { .. } => lower_progress_update(cx),
            WorkflowNodeData::Failure { .. } => lower_failure(cx),
            WorkflowNodeData::SubWorkflow { .. } => lower_subworkflow(cx),
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

fn lower_start(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Start {
        label,
        process_name,
        initial,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_start on non-Start node")
    };
    let ctx = &mut *cx.ctx;

    // Initial tokens are seeded per-Start at instance creation time by
    // `parameterize_air` into `p_{id}_ready` (it strips the `_ready`
    // suffix to find the place). That place id must stay stable.
    let place_id = format!("p_{id}_ready");
    let ready: PlaceHandle<DynamicToken> = ctx.state(&place_id, label);

    // Head of the Start's output chain *before* any artifact
    // registration: the bare ready place, or the tail of the optional
    // process-registration chain.
    let head: PlaceHandle<DynamicToken> = match process_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Default: single-place Start, no process registration.
        None => ready.clone(),
        // Opt-in: derive a per-instance process name from the Start
        // inputs and register a named HPI process via the
        // `process_start` effect. The causality projector
        // (`enrich_processes_from_start_event`) maps the effect
        // result's `name` onto the auto-discovered process row.
        Some(tpl) => {
            // 1. Rhai: copy the seed token, add `_process_name` from
            //    the `{{ field }}` template (resolved at run time
            //    against the token, same safe accessor infra as
            //    human-task interpolation).
            let named: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_named"), format!("{label} - Named"));
            let name_expr = interpolate_to_rhai_expr(tpl);
            ctx.transition(
                format!("t_{id}_proc_name"),
                format!("{label} - Derive Process Name"),
            )
            .auto_input("input", &ready)
            .auto_output("output", &named)
            // `.logic_rhai` (not `.logic`): the builder's inline
            // validator doesn't model `fn` parameters, so the
            // `__pluck` helper's params read as undefined. Same path
            // `wire_edge`/ParallelJoin already use for helper-fn
            // scripts; the engine still parses it at scenario load.
            .logic_rhai(with_pluck_prelude(&format!(
                "let d = input; d._process_name = {name_expr}; #{{ output: d }}"
            )))
            .done();

            // 2. process_start effect: register the process. The
            //    handler reads the name from `_process_name`
            //    (`name_field`) and forwards the full token onward
            //    via `forward_ports: ["main"]` so the workflow
            //    continues with its data intact. The small `process`
            //    token is parked in an internal place (Mekhan's
            //    projector uses causality tags + the effect result,
            //    not this token).
            let proc_out: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_ready_out"), format!("{label} - Output"));
            let proc_sink: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_process"), format!("{label} - Process"));
            ctx.transition(
                format!("t_{id}_proc_start"),
                format!("{label} - Register Process"),
            )
            .auto_input("trigger", &named)
            .auto_output("process", &proc_sink)
            .auto_output("main", &proc_out)
            .process_start(json!({
                "name": label,
                "name_field": "_process_name",
                "forward_ports": ["main"],
            }));

            // Hand the ProcessStarted token place to the End arm so
            // it can complete the same process (read-arc, non-consuming
            // → every End node can complete it independently).
            cx.fixups.process_token_place = Some(proc_sink.clone());

            proc_out
        }
    };

    // Artifact registration: iff the Start declares ≥1 file-upload
    // input, insert a synthetic chain between the Start (post
    // process-start) and the rest of the graph that registers each
    // uploaded file into the catalogue. One segment per file field;
    // a Rhai "shape" transition passes the workflow token through
    // unchanged on `pass` and emits a per-file artifact token on
    // `artifact` (only when the file is actually present), which a
    // reused `catalogue_register` effect consumes (its output is
    // parked, like the process_start `process` sink). With no file
    // inputs nothing is emitted and the compiled output is identical.
    let file_fields: Vec<&str> = initial
        .fields
        .iter()
        .filter(|f| f.kind == crate::models::template::FieldKind::File)
        .map(|f| f.name.as_str())
        .collect();

    let tail: PlaceHandle<DynamicToken> = if file_fields.is_empty() {
        head
    } else {
        let mut prev = head;
        for (i, &fname) in file_fields.iter().enumerate() {
            // ── Places ──────────────────────────────────────────────
            // `cat_out`  : workflow token continues here immediately.
            // `cat_desc` : per-file descriptor (S3 key + catalogue
            //              identity), produced only when the file is
            //              actually present.
            // `cat_art`  : the `catalogue_register` input shape; fed
            //              by the fmeta fold (success) or the degraded
            //              fold (extraction failure).
            // `cat_done` : parked effect output.
            let cat_out: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_cat_out_{i}"),
                format!("{label} - After Artifact {i}"),
            );
            let cat_desc: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_cat_desc_{i}"),
                format!("{label} - Artifact {i} Descriptor"),
            );
            let cat_art: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_cat_art_{i}"),
                format!("{label} - Artifact {i}"),
            );
            let cat_done: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_cat_done_{i}"),
                format!("{label} - Artifact {i} Catalogued"),
            );
            // fmeta branch plumbing (created outside the lifecycle
            // scope so their ids stay stable and the fold/degrade
            // transitions can reference them).
            let fmeta_inbox: PlaceHandle<ExecutorSubmitInput> = ctx.state(
                format!("p_{id}_fmeta_inbox_{i}"),
                format!("{label} - fmeta {i} Inbox"),
            );
            let fmeta_result: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_fmeta_result_{i}"),
                format!("{label} - fmeta {i} Result"),
            );
            let fmeta_fail: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_fmeta_fail_{i}"),
                format!("{label} - fmeta {i} Failure"),
            );
            let fmeta_park: PlaceHandle<DynamicToken> = ctx.state(
                format!("p_{id}_fmeta_park_{i}"),
                format!("{label} - fmeta {i} Descriptor (parked)"),
            );

            // Split: `pass` always carries the unchanged workflow
            // token onward (the workflow never waits for fmeta);
            // `artifact` is a flat descriptor emitted only when the
            // file is present. `_instance_id` (injected into every
            // Start token) keys the per-run dedup id. Omitting
            // `artifact` when the file is absent/null produces no
            // token for that port (route_output_tokens only emits
            // produced ports), so an optional file isn't registered.
            ctx.transition(
                format!("t_{id}_cat_shape_{i}"),
                format!("{label} - Shape Artifact {i}"),
            )
            .auto_input("tok", &prev)
            .auto_output("pass", &cat_out)
            .auto_output("artifact", &cat_desc)
            .logic(format!(
                r#"let d = tok;
let fv = d["{fname}"];
if type_of(fv) == "map" && fv.key != () {{
  #{{
    pass: d,
    artifact: #{{
      execution_id: d._instance_id,
      artifact_id: "start-" + d._instance_id + "-{fname}",
      name: fv.filename,
      mime_type: fv.content_type,
      size_bytes: fv.size,
      storage_path: fv.key
    }}
  }}
}} else {{
  #{{ pass: d }}
}}"#
            ));

            // Build the FileOps `probe` job (runs fmeta against the
            // uploaded blob; `storage` is omitted so the executor
            // uses its globally-configured default store). The job
            // id == artifact_id, unique per instance per field — the
            // correlation key that re-joins the parked descriptor
            // with the executor result. The descriptor is parked so
            // the upload's authoritative name/mime/size/path survive
            // the round-trip (the lifecycle drops everything except
            // job_id/run/detail).
            ctx.transition(
                format!("t_{id}_fmeta_submit_{i}"),
                format!("{label} - fmeta {i} Submit"),
            )
            .auto_input("desc", &cat_desc)
            .auto_output("job", &fmeta_inbox)
            .auto_output("keep", &fmeta_park)
            .logic(
                r#"let dd = desc;
let eid = dd.artifact_id;
#{
  job: #{
    job_id: eid,
    run: 0,
    retries: 0,
    max_retries: 0,
    execution_id: eid,
    spec: #{
      backend: "file_ops",
      inputs: [],
      outputs: [],
      config: #{ operation: "probe", path: dd.storage_path }
    }
  },
  keep: #{
    job_id: eid,
    execution_id: dd.execution_id,
    artifact_id: dd.artifact_id,
    name: dd.name,
    mime_type: dd.mime_type,
    size_bytes: dd.size_bytes,
    storage_path: dd.storage_path
  }
}"#,
            );

            // Reuse the full executor lifecycle (submit → status →
            // result/failure forwarding) for the probe. Scoped so
            // its fixed internal ids don't collide across fields or
            // with AutomatedStep lifecycles.
            let dead_letter = ctx.scoped_prefix(
                format!("{id}_fmeta_{i}"),
                format!("{label} - fmeta {i}"),
                |ctx| {
                    executor_lifecycle(
                        ctx,
                        ExecutorBridges {
                            inbox: fmeta_inbox.clone(),
                            result_out: Some(fmeta_result.clone()),
                            failure_out: Some(fmeta_fail.clone()),
                            process_id: None,
                            process_step: None,
                            catalogue: false,
                            process: false,
                        },
                    )
                    .dead_letter
                },
            );

            // Effect/infra errors land in the lifecycle's dead-letter
            // terminal. Reshape them onto the failure place so the
            // artifact is still catalogued (degraded, no
            // file_metadata) rather than lost.
            ctx.transition(
                format!("t_{id}_fmeta_dl_{i}"),
                format!("{label} - fmeta {i} Dead Letter"),
            )
            .auto_input("dead", &dead_letter)
            .auto_output("out", &fmeta_fail)
            .logic(
                r#"#{ out: #{ job_id: dead.job_id, reason: if dead.reason != () { dead.reason } else { "dead_letter" } } }"#,
            );

            // Success: merge the extracted fmeta JSON into
            // `detail.file_metadata` and emit the fully-annotated
            // `catalogue_register` input. Correlate the parked
            // descriptor with the executor result by job_id.
            ctx.transition(
                format!("t_{id}_fmeta_fold_{i}"),
                format!("{label} - fmeta {i} Fold"),
            )
            .auto_input("res", &fmeta_result)
            .auto_input("kept", &fmeta_park)
            .correlate("res", "kept", "job_id")
            .auto_output("artifact", &cat_art)
            .logic(
                r#"#{
  artifact: #{
    execution_id: kept.execution_id,
    detail: #{
      artifact_id: kept.artifact_id,
      name: kept.name,
      category: "input",
      mime_type: kept.mime_type,
      size_bytes: kept.size_bytes,
      storage_path: kept.storage_path,
      file_metadata: res.detail.outputs.metadata
    }
  }
}"#,
            );

            // Failure/timeout/dead-letter: register the artifact
            // anyway, without file_metadata. Still a single INSERT,
            // so catalogue subscriptions/triggers stay sane.
            ctx.transition(
                format!("t_{id}_fmeta_degrade_{i}"),
                format!("{label} - fmeta {i} Degrade"),
            )
            .auto_input("fail", &fmeta_fail)
            .auto_input("kept", &fmeta_park)
            .correlate("fail", "kept", "job_id")
            .auto_output("artifact", &cat_art)
            .logic(
                r#"#{
  artifact: #{
    execution_id: kept.execution_id,
    detail: #{
      artifact_id: kept.artifact_id,
      name: kept.name,
      category: "input",
      mime_type: kept.mime_type,
      size_bytes: kept.size_bytes,
      storage_path: kept.storage_path
    }
  }
}"#,
            );

            // Unchanged from Phase 1: the INSERT-only catalogue
            // effect, now deferred to the tail of the artifact
            // branch (the net is the staging ground — only annotated
            // entries reach the catalogue on the happy path).
            ctx.transition(
                format!("t_{id}_cat_reg_{i}"),
                format!("{label} - Register Artifact {i}"),
            )
            .auto_input("artifacts", &cat_art)
            .auto_output("catalogued", &cat_done)
            .builtin_effect(&effects::CATALOGUE_REGISTER);

            prev = cat_out;
        }
        prev
    };

    // Foundation fork: park a write-once copy of the Start token so
    // downstream guards / result-mappings can borrow `start.<field>`
    // (read-arc), exactly like a HumanTask/AutomatedStep. Unlike
    // `split_outputs` we do NOT slim the forwarded token — the
    // immediately-following task still interpolates Start fields off the
    // control token (`{{ invoice_id }}`), so we fork rather than split.
    let (data_place_id, p_main) = park_outputs(ctx, id, label, &tail);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: ready,
            output_places: vec![(None, p_main)],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    // Protocol: publish the interface (derives entry + outputs from ports),
    // then enrich with `data_port` since Start is a borrow-reachable parked
    // producer (see `interface.rs` contract table).
    cx.publish_interface().data_port = Some(data_place_id);
    Ok(())
}

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
fn result_mapping_rhai(mappings: &[FieldMapping]) -> (String, String) {
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

fn lower_end(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::End {
        label,
        result_mapping,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_end on non-End node")
    };
    let (rv_lets, rv_val) = result_mapping_rhai(result_mapping);
    let has_result = !result_mapping.is_empty();
    let ctx = &mut *cx.ctx;

    // Incoming edges always land in `p_{id}_done` — keep that id
    // stable (edge wiring + pass-through merges key off the End's
    // input_place).
    let done_id = format!("p_{id}_done");
    let done: PlaceHandle<DynamicToken> = ctx.state(&done_id, label);

    // When (and only when) a result is declared, insert a pure-Rhai shape
    // transition between the stable `p_{id}_done` and the terminal feed that
    // stamps the success envelope onto `exit_code`. The `if "exit_code" in`
    // guard is the Failure→End precedence rule: a token that already flowed
    // through a Failure node keeps its `{ ok: false }` envelope; only an
    // otherwise-unstamped token gets `{ ok: true, value }`. Skipped entirely
    // for bare End so the terminal token (and the instance `result`) is
    // byte-identical to pre-feature behavior.
    let (terminal_feed, terminal_feed_id) = if has_result {
        let shaped: PlaceHandle<DynamicToken> =
            ctx.state(format!("p_{id}_result"), format!("{label} - Result"));
        let logic = format!(
            "{rv_lets}let __rv = {rv_val}; let __out = input; \
             if \"exit_code\" in __out {{ }} \
             else {{ __out.exit_code = #{{ ok: true, value: __rv }}; }} \
             #{{ result: __out }}"
        );
        ctx.transition(
            format!("t_{id}_result_shape"),
            format!("{label} - Result"),
        )
        .auto_input("input", &done)
        .auto_output("result", &shaped)
        .logic_rhai(with_pluck_prelude(&logic))
        .done();
        (shaped, format!("p_{id}_result"))
    } else {
        (done.clone(), done_id)
    };

    let terminal_id = match cx.fixups.process_token_place.clone() {
        // No process was registered by the Start (opt-in unused) —
        // the terminal feed is itself the terminal place.
        None => terminal_feed_id,
        // A Start registered a process — mirror the Start pattern:
        // insert a `process_complete` effect between the (post-shape)
        // feed place and a new terminal. The handler reads `process_id`
        // from the parked `ProcessStarted` token via a read-arc
        // (non-consuming, so multiple End nodes each complete), passes
        // the workflow token through unchanged (so any stamped
        // `exit_code` survives), and the causality projector picks up
        // `completed: true`.
        Some(proc_place) => {
            let completed: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_completed"), format!("{label} - Completed"));
            ctx.transition(
                format!("t_{id}_proc_complete"),
                format!("{label} - Complete Process"),
            )
            .read_input("process", &proc_place)
            .auto_input("done", &terminal_feed)
            .auto_output("completed", &completed)
            .process_complete();

            format!("p_{id}_completed")
        }
    };

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: done,
            output_places: vec![],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    // Protocol: publish (derives entry from ports) then enrich with the
    // workflow-exit terminal. Recorded pre-alias-collapse; `compile.rs`
    // rewrites every interface place id through the alias map post-merge.
    cx.publish_interface().workflow_terminals.push(terminal_id);
    Ok(())
}

/// Foundation: split a data-yielding node's output into a write-once parked
/// **data** place + a slim **control** place, joined by a `t_{id}_yield`
/// transition. Generalizes the Start-parks-`ProcessStarted` precedent to
/// every HumanTask/AutomatedStep. Returns the parked-data place id (the
/// caller publishes on `interface.data_port`) and the control place (the
/// node's new downstream output). Schema refs are left as the default
/// permissive `DynamicToken`; the post-merge phase upgrades the data/ctrl
/// `token_schema` to the typed `#/definitions/*` and registers them.
fn split_outputs(
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
fn park_outputs(
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

fn lower_human_task(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::HumanTask { label, .. } = &cx.node.data else {
        unreachable!("lower_human_task on non-HumanTask node")
    };
    let scope_group = cx.fixups.scope_groups.get(id).cloned();
    let ctx = &mut *cx.ctx;

    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_active: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_active"), format!("{label} - Active"));
    let p_signal: PlaceHandle<DynamicToken> =
        ctx.signal(format!("p_{id}_signal"), format!("{label} - Signal"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_errors: PlaceHandle<EffectError> =
        ctx.state(format!("p_{id}_errors"), format!("{label} - Errors"));

    // t_{id}_request — human_task effect (typed contract)
    let ht_input = p_input.clone().retyped::<HumanTaskRequest>();
    let ht_active = p_active.clone().retyped::<HumanTaskAssigned>();
    let ht_signal = p_signal.clone().retyped::<HumanTaskResponse>();
    ctx.transition(
        format!("t_{id}_request"),
        format!("{label} - Request Human Task"),
    )
    .human_task_to(HumanTaskSubmit {
        task: &ht_input,
        assigned: &ht_active,
        errors: &p_errors,
        response_signal: &ht_signal,
    });

    // t_{id}_finalize — merge signal data into token (SDK correlate)
    ctx.transition(format!("t_{id}_finalize"), format!("{label} - Finalize"))
        .auto_input("state", &p_active)
        .auto_input("signal", &p_signal)
        .correlate("signal", "state", "task_id")
        .auto_output("done", &p_output)
        .logic(build_merge_logic("state", "signal"));

    // Foundation split: park the full human-task output, forward slim control.
    let (data_place_id, p_ctrl) = split_outputs(ctx, id, label, &p_output);

    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![(None, p_ctrl)],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    // HumanTask is a parked producer: split_outputs forks into a data
    // envelope (borrowable via `<slug>.<field>`) + a slim control token.
    cx.publish_interface().data_port = Some(data_place_id);
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
fn declared_outputs_rhai(backend: ExecutionBackendType, output: &Port) -> String {
    let backend_consumes_declared = matches!(
        backend,
        ExecutionBackendType::Python
            | ExecutionBackendType::Kreuzberg
            | ExecutionBackendType::Llm
    );
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

fn lower_automated_step(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    // Scheduled steps dispatch through the long-lived scheduler-net instead of
    // the inline executor lifecycle. Delegated early so the inline path below
    // is byte-identical to pre-feature behaviour (guarded by
    // `automated_step_inline_unchanged`). `matches!` drops the borrow
    // immediately so the delegate can take `cx` mutably.
    if matches!(
        &cx.node.data,
        WorkflowNodeData::AutomatedStep {
            deployment_model: DeploymentModel::Scheduled { .. },
            ..
        }
    ) {
        return lower_automated_step_scheduled(cx);
    }

    // Engine-effect backends (e.g. CatalogueQuery → `catalogue_lookup`):
    // no executor job, lower to the engine's registered builtin effect
    // instead of the executor lifecycle. The handler ID is sourced from
    // the backend decl's `DispatchMode::EngineEffect { handler }` so
    // future engine-effect backends only need a new registry entry.
    if let WorkflowNodeData::AutomatedStep { execution_spec, .. } = &cx.node.data {
        if let Some(decl) = crate::backends::lookup(execution_spec.backend_type) {
            if let crate::backends::DispatchMode::EngineEffect { handler } = decl.dispatch_mode {
                return lower_engine_effect(cx, handler);
            }
        }
    }

    let id = &cx.node.id;
    let WorkflowNodeData::AutomatedStep {
        label,
        execution_spec,
        retry_policy,
        output,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_automated_step on non-AutomatedStep node")
    };

    // Validate and transform editor config → executor format (before closure)
    let backend_type = &execution_spec.backend_type;
    let (mut validated_config, staged_inputs) =
        crate::compiler::backend_configs::validate_and_transform(
            backend_type,
            &execution_spec.config,
            cx.node_files,
            id,
        )?;
    // Inline `{"$ref": "#/definitions/<name>"}` against the workflow-level
    // `definitions` map. After this, the value rhai-literal'd into the job
    // spec is fully self-contained — backends never see a `$ref`. The
    // pre-lowering `validate_schema_refs` pass already surfaced unresolved
    // refs with node id + JSON path, so a failure here would be a logic
    // bug (validation drifted from inlining); still propagate cleanly.
    crate::compiler::schema_refs::inline_refs(&mut validated_config, cx.definitions)
        .map_err(|e| CompileError::SchemaRefUnresolved {
            node_id: id.clone(),
            path: String::new(),
            message: e.to_string(),
        })?;
    // Offload the static config to the per-node side-channel; the publish
    // path uploads it to S3 (see `service::process::publish`), and the
    // executor's `FetchConfigHook` materialises it back into `spec.config`
    // before backend dispatch. The Rhai literal stays a tiny `config_ref`
    // — no more Rhai expression-complexity panics on deeply-nested
    // response_format schemas, and no more multi-KB tokens on every
    // job-firing NATS message.
    let storage_key = cx.config_storage.key(id);
    cx.node_configs.insert(id.clone(), validated_config);
    let config_ref_rhai = format!(
        "#{{ \"storage_path\": \"{}\" }}",
        rhai_str_escape(&storage_key)
    );
    let inputs_rhai =
        json_to_rhai_literal(&serde_json::to_value(&staged_inputs).unwrap_or_default());
    let outputs_rhai = declared_outputs_rhai(*backend_type, output);

    let max_retries = retry_policy.max_retries;
    let ctx = &mut *cx.ctx;

    // Node interface places (outside prefix scope)
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_error: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_error"), format!("{label} - Error"));

    // Scoped prefix: all lifecycle IDs become "{id}/submitted", "{id}/completed", etc.
    let handles = ctx.scoped_prefix(id, label, |ctx| {
        let exec_inbox = ctx.state::<ExecutorSubmitInput>("inbox", "Inbox");
        // Second handle to the same place so the retry path can
        // re-inject a fresh submit after the lifecycle moves `inbox`.
        let exec_inbox_retry = exec_inbox.clone();

        // Snapshot the upstream token into `input.json` — the single
        // accumulating workflow token user code reads via the SDK
        // (`aithericon.token()` / generated `load_input()`); the
        // staged-file name is an implementation detail. Rhai's
        // copy-on-write semantics mean `input` here is the pre-mutation
        // value even though `d` was aliased to it just above.
        // `stream_events` opts the executor into emitting
        // mid-execution metric/progress/phase/log events as NATS
        // signals. Without it the executor builds no StreamContext and
        // streams nothing, so the lifecycle's process_log_* effects
        // (enabled via `process: true` below) would never fire and
        // user metrics/logs would not reach hpi_metrics / hpi_logs.
        // The `/*__BORROWED_INPUTS__*/` marker is a Rhai block comment
        // (no-op until rewritten). For Python AutomatedSteps, post-merge
        // `apply_control_data_foundation` replaces it with one
        // `job_inputs.push(#{ name: "<slug>.json", source: { inline, d_<producer> } })`
        // per `<slug>.<field>` reference detected in the Python source —
        // so the runtime stages the producer's parked data alongside
        // `input.json` and the runner exposes `<slug>` as a Python global.
        // The sentinel survives a no-op replacement (empty pushes) cleanly.
        ctx.transition("prepare", format!("{label} - Prepare"))
            .auto_input("input", &p_input)
            .auto_output("job", &exec_inbox)
            .logic(format!(
                r#"let d = input; d.job_id = "{id}"; d.run = 0; d.retries = 0; d.max_retries = {max_retries}; let job_inputs = {inputs_rhai}; job_inputs.push(#{{ "name": "input.json", "source": #{{ "type": "inline", "value": input }} }}); /*__BORROWED_INPUTS__*/ d.spec = #{{ "backend": "{backend_type}", "inputs": job_inputs, "outputs": {outputs_rhai}, "config_ref": {config_ref_rhai}, "stream_events": ["metric", "progress", "phase", "log"] }}; #{{ job: d }}"#
            ));

        let lc = executor_lifecycle(
            ctx,
            ExecutorBridges {
                inbox: exec_inbox,
                result_out: None,
                failure_out: None,
                process_id: None,
                process_step: None,
                catalogue: true,
                // Route streamed metric/log/phase/progress events through
                // process_log_metric / process_log_message so Mekhan's
                // causality consumer projects them into hpi_metrics /
                // hpi_logs against the causality-discovered process.
                process: true,
            },
        );

        // Wire the lifecycle's failure/timeout outputs into a
        // retry-then-error policy. Re-dispatch goes back through the
        // lifecycle inbox (a fresh executor submit), which is valid
        // for Mekhan's long-lived worker backends.
        build_retry_topology(
            ctx,
            retry_policy,
            &lc.failed,
            &lc.timed_out,
            &exec_inbox_retry,
            &lc.effect_errors,
            &p_error,
        );

        lc
    });

    // Bridge lifecycle outputs to node interface
    ctx.transition(format!("t_{id}_to_output"), format!("{label} - To Output"))
        .auto_input("done", &handles.completed)
        .auto_output("output", &p_output)
        .logic(r#"#{ output: done }"#);

    // Infra-level effect-handler errors (NATS/dispatch) still drain to
    // the node error output; job-level failures are handled by the
    // retry topology above.
    ctx.transition(format!("t_{id}_to_error"), format!("{label} - To Error"))
        .auto_input("dead", &handles.dead_letter)
        .auto_output("error", &p_error)
        .logic(r#"#{ error: dead }"#);

    // Foundation split: park the executor result envelope as write-once data,
    // forward only the slim control token on the success path. The error
    // path is not a data token (it routes to error handlers) — left as-is.
    let (data_place_id, p_ctrl) = split_outputs(ctx, id, label, &p_output);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            // Slim control success output + the unchanged named "error"
            // output. An edge from the node's error handle (source_handle
            // == "error") wires to `p_error` via `find_output_place`; if no
            // error edge exists `p_error` simply has no consumer.
            output_places: vec![
                (None, p_ctrl),
                (Some("error".to_string()), p_error),
            ],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    // AutomatedStep is a parked producer: borrow `<slug>.<field>` reads
    // through the data port.
    cx.publish_interface().data_port = Some(data_place_id);
    Ok(())
}

/// `Scheduled` AutomatedStep: submit a `SchedulerSubmitInput` to the
/// long-lived scheduler-net (`well_known::SCHEDULER_NET_ID`) and take the
/// result / failure back on its named reply channels. The scheduler-net owns
/// queueing, the Nomad/Slurm job template (`job_template_id`), resource
/// allocation, and **retry/backoff** for queued execution — so the workflow
/// net does not re-run a scheduled job itself; a scheduler failure routes
/// straight to the node's error output. No `scoped_prefix` (the topology is
/// small and `p_{id}_*` / `t_{id}_*` ids are already node-unique), so the
/// reply-channel place names line up with `bridge_out_reply_channels`.
fn lower_automated_step_scheduled(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = cx.node.id.clone();
    let WorkflowNodeData::AutomatedStep {
        label,
        execution_spec,
        deployment_model,
        output,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_automated_step_scheduled on non-AutomatedStep node")
    };
    let label = label.clone();
    let DeploymentModel::Scheduled {
        job_template,
        resources,
    } = deployment_model
    else {
        unreachable!("lower_automated_step_scheduled on non-Scheduled step")
    };
    let job_template = job_template.clone();
    let resources: Option<ResourceConfig> = resources.clone();
    let backend_type = execution_spec.backend_type;

    let (mut validated_config, staged_inputs) =
        crate::compiler::backend_configs::validate_and_transform(
            &backend_type,
            &execution_spec.config,
            cx.node_files,
            &id,
        )?;
    crate::compiler::schema_refs::inline_refs(&mut validated_config, cx.definitions)
        .map_err(|e| CompileError::SchemaRefUnresolved {
            node_id: id.clone(),
            path: String::new(),
            message: e.to_string(),
        })?;
    // Side-channel the static config to the publish layer — see the
    // parallel offload in `lower_automated_step` for the rationale.
    let storage_key = cx.config_storage.key(&id);
    cx.node_configs.insert(id.clone(), validated_config);
    let config_ref_rhai = format!(
        "#{{ \"storage_path\": \"{}\" }}",
        rhai_str_escape(&storage_key)
    );
    let inputs_rhai =
        json_to_rhai_literal(&serde_json::to_value(&staged_inputs).unwrap_or_default());
    let resources_rhai = json_to_rhai_literal(
        &serde_json::to_value(&resources).unwrap_or(serde_json::Value::Null),
    );
    let outputs_rhai = declared_outputs_rhai(backend_type, output);
    let backend_wire = backend_type.as_wire_str();
    let job_template_lit = rhai_str_escape(&job_template);
    let id_lit = rhai_str_escape(&id);

    let ctx = &mut *cx.ctx;

    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_error: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_error"), format!("{label} - Error"));

    // Named reply-channel places the scheduler routes back to.
    let result_place = format!("p_{id}_sched_result");
    let failure_place = format!("p_{id}_sched_failure");
    let sched_result: PlaceHandle<DynamicToken> = ctx.bridge_reply_channel(
        result_place.clone(),
        format!("{label} - Scheduler Result"),
        "result",
    );
    let sched_failure: PlaceHandle<DynamicToken> = ctx.bridge_reply_channel(
        failure_place.clone(),
        format!("{label} - Scheduler Failure"),
        "failure",
    );
    let sched_out: PlaceHandle<DynamicToken> = ctx.bridge_out_reply_channels(
        format!("p_{id}_sched_out"),
        format!("{label} - Submit to Scheduler"),
        well_known::SCHEDULER_NET_ID,
        well_known::SCHEDULER_JOB_QUEUE,
        &[
            ("result", result_place.as_str()),
            ("failure", failure_place.as_str()),
        ],
    );

    // prepare: snapshot the upstream token into `input.json` and wrap it as a
    // SchedulerSubmitInput { job_id, model_name, run, retries, max_retries,
    // job_template_id, spec{ backend, inputs, outputs, config, resources } }.
    // See `lower_automated_step` for the `/*__BORROWED_INPUTS__*/` marker —
    // same Python-slug staging story for the scheduled lifecycle.
    ctx.transition(format!("t_{id}_prepare"), format!("{label} - Prepare"))
        .auto_input("input", &p_input)
        .auto_output("job", &sched_out)
        .logic(format!(
            r#"let d = #{{}}; d.job_id = "{id_lit}"; d.model_name = "{id_lit}"; d.run = 0; d.retries = 0; d.max_retries = 0; d.job_template_id = "{job_template_lit}"; let job_inputs = {inputs_rhai}; job_inputs.push(#{{ "name": "input.json", "source": #{{ "type": "inline", "value": input }} }}); /*__BORROWED_INPUTS__*/ d.spec = #{{ "backend": "{backend_wire}", "inputs": job_inputs, "outputs": {outputs_rhai}, "config_ref": {config_ref_rhai}, "resources": {resources_rhai}, "stream_events": ["metric", "progress", "phase", "log"] }}; #{{ job: d }}"#
        ));

    ctx.transition(format!("t_{id}_to_output"), format!("{label} - To Output"))
        .auto_input("res", &sched_result)
        .auto_output("output", &p_output)
        .logic(r#"#{ output: res }"#);

    ctx.transition(format!("t_{id}_to_error"), format!("{label} - To Error"))
        .auto_input("fail", &sched_failure)
        .auto_output("error", &p_error)
        .logic(r#"#{ error: fail }"#);

    // Same data/control split + port registration tail as the inline path.
    let (data_place_id, p_ctrl) = split_outputs(ctx, &id, &label, &p_output);
    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![
                (None, p_ctrl),
                (Some("error".to_string()), p_error),
            ],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface().data_port = Some(data_place_id);
    Ok(())
}

/// Engine-effect backend lowering. Used by AutomatedSteps whose
/// `DispatchMode` is `EngineEffect { handler }` (CatalogueQuery today;
/// future engine-effect backends just register a new decl with a
/// different handler string and reuse this path).
///
/// No executor job / lifecycle / retry — we build the normalized input
/// token from the editor config (via `validate_and_transform`) and fire
/// the named engine builtin effect against the descriptor's
/// `default_input_port` / `default_output_port` (e.g. for
/// `catalogue_lookup`: input port `query`, output `results`), mirroring
/// how `lower_start` emits `catalogue_register`.
///
/// `handler` is the engine-side `EffectDescriptor::handler_id`. Resolved
/// via `effects::builtin_by_id`; a missing handler is a compile-time
/// (well, registry-time) bug — the decl declares a handler the engine
/// doesn't expose. The catalogue_query parity test catches the only
/// existing case end-to-end; future engine-effect backends ship with
/// their own decl + parity assertion.
fn lower_engine_effect(cx: &mut LoweringCtx, handler: &str) -> Result<(), CompileError> {
    let id = cx.node.id.clone();
    let WorkflowNodeData::AutomatedStep {
        label,
        execution_spec,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_engine_effect on non-AutomatedStep node")
    };
    let label = label.clone();
    let backend_type = execution_spec.backend_type;

    let descriptor = effects::builtin_by_id(handler).ok_or_else(|| {
        CompileError::Compilation(format!(
            "engine-effect lowering: handler '{handler}' (declared by {backend_type:?}) is not a registered builtin"
        ))
    })?;
    let input_port = descriptor.default_input_port;
    let output_port = descriptor.default_output_port;

    let (mut query_token, _no_inputs) =
        crate::compiler::backend_configs::validate_and_transform(
            &backend_type,
            &execution_spec.config,
            cx.node_files,
            &id,
        )?;
    crate::compiler::schema_refs::inline_refs(&mut query_token, cx.definitions)
        .map_err(|e| CompileError::SchemaRefUnresolved {
            node_id: id.clone(),
            path: String::new(),
            message: e.to_string(),
        })?;
    let query_rhai = json_to_rhai_literal(&query_token);

    let ctx = &mut *cx.ctx;
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_query: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_query"), format!("{label} - Query"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Results"));
    let p_error: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_error"), format!("{label} - Error"));

    // Build the effect-input token from the (validated) editor config. The
    // inbound workflow token is consumed but not used — engine-effect
    // backends are authored, not data-driven, in v1.
    ctx.transition(
        format!("t_{id}_q_build"),
        format!("{label} - Build Query"),
    )
    .auto_input("input", &p_input)
    .auto_output(input_port, &p_query)
    // The inbound token is consumed by the arc; the query is authored, not
    // data-driven (v1), so the logic ignores `input` and emits the token.
    .logic(format!("#{{ {input_port}: {query_rhai} }}"));

    // Fire the registered builtin effect (input `<input_port>` →
    // `<output_port>`). For catalogue_query this is
    // `catalogue_lookup` with `query` → `results`.
    ctx.transition(
        format!("t_{id}_lookup"),
        format!("{label} - Catalogue Lookup"),
    )
    .auto_input(input_port, &p_query)
    .auto_output(output_port, &p_output)
    .builtin_effect(descriptor);

    let (data_place_id, p_ctrl) = split_outputs(ctx, &id, &label, &p_output);
    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![
                (None, p_ctrl),
                (Some("error".to_string()), p_error),
            ],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface().data_port = Some(data_place_id);
    Ok(())
}

fn lower_decision(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Decision {
        label,
        conditions,
        default_branch,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_decision on non-Decision node")
    };
    let ctx = &mut *cx.ctx;

    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));

    let mut output_places = Vec::new();

    // Normalize once: a blank guard means "always match".
    let guards: Vec<String> = conditions
        .iter()
        .map(|c| {
            if c.guard.trim().is_empty() {
                "true".to_string()
            } else {
                c.guard.clone()
            }
        })
        .collect();
    let n = guards.len();

    // One transition per condition. Precedence is declaration order: branch i
    // fires only when its own guard holds AND every higher-precedence guard
    // does not (switch/case fallthrough). This keeps at most one branch (or
    // the default) enabled per token, so ordering is structural and does not
    // hinge on the engine's enabling-time / specificity / id tiebreak — which
    // can otherwise be skewed by read-arcs injected for borrowed guard data.
    // The descending priority is a redundant, declarative encoding of the same
    // order (and keeps default below every branch, dead-end below default).
    for (i, cond) in conditions.iter().enumerate() {
        let p_out: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_out_{i}"),
            format!("{label} - {}", cond.label),
        );

        let guard = if i == 0 {
            format!("({})", guards[0])
        } else {
            let exclude = (0..i)
                .rev()
                .map(|j| format!("!({})", guards[j]))
                .collect::<Vec<_>>()
                .join(" && ");
            format!("({}) && {exclude}", guards[i])
        };

        ctx.transition(
            format!("t_{id}_branch_{i}"),
            format!("{label} - {}", cond.label),
        )
        .auto_input("input", &p_input)
        .auto_output("output", &p_out)
        .guard_rhai(guard)
        .priority(format!("{}", n - i + 1))
        .logic_rhai("#{ output: input }")
        .done();

        output_places.push((Some(cond.edge_id.clone()), p_out));
    }

    // Default branch: the cascade's terminal `else` — enabled only when no
    // branch guard matched. With zero conditions it stays unconditional
    // (preserves the historical always-route behavior).
    if let Some(default_edge_id) = default_branch {
        let p_default: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_out_default"),
            format!("{label} - Default"),
        );

        let t = ctx
            .transition(format!("t_{id}_default"), format!("{label} - Default"))
            .auto_input("input", &p_input)
            .auto_output("output", &p_default)
            .priority("1");
        let t = if n > 0 {
            let none_match = (0..n)
                .map(|j| format!("!({})", guards[j]))
                .collect::<Vec<_>>()
                .join(" && ");
            t.guard_rhai(none_match)
        } else {
            t
        };
        t.logic_rhai("#{ output: input }").done();

        output_places.push((Some(default_edge_id.clone()), p_default));
    }

    // Unroutable token (no branch matched and no default, or a guard threw at
    // runtime so its negation poisoned the cascade) -> explicit, observable
    // net error instead of a silently stranded token. Unguarded + lowest
    // priority so it only ever wins when nothing else is enabled. The `throw`
    // is a permanent ScriptError: the engine emits ErrorOccurred and consumes
    // the token (no infinite re-fire).
    let deadend_msg =
        format!("decision {label}: token matched no branch and no default branch");
    ctx.transition(
        format!("t_{id}_deadend"),
        format!("{label} - No Match (error)"),
    )
    .auto_input("input", &p_input)
    .priority("0")
    .logic_rhai(format!("throw \"{}\"", rhai_str_escape(&deadend_msg)))
    .done();

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places,
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface();
    Ok(())
}

fn lower_parallel_split(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::ParallelSplit { label, .. } = &cx.node.data else {
        unreachable!("lower_parallel_split on non-ParallelSplit node")
    };
    let outgoing_edges = cx.outgoing_edges;
    let ctx = &mut *cx.ctx;

    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));

    // Pre-create output places before starting the transition builder
    let mut output_places: Vec<(Option<String>, PlaceHandle<DynamicToken>)> = Vec::new();
    for (i, edge) in outgoing_edges.iter().enumerate() {
        let p_out: PlaceHandle<DynamicToken> =
            ctx.state(format!("p_{id}_out_{i}"), format!("{label} - Fork {i}"));
        output_places.push((Some(edge.id.clone()), p_out));
    }

    // Build the transition with multiple outputs
    let mut tb = ctx
        .transition(format!("t_{id}_fork"), format!("{label} - Fork"))
        .auto_input("input", &p_input);

    for (i, (_, p_out)) in output_places.iter().enumerate() {
        let port_name = format!("out_{i}");
        tb = tb.auto_output(&port_name, p_out);
    }

    // Build Rhai source that duplicates input to all output ports
    let port_names: Vec<String> = (0..outgoing_edges.len())
        .map(|i| format!("out_{i}"))
        .collect();
    let rhai_entries: Vec<String> = port_names
        .iter()
        .map(|name| format!("{name}: input"))
        .collect();
    let rhai_source = format!("#{{ {} }}", rhai_entries.join(", "));

    tb.logic_rhai(rhai_source).done();

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places,
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface();
    Ok(())
}

fn lower_parallel_join(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::ParallelJoin {
        label,
        merge_strategy,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_parallel_join on non-ParallelJoin node")
    };
    let merge_strategy = *merge_strategy;
    let incoming_edges = cx.incoming_edges;
    let ctx = &mut *cx.ctx;

    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));

    // Pre-create input places before starting the transition builder
    let mut input_place_ids: Vec<(Option<String>, PlaceHandle<DynamicToken>)> = Vec::new();
    for (i, edge) in incoming_edges.iter().enumerate() {
        let p_in: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_in_{i}"),
            format!("{label} - Join Input {i}"),
        );
        input_place_ids.push((Some(edge.id.clone()), p_in));
    }

    // Build the transition with multiple inputs
    let mut tb = ctx.transition(format!("t_{id}_join"), format!("{label} - Join"));

    for (i, (_, p_in)) in input_place_ids.iter().enumerate() {
        let port_name = format!("in_{i}");
        tb = tb.auto_input(&port_name, p_in);
    }

    tb = tb.auto_output("output", &p_output);

    // Build Rhai merge logic: merge all inputs into one output
    let port_names: Vec<String> = (0..incoming_edges.len())
        .map(|i| format!("in_{i}"))
        .collect();
    let rhai_source = build_join_merge_logic(&port_names, merge_strategy);

    tb.logic_rhai(rhai_source).done();

    // Build edge_id -> input_place mapping for wire_edge to resolve
    let join_input_map: HashMap<String, PlaceHandle<DynamicToken>> = input_place_ids
        .iter()
        .filter_map(|(edge_id, place)| edge_id.as_ref().map(|eid| (eid.clone(), place.clone())))
        .collect();

    let default_input = input_place_ids
        .first()
        .map(|(_, p)| p.clone())
        .unwrap_or_else(|| ctx.state(format!("p_{id}_in_fallback"), "Fallback"));

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: default_input,
            output_places: vec![(None, p_output)],
            input_places: join_input_map,
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface();
    Ok(())
}

/// Unified `Join` lowering. `mode == All` mirrors `lower_parallel_join`'s
/// AND-join (one transition consuming every input place, payloads merged per
/// `merge_strategy`). `mode == Any` is the canonical petri-net XOR-join: N
/// transitions, one per incoming branch, each consuming a single input place
/// and depositing into a *shared* output place (and a shared parked data
/// place). For both modes each branch's inbound payload lands at the parked
/// `p_<id>_data` so downstream `<slug>.<field>` borrows resolve via the
/// standard read-arc pipeline (interface `data_port = p_<id>_data`).
fn lower_join(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Join {
        label,
        mode,
        merge_strategy,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_join on non-Join node")
    };
    let mode = *mode;
    let merge_strategy = merge_strategy.unwrap_or_default();
    let incoming_edges = cx.incoming_edges;
    let ctx = &mut *cx.ctx;

    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_data: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_data"),
        format!("{label} - Parked data"),
    );

    // Pre-create one input place per incoming edge so wire.rs can route each
    // edge to its dedicated input.
    let mut input_place_ids: Vec<(Option<String>, PlaceHandle<DynamicToken>)> = Vec::new();
    for (i, edge) in incoming_edges.iter().enumerate() {
        let p_in: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_in_{i}"),
            format!("{label} - Join Input {i}"),
        );
        input_place_ids.push((Some(edge.id.clone()), p_in));
    }

    match mode {
        JoinMode::All => {
            // Single transition consuming from every input place. AND-fire:
            // requires a token in each branch before firing. Folds via the
            // selected MergeStrategy into the output place, and the merged
            // token also lands at the parked `p_<id>_data` place.
            let mut tb = ctx.transition(format!("t_{id}_join"), format!("{label} - Join"));
            for (i, (_, p_in)) in input_place_ids.iter().enumerate() {
                tb = tb.auto_input(&format!("in_{i}"), p_in);
            }
            tb = tb
                .auto_output("output", &p_output)
                .auto_output("data", &p_data);

            let port_names: Vec<String> = (0..incoming_edges.len())
                .map(|i| format!("in_{i}"))
                .collect();
            let rhai_source = build_join_merge_logic_full(&port_names, merge_strategy, true);
            tb.logic_rhai(rhai_source).done();
        }
        JoinMode::Any => {
            // N transitions, one per branch. Each consumes its dedicated input
            // place and deposits into the shared output + shared parked data
            // place. Per-branch logic is a single-input passthrough.
            for (i, (_, p_in)) in input_place_ids.iter().enumerate() {
                let port_name = format!("in_{i}");
                let rhai_source = build_join_passthrough_logic(&port_name);
                ctx.transition(
                    format!("t_{id}_join_{i}"),
                    format!("{label} - Join branch {i}"),
                )
                .auto_input(&port_name, p_in)
                .auto_output("output", &p_output)
                .auto_output("data", &p_data)
                .logic_rhai(rhai_source)
                .done();
            }
        }
    }

    // Build edge_id -> input_place mapping so wire.rs can resolve each
    // inbound edge to its dedicated input place (same shape used by
    // ParallelJoin).
    let join_input_map: HashMap<String, PlaceHandle<DynamicToken>> = input_place_ids
        .iter()
        .filter_map(|(edge_id, place)| edge_id.as_ref().map(|eid| (eid.clone(), place.clone())))
        .collect();

    let default_input = input_place_ids
        .first()
        .map(|(_, p)| p.clone())
        .unwrap_or_else(|| ctx.state(format!("p_{id}_in_fallback"), "Fallback"));

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: default_input,
            output_places: vec![(None, p_output)],
            input_places: join_input_map,
            input_handles: HashMap::new(),
        },
    );
    // Publish the data port so `<slug>.<field>` borrows resolve through the
    // standard read-arc machinery (matches SubWorkflow / Loop / AutomatedStep).
    cx.publish_interface().data_port = Some(format!("p_{id}_data"));
    Ok(())
}

fn lower_loop(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Loop {
        label,
        max_iterations,
        loop_condition,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_loop on non-Loop node")
    };

    // A Loop must contain at least one body node — a child with
    // `parent_id == loop.id`. An empty Loop (iterate-N-times-doing-nothing)
    // isn't a useful workflow primitive; reject it at publish so the editor
    // can ring the offending container. If a delay/heartbeat is ever needed,
    // add a dedicated Delay node — don't fold two semantics into Loop.
    if cx.children.is_empty() {
        return Err(CompileError::LoopEmpty {
            node_id: id.clone(),
        });
    }

    let scope_group = cx.fixups.scope_groups.get(id).cloned();
    let ctx = &mut *cx.ctx;

    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_body_in: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_body_in"), format!("{label} - Body In"));
    let p_body_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_body_out"), format!("{label} - Body Out"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));

    // Loop iteration counter lives in a *parked* `p_{id}_data` place —
    // independent of the workflow token. This is required for AutomatedStep
    // bodies: the executor envelope (`t_<step>_to_output = #{ output: done }`)
    // strips every workflow-token key (only `job_id`/`run`/`execution_id`/
    // `detail`/`source`/`status` survive). If the counter rode on the token,
    // it would die at the first AutomatedStep body and the loop's own continue
    // guard would fail (input.<slug>.iteration → undefined). Parking the
    // counter makes Loop the source of truth, addressable by:
    //   - Loop's own continue/exit guards (pre-wired d_<slug> binding here).
    //   - Post-loop Rhai consumers (End mappings, Decision guards) via the
    //     standard `<slug>.<field>` borrow resolution (resolve_ref Qualified
    //     branch returns Borrow for Loop).
    //   - Body / post-loop Python AutomatedSteps via automated_step_borrow_plan
    //     (is_parked_producer recognizes Loop), which stages `<slug>.json`
    //     and promotes the namespace as a Python global.
    let slug = cx.node.slug();
    let d_slug = format!("d_{}", id.replace('-', "_"));
    let p_data: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_data"),
        format!("{label} - Iteration Counter (parked)"),
    );

    // Pre-wire the consuming arc + input port `d_<slug>` on continue and the
    // read-arc on exit — both binding `p_<id>_data` for the parked counter.
    // Guards and logic stay in the user-source `<slug>.iteration` form; the
    // standard (c) read-arc synthesis pass picks them up via Borrow
    // resolution and rewrites them to `d_<slug>.iteration` with word-
    // boundary matching (so the pre-wired port name `d_<slug>` doesn't get
    // double-prefixed). The (c) pass's "any arc to this place" guard then
    // leaves the pre-wired arcs alone (skipping the read-arc add that would
    // otherwise duplicate). One rewrite pipeline, one binding name.

    // t_{id}_enter — initialize the parked counter, hand off to body via
    // p_body_in. The workflow token (input) passes through unchanged: no
    // namespace addition. Body children's outgoing edges back to the loop
    // carry `targetHandle: "body_out"` (wire.rs routes those to p_body_out
    // via `input_handles`); the body's incoming edge from the loop carries
    // `sourceHandle: "body_in"` (wire.rs routes from p_body_in via the
    // matching entry in `output_places`).
    ctx.transition(format!("t_{id}_enter"), format!("{label} - Enter Loop"))
        .auto_input("input", &p_input)
        .auto_output("body", &p_body_in)
        .auto_output("data", &p_data)
        .logic_rhai("#{ body: input, data: #{ iteration: 0 } }".to_string())
        .done();

    // t_{id}_continue — loop back: consume body_out + the parked counter,
    // increment, produce a fresh body_in token AND a new parked counter.
    // The token is forwarded unchanged (body can do whatever to it — even
    // strip everything via an AutomatedStep envelope — and the loop still
    // works because the counter lives in `d_<slug>`, not the token).
    ctx.transition(format!("t_{id}_continue"), format!("{label} - Continue"))
        .auto_input("input", &p_body_out)
        .auto_input(d_slug.clone(), &p_data)
        .auto_output("body", &p_body_in)
        .auto_output("data", &p_data)
        .guard_rhai(format!(
            "{slug}.iteration < {max_iterations} && ({loop_condition})"
        ))
        .logic_rhai(format!(
            "#{{ body: input, data: #{{ iteration: {slug}.iteration + 1 }} }}"
        ))
        .done();

    // t_{id}_exit — read-arc the counter (non-consuming, so it stays parked
    // for post-loop consumers' `<slug>.iteration` borrows), forward the
    // body's final token unchanged.
    ctx.transition(format!("t_{id}_exit"), format!("{label} - Exit"))
        .auto_input("input", &p_body_out)
        .read_input(d_slug.clone(), &p_data)
        .auto_output("output", &p_output)
        .guard_rhai(format!(
            "{slug}.iteration >= {max_iterations} || !({loop_condition})"
        ))
        .logic_rhai("#{ output: input }")
        .done();

    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    let mut input_handles = HashMap::new();
    input_handles.insert("body_out".to_string(), p_body_out);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            // Two source-handle outputs: default (None) is the loop's outer
            // `out` (post-exit); "body_in" is the inner handle that feeds
            // body children when they receive a token from the loop.
            output_places: vec![(None, p_output), (Some("body_in".to_string()), p_body_in)],
            input_places: HashMap::new(),
            input_handles,
        },
    );
    // Loop is a parked producer: the iteration counter is stored as a
    // write-once-per-iteration envelope at `p_{id}_data`, schemed as
    // `Data__{id}` by the foundation pass and used by the read-arc
    // synthesis to route `<slug>.iteration` references downstream.
    cx.publish_interface().data_port = Some(format!("p_{id}_data"));
    Ok(())
}

fn lower_scope(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Scope { label, .. } = &cx.node.data else {
        unreachable!("lower_scope on non-Scope node")
    };
    // Scope compiles to a ScenarioGroup. No places/transitions of its own —
    // children are compiled as normal nodes and tagged with this group's ID
    // via the centralised scope-tagging pass in `compile.rs`.
    let group_id = format!("grp_{id}");
    let parent_group = cx.fixups.scope_groups.get(id).cloned();
    cx.fixups.groups.push((group_id, label.clone(), parent_group));
    // Protocol: publish the interface even though Scope has no boundary
    // places (kind alone marks its presence; ownership is filled centrally).
    cx.publish_interface();
    Ok(())
}

fn lower_phase_update(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::PhaseUpdate {
        label,
        phase_name,
        status,
        message,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_phase_update on non-PhaseUpdate node")
    };
    let ctx = &mut *cx.ctx;

    // Pass-through: the shape transition forwards the workflow token
    // unchanged on `out` and emits a canonical serialized
    // `StatusDetail::PhaseChanged` (the `event_type`-tagged form) on
    // `sig`; the effect transition runs the typed `process_phase`
    // effect, whose `effect_result` is the verbatim `StatusDetail`. The
    // causality consumer deserializes it whole and projects into
    // `hpi_processes.config.progress.phases`. The process is resolved
    // by tag propagation from the consumed (process-tagged) token —
    // no read-arc needed; outside a named process this is a no-op.
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_pu_out"), format!("{label} - Output"));
    let p_sig: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_pu_sig"),
        format!("{label} - Phase Detail"),
    );
    let p_done: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_pu_done"), format!("{label} - Recorded"));

    let name_expr = interpolate_to_rhai_expr(phase_name);
    let status_lit = match status {
        PhaseUpdateStatus::Running => "running",
        PhaseUpdateStatus::Completed => "completed",
        PhaseUpdateStatus::Failed => "failed",
        PhaseUpdateStatus::Skipped => "skipped",
    };
    // Bind interpolations to locals so the map literal stays shallow
    // (avoids the debug-build Rhai expr-depth limit) — same shape as
    // the Start `process_name` transition.
    let (msg_let, detail_msg) = match message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(m) => {
            let e = interpolate_to_rhai_expr(m);
            (
                format!("let __mg = {e}; "),
                ", message: __mg".to_string(),
            )
        }
        None => (String::new(), String::new()),
    };
    let logic = format!(
        "let __pn = {name_expr}; {msg_let}#{{ out: input, sig: #{{ \
         event_type: \"phase_changed\", phase_name: __pn, \
         status: \"{status_lit}\"{detail_msg} }} }}"
    );
    ctx.transition(
        format!("t_{id}_pu_shape"),
        format!("{label} - Phase Update"),
    )
    .auto_input("input", &p_input)
    .auto_output("out", &p_out)
    .auto_output("sig", &p_sig)
    .logic_rhai(with_pluck_prelude(&logic))
    .done();

    ctx.transition(format!("t_{id}_pu_emit"), format!("{label} - Record Phase"))
        .auto_input("phase", &p_sig)
        .auto_output("recorded", &p_done)
        .builtin_effect(&effects::PROCESS_PHASE);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![(None, p_out)],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface();
    Ok(())
}

fn lower_progress_update(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::ProgressUpdate {
        label,
        fraction,
        message,
        current_step,
        total_steps,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_progress_update on non-ProgressUpdate node")
    };
    let ctx = &mut *cx.ctx;

    // Pass-through: the shape transition forwards the token on `out`
    // and emits a canonical serialized `StatusDetail::ProgressUpdated`
    // (the `event_type`-tagged form) on `sig`; the effect transition
    // runs the typed `process_progress` effect, whose `effect_result`
    // is the verbatim `StatusDetail`. The causality consumer
    // deserializes it whole and projects into
    // `hpi_processes.config.progress`. No-op outside a named process.
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_pu_out"), format!("{label} - Output"));
    let p_sig: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_pu_sig"),
        format!("{label} - Progress Detail"),
    );
    let p_done: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_pu_done"), format!("{label} - Recorded"));

    // f64 Debug always round-trips with a decimal point ("1.0", not
    // "1") so Rhai parses it as a float, matching the typed
    // `StatusDetail::ProgressUpdated.fraction`.
    let frac = format!("{fraction:?}");
    let cur = current_step.as_ref().map_or(0, |v| *v);
    let tot = total_steps.as_ref().map_or(0, |v| *v);
    let (msg_let, detail_msg) = match message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(m) => {
            let e = interpolate_to_rhai_expr(m);
            (format!("let __mg = {e}; "), ", message: __mg".to_string())
        }
        None => (String::new(), String::new()),
    };
    let logic = format!(
        "{msg_let}#{{ out: input, sig: #{{ \
         event_type: \"progress_updated\", fraction: {frac}, \
         current_step: {cur}, total_steps: {tot}{detail_msg} }} }}"
    );
    ctx.transition(
        format!("t_{id}_pu_shape"),
        format!("{label} - Progress Update"),
    )
    .auto_input("input", &p_input)
    .auto_output("out", &p_out)
    .auto_output("sig", &p_sig)
    .logic_rhai(with_pluck_prelude(&logic))
    .done();

    ctx.transition(
        format!("t_{id}_pu_emit"),
        format!("{label} - Record Progress"),
    )
    .auto_input("progress", &p_sig)
    .auto_output("recorded", &p_done)
    .builtin_effect(&effects::PROCESS_PROGRESS);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![(None, p_out)],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface();
    Ok(())
}

fn lower_failure(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Failure {
        label,
        failure_message,
        error_result_mapping,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_failure on non-Failure node")
    };
    let (er_lets, er_val) = result_mapping_rhai(error_result_mapping);
    let ctx = &mut *cx.ctx;

    // Pass-through: shape transition forwards the workflow token
    // unchanged on `out` (the net continues to its normal End) and
    // emits a `#{ reason }` breadcrumb on `fail`; the effect
    // transition runs the tolerant `process_fail` builtin. The
    // causality consumer resolves the owning process by tag
    // propagation from the consumed (process-tagged) token — no
    // read-arc; outside a named process this is a no-op.
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_out: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_fail_out"), format!("{label} - Output"));
    let p_sig: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_fail_sig"),
        format!("{label} - Failure Breadcrumb"),
    );
    let p_done: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_fail_done"), format!("{label} - Failed"));

    // Bind the interpolation to a local so the map literal stays
    // shallow (debug-build Rhai expr-depth limit) — same shape as the
    // PhaseUpdate / ProgressUpdate arms.
    let (msg_let, reason_val) = match failure_message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(m) => {
            let e = interpolate_to_rhai_expr(m);
            (format!("let __fm = {e}; "), "__fm".to_string())
        }
        None => (String::new(), "\"\"".to_string()),
    };
    // Beyond the original `#{ reason }` breadcrumb, stamp the error envelope
    // onto the forwarded token's `exit_code`. Reaching a Failure node *is* a
    // business-failure declaration, so this is unconditional — the net keeps
    // running to its normal End, whose result-shape guard (`if "exit_code"
    // in`) then preserves this envelope instead of overwriting it. Every map
    // literal is bound to a shallow local first (debug-build Rhai expr-depth
    // limit) — same recipe as PhaseUpdate's `__mg`.
    let logic = format!(
        "{msg_let}{er_lets}let __er = {er_val}; \
         let __ec = #{{ reason: {reason_val}, value: __er }}; \
         let __out = input; __out.exit_code = #{{ ok: false, error: __ec }}; \
         #{{ out: __out, fail: #{{ reason: {reason_val} }} }}"
    );
    ctx.transition(format!("t_{id}_fail_shape"), format!("{label} - Failure"))
        .auto_input("input", &p_input)
        .auto_output("out", &p_out)
        .auto_output("fail", &p_sig)
        .logic_rhai(with_pluck_prelude(&logic))
        .done();

    ctx.transition(
        format!("t_{id}_fail_emit"),
        format!("{label} - Fail Process"),
    )
    .auto_input("failure", &p_sig)
    .auto_output("failed", &p_done)
    .builtin_effect(&effects::PROCESS_FAIL);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![(None, p_out)],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface();
    Ok(())
}

/// Sub-workflow call/return. Reuses the engine's `spawn_net` machinery: the
/// child template is recursively compiled + made spawn-callable at the
/// *parent's* publish time and supplied via `cx.sub_air` (frozen per
/// `version_pin`). We emit the same parent-side topology `Context::spawn`
/// produces — request → spawn effect (child AIR embedded in `effect_config`)
/// → `bridge_out` of the mapped initial token to a freshly spawned child,
/// with the child's terminal result returning on a `bridge_in` reply place
/// and its failure on a `bridge_in` failure place. The callable contract
/// guarantees the child exposes the fixed boundary places `inbox`
/// (bridge_in), `reply_out` (bridge_reply) and `fail_out` (bridge_out_param).
///
/// Sequential call/return is the intended pattern (the parent waits for the
/// child result before continuing — exactly the BO campaign loop). Concurrent
/// in-flight invocations of the *same* SubWorkflow node are not supported in
/// v1; reply routing is per parent-instance via `ReplyRouting.reply_to`.
fn lower_subworkflow(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::SubWorkflow {
        label,
        template_id,
        input_mapping,
        output,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_subworkflow on non-SubWorkflow node")
    };

    // The child AIR is resolved + made-callable + frozen by the publish/preview
    // handler. Absent ⇒ this graph was compiled through a path that doesn't
    // resolve sub-workflows (back-compat `compile_to_air`); surface it keyed to
    // the node so the editor canvas rings it.
    let resolved = cx
        .sub_air
        .get(id)
        .ok_or_else(|| CompileError::SubWorkflowUnresolved {
            node_id: id.clone(),
            template_id: template_id.to_string(),
        })?;
    let child_air = resolved.air.clone();
    let child_scenario_name = format!("subworkflow_{id}_child");

    // input_mapping → the token bridged into the child's Start. Empty ⇒ the
    // inbound accumulating token passes through unchanged.
    let (im_lets, im_val) = result_mapping_rhai(input_mapping);
    let init_expr = if input_mapping.is_empty() {
        "input".to_string()
    } else {
        im_val
    };

    // Declared output port → how the child's terminal result maps back onto
    // the workflow token at the join. Empty fields ⇒ pass the child result
    // through opaquely (consistent with AutomatedStep's envelope semantics).
    //
    // The child's End node stamps its `resultMapping` under
    // `exit_code: { ok: true, value: <fields> }` on the workflow token (see
    // lower_end's result_shape transition). The terminal token that reaches
    // `reply_out` therefore carries the declared output fields nested at
    // `exit_code.value.<field>` — NOT at the top level. Reading
    // `reply[<field>]` worked transiently when the SDK's executor_lifecycle
    // terminals (`<step>/completed`) raced past the End and won the reply,
    // because that raw envelope had the executor's `outputs.<field>` at
    // depth-1; once publish.rs filters reply sources to End-derived terminals
    // only, the join MUST unwrap the `exit_code.value` envelope.
    let join_logic = if output.fields.is_empty() {
        r#"let __v = if "exit_code" in reply && type_of(reply.exit_code) == "map" && "value" in reply.exit_code { reply.exit_code.value } else { reply }; #{ output: __v }"#.to_string()
    } else {
        let entries: Vec<String> = output
            .fields
            .iter()
            .map(|f| {
                let k = serde_json::to_string(&f.name)
                    .unwrap_or_else(|_| "\"\"".to_string());
                format!("{k}: __v[{k}]")
            })
            .collect();
        format!(
            r#"let __v = if "exit_code" in reply && type_of(reply.exit_code) == "map" && "value" in reply.exit_code {{ reply.exit_code.value }} else {{ reply }}; #{{ output: #{{ {} }} }}"#,
            entries.join(", ")
        )
    };

    let ctx = &mut *cx.ctx;

    // Node interface places.
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_error: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_error"), format!("{label} - Error"));

    // Spawn request + confirmation.
    let p_request: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_request"),
        format!("{label} - Spawn Request"),
    );
    let p_spawned: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_spawned"), format!("{label} - Spawned"));

    // Bridge places — fixed callable contract on the child.
    let reply_place_id = format!("p_{id}_reply");
    let failure_place_id = format!("p_{id}_failure");
    // Plain `bridge_in`, NOT `bridge_in_from(child_scenario_name, …)`. The
    // child is spawned with a dynamic runtime net id — `subworkflow_{id}_child`
    // is never a statically-deployed net — so a static source-net annotation
    // makes the engine's Strict bridge check reject the parent instance at the
    // Running transition (BRIDGE_SOURCE_NET_MISSING). The annotation is UI-only
    // ("metadata for visualization … does not affect execution" per the SDK):
    // the spawn handler injects `parent_net_id` + `reply_place`/`failure_place`
    // so the child's reply_out/fail_out route back here by id at runtime —
    // exactly why the outbox below uses the dynamic `$result.child_net_id`.
    let p_reply: PlaceHandle<DynamicToken> = ctx.bridge_in(
        reply_place_id.clone(),
        format!("{label} - Reply"),
    );
    let p_failure: PlaceHandle<DynamicToken> = ctx.bridge_in(
        failure_place_id.clone(),
        format!("{label} - Failure"),
    );
    let p_outbox: PlaceHandle<DynamicToken> = ctx.bridge_out_labeled(
        format!("p_{id}_outbox"),
        format!("{label} - Outbox"),
        "$result.child_net_id",
        "inbox",
        Some(reply_place_id.clone()),
        child_scenario_name.clone(),
    );

    // Shape: upstream token → spawn request { initial_token, target_place }.
    ctx.transition(
        format!("t_{id}_shape"),
        format!("{label} - Prepare Sub-workflow"),
    )
    .auto_input("input", &p_input)
    .auto_output("spawn_request", &p_request)
    .logic_rhai(with_pluck_prelude(&format!(
        r#"{im_lets}let __ci = ({init_expr}); #{{ spawn_request: #{{ initial_token: __ci, target_place: "inbox" }} }}"#
    )))
    .done();

    // Spawn effect: embed the made-callable child AIR; the handler injects
    // `parent_net_id` and merges `reply_place`/`failure_place` into the child's
    // params so its boundary bridges resolve back to this parent instance.
    let effect_config = json!({
        "scenario": child_air,
        "parameters": {
            "reply_place": reply_place_id,
            "failure_place": failure_place_id,
        },
        "template_id": child_scenario_name,
    });
    ctx.transition(format!("t_{id}_spawn"), format!("{label} - Spawn Child"))
        .auto_input("spawn_request", &p_request)
        .auto_output("spawned", &p_spawned)
        .auto_output("bridge", &p_outbox)
        .effect_with_config(effects::SPAWN_NET.handler_id, effect_config);

    // Join success: child terminal result → node output (declared mapping).
    ctx.transition(format!("t_{id}_join"), format!("{label} - Join Result"))
        .auto_input("reply", &p_reply)
        .auto_output("output", &p_output)
        .logic(join_logic);

    // Failure: child failure → node error output.
    ctx.transition(
        format!("t_{id}_fail"),
        format!("{label} - On Child Failure"),
    )
    .auto_input("reply", &p_failure)
    .auto_output("error", &p_error)
    .logic(r#"#{ error: reply }"#);

    // Foundation split: park the child result as write-once data, forward the
    // slim control token. Identical tail to lower_automated_step.
    let (data_place_id, p_ctrl) = split_outputs(ctx, id, label, &p_output);

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![
                (None, p_ctrl),
                (Some("error".to_string()), p_error),
            ],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    // SubWorkflow is a parked producer: child's reply envelope is borrowable
    // as `<slug>.<field>` via the same read-arc machinery used for
    // AutomatedStep.
    cx.publish_interface().data_port = Some(data_place_id);
    Ok(())
}
