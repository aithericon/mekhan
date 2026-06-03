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
                &["datacenter"],
                cx.known_resources,
                cx.container_specs.get(&cx.node.id),
            )?;
            return lower_pooled_body(cx, binding);
        }
    }

    if matches!(
        &cx.node.data,
        WorkflowNodeData::AutomatedStep {
            deployment_model: DeploymentModel::Executor { capacity: Some(_) },
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
        stream_output,
        stream_input,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_automated_step on non-AutomatedStep node")
    };
    let stream_output = *stream_output;
    // A `streamInput` AutomatedStep is a long-lived streaming reducer: it is
    // seeded at net entry, receives the upstream producer's chunks over IPC, and
    // folds them in-process. The executor opts into the inbound chunk feed when
    // `feed_chunks` is set — derived directly from the node's own flag.
    let stream_input = *stream_input;
    let feed_chunks = stream_input;

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
            // Default inline worker-pool body: stamp the constant per-backend
            // namespace so the engine routes to `executor.<wire>` instead of the
            // retired `executor_jobs` fallback. A static literal — no borrow ref.
            // We've necessarily reached this code with an `ExecutorJob` backend
            // (the `EngineEffect` arm above early-returns via `lower_engine_effect`
            // before any job-token build), so the DispatchMode gate is STRUCTURAL.
            format!(
                r#" d.executor_namespace = "{ns}";"#,
                ns = backend_type.executor_namespace()
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

    // Streaming side-channel: when `stream_output` is set, mint a Signal place
    // `p_{id}_stream` (intentionally multi-token — one token per executor
    // Output event, i.e. per `set_output(name, value)` the job produces) at
    // NODE scope and hand it to the lifecycle's `stream_output` bridge below.
    // The lifecycle's `log_output` transition grows a second output arc onto
    // this place, copying each Output event here AND onto `output_log`. A
    // downstream edge from the node's "stream" handle consumes from here
    // (registered in `output_places`) and reads `{ name, value }` off the
    // token's `.detail`. Leftover stream tokens never block `NetCompleted`
    // (Signal is never terminal); the slim control token still governs
    // completion.
    let p_stream: Option<PlaceHandle<DynamicToken>> = if stream_output {
        Some(ctx.signal(format!("p_{id}_stream"), format!("{label} - Stream")))
    } else {
        None
    };
    // Clone for the move into the `scoped_prefix` closure (the original is
    // consumed by `output_places` registration after the closure returns).
    let p_stream_bridge = p_stream.clone();

    // ── streamInput reducer interface (long-lived in-process fold) ──────────
    // When `stream_input` is set this node is a streaming REDUCER: it is seeded
    // at net entry so its executor job starts immediately (the post-mortem
    // "immediate bootstrap" — `p_exec_id` is always populated even for an empty
    // stream), receives the producer's chunks over IPC, and folds them in the
    // Python loop (`aithericon.chunks()`). The chunk feed + EOF arcs are minted
    // after the lifecycle closure (they reference the lifecycle's scoped
    // `p_{id}_submitted`). The control `in` edge is the EOF trigger — it is
    // routed to `p_{id}_control_in` (NOT `p_input`) via `input_handles` below.
    let stream_reducer = if stream_input {
        let p_control_in: PlaceHandle<DynamicToken> =
            ctx.state(format!("p_{id}_control_in"), format!("{label} - Control In"));
        let p_stream_in: PlaceHandle<DynamicToken> =
            ctx.state(format!("p_{id}_stream_in"), format!("{label} - Stream In"));
        let p_dense_seq: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_dense_seq"),
            format!("{label} - Dense Sequence Counter"),
        );
        let p_exec_id: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_exec_id"),
            format!("{label} - Reducer Execution ID"),
        );
        let p_feed_inbox: PlaceHandle<DynamicToken> =
            ctx.state(format!("p_{id}_feed_inbox"), format!("{label} - Feed Inbox"));
        // One-shot gate for `t_capture_exec_id`. `t_capture` read-arcs the
        // lifecycle's `submitted` place (non-consuming, so the lifecycle keeps
        // it) — without a CONSUMING input it would be perpetually enabled and
        // fire forever, flooding `p_exec_id` and starving the rest of the net
        // (the producer never starts). This seeded gate is consumed on the first
        // firing so the capture happens exactly once.
        let p_exec_gate: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_exec_gate"),
            format!("{label} - Exec-ID Capture Gate"),
        );
        ctx.seed_one(&p_exec_gate, DynamicToken::new(json!({})));
        ctx.seed_one(&p_dense_seq, DynamicToken::new(json!({ "n": 0 })));
        // Immediate bootstrap: seed a null input so `prepare` fires on net entry
        // and the reducer job submits. The reducer reads chunks ONLY via IPC, so
        // the seed value is inert (no first-chunk duplication).
        ctx.seed_one(&p_input, DynamicToken::new(json!({})));
        Some((
            p_control_in,
            p_stream_in,
            p_dense_seq,
            p_exec_id,
            p_feed_inbox,
            p_exec_gate,
        ))
    } else {
        None
    };

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
        // PROTOTYPE — when `stream_output` is set, opt the executor into
        // emitting `output` events PER `set_output` CALL (mid-execution) so the
        // node's "stream" port delivers each value while the step still runs,
        // instead of only at job end (which races net completion). The executor
        // gates per-call OutputSet emission on this `output` category being in
        // `stream_events`; non-streaming steps omit it and are unaffected.
        let stream_events_rhai = if stream_output {
            r#"["metric", "progress", "phase", "log", "output"]"#
        } else {
            r#"["metric", "progress", "phase", "log"]"#
        };
        let prepare_logic = format!(
            r#"let d = input; d.job_id = "{id}"; d.run = 0; d.retries = 0; d.max_retries = {max_retries}; let job_inputs = {inputs_rhai}; job_inputs.push(#{{ "name": "input.json", "source": #{{ "type": "inline", "value": input }} }}); /*__BORROWED_INPUTS__*/ d.spec = #{{ "backend": "{backend_type}", "inputs": job_inputs, "outputs": {outputs_rhai}, "config_ref": {config_ref_rhai}, "stream_events": {stream_events_rhai} }}; d.feed_chunks = {feed_chunks};{ns_frag}{job_template_frag}{container_frag} #{{ job: d }}"#
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
                // When set, the lifecycle's `log_output` transition ALSO copies
                // each Output event onto this place (one token per
                // `set_output(name, value)`) so the node's "stream" handle fires
                // the downstream once per output.
                stream_output: p_stream_bridge,
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

    // ── streamInput chunk-feed + EOF arcs (relocated LiveReduce machinery) ──
    // The reducer job is started by the seed above. These arcs feed each
    // upstream chunk to it over IPC and send a clean EOF when the producer
    // completes. The reducer's own terminal output flows through the standard
    // `t_to_output` → `split_outputs` tail below — there is no separate collect
    // (the node IS the reducer, not a container around one).
    if let Some((p_control_in, p_stream_in, p_dense_seq, p_exec_id, p_feed_inbox, p_exec_gate)) =
        &stream_reducer
    {
        // Capture the reducer's execution id from the lifecycle's submitted
        // state (read-arc, non-consuming) so it is always available for the EOF
        // sentinel even on an empty (`stream_count == 0`) stream. The lifecycle
        // runs inside `scoped_prefix(id)`, so its internal `submitted` state is
        // named `<id>/submitted` (scoped slash-form), NOT the `p_<id>_*`
        // interface form — referencing the latter yields the engine's
        // "Unknown place reference" 400 at deploy.
        let p_submitted = PlaceHandle::<DynamicToken>::external(format!("{id}/submitted"));
        ctx.transition(
            format!("t_{id}_capture_exec_id"),
            format!("{label} - Capture Exec ID"),
        )
        .auto_input("gate", p_exec_gate)
        .read_input("submitted", &p_submitted)
        .auto_output("exec_id", p_exec_id)
        .logic_rhai("#{ exec_id: #{ id: submitted.execution_id } }".to_string())
        .done();

        // Feed each chunk over IPC, renumbering 0..N-1 with the node's own dense
        // counter so the executor ReorderBuffer never wedges on the producer's
        // sparse global `sequence`.
        ctx.transition(format!("t_{id}_feed"), format!("{label} - Feed Chunk"))
            .auto_input("chunk", p_stream_in)
            .auto_input("seq", p_dense_seq)
            .read_input("exec", p_exec_id)
            .auto_output("feed", p_feed_inbox)
            .auto_output("seq", p_dense_seq)
            .logic_rhai(
                "#{ feed: #{ execution_id: exec.id, value: chunk.detail.value, sequence: seq.n }, seq: #{ n: seq.n + 1 } }"
                    .to_string(),
            )
            .done();

        ctx.transition(
            format!("t_{id}_feed_effect"),
            format!("{label} - Stream Feed Effect"),
        )
        .auto_input("feed", p_feed_inbox)
        .builtin_effect(&petri_domain::effects::EXECUTOR_STREAM_FEED);

        // EOF: when the producer's control token arrives on `p_control_in` (its
        // `out` → this node's `in`), send the EOF sentinel. The sequence is the
        // dense total (`stream_count`), always one past the last chunk.
        ctx.transition(format!("t_{id}_eof"), format!("{label} - Feed EOF"))
            .auto_input("ctrl", p_control_in)
            .read_input("exec", p_exec_id)
            .auto_output("feed", p_feed_inbox)
            .logic_rhai(
                "let __seq = if \"stream_count\" in ctrl { ctrl.stream_count } else { 0 }; \
                 #{ feed: #{ execution_id: exec.id, sequence: __seq, is_eof: true } }"
                    .to_string(),
            )
            .done();
    }

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
    } else if stream_output {
        // Streaming producer: the slim control token additionally carries
        // `stream_count` (the end-of-stream item count) so a downstream
        // StreamConsumer's `t_close` can size its gather barrier. Plain
        // `split_outputs` strips `detail`, losing it.
        split_outputs_streaming(ctx, id, label, &p_output)
    } else {
        split_outputs(ctx, id, label, &p_output)
    };

    // Slim control success output, plus the named "error" output ONLY when the
    // handle is wired. An edge from the node's error handle (source_handle ==
    // "error") wires to `p_error` via `find_output_place`. When unwired we omit
    // the entry entirely (the failure crashes the net instead), so wire.rs never
    // attaches a consumer to a non-existent port.
    let mut output_places = vec![(None, p_ctrl)];
    if let Some(p_error) = p_error {
        output_places.push((Some("error".to_string()), p_error));
    }
    // PROTOTYPE — register the "stream" handle → `p_{id}_stream` so a normal
    // edge from that handle (sourceHandle == "stream") wires the Signal place to
    // the downstream transition via `wire_edge`/`find_output_place`. No special
    // consuming transition is needed — the standard edge-wiring path applies.
    if let Some(p_stream) = p_stream {
        output_places.push((Some("stream".to_string()), p_stream));
    }
    // streamInput reducer: route the producer's `stream` edge to `p_stream_in`
    // (chunks) and its control `out` → this node's `in` edge to `p_control_in`
    // (the EOF trigger), NOT to the seeded `p_input` (which only ever holds the
    // bootstrap token consumed by `prepare`).
    let mut input_handles = HashMap::new();
    if let Some((p_control_in, p_stream_in, _, _, _, _)) = stream_reducer {
        input_handles.insert("stream".to_string(), p_stream_in);
        input_handles.insert("in".to_string(), p_control_in);
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
/// resolved to a `concurrency_limit` resource.
pub(super) struct PoolBinding {
    /// Deterministic backing net id (`pool-<resource_id>`) the claim/register/
    /// release bridges target.
    pub(super) backing_net_id: String,
    /// `Lease__<kind>` — the AIR definition name for the typed grant/lease.
    pub(super) lease_def_name: String,
    /// The kind's lease JSON Schema, registered into `scenario.definitions`.
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

/// Resolve a pool-resource alias (required) → a [`PoolBinding`], gated to a set
/// of acceptable `expected_kinds`.
///
/// Shared by the two claim/grant/register/release entry points — they differ
/// ONLY in which alias they resolve and which kinds they accept:
/// - `Executor { capacity: { alias } }` → `["concurrency_limit", "runner_group"]`. Both
///   are platform-owned in-net capacity pools with the IDENTICAL cross-net
///   handshake (same `pool-<id>` net id + claim/register/release inboxes +
///   `"grant"` reply), so the downstream body-wrapping is shared; only the
///   `Lease__<kind>` shape and the presence-only `"fail"` path differ, which the
///   returned [`PoolBinding::backend`] discriminates.
/// - `Scheduled { scheduler: alias, .. }` → `["datacenter"]` (R4). Same body
///   wrapping; only the backing net + `Lease__<kind>` differ.
///
/// Errors:
/// - alias not in `known_resources` → `WorkspaceResourceUnknown` (normally
///   caught earlier at publish by `discover_known_resources`).
/// - alias resolves to a kind not in `expected_kinds` → a kind-specific
///   CompileError (`ResourcePoolNotAPool` for the Executor.capacity entry,
///   `SchedulerNotADatacenter` for the Scheduled entry) steering the author to
///   the right deployment model.
/// - `request` fails validation against the kind's `claim_schema` →
///   `ResourcePoolRequestInvalid`.
pub(super) fn resolve_binding(
    node_id: &str,
    alias: &str,
    request: Option<&serde_json::Value>,
    expected_kinds: &[&str],
    known: &crate::compiler::resource_refs::KnownResources,
    container: Option<&crate::compiler::compile::CompilerContainerSpec>,
) -> Result<PoolBinding, CompileError> {
    let resource = known
        .get(alias)
        .ok_or_else(|| CompileError::WorkspaceResourceUnknown {
            node_id: node_id.to_string(),
            alias: alias.to_string(),
        })?;
    let kind = resource.type_name.clone();

    // The `pool_kind` lookup gates "is it a pool kind at all"; the
    // `expected_kinds.contains` gate enforces the Executor/Scheduled split
    // (concurrency_limit + runner_group belong under Executor.capacity, a datacenter under
    // Scheduled). A wrong/non-pool kind yields the entry-point-appropriate error.
    // The error variant is keyed on whether the Scheduled entry called us (its
    // only acceptable kind is "datacenter").
    let scheduled_entry = expected_kinds == ["datacenter"];
    let wrong_kind = || -> CompileError {
        if scheduled_entry {
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
    if !expected_kinds.contains(&kind.as_str()) {
        return Err(wrong_kind());
    }

    // Flavor-gated connection validation for datacenters. Both the per-step
    // `Scheduled.scheduler` lease path and the loop `Loop.lease.scheduler`
    // path funnel through here, so this is the single choke point. We assert
    // the PUBLIC connection fields the flavor needs are present; the required
    // SECRET (slurm `ssh_key`) is structurally guaranteed by the resource-create
    // validator. Hard-fail with no fallback.
    if kind == "datacenter" {
        if let Some(missing) = datacenter_missing_connection_fields(&resource.public_config) {
            return Err(CompileError::DatacenterConnectionIncomplete {
                node_id: node_id.to_string(),
                alias: alias.to_string(),
                flavor: missing.0,
                missing: missing.1,
            });
        }
    }

    // Validate `request` against the kind's claim_schema before we bake it into
    // the ClaimRequest. Same `jsonschema` crate/version the engine
    // `SchemaRegistry` uses, so compile-time and runtime agree.
    let validated_req: Option<serde_json::Value> = match request {
        None => None,
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
            Some(req.clone())
        }
    };

    // Container injection (lease path): a `datacenter` lease whose holder step
    // carries a `CompilerContainerSpec` gets the spec merged into the claim
    // `request` JSON under a `container` key — the claim `request` flows VERBATIM
    // to the engine's `acquire_lease`, which reads `request.get("container")`
    // (slurm_allocator.rs) to wrap the held alloc's drain executor in the `.sif`.
    // The merge happens AFTER schema validation (the container key is not part of
    // the kind's claim_schema). When there is no container OR the kind is not a
    // datacenter, `request_rhai` is byte-identical to the pre-container path
    // (this is the hard concurrency_limit / no-container invariant).
    let request_rhai = match container.filter(|_| kind == "datacenter") {
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
        lease_def_name: format!("Lease__{kind}"),
        // Strip the schemars envelope (`$schema`, `title`) so the registered
        // definition is a bare object schema matching the `Data__`/`Ctrl__`
        // convention — the engine wraps it as `{definitions, $ref}` and a
        // nested draft `$schema` would be redundant noise. The lease schema may
        // carry an inlined `oneOf` (the `datacenter` flavor union) but is
        // SELF-CONTAINED — `pool::schema_value` inlines subschemas, so there is
        // no internal `$ref`/`definitions` to lift.
        lease_schema: sanitize_definition_schema((pool_desc.lease_schema)()),
        request_rhai,
        backend: pool_desc.backend,
    })
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
        },
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_automated_step_pooled only runs for Executor capacity:Some")
    };
    // Resolve `Executor.capacity.alias` (required) against the workspace-resource
    // manifest: a `concurrency_limit` OR `runner_group` resource → `{resource_id, kind}`
    // → the deterministic backing net `pool-<resource_id>`, validated `request`,
    // and a typed, body-visible lease (R2/R3 + Phase 3). Both kinds share the
    // IDENTICAL claim/grant/register/release handshake; the returned binding's
    // `backend` discriminates the presence-only `"fail"` path inside
    // `lower_pooled_body`. A non-pool / datacenter alias is a CompileError.
    let pool_binding = resolve_binding(
        &cx.node.id,
        &binding.alias,
        binding.request.as_ref(),
        &["concurrency_limit", "runner_group"],
        cx.known_resources,
        // concurrency_limit is OUR worker pool, not a cluster — no container injection;
        // `None` keeps concurrency_limit claim AIR byte-identical (hard invariant).
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
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_pooled_body on non-AutomatedStep node")
    };
    let label = label.clone();
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
                // TODO(streaming-output): the `stream_output` "stream" handle is
                // wired only on the plain inline executor path
                // (`lower_automated_step`). Pooled/leased steps do not yet
                // expose the stream side-channel — `None` keeps this path
                // byte-identical. Plumbing `p_{id}_stream` through here would
                // mirror the inline path exactly.
                stream_output: None,
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
    // The routed `grant` token IS the typed lease (validated vs `Lease__<kind>`
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
    // Default-route to the per-backend worker-pool namespace (`executor.<wire>`),
    // then let a grant-supplied namespace override. A concurrency_limit grant carries
    // no `executor_namespace` (only `grant_id`/`unit_id`), so without this
    // default its body would fall back to the RETIRED `executor_jobs` queue —
    // which the worker-pool daemon no longer consumes — and rot silently. With
    // the default, concurrency_limit bodies are drained by the shared worker-pool
    // daemon on `executor.<wire>`. presence grants (`runner.{id}`) and
    // datacenter/lease grants (`lease-<grant>`) DO carry an `executor_namespace`,
    // so the override wins and their exclusive routing is unchanged at runtime.
    let ns_stamp = format!(
        r#" d.executor_namespace = "{ns}"; if grant.executor_namespace != () {{ d.executor_namespace = grant.executor_namespace; }} "#,
        ns = backend_type.executor_namespace()
    );

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
            r#"let input = pending.input; let d = input; d.job_id = "{id_lit}"; d.run = 0; d.retries = 0; d.max_retries = {max_retries}; let job_inputs = {inputs_rhai}; job_inputs.push(#{{ "name": "input.json", "source": #{{ "type": "inline", "value": input }} }}); {lease_stage_push}{ns_stamp}{job_template_frag}/*__BORROWED_INPUTS__*/ d.spec = #{{ "backend": "{backend_wire}", "inputs": job_inputs, "outputs": {outputs_rhai}, "config_ref": {config_ref_rhai}, "stream_events": ["metric", "progress", "phase", "log"] }}; #{{ job: d, reg: {reg_payload}, held: {held_payload} }}"#
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

    // Foundation split + port registration tail — identical to the inline path.
    let (data_place_id, p_ctrl) = split_outputs(ctx, &id, &label, &p_output);
    let mut output_places = vec![(None, p_ctrl)];
    if let Some(p_error) = p_error {
        output_places.push((Some("error".to_string()), p_error));
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
