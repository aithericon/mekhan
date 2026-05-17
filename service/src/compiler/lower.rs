//! Per-node lowering: each workflow node type expands into its Petri
//! places/transitions via the [`NodeLowering`] trait. [`expand_node`] is the
//! thin dispatch; the real work lives in one `lower_*` function per variant.

use crate::compiler::error::CompileError;
use crate::compiler::rhai_gen::{
    build_join_merge_logic, build_merge_logic, build_retry_topology, interpolate_to_rhai_expr,
    json_to_rhai_literal, with_pluck_prelude,
};
use crate::models::template::{
    PhaseUpdateStatus, WorkflowEdge, WorkflowNode, WorkflowNodeData,
};
use aithericon_executor_domain::InputSource;
use aithericon_sdk::components::executor_lifecycle::{executor_lifecycle, ExecutorBridges};
use aithericon_sdk::{
    effects, Context, DynamicToken, EffectError, ExecutorSubmitInput, HumanTaskAssigned,
    HumanTaskRequest, HumanTaskResponse, HumanTaskSubmit, PlaceHandle,
};
use serde_json::json;
use std::collections::HashMap;

/// Per-node, per-filename input source map. Built by the publish handler from
/// the node's Y.Doc files (resolved to S3 keys via `InputSource::StoragePath`)
/// or, for the stateless preview compile, materialized from inline content via
/// `InputSource::Raw`.
pub type NodeFiles = HashMap<String, HashMap<String, InputSource>>;

/// Instruction to merge `dead` place into `survivor` place.
/// All references to `dead` become references to `survivor`, then `dead` is removed.
pub(crate) struct PlaceMerge {
    pub(crate) dead: String,
    pub(crate) survivor: String,
}

/// Tracks post-processing fixups that must be applied after ctx.build().
#[derive(Default)]
pub(crate) struct PostProcess {
    /// Place IDs that should be changed to "terminal" type.
    pub(crate) terminal_place_ids: Vec<String>,
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
}

/// Everything a single node's lowering needs: the shared build `ctx`, the
/// accumulating `ports`/`fixups` maps, plus the node-local view (its node,
/// incident edges, staged files).
pub(crate) struct LoweringCtx<'a, 'c> {
    pub(crate) node: &'a WorkflowNode,
    pub(crate) outgoing_edges: &'a [&'a WorkflowEdge],
    pub(crate) incoming_edges: &'a [&'a WorkflowEdge],
    pub(crate) ctx: &'c mut Context,
    pub(crate) ports: &'c mut HashMap<String, NodePorts>,
    pub(crate) fixups: &'c mut PostProcess,
    pub(crate) node_files: &'a HashMap<String, InputSource>,
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
            WorkflowNodeData::Loop { .. } => lower_loop(cx),
            WorkflowNodeData::Scope { .. } => lower_scope(cx),
            WorkflowNodeData::PhaseUpdate { .. } => lower_phase_update(cx),
            WorkflowNodeData::ProgressUpdate { .. } => lower_progress_update(cx),
            WorkflowNodeData::Failure { .. } => lower_failure(cx),
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
pub(crate) fn expand_node(
    node: &WorkflowNode,
    outgoing_edges: &[&WorkflowEdge],
    incoming_edges: &[&WorkflowEdge],
    ctx: &mut Context,
    ports: &mut HashMap<String, NodePorts>,
    fixups: &mut PostProcess,
    node_files: &HashMap<String, InputSource>,
) -> Result<(), CompileError> {
    let mut cx = LoweringCtx {
        node,
        outgoing_edges,
        incoming_edges,
        ctx,
        ports,
        fixups,
        node_files,
    };
    node.lower(&mut cx)
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

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: ready,
            output_places: vec![(None, tail)],
            input_places: HashMap::new(),
        },
    );
    Ok(())
}

fn lower_end(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::End { label, .. } = &cx.node.data else {
        unreachable!("lower_end on non-End node")
    };
    let ctx = &mut *cx.ctx;

    // Incoming edges always land in `p_{id}_done` — keep that id
    // stable (edge wiring + pass-through merges key off the End's
    // input_place).
    let done_id = format!("p_{id}_done");
    let done: PlaceHandle<DynamicToken> = ctx.state(&done_id, label);

    match cx.fixups.process_token_place.clone() {
        // No process was registered by the Start (opt-in unused) —
        // the End is a bare terminal, unchanged behavior.
        None => {
            cx.fixups.terminal_place_ids.push(done_id);
        }
        // A Start registered a process — mirror the Start pattern:
        // insert a `process_complete` effect between the (stable)
        // incoming place and a new terminal. The handler reads
        // `process_id` from the parked `ProcessStarted` token via a
        // read-arc (non-consuming, so multiple End nodes each
        // complete), passes the workflow token through, and the
        // causality projector picks up `completed: true`.
        Some(proc_place) => {
            let completed: PlaceHandle<DynamicToken> =
                ctx.state(format!("p_{id}_completed"), format!("{label} - Completed"));
            ctx.transition(
                format!("t_{id}_proc_complete"),
                format!("{label} - Complete Process"),
            )
            .read_input("process", &proc_place)
            .auto_input("done", &done)
            .auto_output("completed", &completed)
            .process_complete();

            cx.fixups
                .terminal_place_ids
                .push(format!("p_{id}_completed"));
        }
    }

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: done,
            output_places: vec![],
            input_places: HashMap::new(),
        },
    );
    Ok(())
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

    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![(None, p_output)],
            input_places: HashMap::new(),
        },
    );
    Ok(())
}

fn lower_automated_step(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::AutomatedStep {
        label,
        execution_spec,
        retry_policy,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_automated_step on non-AutomatedStep node")
    };

    // Validate and transform editor config → executor format (before closure)
    let backend_type = &execution_spec.backend_type;
    let (validated_config, staged_inputs) =
        crate::compiler::backend_configs::validate_and_transform(
            backend_type,
            &execution_spec.config,
            cx.node_files,
        )?;
    let config_rhai = json_to_rhai_literal(&validated_config);
    let inputs_rhai =
        json_to_rhai_literal(&serde_json::to_value(&staged_inputs).unwrap_or_default());

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
        ctx.transition("prepare", format!("{label} - Prepare"))
            .auto_input("input", &p_input)
            .auto_output("job", &exec_inbox)
            .logic(format!(
                r#"let d = input; d.job_id = "{id}"; d.run = 0; d.retries = 0; d.max_retries = {max_retries}; let job_inputs = {inputs_rhai}; job_inputs.push(#{{ "name": "input.json", "source": #{{ "type": "inline", "value": input }} }}); d.spec = #{{ "backend": "{backend_type}", "inputs": job_inputs, "outputs": [], "config": {config_rhai}, "stream_events": ["metric", "progress", "phase", "log"] }}; #{{ job: d }}"#
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

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            // Default success output + a named "error" output. An edge
            // drawn from the node's error handle (source_handle ==
            // "error") wires to `p_error` via `find_output_place`; if
            // no error edge exists `p_error` simply has no consumer
            // (the prior dead-end-on-failure behaviour).
            output_places: vec![(None, p_output), (Some("error".to_string()), p_error)],
            input_places: HashMap::new(),
        },
    );
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

    // One transition per condition (competing transitions from the same input)
    for (i, cond) in conditions.iter().enumerate() {
        let p_out: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_out_{i}"),
            format!("{label} - {}", cond.label),
        );

        ctx.transition(
            format!("t_{id}_branch_{i}"),
            format!("{label} - {}", cond.label),
        )
        .auto_input("input", &p_input)
        .auto_output("output", &p_out)
        .guard_rhai(&cond.guard)
        .logic_rhai("#{ output: input }")
        .done();

        output_places.push((Some(cond.edge_id.clone()), p_out));
    }

    // Default branch (no guard)
    if let Some(default_edge_id) = default_branch {
        let p_default: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_out_default"),
            format!("{label} - Default"),
        );

        ctx.transition(format!("t_{id}_default"), format!("{label} - Default"))
            .auto_input("input", &p_input)
            .auto_output("output", &p_default)
            .logic_rhai("#{ output: input }")
            .done();

        output_places.push((Some(default_edge_id.clone()), p_default));
    }

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places,
            input_places: HashMap::new(),
        },
    );
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
        },
    );
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
        },
    );
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

    let counter_key = format!("_loop_{id}_count");

    // t_{id}_enter — initialize loop counter
    ctx.transition(format!("t_{id}_enter"), format!("{label} - Enter Loop"))
        .auto_input("input", &p_input)
        .auto_output("body", &p_body_in)
        .logic_rhai(format!(
            "let d = input; d.{counter_key} = 0; #{{ body: d }}"
        ))
        .done();

    // t_{id}_continue — loop back
    ctx.transition(format!("t_{id}_continue"), format!("{label} - Continue"))
        .auto_input("input", &p_body_out)
        .auto_output("body", &p_body_in)
        .guard_rhai(format!(
            "input.{counter_key} < {max_iterations} && ({loop_condition})"
        ))
        .logic_rhai(format!(
            "let d = input; d.{counter_key} = d.{counter_key} + 1; #{{ body: d }}"
        ))
        .done();

    // t_{id}_exit — exit loop
    ctx.transition(format!("t_{id}_exit"), format!("{label} - Exit"))
        .auto_input("input", &p_body_out)
        .auto_output("output", &p_output)
        .guard_rhai(format!(
            "input.{counter_key} >= {max_iterations} || !({loop_condition})"
        ))
        .logic_rhai("#{ output: input }")
        .done();

    cx.fixups
        .groups
        .push((format!("grp_{id}"), label.clone(), scope_group));

    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places: vec![(None, p_output)],
            input_places: HashMap::new(),
        },
    );
    Ok(())
}

fn lower_scope(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Scope { label, .. } = &cx.node.data else {
        unreachable!("lower_scope on non-Scope node")
    };
    // Scope compiles to a ScenarioGroup. No places/transitions —
    // children are compiled as normal nodes and tagged with this group's ID.
    let group_id = format!("grp_{id}");
    let parent_group = cx.fixups.scope_groups.get(id).cloned();
    cx.fixups.groups.push((group_id, label.clone(), parent_group));
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
        },
    );
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
        },
    );
    Ok(())
}

fn lower_failure(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let id = &cx.node.id;
    let WorkflowNodeData::Failure {
        label,
        failure_message,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_failure on non-Failure node")
    };
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
    let logic = format!("{msg_let}#{{ out: input, fail: #{{ reason: {reason_val} }} }}");
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
        },
    );
    Ok(())
}
