//! `WorkflowNodeData::AutomatedStep` lowering. Three dispatch arms:
//!
//! - `lower_automated_step` (the public entry) — executor-pool lifecycle
//!   for the normal `DeploymentModel::Executor` path; offloads the static
//!   config to the per-node side-channel and emits a slim `config_ref`
//!   Rhai literal.
//! - `lower_automated_step_scheduled` — `DeploymentModel::Scheduled` jobs
//!   that submit through the long-lived scheduler-net.
//! - `lower_engine_effect` — backends whose `DispatchMode::EngineEffect`
//!   maps to a registered engine builtin (e.g. `catalogue_lookup`).

use super::*;

pub(crate) fn lower_automated_step(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    // Dispatch on `deployment_model` (post-R3 consolidation: admission folded
    // into it). `matches!` drops the borrow immediately so each delegate can
    // take `cx` mutably.
    //
    //   - Scheduled { op: Submit } → lower_automated_step_scheduled (today's
    //                                scheduler-net path, byte-identical).
    //   - Scheduled { op: Lease }  → lower_automated_step_scheduled_lease (R4 —
    //                                hold a datacenter lease, REUSES the pooled
    //                                claim/grant/register/release body-wrapping).
    //   - Executor { pool: Some }  → lower_automated_step_pooled (token_pool
    //                                admission, R2/R3 machinery — same wrapping).
    //   - Executor { pool: None }  → falls through to the plain executor lowering
    //                                below (BYTE-IDENTICAL — guarded by
    //                                `automated_step_executor_unchanged`).
    if matches!(
        &cx.node.data,
        WorkflowNodeData::AutomatedStep {
            deployment_model: DeploymentModel::Scheduled {
                operation: ScheduledOperation::Lease,
                ..
            },
            ..
        }
    ) {
        return lower_automated_step_scheduled_lease(cx);
    }

    if matches!(
        &cx.node.data,
        WorkflowNodeData::AutomatedStep {
            deployment_model: DeploymentModel::Scheduled { .. },
            ..
        }
    ) {
        return lower_automated_step_scheduled(cx);
    }

    if matches!(
        &cx.node.data,
        WorkflowNodeData::AutomatedStep {
            deployment_model: DeploymentModel::Executor { pool: Some(_) },
            ..
        }
    ) {
        return lower_automated_step_pooled(cx);
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
        stream_output,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_automated_step on non-AutomatedStep node")
    };
    let stream_output = *stream_output;

    // Is this the terminal node of a Map body? If so it must forward its FULL
    // completed envelope (park data AND the whole token) so the Map's
    // `t_<map>_collect` can read `body.detail.outputs.<resultVar>` + the
    // preserved `__map_idx`/`__map_id` leaves. Shared gate — see
    // `super::is_map_body_terminal`.
    let is_map_body_terminal =
        super::is_map_body_terminal(cx.graph, cx.node.parent_id.as_deref(), cx.outgoing_edges);

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

    // PROTOTYPE — streaming side-channel: when `stream_output` is set, mint a
    // Signal place `p_{id}_stream` (intentionally multi-token — one token per
    // executor Log event) at NODE scope and hand it to the lifecycle's
    // `stream_log` bridge below. The lifecycle's fanout copies each log token
    // here AND keeps hpi_logs intact. A downstream edge from the node's "stream"
    // handle consumes from here (registered in `output_places`). Leftover stream
    // tokens never block `NetCompleted` (Signal is never terminal); the slim
    // control token still governs completion.
    let p_stream: Option<PlaceHandle<DynamicToken>> = if stream_output {
        Some(ctx.signal(format!("p_{id}_stream"), format!("{label} - Stream")))
    } else {
        None
    };
    // Clone for the move into the `scoped_prefix` closure (the original is
    // consumed by `output_places` registration after the closure returns).
    let p_stream_bridge = p_stream.clone();

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
                // PROTOTYPE — when set, the lifecycle ALSO copies each Log
                // event onto this place (one token per `log_info()` call) so
                // the node's "stream" handle fires the downstream once per log.
                stream_log: p_stream_bridge,
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
    //
    // EXCEPTION — a Map body terminal forks instead (park data AND forward the
    // FULL completed envelope), so the Map's collect can lift the parked
    // business output (`detail.outputs.<resultVar>`) and the preserved
    // `__map_*` correlation leaves off the forwarded token. The parked data
    // place is still produced, so any downstream `<slug>.<field>` borrow is
    // unaffected; only the token handed to the container's `body_out` changes.
    let (data_place_id, p_ctrl) = if is_map_body_terminal {
        park_outputs(ctx, id, label, &p_output)
    } else {
        split_outputs(ctx, id, label, &p_output)
    };

    // Slim control success output + the unchanged named "error" output. An
    // edge from the node's error handle (source_handle == "error") wires to
    // `p_error` via `find_output_place`; if no error edge exists `p_error`
    // simply has no consumer.
    let mut output_places = vec![
        (None, p_ctrl),
        (Some("error".to_string()), p_error),
    ];
    // PROTOTYPE — register the "stream" handle → `p_{id}_stream` so a normal
    // edge from that handle (sourceHandle == "stream") wires the Signal place to
    // the downstream transition via `wire_edge`/`find_output_place`. No special
    // consuming transition is needed — the standard edge-wiring path applies.
    if let Some(p_stream) = p_stream {
        output_places.push((Some("stream".to_string()), p_stream));
    }
    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places,
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
        scheduler,
        job_template,
        resources,
        operation,
        ..
    } = deployment_model
    else {
        unreachable!("lower_automated_step_scheduled on non-Scheduled step")
    };
    // This entry handles `operation: Submit` only — the dispatcher routes
    // `operation: Lease` to `lower_automated_step_scheduled_lease` (R4).
    debug_assert!(
        matches!(operation, ScheduledOperation::Submit),
        "lower_automated_step_scheduled must only see operation: submit"
    );
    // `scheduler` binds a `datacenter` resource (docs/13) for the LEASE path;
    // for SUBMIT it is unused — the env-global scheduler-net services the submit
    // (byte-identical). Bound to acknowledge the field without changing AIR.
    let _scheduler = scheduler.clone();
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

/// Everything the pooled lowering needs once `Inline.pool.alias` has been
/// resolved to a `token_pool` resource.
struct PoolBinding {
    /// Deterministic backing net id (`pool-<resource_id>`) the claim/register/
    /// release bridges target.
    backing_net_id: String,
    /// `Lease__<kind>` — the AIR definition name for the typed grant/lease.
    lease_def_name: String,
    /// The kind's lease JSON Schema, registered into `scenario.definitions`.
    lease_schema: serde_json::Value,
    /// The validated `request` params rendered as a Rhai literal (`()` when
    /// `binding.request` is absent).
    request_rhai: String,
}

/// Resolve a pool-resource alias (required) → a [`PoolBinding`], gated to a
/// single `expected_kind`.
///
/// Shared by the two claim/grant/register/release entry points — they differ
/// ONLY in which alias they resolve and which kind they require:
/// - `Executor { pool: { alias } }` → `expected_kind = "token_pool"` (R2/R3).
/// - `Scheduled { scheduler: alias, operation: lease }` → `expected_kind =
///   "datacenter"` (R4). The downstream body-wrapping is identical; only the
///   backing net + `Lease__<kind>` differ, which this binding carries.
///
/// Errors:
/// - alias not in `known_resources` → `WorkspaceResourceUnknown` (normally
///   caught earlier at publish by `discover_known_resources`).
/// - alias resolves to a kind other than `expected_kind` → a kind-specific
///   CompileError (`ResourcePoolNotAPool` for token_pool, `SchedulerNotADatacenter`
///   for datacenter) steering the author to the right deployment model.
/// - `request` fails validation against the kind's `claim_schema` →
///   `ResourcePoolRequestInvalid`.
fn resolve_binding(
    node_id: &str,
    alias: &str,
    request: Option<&serde_json::Value>,
    expected_kind: &str,
    known: &crate::compiler::resource_refs::KnownResources,
) -> Result<PoolBinding, CompileError> {
    let resource = known.get(alias).ok_or_else(|| {
        CompileError::WorkspaceResourceUnknown {
            node_id: node_id.to_string(),
            alias: alias.to_string(),
        }
    })?;
    let kind = resource.type_name.clone();

    // The `pool_kind` lookup gates "is it a pool kind at all"; the
    // `== expected_kind` gate enforces the Executor/Scheduled split (a
    // token_pool belongs under Executor.pool, a datacenter under Scheduled).
    // A wrong/non-pool kind yields the entry-point-appropriate error.
    let wrong_kind = || -> CompileError {
        if expected_kind == "datacenter" {
            CompileError::SchedulerNotADatacenter {
                node_id: node_id.to_string(),
                alias: alias.to_string(),
                kind: kind.clone(),
            }
        } else {
            CompileError::ResourcePoolNotAPool {
                node_id: node_id.to_string(),
                alias: alias.to_string(),
                kind: kind.clone(),
            }
        }
    };
    let pool_desc = aithericon_resources::pool::pool_kind(&kind).ok_or_else(wrong_kind)?;
    if kind != expected_kind {
        return Err(wrong_kind());
    }

    // Validate `request` against the kind's claim_schema before we bake it into
    // the ClaimRequest. Same `jsonschema` crate/version the engine
    // `SchemaRegistry` uses, so compile-time and runtime agree.
    let request_rhai = match request {
        None => "()".to_string(),
        Some(req) => {
            let claim_schema = (pool_desc.claim_schema)();
            let validator = jsonschema::validator_for(&claim_schema).map_err(|e| {
                CompileError::ResourcePoolRequestInvalid {
                    node_id: node_id.to_string(),
                    alias: alias.to_string(),
                    message: format!("claim_schema failed to compile: {e}"),
                }
            })?;
            if let Some(err) = validator.iter_errors(req).next() {
                return Err(CompileError::ResourcePoolRequestInvalid {
                    node_id: node_id.to_string(),
                    alias: alias.to_string(),
                    message: err.to_string(),
                });
            }
            json_to_rhai_literal(req)
        }
    };

    Ok(PoolBinding {
        backing_net_id: well_known::pool_net_id(resource.id),
        lease_def_name: format!("Lease__{kind}"),
        // Strip the schemars envelope (`$schema`, `title`) so the registered
        // definition is a bare object schema matching the `Data__`/`Ctrl__`
        // convention — the engine wraps it as `{definitions, $ref}` and a
        // nested draft `$schema` would be redundant noise. These lease schemas
        // are flat (no internal `$ref`/`definitions`), so nothing else needs
        // lifting.
        lease_schema: sanitize_definition_schema((pool_desc.lease_schema)()),
        request_rhai,
    })
}

/// Drop schemars' root-only envelope keys (`$schema`, `title`) so a
/// `schema_for!`-derived schema can be embedded as a `#/definitions/*` entry
/// next to the compiler's hand-built `Data__*` definitions.
fn sanitize_definition_schema(mut schema: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
    }
    schema
}

/// Pooled (`Executor { pool: Some }`) AutomatedStep: the executor-lifecycle
/// body wrapped in a **claim / register / release** handshake against the
/// resolved `token_pool` resource's backing net (`well_known::pool_net_id`,
/// built by `mekhan_service::petri::pool_net::build_token_pool_net`). The pool's
/// `t_grant` fires only when capacity is free, so an empty pool simply leaves
/// the claim queued — admission control falls straight out of the Petri firing
/// rule (`docs/14`).
///
/// ## grant_id — globally unique AND replay-deterministic (TASK 0)
///
/// The pool correlates register / release / reap by `grant_id` ACROSS
/// instances, so it must be globally unique. It must ALSO be replay-safe: the
/// engine re-folds the event log on replay, so a `uuid()` / `random()` would
/// mint a *different* id on replay and break correlation (and the M1
/// replay-determinism invariant). There is no engine-injected net/instance id
/// reachable in transition Rhai — `RhaiRuntime` builds the transition scope
/// purely from the input token bindings (`rhai_runtime.rs::build_scope`), and
/// the only registered helpers are `__pluck` (the adapter-only `random()` /
/// `timestamp()` are NOT on the transition engine).
///
/// What IS reachable is `input._instance_id`: the launcher stamps the instance
/// UUID onto every Start token (`petri::instance.rs` injects `_instance_id`),
/// it is preserved on every slim control token (`YIELD_LOGIC` keeps `_`-prefixed
/// keys), and it is a value fixed at launch in the event log — so it replays
/// identically. We therefore derive
///
///   `grant_id = <input._instance_id> ":" <node_id>`
///
/// — unique per (instance, node) hence globally unique even across concurrent
/// instances of the same template, and a pure function of journaled token data
/// (no clock, no RNG) hence replay-deterministic. `<node_id>` is a compile-time
/// constant baked into the Rhai literal.
///
/// ## Topology (places + transitions + arcs)
///
/// ```text
///  p_input ─[t_claim]─▶ p_pending (parks {input, grant_id})
///                    └─▶ p_claim_out  (bridge → pool/claim_inbox, reply "grant")
///  p_grant_inbox ◀─(reply "grant", Grant{grant_id,gpu_id})
///  {p_pending, p_grant_inbox} ─[t_acquire (correlate grant_id)]
///       ─▶ {id}/inbox      (executor job spec — same shape as inline prepare)
///       ─▶ p_register_out  (bridge → pool/register_inbox, HoldReg{grant_id,gpu_id})
///       ─▶ p_held          (parks {grant_id, gpu_id} for the release echo)
///  …executor lifecycle + retry topology (hold persists across retries)…
///  {completed, p_held} ─[t_to_output]─▶ p_output + p_release_out (ReleaseRequest)
///  {p_exhausted, p_held} ─[t_to_error]─▶ p_error + p_release_out (ReleaseRequest)
/// ```
///
/// Exactly one of `t_to_output` / `t_to_error` fires per run (they race for the
/// single `p_held` token), so the release bridges **exactly once** on every
/// terminal path — the load-bearing leak-prevention invariant (`docs/14`).
///
/// The error terminal is `p_exhausted`, NOT the lifecycle's `dead_letter`:
/// `dead_letter` is an unreachable sink (executor_lifecycle.rs:186) so the
/// retry topology's `exhausted` transition is the real failure exit. We route
/// it to a dedicated `p_exhausted` place (instead of straight to `p_error` as
/// the inline path does) so the hold is consumed + released BEFORE the error
/// surfaces — otherwise a failed job would strand its capacity token.
///
/// ## Reply-routing taint (docs/14)
///
/// `t_acquire` consumes the routed grant. `route_output_tokens` stamps the
/// grant's reply routing onto `t_acquire`'s internal outputs (`p_held`, the
/// job token) — but that is harmless here: the capacity token recycled into
/// the pool is built by the pool's own `t_release` from the CLEAN `ReleaseRequest`
/// (it crossed a plain `bridge_out`) and the CLEAN registered `in_use` hold (it
/// crossed a plain `bridge_out` too). The tainted `p_held` never re-enters the
/// pool, so capacity tokens stay clean and the pool does not wedge.
fn lower_automated_step_pooled(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let WorkflowNodeData::AutomatedStep {
        deployment_model: DeploymentModel::Executor { pool: Some(binding) },
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_automated_step_pooled only runs for Executor pool:Some")
    };
    // Resolve `Executor.pool.alias` (required) against the workspace-resource
    // manifest: a `token_pool` resource → `{resource_id, kind}` → the deterministic
    // backing net `pool-<resource_id>`, validated `request`, and a typed,
    // body-visible lease (R2/R3). A non-token_pool alias is a CompileError.
    let pool_binding = resolve_binding(
        &cx.node.id,
        &binding.alias,
        binding.request.as_ref(),
        "token_pool",
        cx.known_resources,
    )?;
    lower_pooled_body(cx, pool_binding)
}

/// `Scheduled { operation: Lease }` (R4): hold a lease on an external cluster
/// (`datacenter` resource) for the step's duration. REUSES the exact same
/// claim/grant/register/release body-wrapping as the token-pool path — the
/// instance side is identical; only the backing net (`pool-<id>` = the R4b
/// datacenter lease-adapter) and the lease kind (`Lease__datacenter`) differ,
/// both of which `resolve_binding` carries on the [`PoolBinding`].
///
/// `Scheduled { operation: Submit }` stays the byte-identical scheduler-net
/// path in [`lower_automated_step_scheduled`]; only the `Lease` operation routes
/// here.
fn lower_automated_step_scheduled_lease(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let WorkflowNodeData::AutomatedStep {
        deployment_model:
            DeploymentModel::Scheduled {
                scheduler,
                request,
                ..
            },
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_automated_step_scheduled_lease only runs for Scheduled operation:lease")
    };
    // `operation: lease` REQUIRES a `scheduler` alias — there is no env-global
    // lease (the lease lifecycle is owned by a specific datacenter's allocator).
    let Some(alias) = scheduler.as_deref().filter(|a| !a.is_empty()) else {
        return Err(CompileError::Compilation(format!(
            "node '{}': Scheduled `operation: lease` requires a `scheduler` datacenter alias \
             (there is no env-global lease — the lease is held against a specific allocator)",
            cx.node.id
        )));
    };
    let binding = resolve_binding(
        &cx.node.id,
        alias,
        request.as_ref(),
        "datacenter",
        cx.known_resources,
    )?;
    lower_pooled_body(cx, binding)
}

/// The shared claim/grant/register/release body-wrapping, parameterized by the
/// resolved [`PoolBinding`]. Both `Executor { pool: Some }` and
/// `Scheduled { operation: Lease }` call this with their respective binding;
/// the topology + executor job-spec are byte-identical regardless of backend.
fn lower_pooled_body(cx: &mut LoweringCtx, pool_binding: PoolBinding) -> Result<(), CompileError> {
    let id = cx.node.id.clone();
    let WorkflowNodeData::AutomatedStep {
        label,
        execution_spec,
        retry_policy,
        output,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_pooled_body on non-AutomatedStep node")
    };
    let label = label.clone();
    let retry_policy = retry_policy.clone();
    let backend_type = execution_spec.backend_type;

    // Same config offload + staged inputs + declared outputs as the inline
    // path (`lower_automated_step`) — keep the job-spec Rhai byte-for-byte
    // structurally identical so a pooled node executes its body the same way.
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
    let storage_key = cx.config_storage.key(&id);
    cx.node_configs.insert(id.clone(), validated_config);
    let config_ref_rhai = format!(
        "#{{ \"storage_path\": \"{}\" }}",
        rhai_str_escape(&storage_key)
    );
    let inputs_rhai =
        json_to_rhai_literal(&serde_json::to_value(&staged_inputs).unwrap_or_default());
    let outputs_rhai = declared_outputs_rhai(backend_type, output);
    let backend_wire = backend_type.as_wire_str();
    let max_retries = retry_policy.max_retries;
    let id_lit = rhai_str_escape(&id);

    // grant_id literal builder (see the doc comment for the replay-safety
    // argument). Built inside the Rhai logic from `input._instance_id` so it
    // is a pure function of journaled token data.
    let grant_id_expr = format!(r#"(input._instance_id + ":{id_lit}")"#);

    // Record the typed-lease definition + the grant-inbox place to type, while
    // we still hold `cx` (the `&mut *cx.ctx` reborrow below would block
    // `cx.fixups`). The grant inbox is created OUTSIDE the lifecycle scope, so
    // its id is the unprefixed `p_{id}_grant_inbox`. `compile_to_air` drains
    // these after `ctx.build()`.
    cx.fixups
        .lease_definitions
        .push((pool_binding.lease_def_name.clone(), pool_binding.lease_schema.clone()));
    cx.fixups
        .lease_inbox_schemas
        .push((format!("p_{id}_grant_inbox"), pool_binding.lease_def_name.clone()));

    let ctx = &mut *cx.ctx;

    // ── Node-interface places (outside the lifecycle scope) ─────────────────
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_error: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_error"), format!("{label} - Error"));

    // Grant reply lands here (consumable `state` place w/ bridge_reply_channel,
    // same proven-consumable kind the scheduled path uses for `sched_result`).
    let grant_inbox_place = format!("p_{id}_grant_inbox");
    let p_grant_inbox: PlaceHandle<DynamicToken> = ctx.bridge_reply_channel(
        grant_inbox_place.clone(),
        format!("{label} - Grant Inbox"),
        "grant",
    );
    // The net all three handshake bridges target: the resolved resource's
    // deterministic backing net `pool-<resource_id>`. The inbox place names
    // (`claim_inbox` / `register_inbox` / `release_inbox`) are the shared
    // cross-net contract the `build_token_pool_net` net implements.
    let pool_net_id: &str = &pool_binding.backing_net_id;
    // Claim bridge_out, routing the pool's "grant" reply back to p_grant_inbox.
    let p_claim_out: PlaceHandle<DynamicToken> = ctx.bridge_out_reply_channels(
        format!("p_{id}_claim_out"),
        format!("{label} - Claim Capacity"),
        pool_net_id,
        well_known::POOL_CLAIM_INBOX,
        &[("grant", grant_inbox_place.as_str())],
    );
    // Register + release bridges are PLAIN (no reply routing) so the pool's
    // recycled capacity tokens stay clean — see the taint note above.
    let p_register_out: PlaceHandle<DynamicToken> = ctx.bridge_out(
        format!("p_{id}_register_out"),
        format!("{label} - Register Hold"),
        pool_net_id,
        well_known::POOL_REGISTER_INBOX,
    );
    let p_release_out: PlaceHandle<DynamicToken> = ctx.bridge_out(
        format!("p_{id}_release_out"),
        format!("{label} - Release Capacity"),
        pool_net_id,
        well_known::POOL_RELEASE_INBOX,
    );

    // Internal parking places.
    let p_pending: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_pending"),
        format!("{label} - Pending (input + grant_id, awaiting grant)"),
    );
    let p_held: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_held"),
        format!("{label} - Held (grant_id + gpu_id, release echo)"),
    );
    // Retry-exhausted SINK. The inline path lets `build_retry_topology` write
    // its `exhausted` transition straight to `p_error` — but that transition
    // does NOT consume `p_held`, so on the failure path the hold would be
    // stranded and the capacity token leaked. We therefore give the retry
    // topology a DEDICATED exhausted place and route it through a
    // held-consuming transition (`t_to_error` below) so EVERY terminal exit
    // releases. This is the structural enforcement of the docs/14
    // every-body-exit-arcs-to-release_out invariant.
    let p_exhausted: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_exhausted"),
        format!("{label} - Retries Exhausted (awaiting release)"),
    );

    // ── ClaimRequest payload: `grant_id` + the validated `request` params (the
    // kind's claim-schema shape; `()` when omitted) so the backing net's
    // `t_grant` can size/shape the grant.
    let claim_payload = format!("#{{ grant_id: gid, request: {} }}", pool_binding.request_rhai);
    // ── t_claim: mint grant_id, emit ClaimRequest, park {input, grant_id} ───
    ctx.transition(format!("t_{id}_claim"), format!("{label} - Claim"))
        .auto_input("input", &p_input)
        .auto_output("claim", &p_claim_out)
        .auto_output("pending", &p_pending)
        .logic(format!(
            r#"let gid = {grant_id_expr}; #{{ claim: {claim_payload}, pending: #{{ input: input, grant_id: gid }} }}"#
        ));

    // ── Lifecycle scope: build the inbox INSIDE the scope (so the lifecycle's
    // submit consumes it), then the lifecycle + retry topology. The acquire
    // transition (below, outside the scope) writes the job spec into it. ─────
    let handles = ctx.scoped_prefix(id.as_str(), label.as_str(), |ctx| {
        let exec_inbox = ctx.state::<ExecutorSubmitInput>("inbox", "Inbox");
        let exec_inbox_retry = exec_inbox.clone();

        let lc = executor_lifecycle(
            ctx,
            ExecutorBridges {
                inbox: exec_inbox.clone(),
                result_out: None,
                failure_out: None,
                process_id: None,
                process_step: None,
                catalogue: true,
                process: true,
                // TODO(streaming-output prototype): the `stream_output` "stream"
                // handle is wired only on the plain inline executor path
                // (`lower_automated_step`). Pooled/leased steps do not yet
                // expose the stream side-channel — `None` keeps this path
                // byte-identical. Plumbing `p_{id}_stream` through here would
                // mirror the inline path exactly.
                stream_log: None,
            },
        );

        // Retry re-injects a fresh submit into the SAME inbox — the hold
        // (p_held) persists across retries, so we do NOT re-claim per retry.
        // The retry topology's terminal `exhausted` edge drains to
        // `p_exhausted` (NOT `p_error`) so the hold can be released first.
        build_retry_topology(
            ctx,
            &retry_policy,
            &lc.failed,
            &lc.timed_out,
            &exec_inbox_retry,
            &lc.effect_errors,
            &p_exhausted,
        );

        (exec_inbox, lc)
    });
    let (exec_inbox, handles) = handles;

    // ── t_acquire: grant arrived. Consume {pending, grant} (correlate
    // grant_id), build the executor job spec (same structure as the inline
    // `prepare`), register the hold over the plain bridge, and park
    // {grant_id, gpu_id} for the release echo. The `/*__BORROWED_INPUTS__*/`
    // marker is preserved so Python `<slug>.<field>` staging still rewrites it
    // post-merge exactly as for inline nodes. The job's `input.json` is the
    // ORIGINAL upstream token parked in `pending.input`. ───────────────────
    //
    // The routed `grant` token IS the typed lease (validated vs `Lease__<kind>`
    // on `p_grant_inbox`). We (a) stage it into the body as `lease.json` so
    // body code reads `lease.<field>` (e.g. `lease.unit_id`), mirroring the
    // resource-envelope `<alias>.json` staging, and (b) park the WHOLE lease on
    // `p_held` so `t_to_output` can merge it into the parked data envelope.
    // `grant` still carries `grant_id` (the correlation key), so the
    // release-by-grant_id path is unchanged.
    let lease_stage_push = r#"job_inputs.push(#{ "name": "lease.json", "source": #{ "type": "inline", "value": grant } }); "#;
    // Carry the full lease so the hold echo + held parking both keep every
    // lease field; `grant.grant_id` is the correlation key the pool keys
    // register/release on.
    let reg_payload = "grant";
    let held_payload = "grant";
    ctx.transition(format!("t_{id}_acquire"), format!("{label} - Acquire"))
        .auto_input("pending", &p_pending)
        .auto_input("grant", &p_grant_inbox)
        .correlate("grant", "pending", "grant_id")
        .auto_output("job", &exec_inbox)
        .auto_output("reg", &p_register_out)
        .auto_output("held", &p_held)
        .logic(format!(
            r#"let input = pending.input; let d = input; d.job_id = "{id_lit}"; d.run = 0; d.retries = 0; d.max_retries = {max_retries}; let job_inputs = {inputs_rhai}; job_inputs.push(#{{ "name": "input.json", "source": #{{ "type": "inline", "value": input }} }}); {lease_stage_push}/*__BORROWED_INPUTS__*/ d.spec = #{{ "backend": "{backend_wire}", "inputs": job_inputs, "outputs": {outputs_rhai}, "config_ref": {config_ref_rhai}, "stream_events": ["metric", "progress", "phase", "log"] }}; #{{ job: d, reg: {reg_payload}, held: {held_payload} }}"#
        ));

    // ── Terminal exits: BOTH consume p_held and BOTH arc to p_release_out.
    // Success path: lifecycle `completed` + held → output + release. ────────
    //
    // Merge the held lease into the output envelope under a `lease` key BEFORE
    // it is parked by `split_outputs` (→ `p_{id}_data`), so a downstream
    // `<slug>.lease.<field>` borrow resolves through the standard read-arc
    // pipeline against the parked data place. The parked `Data__<id>` schema is
    // `additionalProperties: true`, so the extra `lease` key validates.
    let to_output_logic =
        r#"let out = done; out.lease = held; #{ output: out, release: #{ grant_id: held.grant_id } }"#;
    ctx.transition(format!("t_{id}_to_output"), format!("{label} - To Output"))
        .auto_input("done", &handles.completed)
        .auto_input("held", &p_held)
        .auto_output("output", &p_output)
        .auto_output("release", &p_release_out)
        .logic(to_output_logic);

    // Error path: retries exhausted (the ONLY reachable executor-failure
    // terminal — `dead_letter` is an unreachable lifecycle sink, see
    // executor_lifecycle.rs:186). Consume `{p_exhausted, p_held}` → error +
    // release. This held-consuming reconciliation guarantees the failure exit
    // releases the hold. `held` is unused in the error payload (the failure
    // token `err` already carries job context) but consuming it is the whole
    // point — it frees the capacity. The `dead_letter` handle is ignored (no
    // consumer), exactly as in the inline path.
    let _ = &handles.dead_letter;
    ctx.transition(format!("t_{id}_to_error"), format!("{label} - To Error"))
        .auto_input("err", &p_exhausted)
        .auto_input("held", &p_held)
        .auto_output("error", &p_error)
        .auto_output("release", &p_release_out)
        .logic(r#"#{ error: err, release: #{ grant_id: held.grant_id } }"#);

    // Foundation split + port registration tail — identical to the inline path.
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
