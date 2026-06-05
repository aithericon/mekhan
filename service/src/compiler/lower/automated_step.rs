//! `WorkflowNodeData::AutomatedStep` lowering. Two dispatch arms:
//!
//! - `lower_automated_step` (the public entry) — executor-pool lifecycle
//!   for the normal `DeploymentModel::Executor` path; offloads the static
//!   config to the per-node side-channel and emits a slim `config_ref`
//!   Rhai literal. Standalone `Scheduled` nodes also route through the
//!   single-node lease lifecycle (reusing the pooled machinery).
//! - `lower_engine_effect` — backends whose `DispatchMode::EngineEffect`
//!   maps to a registered engine builtin (e.g. `catalogue_lookup`).

use super::*;

pub(crate) fn lower_automated_step(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    // Dispatch on `deployment_model` (post-R3 consolidation: admission folded
    // into it). `matches!` drops the borrow immediately so each delegate can
    // take `cx` mutably.
    //
    //   - Scheduled                → standalone lease lifecycle (lower_pooled_body).
    //   - Executor { capacity: Some }  → lower_automated_step_pooled (concurrency_limit
    //                                admission, R2/R3 machinery — same wrapping).
    //   - Executor { capacity: None }  → falls through to the plain executor lowering
    //                                below (BYTE-IDENTICAL).

    // A `Scheduled` body that runs ON a held lease is NO
    // LONGER a separate cluster dispatch. The rework retargets it to the EXECUTOR
    // enqueue path with a per-job `executor_namespace` borrowed from the
    // enclosing lease holder — the held alloc runs ONE persistent drain executor
    // on the lease namespace, and the body just enqueues to it.
    //
    // "Runs on a lease" is IMPLICIT BY CONTAINMENT: the step sits inside a
    // `LeaseScope` (or a leased `Loop`), detected by
    // `enclosing_leased_scope_slug` walking the `parent_id` chain. There is no
    // per-step flag — the enclosure is the only signal.
    //
    // A `Scheduled` node with NO enclosing lease holder now performs a
    // single-step lease lifecycle (acquire -> run -> release) targeting its
    // resolved datacenter resource.
    if let WorkflowNodeData::AutomatedStep {
        deployment_model: DeploymentModel::Scheduled { scheduler, .. },
        ..
    } = &cx.node.data
    {
        if enclosing_leased_scope_slug(cx.node, cx.graph).is_none() {
            // Standalone Scheduled step: perform single-node lease lifecycle.
            let alias = scheduler.as_deref().filter(|a| !a.is_empty()).ok_or_else(|| {
                // Every Scheduled step REQUIRES a concrete cluster — the selection
                // pass (scheduler_select.rs) enforces this, so absence here is a
                // hard unresolved error.
                CompileError::SchedulerUnresolved {
                    node_id: cx.node.id.clone(),
                }
            })?;

            // Resolve the datacenter binding and delegate to the pooled body wrapping.
            // A per-node container spec (if any) is merged into the lease claim
            // `request` so the held alloc's drain executor runs in the `.sif`.
            let binding = resolve_binding(
                &cx.node.id,
                alias,
                None, // Scheduled steps don't have a 'request' field
                DeploymentRole::SchedulerLease,
                cx.known_resources,
                cx.container_specs.get(&cx.node.id),
            )?;
            return lower_pooled_body(cx, binding);
        }
    }

    // Identity-plane mutual-exclusion guard (docs/23/24): `capacity` (presence-PUSH
    // admission, R3) and `group` (plain PULL routing coordinate) are mutually
    // exclusive. A grouped step stays on the plain pull lowering below and must
    // NOT enter `lower_automated_step_pooled` (the claim/grant handshake). Reject
    // a step that asks for both BEFORE the pooled dispatch.
    if let WorkflowNodeData::AutomatedStep {
        deployment_model:
            DeploymentModel::Executor {
                capacity: Some(_),
                group: Some(_),
            },
        ..
    } = &cx.node.data
    {
        return Err(CompileError::CapacityGroupConflict {
            node_id: cx.node.id.clone(),
        });
    }

    if matches!(
        &cx.node.data,
        WorkflowNodeData::AutomatedStep {
            deployment_model: DeploymentModel::Executor { capacity: Some(_), .. },
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
            if let crate::backends::DispatchMode::EngineEffect { handler } = decl.meta.dispatch_mode
            {
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
        channels,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_automated_step on non-AutomatedStep node")
    };

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
    crate::compiler::schema_refs::inline_refs(&mut validated_config, cx.definitions).map_err(
        |e| CompileError::SchemaRefUnresolved {
            node_id: id.clone(),
            path: String::new(),
            message: e.to_string(),
        },
    )?;
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
    // Streaming-channel manifest (docs/25): the declared channels, baked into
    // the job spec so the worker validates each `emit`/`scatter` channel name.
    // `[]` for a channel-less step ⇒ byte-stable spec. Cloned out of the node
    // data here, before the `&mut *cx.ctx` reborrow ends the `cx.node.data`
    // borrow; `lower_channels` (below) consumes the same slice.
    let channels = channels.clone();
    let channels_rhai = super::channels::channel_manifest_rhai(&channels);

    let max_retries = retry_policy.max_retries;

    // ── Lease retarget seam ─────────────────────────────────────────────────
    // A `Scheduled { Submit }` body that runs on a held lease lowers HERE (the
    // plain executor lifecycle), NOT through a separate cluster dispatch. "Runs on a lease"
    // is IMPLICIT BY CONTAINMENT — `enclosing_leased_scope_slug` walks the
    // `parent_id` chain to the nearest LeaseScope / leased Loop and returns its
    // slug. We stamp a per-job `executor_namespace` onto the TOP of the job token
    // `d` (NOT inside `d.spec`) — the engine's `ExecutorSubmitHandler` reads
    // `d.executor_namespace` off `job_data` and publishes to the lease-scoped
    // NATS queue (`lease-<grant_id>`) the held alloc's persistent drain executor
    // is consuming. The dotted `<holder_slug>.lease.executor_namespace` is a RAW
    // borrow ref: the matching arm in `guard_readarc_plan` registers the
    // same-shaped Guard borrow, so the standard read-arc pipeline
    // (`apply_guard_borrows`) wires a read-arc into the holder's parked
    // `p_<holder>_data` and word-boundary-rewrites the dotted text to
    // `d_<holder>.lease.executor_namespace`. Only a `Scheduled { Submit }` step
    // is lease-bound (a plain inline `Executor` step inside the scope still runs
    // on the normal worker); no enclosing holder ⇒ no fragment.
    //
    // NON-leased (default inline) path: there is no held lease to borrow from,
    // so the namespace is a COMPILE-TIME CONSTANT — the per-backend worker-pool
    // queue `executor.<wire>` from the locked contract
    // (`ExecutionBackendType::executor_namespace`). We've necessarily reached
    // this code with an `ExecutorJob` backend: the `EngineEffect` arm above
    // early-returns via `lower_engine_effect` before any job-token build, so the
    // DispatchMode gate is STRUCTURAL — no redundant dispatch_mode re-check.
    // Stamping a plain string literal (no borrow ref) means `logic()`'s
    // build-time validation still applies (the `ns_frag.is_empty()` branch
    // below selects `logic()` only when EMPTY — but the leased case is the only
    // one that needs the `logic_rhai` deferral, so we MUST keep this constant
    // stamp on the `logic()` side). See the branch-selection note below.
    // Unified worker dispatch (docs/23/24): EVERY default-inline executor step
    // routes through a worker GROUP partition on the parallel `executor-<wire>-grp`
    // stream. The step's `Executor.group` alias names the group; a step that names
    // NO group is stamped with the workspace's always-seeded `default` worker group.
    // The partition token is the group's `capacity`-resource UUID (NOT its alias):
    // workspace-safe by construction (two workspaces can both own a `default` group
    // without colliding on a queue) and a valid JetStream stream + NATS subject
    // token. The `capacity` + `group` mutual-exclusion is gated up top before the
    // pooled dispatch, so here we have at most a plain `group`. Only meaningful on
    // the default-inline (non-leased) arm: a leased body borrows the holder's
    // namespace.
    //
    // Resolve the alias → UUID through the same `cx.known_resources` registry the
    // datacenter/limit resolution uses: `discover_resource_globals` already added
    // the step's group (or `default`) as a head, so the worker `capacity` row is in
    // the map keyed by its path. The validity gate runs FIRST (a malformed explicit
    // alias is a clearer error than a failed lookup); an unresolved group (including
    // a missing `default`, which should never happen — it is always seeded) is a
    // hard `WorkerGroupUnresolved`.
    let step_group_alias: &str = match &cx.node.data {
        WorkflowNodeData::AutomatedStep {
            deployment_model: DeploymentModel::Executor { group: Some(g), .. },
            ..
        } if !g.is_empty() => g.as_str(),
        _ => crate::worker_groups::DEFAULT_WORKER_GROUP_PATH,
    };
    if !step_group_alias.is_empty()
        && !step_group_alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(CompileError::GroupTokenInvalid {
            node_id: cx.node.id.clone(),
            group: step_group_alias.to_string(),
        });
    }
    // The routing PARTITION is the worker group's capacity-resource UUID, resolved
    // through `cx.known_resources` (the publish path's `discover_resource_globals`
    // injects the group/`default` head, so the worker `capacity` row is in the
    // map keyed by its path). When the registry does NOT carry the row — the
    // direct `compile_to_air` path used by tests + analyze, which threads an empty
    // `KnownResources` and no DB — fall back to the alias itself as the partition
    // token. The alias is already validated as a safe subject token above, and for
    // the implicit group it is literally `default`; this keeps the no-registry
    // compile path working while production gets the workspace-safe UUID.
    let step_group_partition: String = cx
        .known_resources
        .get(step_group_alias)
        .map(|r| r.id.to_string())
        .unwrap_or_else(|| step_group_alias.to_string());

    let leased_holder = enclosing_leased_scope_slug(cx.node, cx.graph);
    // Branch selection (preserved): the leased (borrow-carrying) literal must
    // ride `logic_rhai` (deferred validation, since `apply_guard_borrows` later
    // rewrites the not-yet-bound `<holder>` root var); the default-inline stamp
    // is a CONSTANT-string literal with no unbound root var and never matches the
    // borrowed `<holder>.lease.executor_namespace` shape, so it belongs on the
    // eager `logic()` side and is a no-op for the borrowed-inputs / read-arc
    // pipeline. We therefore key the branch below on whether the fragment carries
    // a borrow (the leased case), NOT on emptiness.
    let ns_frag_is_borrowed = leased_holder.is_some();
    let ns_frag = match leased_holder {
        Some(holder_slug) => {
            // Leased body: RAW borrow of the holder's lease namespace,
            // post-build rewritten to `d_<holder>.lease.executor_namespace`.
            // UNCHANGED.
            format!(r#" d.executor_namespace = {holder_slug}.lease.executor_namespace;"#)
        }
        None => {
            // Default inline worker body: stamp the per-backend GROUP namespace
            // `executor-<wire>-grp/<partition>` so the engine routes to the
            // group's partition on the parallel `executor-<wire>-grp` stream. We've
            // necessarily reached this code with an `ExecutorJob` backend (the
            // `EngineEffect` arm above early-returns via `lower_engine_effect`
            // before any job-token build), so the DispatchMode gate is STRUCTURAL.
            //
            // `<partition>` is the worker group's capacity-resource UUID (the
            // step's named group, or the workspace's `default` group). There is no
            // bare `executor-<wire>` dispatch path any more — every job is grouped.
            //
            // LEASE PROPAGATION (runner-based lease): if the inbound token carries
            // an INHERITED `_executor_namespace` leaf, honor it over the group
            // default. A `LeaseScope` over a presence runner parks
            // `runner.<id>`, and a SubWorkflow nested in that scope injects it as
            // `_executor_namespace` onto the spawned child's token; the `_`-prefix
            // makes it thread through the child verbatim, so a child net's plain
            // executor steps land on the SAME held runner. Absent the leaf (every
            // non-lease flow), the else-branch is the unchanged group default —
            // runtime-identical, only the AIR text gains the conditional. `input`
            // is the bound prepare input, so this stays on the eager `logic()`
            // path (no unbound root var, unlike the leased borrow branch).
            format!(
                r#" if input._executor_namespace != () {{ d.executor_namespace = input._executor_namespace; }} else {{ d.executor_namespace = "{ns}"; }}"#,
                ns = backend_type.executor_namespace_for_group(&step_group_partition)
            )
        }
    };

    // Slug forwarding (B-staging, Phase 4): stamp the resolved scheduler
    // `job_template` slug onto the job token as `job_template_id` so the engine's
    // `SchedulerSubmitHandler` dispatches the registered parameterized job by that
    // name. A static string literal (no borrow ref), so it never needs the
    // `logic_rhai` deferral. Empty for non-Scheduled steps / an unresolved slug —
    // then the handler's config default applies (the legacy path).
    let job_template_frag = scheduled_job_template_frag(cx.node);

    // Container injection (submit path): a per-node `CompilerContainerSpec`
    // stamps `d.container = #{ sif_path, binds, nv }` onto the job token so the
    // engine's Slurm `submit` (which reads `token_data.get("container")`) runs
    // the job inside the `.sif`. A static literal (no borrow ref), so it rides
    // whichever `logic()`/`logic_rhai()` branch `ns_frag` already selected. Empty
    // for a node with no container spec (the byte-identical no-container path).
    let container_frag = cx
        .container_specs
        .get(id)
        .map(|c| {
            format!(
                " d.container = {};",
                json_to_rhai_literal(&serde_json::to_value(c).unwrap_or(serde_json::Value::Null))
            )
        })
        .unwrap_or_default();

    // Rust panic/Result model: a WIRED error handle (`source_handle == "error"`)
    // means a permanent failure routes to the handler (handled `Result::Err`,
    // net continues); an UNWIRED handle means a permanent failure crashes the
    // net (unhandled panic → NetFailed). Read `cx.outgoing_edges` BEFORE the
    // `&mut *cx.ctx` reborrow below (which mutably borrows `cx`).
    // An AutomatedStep used as an agent tool has no authored `error` edge, so
    // force `error_handled = true` (mint `p_error`) so the engine-bridged tool
    // failure surfaces to the agent's on_tool_error wiring instead of
    // dead-end-throwing and crashing the agent — same rationale as the
    // SubWorkflow tool path.
    let error_handled = cx.is_agent_tool || super::error_path_wired(cx.outgoing_edges);
    let panic_label = label.clone();

    let ctx = &mut *cx.ctx;

    // Node interface places (outside prefix scope). `p_error` only exists when
    // the error handle is wired; an unwired node crashes instead of parking.
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_error: Option<PlaceHandle<DynamicToken>> = if error_handled {
        Some(ctx.state(format!("p_{id}_error"), format!("{label} - Error")))
    } else {
        None
    };

    // Streaming channels (docs/25): pre-create the `control_emit` inbox (only
    // when the node declares ≥1 OUT control channel) so the lifecycle's submit
    // transition can register `event_routes["control_emit"]` → its id, then hand
    // the SAME handle to `lower_channels` below to drain it. `None` (no place) ⇒
    // AIR byte-stable for channel-less steps.
    let p_control_in = super::channels::control_inbox(ctx, id, label, &channels);

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
        let stream_events_rhai = r#"["metric", "progress", "phase", "log"]"#;
        let prepare_logic = format!(
            r#"let d = input; d.job_id = "{id}"; d.run = 0; d.retries = 0; d.max_retries = {max_retries}; let job_inputs = {inputs_rhai}; job_inputs.push(#{{ "name": "input.json", "source": #{{ "type": "inline", "value": input }} }}); /*__BORROWED_INPUTS__*/ d.spec = #{{ "backend": "{backend_type}", "inputs": job_inputs, "outputs": {outputs_rhai}, "config_ref": {config_ref_rhai}, "stream_events": {stream_events_rhai}, "channels": {channels_rhai} }};{ns_frag}{job_template_frag}{container_frag} #{{ job: d }}"#
        );
        let prepare = ctx
            .transition("prepare", format!("{label} - Prepare"))
            .auto_input("input", &p_input)
            .auto_output("job", &exec_inbox);
        if !ns_frag_is_borrowed {
            // Common path: fail-fast build-time script validation. The literal
            // carries no borrowed root var — either no `ns_frag` (legacy) or a
            // constant `executor.<wire>` string literal (default worker-pool) —
            // so `logic()` can validate variable bindings eagerly.
            prepare.logic(prepare_logic);
        } else {
            // lease-bound body: the literal carries the RAW
            // `<loop>.lease.executor_namespace` borrow, which the post-build
            // read-arc pipeline (`apply_guard_borrows`) rewrites to
            // `d_<loop>.lease.executor_namespace` and binds via a synthesized
            // read-arc into `p_<loop>_data`. `logic()`'s build-time validation
            // would reject the not-yet-bound `<loop>` root var, so use
            // `logic_rhai` (the same deferral every Loop/Decision guard relies
            // on — the engine validates the final rewritten Rhai at load).
            prepare.logic_rhai(prepare_logic).done();
        }

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
                // Streaming channels (docs/25) are synthesized as a separate
                // control-emit path, not via the lifecycle's Output side-channel.
                stream_output: None,
                // The pre-created control-emit inbox (docs/25). `Some` only when
                // the node has ≥1 OUT control channel; the submit transition
                // registers `event_routes["control_emit"]` → its id.
                control_in: p_control_in.clone(),
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
            p_error.as_ref(),
            error_handled,
            &panic_label,
        );

        lc
    });

    // Bridge lifecycle outputs to node interface
    ctx.transition(format!("t_{id}_to_output"), format!("{label} - To Output"))
        .auto_input("done", &handles.completed)
        .auto_output("output", &p_output)
        .logic(r#"#{ output: done }"#);

    // Infra-level effect-handler errors (NATS/dispatch) drain to the node
    // error output when wired; job-level failures are handled by the retry
    // topology above. When the error handle is UNWIRED, an infra dead-letter
    // also has no handler — crash the net (panic → NetFailed) for consistency
    // with the exhausted path rather than stranding the token.
    if let Some(p_error) = &p_error {
        ctx.transition(format!("t_{id}_to_error"), format!("{label} - To Error"))
            .auto_input("dead", &handles.dead_letter)
            .auto_output("error", p_error)
            .logic(r#"#{ error: dead }"#);
    } else {
        let msg = format!(
            "automated step '{label}' dead-lettered (infra failure) and no error handler is wired"
        );
        ctx.transition(
            format!("t_{id}_to_error_deadend"),
            format!("{label} - Dead-letter (no handler — crash net)"),
        )
        .auto_input("dead", &handles.dead_letter)
        .logic_rhai(format!("throw \"{}\"", rhai_str_escape(&msg)))
        .done();
    }

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

    // Streaming channels (docs/25): synthesize one place per control channel +
    // the `control_emit` ingestion seam (inbox + effect transition with
    // `channel_routes`) + scatter gather. Returns the per-channel wiring ports
    // folded into NodePorts below. No control channels ⇒ no topology (byte-stable).
    let lowered_channels = super::channels::lower_channels(
        ctx,
        id,
        label,
        &cx.node.id,
        &channels,
        &cx.graph.edges,
        p_control_in,
        &p_input,
    );

    // Slim control success output, plus the named "error" output ONLY when the
    // handle is wired. An edge from the node's error handle (source_handle ==
    // "error") wires to `p_error` via `find_output_place`. When unwired we omit
    // the entry entirely (the failure crashes the net instead), so wire.rs never
    // attaches a consumer to a non-existent port.
    let mut output_places = vec![(None, p_ctrl)];
    if let Some(p_error) = p_error {
        output_places.push((Some("error".to_string()), p_error));
    }
    // Fold channel ports: OUT channels become source-handle outputs (edges wire
    // off `sourceHandle == name`); IN channels become named input handles
    // (`targetHandle == name`).
    let mut input_handles = HashMap::new();
    for port in lowered_channels.ports {
        match port.direction {
            crate::models::template::ChannelDirection::Out => {
                output_places.push((Some(port.name), port.place));
            }
            crate::models::template::ChannelDirection::In => {
                input_handles.insert(port.name, port.place);
            }
        }
    }
    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places,
            input_places: HashMap::new(),
            input_handles,
        },
    );
    // AutomatedStep is a parked producer: borrow `<slug>.<field>` reads
    // through the data port.
    cx.publish_interface().data_port = Some(data_place_id);
    Ok(())
}


/// Everything the pooled lowering needs once `Inline.capacity.alias` has been
/// resolved to a net-backed `capacity` (or `datacenter`) resource and its
/// dispatch backend.
pub(super) struct PoolBinding {
    /// Deterministic backing net id (`pool-<resource_id>`) the claim/register/
    /// release bridges target.
    pub(super) backing_net_id: String,
    /// `Lease__<backend>` — the AIR definition name for the typed grant/lease.
    pub(super) lease_def_name: String,
    /// The backend's lease JSON Schema, registered into `scenario.definitions`.
    pub(super) lease_schema: serde_json::Value,
    /// The validated `request` params rendered as a Rhai literal (`()` when
    /// `binding.request` is absent).
    pub(super) request_rhai: String,
    /// The resolved kind's pool backend. The claim/register/release handshake is
    /// backend-INDEPENDENT (same net id + inboxes + `"grant"` reply), but the
    /// `Presence` backend additionally carries a `"fail"` reply channel on the
    /// claim + register bridges so a `t_reap_held` "fail" reply (a runner that
    /// vanished while holding the unit) fails the holding instance fast. The
    /// static `Tokens` (and `Scheduler`) backends never emit `presence_expired`,
    /// so their lowering stays byte-identical (no fail path is wired).
    pub(super) backend: aithericon_resources::pool::PoolBackend,
}

/// Which deployment role resolved a pool-resource binding — the Executor.capacity
/// entry (token/presence in-net admission) or the Scheduled/LeaseScope entry (a
/// scheduler lease). This is the BACKEND-keyed replacement for the old
/// `expected_kinds: &[&str]` gate: instead of naming acceptable kind strings, the
/// caller names its role and [`resolve_binding`] checks the alias's resolved
/// [`CapacityBackend`] is one the role accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeploymentRole {
    /// `Executor { capacity: { alias } }` — accepts `Tokens` or `Presence`
    /// (platform-owned in-net admission pools).
    ExecutorCapacity,
    /// `Scheduled { scheduler: alias, .. }` — accepts `Scheduler` ONLY (a lease
    /// against an external allocator, i.e. a `datacenter`). A standalone cluster
    /// submit is never presence-backed.
    SchedulerLease,
    /// `LeaseScope { lease.pool }` — holds ONE unit of capacity across its body.
    /// Accepts `Scheduler` (a datacenter allocation) OR `Presence` (a single held
    /// lab runner). Both park a typed lease whose `executor_namespace` the body
    /// steps inherit by containment (`lease-<grant>` vs `runner.<id>`). Rejects
    /// `Tokens` (in-net admission, no held namespace) and every non-pool backend.
    LeaseHolder,
}

/// Resolve a pool-resource alias (required) → a [`PoolBinding`], gated to the
/// backend(s) a [`DeploymentRole`] accepts.
///
/// Shared by the two claim/grant/register/release entry points — they differ
/// ONLY in which alias they resolve and which backend they accept:
/// - [`DeploymentRole::ExecutorCapacity`] → `Tokens` | `Presence`. Both are
///   platform-owned in-net capacity pools with the IDENTICAL cross-net handshake
///   (same `pool-<id>` net id + claim/register/release inboxes + `"grant"`
///   reply), so the downstream body-wrapping is shared; only the
///   `Lease__<backend>` shape and the presence-only `"fail"` path differ, which
///   the returned [`PoolBinding::backend`] discriminates.
/// - [`DeploymentRole::SchedulerLease`] → `Scheduler` (R4). Same body wrapping;
///   only the backing net + `Lease__scheduler` differ.
///
/// The alias's backend is resolved through the SINGLE dispatch authority
/// ([`crate::models::capacity::axes_for_resource`] → [`CapacityBackend`]), NOT a
/// kind-string switch: a `capacity` parses its `public_config` axes; a
/// `datacenter` returns its locked lease axes (→ `Scheduler`). The net-backed
/// [`CapacityBackend`] then maps to a [`aithericon_resources::pool::PoolBackend`]
/// whose claim/lease schemas this reads via `pool::schemas_for_backend`.
///
/// Errors:
/// - alias not in `known_resources` → `WorkspaceResourceUnknown` (normally
///   caught earlier at publish by `discover_known_resources`).
/// - alias resolves to a backend the role does not accept (incl. `Queue` /
///   `Deferred` / a non-pool resource) → a role-specific CompileError
///   (`ResourcePoolNotAPool` for the Executor.capacity entry,
///   `SchedulerNotADatacenter` for the Scheduled entry) steering the author to
///   the right deployment model.
/// - `request` fails validation against the backend's claim schema →
///   `ResourcePoolRequestInvalid`.
pub(super) fn resolve_binding(
    node_id: &str,
    alias: &str,
    request: Option<&serde_json::Value>,
    role: DeploymentRole,
    known: &crate::compiler::resource_refs::KnownResources,
    container: Option<&crate::compiler::compile::CompilerContainerSpec>,
) -> Result<PoolBinding, CompileError> {
    use aithericon_resources::pool::PoolBackend;

    let resource = known
        .get(alias)
        .ok_or_else(|| CompileError::WorkspaceResourceUnknown {
            node_id: node_id.to_string(),
            alias: alias.to_string(),
        })?;
    let kind = resource.type_name.clone();

    // A role-appropriate "this alias isn't the right kind of pool" error, named
    // by the resolved BACKEND (or "not a pool" when the resource resolves to no
    // dispatch backend at all). The Executor.capacity entry steers the author to
    // Scheduled; the Scheduled entry steers back to Executor.capacity.
    let wrong_backend = |label: &str| -> CompileError {
        match role {
            DeploymentRole::SchedulerLease => CompileError::SchedulerNotADatacenter {
                node_id: node_id.to_string(),
                alias: alias.to_string(),
                backend: label.to_string(),
            },
            DeploymentRole::ExecutorCapacity => CompileError::ResourcePoolNotAPool {
                node_id: node_id.to_string(),
                alias: alias.to_string(),
                backend: label.to_string(),
            },
            DeploymentRole::LeaseHolder => CompileError::LeaseScopeNotLeasable {
                node_id: node_id.to_string(),
                alias: alias.to_string(),
                backend: label.to_string(),
            },
        }
    };

    // Resolve the alias's axes → CapacityBackend through the single authority.
    // A non-object public_config can't carry axes (and a `capacity` with garbage
    // config resolves to None) — treat as "not a pool".
    let public_map = resource
        .public_config
        .as_object()
        .cloned()
        .unwrap_or_default();
    let capacity_backend = crate::models::capacity::axes_for_resource(&kind, &public_map)
        .map(|axes| axes.backend());

    // Map the resolved CapacityBackend to a net-backed PoolBackend the role
    // accepts. Queue / Deferred / None have no admission net, so they are a
    // "not a pool" error for either role. The role then narrows further:
    // ExecutorCapacity rejects Scheduler; SchedulerLease accepts ONLY Scheduler.
    let backend: PoolBackend = match capacity_backend {
        Some(crate::models::capacity::CapacityBackend::Tokens) => PoolBackend::Tokens,
        Some(crate::models::capacity::CapacityBackend::Presence) => PoolBackend::Presence,
        Some(crate::models::capacity::CapacityBackend::Scheduler) => PoolBackend::Scheduler,
        // A worker queue / deferred-quota / non-pool resource has no admission
        // net for either role.
        Some(crate::models::capacity::CapacityBackend::Queue) => return Err(wrong_backend("queue")),
        Some(crate::models::capacity::CapacityBackend::Deferred) => {
            return Err(wrong_backend("deferred"))
        }
        None => return Err(wrong_backend("non-pool")),
    };
    let role_accepts = match role {
        DeploymentRole::ExecutorCapacity => {
            matches!(backend, PoolBackend::Tokens | PoolBackend::Presence)
        }
        DeploymentRole::SchedulerLease => matches!(backend, PoolBackend::Scheduler),
        // A held lease can be backed by a datacenter alloc OR a presence runner;
        // a `tokens` concurrency limit has no held namespace and is rejected.
        DeploymentRole::LeaseHolder => {
            matches!(backend, PoolBackend::Scheduler | PoolBackend::Presence)
        }
    };
    if !role_accepts {
        // e.g. a Scheduler-backed alias under Executor.capacity, or a
        // Tokens/Presence-backed alias under Scheduled.
        let label = match backend {
            PoolBackend::Tokens => "tokens",
            PoolBackend::Presence => "presence",
            PoolBackend::Scheduler => "scheduler",
        };
        return Err(wrong_backend(label));
    }

    // Flavor-gated connection validation for the scheduler lease (datacenter).
    // Both the per-step `Scheduled.scheduler` lease path and the loop
    // `Loop.lease.scheduler` path funnel through here, so this is the single
    // choke point. We assert the PUBLIC connection fields the flavor needs are
    // present; the required SECRET (slurm `ssh_key`) is structurally guaranteed
    // by the resource-create validator. Hard-fail with no fallback.
    if backend == PoolBackend::Scheduler {
        if let Some(missing) = datacenter_missing_connection_fields(&resource.public_config) {
            return Err(CompileError::DatacenterConnectionIncomplete {
                node_id: node_id.to_string(),
                alias: alias.to_string(),
                flavor: missing.0,
                missing: missing.1,
            });
        }
    }

    // The backend's claim/lease schemas, keyed by PoolBackend (the service owns
    // axes → backend; the shared crate owns backend → schema).
    let pool_schemas = aithericon_resources::pool::schemas_for_backend(backend);

    // Validate `request` against the backend's claim schema before we bake it
    // into the ClaimRequest. Same `jsonschema` crate/version the engine
    // `SchemaRegistry` uses, so compile-time and runtime agree.
    let validated_req: Option<serde_json::Value> = match request {
        None => None,
        Some(req) => {
            let validator = jsonschema::validator_for(&pool_schemas.claim).map_err(|e| {
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
            Some(req.clone())
        }
    };

    // Container injection (lease path): a scheduler lease whose holder step
    // carries a `CompilerContainerSpec` gets the spec merged into the claim
    // `request` JSON under a `container` key — the claim `request` flows VERBATIM
    // to the engine's `acquire_lease`, which reads `request.get("container")`
    // (slurm_allocator.rs) to wrap the held alloc's drain executor in the `.sif`.
    // The merge happens AFTER schema validation (the container key is not part of
    // the backend's claim_schema). When there is no container OR the backend is
    // not the scheduler, `request_rhai` is byte-identical to the pre-container
    // path (this is the hard token-pool / no-container invariant).
    let request_rhai = match container.filter(|_| backend == PoolBackend::Scheduler) {
        Some(spec) => {
            let mut map = match validated_req {
                Some(serde_json::Value::Object(obj)) => obj,
                _ => serde_json::Map::new(),
            };
            map.insert(
                "container".to_string(),
                serde_json::to_value(spec).map_err(|e| {
                    CompileError::ResourcePoolRequestInvalid {
                        node_id: node_id.to_string(),
                        alias: alias.to_string(),
                        message: format!("container spec failed to serialize: {e}"),
                    }
                })?,
            );
            json_to_rhai_literal(&serde_json::Value::Object(map))
        }
        None => match validated_req {
            None => "()".to_string(),
            Some(req) => json_to_rhai_literal(&req),
        },
    };

    Ok(PoolBinding {
        backing_net_id: well_known::pool_net_id(resource.id),
        lease_def_name: lease_def_name(backend),
        // Strip the schemars envelope (`$schema`, `title`) so the registered
        // definition is a bare object schema matching the `Data__`/`Ctrl__`
        // convention — the engine wraps it as `{definitions, $ref}` and a
        // nested draft `$schema` would be redundant noise. The lease schema may
        // carry an inlined `oneOf` (the scheduler flavor union) but is
        // SELF-CONTAINED — `pool::schema_value` inlines subschemas, so there is
        // no internal `$ref`/`definitions` to lift.
        lease_schema: sanitize_definition_schema(pool_schemas.lease),
        request_rhai,
        backend,
    })
}

/// The AIR definition name for a backend's typed grant/lease: `Lease__tokens` /
/// `Lease__presence` / `Lease__scheduler`. Per-BACKEND (not per-kind) so the
/// two presence/token capacity kinds AND the datacenter all land on their
/// backend's one stable name. `compile_to_air` deduplicates definitions on this
/// name (one `Lease__tokens` regardless of N pooled nodes) and the grant-inbox
/// place's `token_schema` is `#/definitions/<name>`.
pub(super) fn lease_def_name(backend: aithericon_resources::pool::PoolBackend) -> String {
    use aithericon_resources::pool::PoolBackend;
    let suffix = match backend {
        PoolBackend::Tokens => "tokens",
        PoolBackend::Presence => "presence",
        PoolBackend::Scheduler => "scheduler",
    };
    format!("Lease__{suffix}")
}

/// Validate a `datacenter` resource's public connection config against its
/// declared `scheduler_flavor`. Returns `Some((flavor, missing_fields))` when
/// a flavor's required PUBLIC fields are absent, else `None`.
///
/// Required public fields per flavor (the matching secret is enforced by the
/// resource-create validator, not here):
/// - `slurm` → `ssh_host`, `ssh_user`, `template_dir`
/// - `nomad` → `nomad_addr`
/// - `http`  → `allocator_url`
///
/// An unknown/absent flavor is treated as "http" (the default leg). A field
/// counts as present when it's a non-null, non-empty-string JSON value.
pub(super) fn datacenter_missing_connection_fields(
    public_config: &serde_json::Value,
) -> Option<(String, Vec<String>)> {
    let present = |key: &str| -> bool {
        match public_config.get(key) {
            None | Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::String(s)) => !s.trim().is_empty(),
            Some(_) => true,
        }
    };

    let flavor = public_config
        .get("scheduler_flavor")
        .and_then(|v| v.as_str())
        .unwrap_or("http")
        .to_string();

    let required: &[&str] = match flavor.as_str() {
        "slurm" => &["ssh_host", "ssh_user", "template_dir"],
        "nomad" => &["nomad_addr"],
        // "http" and any unrecognized flavor fall back to the HTTP leg.
        _ => &["allocator_url"],
    };

    let missing: Vec<String> = required
        .iter()
        .filter(|k| !present(k))
        .map(|k| (*k).to_string())
        .collect();

    if missing.is_empty() {
        None
    } else {
        Some((flavor, missing))
    }
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

/// Pooled (`Executor { capacity: Some }`) AutomatedStep: the executor-lifecycle
/// body wrapped in a **claim / register / release** handshake against the
/// resolved `concurrency_limit` resource's backing net (`well_known::pool_net_id`,
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
        deployment_model: DeploymentModel::Executor {
            capacity: Some(binding),
            ..
        },
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_automated_step_pooled only runs for Executor capacity:Some")
    };
    // Resolve `Executor.capacity.alias` (required) against the workspace-resource
    // manifest: a `capacity` resource whose axes resolve to a Tokens (seeded) or
    // Presence (instrument) backend → the deterministic backing net
    // `pool-<resource_id>`, validated `request`, and a typed, body-visible lease
    // (R2/R3 + Phase 3). Both backends share the IDENTICAL
    // claim/grant/register/release handshake; the returned binding's `backend`
    // discriminates the presence-only `"fail"` path inside `lower_pooled_body`. A
    // queue/deferred/scheduler/non-pool alias is a CompileError.
    let pool_binding = resolve_binding(
        &cx.node.id,
        &binding.alias,
        binding.request.as_ref(),
        DeploymentRole::ExecutorCapacity,
        cx.known_resources,
        // A token/presence pool is OUR in-net admission pool, not a cluster — no
        // container injection; `None` keeps the token-pool claim AIR
        // byte-identical (hard invariant).
        None,
    )?;
    lower_pooled_body(cx, pool_binding)
}

/// Rhai fragment stamping a resolved scheduler `job_template` slug onto the job
/// token `d` as `job_template_id` (B-staging slug forwarding, Phase 4). The
/// engine's `SchedulerSubmitHandler` reads `job_data.job_template_id` and
/// dispatches THAT registered parameterized job (falling back to its config
/// default only when the field is absent), so this is how a `Scheduled` step's
/// Phase-3-resolved template name finally reaches the cluster dispatch.
///
/// A bare string-literal assignment (no borrow ref) — safe under `logic()`'s
/// build-time validation. Empty for a non-Scheduled step or an unresolved
/// (empty) slug, leaving the legacy config-default path untouched.
fn scheduled_job_template_frag(node: &WorkflowNode) -> String {
    if let WorkflowNodeData::AutomatedStep {
        deployment_model: DeploymentModel::Scheduled { job_template, .. },
        ..
    } = &node.data
    {
        let slug = job_template.trim();
        if !slug.is_empty() {
            return format!(r#" d.job_template_id = "{}";"#, rhai_str_escape(slug));
        }
    }
    String::new()
}

/// The shared claim/grant/register/release body-wrapping, parameterized by the
/// resolved [`PoolBinding`]. `Executor { capacity: Some }` calls this;
/// the topology + executor job-spec are byte-identical regardless of backend.
fn lower_pooled_body(cx: &mut LoweringCtx, pool_binding: PoolBinding) -> Result<(), CompileError> {
    let id = cx.node.id.clone();
    let WorkflowNodeData::AutomatedStep {
        label,
        execution_spec,
        retry_policy,
        output,
        requirements,
        channels,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_pooled_body on non-AutomatedStep node")
    };
    let label = label.clone();
    // Map-body-terminal gate (computed before the `&mut *cx.ctx` reborrow, same
    // as the inline path). A pooled AutomatedStep that terminates a Map body must
    // forward its FULL completed envelope so the Map's `t_<map>_collect` can read
    // `body.detail.outputs.<resultVar>` + the preserved `__map_idx`/`__map_id`
    // correlation leaves — the slim `split_outputs` control token carries neither.
    // The `out` envelope built by `t_<id>_to_output` already IS that shape (`done`
    // carries `detail.outputs.*`, and the executor lifecycle preserves the
    // `_`-prefixed `__map_*` leaves threaded in via `pending.input`), so only the
    // foundation tail differs: `park_outputs` (fork: park data AND forward the
    // whole token) instead of `split_outputs`. Shared gate — see
    // `super::is_map_body_terminal` (same one the inline path + SubWorkflow use).
    let is_map_body_terminal =
        super::is_map_body_terminal(cx.graph, cx.node.parent_id.as_deref(), cx.outgoing_edges);
    // Streaming-channel manifest + synthesis data, cloned out before the
    // `&mut *cx.ctx` reborrow. Same handling as the inline path.
    let channels = channels.clone();
    let channels_rhai = super::channels::channel_manifest_rhai(&channels);
    let retry_policy = *retry_policy;
    let backend_type = execution_spec.backend_type;
    // Capture the authored placement Requirements as a Rhai literal NOW, while we
    // still hold `&cx.node.data` (the `&mut *cx.ctx` reborrow below ends this
    // borrow). Serialized to JSON then lowered to a Rhai map so the presence
    // pool's `t_grant` guard `satisfies(claim.requirements, unit.caps)` can read
    // `requirements.constraints`. `None` (or an empty set) ⇒ `#{ constraints: [] }`
    // (matches anything — the guard short-circuits to true). Gated on
    // `is_presence` at the claim-payload site so concurrency_limit / Scheduled AIR stays
    // byte-identical (no `requirements` key in their claim).
    let requirements_rhai = match requirements {
        Some(req) if !req.constraints.is_empty() => {
            json_to_rhai_literal(&serde_json::to_value(req).unwrap_or_default())
        }
        _ => "#{ constraints: [] }".to_string(),
    };

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
    crate::compiler::schema_refs::inline_refs(&mut validated_config, cx.definitions).map_err(
        |e| CompileError::SchemaRefUnresolved {
            node_id: id.clone(),
            path: String::new(),
            message: e.to_string(),
        },
    )?;
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

    // Rust panic/Result model. The pooled path ALWAYS routes the exhausted
    // token through the held-consuming release transition (`t_to_error`) so
    // capacity is freed on every exit (docs/14); the wired/unwired choice only
    // changes what happens AFTER release — park into `p_error` (wired) or fall
    // into a throwing panic transition (unwired). Read edges before reborrow.
    // is_agent_tool: see lower_automated_step — a tool child forces p_error so
    // its failure feeds the agent's on_tool_error wiring, never a crash.
    let error_handled = cx.is_agent_tool || super::error_path_wired(cx.outgoing_edges);

    // Slug forwarding (B-staging, Phase 4) — see `lower_automated_step`. For a
    // standalone `Scheduled` datacenter `submit` this is THE path that reaches
    // the engine's `SchedulerSubmitHandler`; stamping `job_template_id` makes it
    // dispatch the registered parameterized job by the resolved slug. Empty for a
    // concurrency_limit body (not Scheduled) — harmless. Read `cx.node` before reborrow.
    let job_template_frag = scheduled_job_template_frag(cx.node);

    // grant_id literal builder (see the doc comment for the replay-safety
    // argument). Built inside the Rhai logic from `input._instance_id` so it
    // is a pure function of journaled token data.
    let grant_id_expr = format!(r#"(input._instance_id + ":{id_lit}")"#);

    // Record the typed-lease definition + the grant-inbox place to type, while
    // we still hold `cx` (the `&mut *cx.ctx` reborrow below would block
    // `cx.fixups`). The grant inbox is created OUTSIDE the lifecycle scope, so
    // its id is the unprefixed `p_{id}_grant_inbox`. `compile_to_air` drains
    // these after `ctx.build()`.
    cx.fixups.lease_definitions.push((
        pool_binding.lease_def_name.clone(),
        pool_binding.lease_schema.clone(),
    ));
    cx.fixups.lease_inbox_schemas.push((
        format!("p_{id}_grant_inbox"),
        pool_binding.lease_def_name.clone(),
    ));

    // Presence pools (Phase 3) admit emergent capacity from runners that check
    // in and reap it when a runner expires. A reaped HELD unit (a runner that
    // vanished while its unit was claimed by a running instance) makes the
    // pool's `t_reap_held` emit a `{ runner_id, unit_id }` notice on the `"fail"`
    // reply channel — resolved from the HELD unit's carried routing — so the
    // holding instance must fail fast (its job is enqueued in a now-dead
    // `runner.<id>` namespace). We therefore (presence ONLY): route a `"fail"`
    // channel on the claim bridge, register the hold over a bridge carrying ONLY
    // that `"fail"` channel (NEVER `"grant"` — preserves the docs/14 taint rule),
    // and emit a register-then-abort pair that throws → NetFailed.
    //
    // The static `Tokens` (concurrency_limit) and `Scheduler` (datacenter) backends
    // never emit `presence_expired`, so this whole block is skipped and their
    // AIR is byte-identical to before.
    let is_presence = pool_binding.backend == aithericon_resources::pool::PoolBackend::Presence;

    let ctx = &mut *cx.ctx;

    // ── Node-interface places (outside the lifecycle scope) ─────────────────
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    // `p_error` only when the error handle is wired; otherwise the failure
    // exit releases capacity and then crashes the net via `t_{id}_panic`.
    let p_error: Option<PlaceHandle<DynamicToken>> = if error_handled {
        Some(ctx.state(format!("p_{id}_error"), format!("{label} - Error")))
    } else {
        None
    };

    // Streaming channels (docs/25): pre-create the `control_emit` inbox (only
    // when the node declares ≥1 OUT control channel) so the lifecycle's submit
    // transition can register `event_routes["control_emit"]` → its id; the same
    // handle is drained by `lower_channels` below. `None` ⇒ AIR byte-stable.
    let p_control_in = super::channels::control_inbox(ctx, id.as_str(), label.as_str(), &channels);

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

    // Presence-only: the held-runner-death "fail" reply inbox. The pool's
    // `t_reap_held` routes a `{ runner_id, unit_id }` notice here over the
    // `"fail"` channel resolved from the HELD unit's carried routing.
    let lease_failed_inbox_place = format!("p_{id}_lease_failed");
    let p_lease_failed_inbox: Option<PlaceHandle<DynamicToken>> = if is_presence {
        Some(ctx.bridge_reply_channel(
            lease_failed_inbox_place.clone(),
            format!("{label} - Runner-Lost Inbox"),
            well_known::POOL_FAIL_CHANNEL,
        ))
    } else {
        None
    };

    // Claim bridge_out. concurrency_limit/datacenter route ONLY the "grant" reply
    // (byte-identical to before). A presence pool ALSO routes the "fail" reply
    // (held-runner death) so the death notice reaches THIS instance/holder.
    let p_claim_out: PlaceHandle<DynamicToken> = if is_presence {
        ctx.bridge_out_reply_channels(
            format!("p_{id}_claim_out"),
            format!("{label} - Claim Capacity"),
            pool_net_id,
            well_known::POOL_CLAIM_INBOX,
            &[
                ("grant", grant_inbox_place.as_str()),
                (well_known::POOL_FAIL_CHANNEL, lease_failed_inbox_place.as_str()),
            ],
        )
    } else {
        ctx.bridge_out_reply_channels(
            format!("p_{id}_claim_out"),
            format!("{label} - Claim Capacity"),
            pool_net_id,
            well_known::POOL_CLAIM_INBOX,
            &[("grant", grant_inbox_place.as_str())],
        )
    };
    // Register bridge. concurrency_limit/datacenter register over a PLAIN bridge (no
    // reply routing) so recycled capacity tokens stay clean (docs/14 taint).
    // A presence pool registers the hold over a bridge carrying ONLY the "fail"
    // channel — NEVER "grant" — so `t_reap_held` can resolve the holder's fail
    // address from the in_use hold's routing, while the recycled unit (rebuilt
    // by the pool's own `t_release`/`t_reap_free` from clean data) never carries
    // stale "grant" routing that could wedge the pool.
    let p_register_out: PlaceHandle<DynamicToken> = if is_presence {
        ctx.bridge_out_reply_channels(
            format!("p_{id}_register_out"),
            format!("{label} - Register Hold"),
            pool_net_id,
            well_known::POOL_REGISTER_INBOX,
            &[(well_known::POOL_FAIL_CHANNEL, lease_failed_inbox_place.as_str())],
        )
    } else {
        ctx.bridge_out(
            format!("p_{id}_register_out"),
            format!("{label} - Register Hold"),
            pool_net_id,
            well_known::POOL_REGISTER_INBOX,
        )
    };
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
    //
    // PRESENCE pools ONLY additionally carry the step's placement `requirements`
    // so the presence pool's guarded `t_grant`
    // (`satisfies(claim.requirements, unit.caps)`) can admit only a runner whose
    // advertised caps satisfy every constraint. concurrency_limit (static `Tokens`)
    // claims get NO `requirements` field — their `t_grant` is UNGUARDED — so
    // their claim AIR stays byte-identical to pre-Phase-4. This is the sole
    // claim-payload divergence between the two backends.
    let claim_payload = if is_presence {
        format!(
            "#{{ grant_id: gid, request: {}, requirements: {} }}",
            pool_binding.request_rhai, requirements_rhai
        )
    } else {
        format!(
            "#{{ grant_id: gid, request: {} }}",
            pool_binding.request_rhai
        )
    };
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
                // Streaming channels (docs/25) are synthesized as a separate
                // control-emit path, not via the lifecycle's Output side-channel.
                stream_output: None,
                // Pre-created control-emit inbox (docs/25). `Some` only when the
                // node has ≥1 OUT control channel.
                control_in: p_control_in.clone(),
            },
        );

        // Retry re-injects a fresh submit into the SAME inbox — the hold
        // (p_held) persists across retries, so we do NOT re-claim per retry.
        // The retry topology's terminal `exhausted` edge drains to
        // `p_exhausted` (NOT `p_error`) so the hold can be released first.
        // The pooled exhausted token MUST flow to `p_exhausted` (consumed by
        // the held-releasing `t_to_error`), regardless of whether the node's
        // error handle is wired — capacity release is non-negotiable (docs/14).
        // So pass `error_handled = true` here (route to the sink); the
        // wired/unwired panic decision is made downstream at `t_to_error`.
        build_retry_topology(
            ctx,
            &retry_policy,
            &lc.failed,
            &lc.timed_out,
            &exec_inbox_retry,
            &lc.effect_errors,
            Some(&p_exhausted),
            true,
            label.as_str(),
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
    // The routed `grant` token IS the typed lease (validated vs `Lease__<backend>`
    // on `p_grant_inbox`). We (a) stage it into the body as `lease.json` so
    // body code reads `lease.<field>` (e.g. `lease.unit_id`), mirroring the
    // resource-envelope `<alias>.json` staging, and (b) park the WHOLE lease on
    // `p_held` so `t_to_output` can merge it into the parked data envelope.
    // `grant` still carries `grant_id` (the correlation key), so the
    // release-by-grant_id path is unchanged.
    //
    // If the lease carries an `executor_namespace` (emitted by datacenter
    // allocators), we stamp it onto the job token so the engine's submit
    // handler targets the warm executor.
    let lease_stage_push = r#"job_inputs.push(#{ "name": "lease.json", "source": #{ "type": "inline", "value": grant } }); "#;
    // Default-route to the workspace's `default` worker GROUP partition
    // (`executor-<wire>-grp/<default_uuid>`), then let a grant-supplied namespace
    // override. A concurrency_limit grant carries no `executor_namespace` (only
    // `grant_id`/`unit_id`), so without this default its body would have no queue
    // to land on — there is no bare `executor-<wire>` dispatch path any more
    // (unified single-stream model). With the default, concurrency_limit bodies
    // are drained by the workspace's default-group workers. presence grants
    // (`runner.{id}`) and datacenter/lease grants (`lease-<grant>`) DO carry an
    // `executor_namespace`, so the override wins and their exclusive routing is
    // unchanged at runtime. The partition is the default group's capacity-resource
    // UUID, resolved through `cx.known_resources` (the seeder + the `discover` head
    // injection guarantee it is in the map in production); the no-registry compile
    // path (tests / analyze) falls back to the literal `default` token.
    let default_group_partition: String = cx
        .known_resources
        .get(crate::worker_groups::DEFAULT_WORKER_GROUP_PATH)
        .map(|r| r.id.to_string())
        .unwrap_or_else(|| crate::worker_groups::DEFAULT_WORKER_GROUP_PATH.to_string());
    let ns_stamp = format!(
        r#" d.executor_namespace = "{ns}"; if grant.executor_namespace != () {{ d.executor_namespace = grant.executor_namespace; }} "#,
        ns = backend_type.executor_namespace_for_group(&default_group_partition)
    );

    // Carry the full lease so the hold echo + held parking both keep every
    // lease field; `grant.grant_id` is the correlation key the pool keys
    // register/release on.
    let reg_payload = "grant";
    let held_payload = "grant";
    let stream_events_rhai = r#"["metric", "progress", "phase", "log"]"#;
    ctx.transition(format!("t_{id}_acquire"), format!("{label} - Acquire"))
        .auto_input("pending", &p_pending)
        .auto_input("grant", &p_grant_inbox)
        .correlate("grant", "pending", "grant_id")
        .auto_output("job", &exec_inbox)
        .auto_output("reg", &p_register_out)
        .auto_output("held", &p_held)
        .logic(format!(
            r#"let input = pending.input; let d = input; d.job_id = "{id_lit}"; d.run = 0; d.retries = 0; d.max_retries = {max_retries}; let job_inputs = {inputs_rhai}; job_inputs.push(#{{ "name": "input.json", "source": #{{ "type": "inline", "value": input }} }}); {lease_stage_push}{ns_stamp}{job_template_frag}/*__BORROWED_INPUTS__*/ d.spec = #{{ "backend": "{backend_wire}", "inputs": job_inputs, "outputs": {outputs_rhai}, "config_ref": {config_ref_rhai}, "stream_events": {stream_events_rhai}, "channels": {channels_rhai} }}; #{{ job: d, reg: {reg_payload}, held: {held_payload} }}"#
        ));

    // ── Terminal exits: BOTH consume p_held and BOTH arc to p_release_out.
    // Success path: lifecycle `completed` + held → output + release. ────────
    //
    // Merge the held lease into the output envelope under a `lease` key BEFORE
    // it is parked by `split_outputs` (→ `p_{id}_data`), so a downstream
    // `<slug>.lease.<field>` borrow resolves through the standard read-arc
    // pipeline against the parked data place. The parked `Data__<id>` schema is
    // `additionalProperties: true`, so the extra `lease` key validates.
    let to_output_logic = r#"let out = done; out.lease = held; #{ output: out, release: #{ grant_id: held.grant_id } }"#;
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
    if let Some(p_error) = &p_error {
        // Wired: release capacity AND park the error token for the handler.
        ctx.transition(format!("t_{id}_to_error"), format!("{label} - To Error"))
            .auto_input("err", &p_exhausted)
            .auto_input("held", &p_held)
            .auto_output("error", p_error)
            .auto_output("release", &p_release_out)
            .logic(r#"#{ error: err, release: #{ grant_id: held.grant_id } }"#);
    } else {
        // Unwired: release capacity FIRST (every-exit-releases invariant), then
        // crash the net. `t_to_error` consumes {p_exhausted, p_held}, emits the
        // release, and parks the error token into `p_{id}_panic_in`; the
        // separate `t_{id}_panic` then throws (permanent ScriptError → NetFailed).
        // Park-then-throw keeps the release arc intact — capacity is freed
        // before the unwind.
        let p_panic_in: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_panic_in"),
            format!("{label} - Panic (released, awaiting crash)"),
        );
        ctx.transition(format!("t_{id}_to_error"), format!("{label} - To Error"))
            .auto_input("err", &p_exhausted)
            .auto_input("held", &p_held)
            .auto_output("panic", &p_panic_in)
            .auto_output("release", &p_release_out)
            .logic(r#"#{ panic: err, release: #{ grant_id: held.grant_id } }"#);

        let msg = format!("pooled step '{label}' failed and no error handler is wired");
        ctx.transition(
            format!("t_{id}_panic"),
            format!("{label} - Crash Net (no handler)"),
        )
        .auto_input("panic", &p_panic_in)
        .logic_rhai(format!("throw \"{}\"", rhai_str_escape(&msg)))
        .done();
    }

    // ── Presence-only: fail-fast on held-runner death (Phase 3) ─────────────
    // Symmetric with `lease_bridge`'s `t_lease_failed_register` + `t_lease_abort`
    // (held-allocation death), but for a presence-pool hold: the pool's
    // `t_reap_held` emitted a `{ runner_id, unit_id }` notice on the "fail"
    // channel carried by the in_use hold's routing, landing in
    // `p_{id}_lease_failed`. A register parks the death flag write-once; the
    // abort then CONSUMES `p_held` (so neither `t_to_output` nor `t_to_error`
    // can still fire — the holder can NEVER complete normally once its runner is
    // gone) and read-arcs the flag, then `throw`s → ErrorOccurred + NetFailed,
    // which the existing panic-on-unconnected-failure / subworkflow-failure
    // machinery carries to the caller (the dead-while-running path). No release
    // is bridged: `t_reap_held` already dropped the hold from the pool's
    // `in_use`, so a release-by-grant_id would have nothing to correlate.
    //
    // Skipped entirely for concurrency_limit/datacenter (no `presence_expired` exists),
    // keeping their AIR byte-identical.
    if let Some(p_lease_failed_inbox) = &p_lease_failed_inbox {
        let p_lease_failed: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_lease_failed_parked"),
            format!("{label} - Runner Lost (parked)"),
        );
        // Register the death notice write-once so the abort can fire even mid-run
        // (the runner can vanish while the job is still executing).
        ctx.transition(
            format!("t_{id}_lease_failed_register"),
            format!("{label} - Register Runner Loss"),
        )
        .auto_input("fail", p_lease_failed_inbox)
        .auto_output("flag", &p_lease_failed)
        .logic_rhai("#{ flag: #{ unit_id: fail.unit_id, failed: true } }")
        .done();

        let df = format!("df_{}", id.replace('-', "_"));
        let abort_msg = format!(
            "pooled step '{label}': the runner holding this unit went away mid-run \
             (its drain executor is gone; the enqueued job would hang in a dead namespace) \
             — failing fast"
        );
        ctx.transition(
            format!("t_{id}_lease_abort"),
            format!("{label} - Runner Lost (abort)"),
        )
        .auto_input("held", &p_held)
        .read_input(df.clone(), &p_lease_failed)
        .guard_rhai(format!("{df}.failed == true"))
        .priority("100")
        .logic_rhai(format!("throw \"{}\"", rhai_str_escape(&abort_msg)))
        .done();
    }

    // Streaming channels (docs/25): same synthesis as the inline path.
    let lowered_channels = super::channels::lower_channels(
        ctx,
        &id,
        &label,
        &id,
        &channels,
        &cx.graph.edges,
        p_control_in,
        &p_input,
    );

    // Foundation tail — mirrors the inline path. A Map body terminal forks the
    // FULL completed envelope (`park_outputs`) so the Map collect can lift
    // `body.detail.outputs.<resultVar>` + the `__map_*` leaves; otherwise the
    // slim `split_outputs` control token. Either way the parked data place is
    // produced, so any downstream `<slug>.<field>` borrow is unaffected.
    let (data_place_id, p_ctrl) = if is_map_body_terminal {
        park_outputs(ctx, &id, &label, &p_output)
    } else {
        split_outputs(ctx, &id, &label, &p_output)
    };
    let mut output_places = vec![(None, p_ctrl)];
    if let Some(p_error) = p_error {
        output_places.push((Some("error".to_string()), p_error));
    }
    let mut input_handles = HashMap::new();
    for port in lowered_channels.ports {
        match port.direction {
            crate::models::template::ChannelDirection::Out => {
                output_places.push((Some(port.name), port.place));
            }
            crate::models::template::ChannelDirection::In => {
                input_handles.insert(port.name, port.place);
            }
        }
    }
    cx.ports.insert(
        id.clone(),
        NodePorts {
            input_place: p_input,
            output_places,
            input_places: HashMap::new(),
            input_handles,
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

    let (mut query_token, _no_inputs) = crate::compiler::backend_configs::validate_and_transform(
        &backend_type,
        &execution_spec.config,
        cx.node_files,
        &id,
    )?;
    crate::compiler::schema_refs::inline_refs(&mut query_token, cx.definitions).map_err(|e| {
        CompileError::SchemaRefUnresolved {
            node_id: id.clone(),
            path: String::new(),
            message: e.to_string(),
        }
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
    ctx.transition(format!("t_{id}_q_build"), format!("{label} - Build Query"))
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
            output_places: vec![(None, p_ctrl), (Some("error".to_string()), p_error)],
            input_places: HashMap::new(),
            input_handles: HashMap::new(),
        },
    );
    cx.publish_interface().data_port = Some(data_place_id);
    Ok(())
}

/// Resolve the slug of the lease HOLDER that ENCLOSES `node` — the nearest
/// ancestor (via the `parent_id` chain) that is a `LeaseScope` or a leased
/// `Loop` (`lease: Some`). Returns the holder's
/// `slug()` — the exact key the borrow pipeline + `out_shape_{loop,lease_scope}`
/// use for `<holder>.lease.<field>` — so the injected
/// `<holder_slug>.lease.executor_namespace` ref lines up with the read-arc the
/// matching `guard_readarc_plan` arm registers.
///
/// "Runs on a lease" is IMPLICIT BY CONTAINMENT — there is no per-step flag.
/// The walk climbs the `parent_id` chain UP to the nearest ancestor that HOLDS
/// a lease: a `LeaseScope` (always holds) OR a leased `Loop` (`lease: Some`). A
/// `Scheduled` step can sit inside a plain `Loop` inside a `LeaseScope` (the
/// holder is 2 levels up), so the chain-walk — not just the direct parent — is
/// what makes containment work. Nearest-holder-wins. A plain (lease-less) Loop
/// is transparent: keep climbing past it.
///
/// `None` when the body has no parent, no ancestor is a lease holder, or every
/// enclosing Loop is lease-less — in which case a `Scheduled` body
/// performs its own standalone single-step lease lifecycle.
pub(crate) fn enclosing_leased_scope_slug(
    node: &WorkflowNode,
    graph: &WorkflowGraph,
) -> Option<String> {
    let mut current = node.parent_id.as_deref();
    while let Some(pid) = current {
        let parent = graph.nodes.iter().find(|n| n.id == pid)?;
        match &parent.data {
            WorkflowNodeData::LeaseScope { .. } => return Some(parent.slug()),
            _ => {
                current = parent.parent_id.as_deref();
            }
        }
    }
    None
}

#[cfg(test)]
mod datacenter_connection_tests {
    use super::datacenter_missing_connection_fields;
    use serde_json::json;

    #[test]
    fn slurm_complete_is_ok() {
        let cfg = json!({
            "scheduler_flavor": "slurm",
            "ssh_host": "login.test",
            "ssh_user": "runner",
            "template_dir": "/opt/jobs",
        });
        assert!(datacenter_missing_connection_fields(&cfg).is_none());
    }

    #[test]
    fn slurm_missing_host_and_dir() {
        let cfg = json!({ "scheduler_flavor": "slurm", "ssh_user": "runner" });
        let (flavor, missing) = datacenter_missing_connection_fields(&cfg).expect("incomplete");
        assert_eq!(flavor, "slurm");
        assert_eq!(
            missing,
            vec!["ssh_host".to_string(), "template_dir".to_string()]
        );
    }

    #[test]
    fn slurm_empty_string_counts_as_missing() {
        let cfg = json!({
            "scheduler_flavor": "slurm",
            "ssh_host": "   ",
            "ssh_user": "runner",
            "template_dir": "/opt/jobs",
        });
        let (_, missing) = datacenter_missing_connection_fields(&cfg).expect("blank is missing");
        assert_eq!(missing, vec!["ssh_host".to_string()]);
    }

    #[test]
    fn nomad_needs_addr() {
        let ok = json!({ "scheduler_flavor": "nomad", "nomad_addr": "http://nomad:4646" });
        assert!(datacenter_missing_connection_fields(&ok).is_none());
        let bad = json!({ "scheduler_flavor": "nomad" });
        let (flavor, missing) = datacenter_missing_connection_fields(&bad).expect("incomplete");
        assert_eq!(flavor, "nomad");
        assert_eq!(missing, vec!["nomad_addr".to_string()]);
    }

    #[test]
    fn http_default_needs_allocator_url() {
        // Absent flavor falls back to the http leg.
        let bad = json!({});
        let (flavor, missing) = datacenter_missing_connection_fields(&bad).expect("incomplete");
        assert_eq!(flavor, "http");
        assert_eq!(missing, vec!["allocator_url".to_string()]);

        let ok = json!({ "scheduler_flavor": "http", "allocator_url": "http://a.test" });
        assert!(datacenter_missing_connection_fields(&ok).is_none());
    }
}

#[cfg(test)]
mod slug_forwarding_tests {
    use super::scheduled_job_template_frag;
    use crate::models::template::WorkflowNode;
    use serde_json::json;

    fn scheduled_node(job_template: &str) -> WorkflowNode {
        serde_json::from_value(json!({
            "id": "n1",
            "type": "automated_step",
            "slug": "n1",
            "position": { "x": 0.0, "y": 0.0 },
            "data": {
                "type": "automated_step",
                "label": "Step",
                "executionSpec": { "backendType": "docker", "config": { "image": "alpine:latest" } },
                "deploymentModel": { "mode": "scheduled", "scheduler": "nomad_dc", "jobTemplate": job_template },
            }
        }))
        .expect("scheduled node fixture")
    }

    fn executor_node() -> WorkflowNode {
        serde_json::from_value(json!({
            "id": "n2",
            "type": "automated_step",
            "slug": "n2",
            "position": { "x": 0.0, "y": 0.0 },
            "data": {
                "type": "automated_step",
                "label": "Step",
                "executionSpec": { "backendType": "docker", "config": { "image": "alpine:latest" } },
                "deploymentModel": { "mode": "executor" },
            }
        }))
        .expect("executor node fixture")
    }

    #[test]
    fn resolved_slug_is_stamped_as_job_template_id() {
        // A Scheduled step whose `job_template` was resolved to a concrete slug
        // (Phase 3) forwards it to the engine submit handler via `d.job_template_id`.
        let node = scheduled_node("petri_stage_demo");
        assert_eq!(
            scheduled_job_template_frag(&node),
            r#" d.job_template_id = "petri_stage_demo";"#
        );
    }

    #[test]
    fn empty_slug_yields_no_frag() {
        // No ref / unresolved slug ⇒ the engine handler's config default applies.
        assert_eq!(scheduled_job_template_frag(&scheduled_node("")), "");
        assert_eq!(scheduled_job_template_frag(&scheduled_node("   ")), "");
    }

    #[test]
    fn non_scheduled_step_yields_no_frag() {
        // A concurrency_limit / plain Executor body carries no job-template name.
        assert_eq!(scheduled_job_template_frag(&executor_node()), "");
    }

    #[test]
    fn slug_is_rhai_escaped() {
        // Defensive: a slug with a quote can't break out of the literal (slugs are
        // validated snake_case at create, but the stamp must escape regardless).
        let node = scheduled_node(r#"a"b"#);
        let frag = scheduled_job_template_frag(&node);
        assert!(frag.contains(r#"\""#), "quote must be escaped: {frag}");
    }
}

/// Container-injection lowering: a `CompilerContainerSpec` keyed on a node id
/// must land (a) on the submit path as `d.container = #{ … }` in the plain
/// executor `prepare` logic, and (b) on the lease path as a `container: #{ … }`
/// key inside the datacenter claim `request` literal. An EMPTY `container_specs`
/// map must leave AIR byte-identical to today (no container key anywhere).
#[cfg(test)]
mod container_injection_tests {
    use crate::compiler::resource_refs::{KnownResource, KnownResources};
    use crate::compiler::{
        compile_to_air_with_options, CompileOptions, CompilerContainerSpec,
    };
    use crate::models::template::WorkflowGraph;
    use serde_json::json;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn spec() -> CompilerContainerSpec {
        CompilerContainerSpec {
            sif_path: "/shared/sif/by-ref/python_3_12_slim.sif".to_string(),
            binds: vec![
                "/opt/petri/bin".to_string(),
                "/shared/venv-cache/python_3_12_slim".to_string(),
            ],
            nv: false,
        }
    }

    /// Pull a transition's Rhai logic source out of the compiled AIR, matching
    /// either the utoipa-tagged (`{ "Rhai": { "source" } }`) or the serde
    /// flat-tag (`{ "type": "rhai", "source" }`) shape.
    fn logic_source(air: &serde_json::Value, transition_id: &str) -> String {
        let transitions = air
            .get("transitions")
            .and_then(|t| t.as_array())
            .expect("transitions array");
        let t = transitions
            .iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(transition_id))
            .unwrap_or_else(|| panic!("transition {transition_id} not found"));
        let logic = t.get("logic").expect("transition has logic");
        logic
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .and_then(|s| s.as_str())
            .or_else(|| logic.get("source").and_then(|s| s.as_str()))
            .unwrap_or_else(|| panic!("logic source not findable for {transition_id}"))
            .to_string()
    }

    /// A linear graph `start → <step> → end` where `<step>` is an
    /// `executor`-mode AutomatedStep. The plain executor lowering builds the
    /// `<id>/prepare` transition that carries `container_frag`.
    fn executor_step_graph() -> WorkflowGraph {
        serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"step","type":"automated_step","slug":"step","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Step",
                     "executionSpec":{"backendType":"docker","config":{"image":"python:3.12-slim"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"executor"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"step","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"step","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("executor graph deser")
    }

    /// A linear graph `start → <step> → end` where `<step>` is a standalone
    /// `scheduled` AutomatedStep bound to a `datacenter` resource — it lowers
    /// through the single-node lease lifecycle (`t_<id>_claim`).
    fn scheduled_step_graph() -> WorkflowGraph {
        serde_json::from_value(json!({
          "nodes": [
            {"id":"start","type":"start","position":{"x":0,"y":0},
             "data":{"type":"start","label":"Start"}},
            {"id":"step","type":"automated_step","slug":"step","position":{"x":0,"y":0},
             "data":{"type":"automated_step","label":"Step",
                     "executionSpec":{"backendType":"docker","config":{"image":"python:3.12-slim"}},
                     "retryPolicy":{"maxRetries":0,"strategy":{"type":"immediate"}},
                     "deploymentModel":{"mode":"scheduled","scheduler":"hpc","jobTemplate":"petri_demo"}}},
            {"id":"end","type":"end","position":{"x":0,"y":0},
             "data":{"type":"end","label":"End"}}
          ],
          "edges":[
            {"id":"e1","source":"start","target":"step","targetHandle":"in","type":"sequence"},
            {"id":"e2","source":"step","target":"end","targetHandle":"in","type":"sequence"}
          ]
        }))
        .expect("scheduled graph deser")
    }

    /// A complete `slurm` datacenter resource (so `resolve_binding`'s
    /// flavor-gated connection validation passes).
    fn datacenter_known() -> KnownResources {
        let mut known = KnownResources::new();
        known.insert(
            "hpc".to_string(),
            KnownResource {
                id: Uuid::new_v4(),
                type_name: "datacenter".to_string(),
                latest_version: 1,
                public_config: json!({
                    "scheduler_flavor": "slurm",
                    "ssh_host": "login.test",
                    "ssh_user": "runner",
                    "template_dir": "/opt/jobs",
                }),
            },
        );
        known
    }

    #[test]
    fn submit_path_stamps_container_onto_job_token() {
        let graph = executor_step_graph();
        let mut container_specs = HashMap::new();
        container_specs.insert("step".to_string(), spec());

        let crate::compiler::CompileArtifacts { air, .. } = compile_to_air_with_options(
            &graph,
            "container-submit",
            "test",
            &HashMap::new(),
            CompileOptions {
                container_specs: &container_specs,
                ..Default::default()
            },
        )
        .expect("compile ok");

        let logic = logic_source(&air, "step/prepare");
        assert!(
            logic.contains("d.container = #{"),
            "prepare must stamp d.container; got:\n{logic}"
        );
        assert!(
            logic.contains("/shared/sif/by-ref/python_3_12_slim.sif"),
            "prepare must embed the by-ref sif path; got:\n{logic}"
        );
    }

    #[test]
    fn lease_path_merges_container_into_claim_request() {
        let graph = scheduled_step_graph();
        let known = datacenter_known();
        let known_globals = crate::compiler::named_global::globals_from_resources(&known);
        let mut container_specs = HashMap::new();
        container_specs.insert("step".to_string(), spec());

        let crate::compiler::CompileArtifacts { air, .. } = compile_to_air_with_options(
            &graph,
            "container-lease",
            "test",
            &HashMap::new(),
            CompileOptions {
                known_globals: &known_globals,
                container_specs: &container_specs,
                ..Default::default()
            },
        )
        .expect("compile ok");

        // The single-node lease lifecycle's claim transition embeds the
        // ClaimRequest literal (`#{ grant_id: …, request: { … } }`). The merged
        // container rides as a `json_to_rhai_literal` map entry, whose keys are
        // emitted quoted — so the request carries `"container": #{ … }`.
        let logic = logic_source(&air, "t_step_claim");
        assert!(
            logic.contains(r#""container": #{"#),
            "claim request must carry the container key; got:\n{logic}"
        );
        assert!(
            logic.contains("/shared/sif/by-ref/python_3_12_slim.sif"),
            "claim request container must embed the sif path; got:\n{logic}"
        );
    }

    #[test]
    fn empty_container_specs_is_byte_identical_no_container_key() {
        // The hard invariant: an empty container_specs map must NOT introduce a
        // `container` key on either path — AIR stays byte-identical to today.
        let exec_graph = executor_step_graph();
        let crate::compiler::CompileArtifacts { air: exec_air, .. } = compile_to_air_with_options(
            &exec_graph,
            "no-container-submit",
            "test",
            &HashMap::new(),
            CompileOptions::default(),
        )
        .expect("compile ok");
        let exec_logic = logic_source(&exec_air, "step/prepare");
        assert!(
            !exec_logic.contains("d.container"),
            "empty container_specs must not stamp d.container; got:\n{exec_logic}"
        );

        let sched_graph = scheduled_step_graph();
        let known = datacenter_known();
        let known_globals = crate::compiler::named_global::globals_from_resources(&known);
        let crate::compiler::CompileArtifacts { air: sched_air, .. } = compile_to_air_with_options(
            &sched_graph,
            "no-container-lease",
            "test",
            &HashMap::new(),
            CompileOptions {
                known_globals: &known_globals,
                ..Default::default()
            },
        )
        .expect("compile ok");
        let claim_logic = logic_source(&sched_air, "t_step_claim");
        assert!(
            !claim_logic.contains("container"),
            "empty container_specs must not add a container key to the claim request; got:\n{claim_logic}"
        );
    }
}
