//! `WorkflowNodeData::AutomatedStep` lowering. Three dispatch arms:
//!
//! - `lower_automated_step` (the public entry) — inline executor lifecycle
//!   for the normal `DeploymentModel::Inline` path; offloads the static
//!   config to the per-node side-channel and emits a slim `config_ref`
//!   Rhai literal.
//! - `lower_automated_step_scheduled` — `DeploymentModel::Scheduled` jobs
//!   that submit through the long-lived scheduler-net.
//! - `lower_engine_effect` — backends whose `DispatchMode::EngineEffect`
//!   maps to a registered engine builtin (e.g. `catalogue_lookup`).

use super::*;

pub(crate) fn lower_automated_step(cx: &mut LoweringCtx) -> Result<(), CompileError> {
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
            if let crate::backends::DispatchMode::EngineEffect { handler } = decl.meta.dispatch_mode {
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
