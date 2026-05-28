//! `WorkflowNodeData::Start` lowering. Builds the entry-point chain:
//! seed token → optional process-registration → optional artifact-registration
//! per file-upload input → parked-data fork (so downstream nodes can borrow
//! `start.<field>` via read-arc synthesis).

use super::*;

pub(crate) fn lower_start(cx: &mut LoweringCtx) -> Result<(), CompileError> {
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
            // `wire_edge`/Join already use for helper-fn
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
