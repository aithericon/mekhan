//! `WorkflowNodeData::Agent` lowering — `docs/12-agent-node-design.md`.
//!
//! - Degenerate path (`max_turns == 1` + no `stop_when` + no tool children)
//!   synthesizes the equivalent `AutomatedStep(Llm)` and delegates so the
//!   compiled net is byte-identical to a hand-authored single-shot LLM step.
//! - Full agent loop emits the parked-state + LLM call + multi-transition
//!   route topology described in `docs/12` § 3. State migrates through
//!   discrete "phase" places (`p_state` between turns, `p_state_in_flight`
//!   during the LLM call, `p_state_in_tool` during a tool dispatch) so
//!   exactly one transition is enabled at any cycle point — no double-fire
//!   races and no need for the engine to enforce mutual exclusion.

use super::*;

pub(crate) fn lower_agent(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let WorkflowNodeData::Agent {
        max_turns,
        stop_when,
        context_strategy,
        deployment_model,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_agent on non-Agent node")
    };

    // Tool detection: nodes the agent reaches via outgoing edges keyed by
    // `source_handle == "tools"` (orchestrator pre-indexes these into
    // `cx.agent_tools`). The LLM-facing `tool_name` is derived from the
    // target node's own `data.label()` (slugified to Rhai-identifier-safe
    // via `sanitize_slug`) and `tool_description` from `data.description()`
    // — single source of truth, no separate `tool_meta` field. A tools
    // edge to a node whose label slugifies to "node" (the empty/junk
    // fallback in `sanitize_slug`) is a hard compile error so authoring
    // mistakes surface at publish, not silently at the first tool-use
    // turn.
    let mut tool_children: Vec<&WorkflowNode> = Vec::with_capacity(cx.agent_tools.len());
    for &child in cx.agent_tools.iter() {
        let raw_label = child.data.label();
        if raw_label.trim().is_empty() {
            return Err(CompileError::Compilation(format!(
                "agent node '{}': tool edge targets node '{}' which has an \
                 empty label — the LLM addresses tools by name. Set a label \
                 on the target node (it becomes the tool's name after \
                 slugification).",
                cx.node.id, child.id
            )));
        }
        tool_children.push(child);
    }
    let has_tool_children = !tool_children.is_empty();

    // Degenerate fast path: when the agent has zero tool children AND
    // `max_turns == 1` AND `stop_when.is_none()`, lower byte-identically
    // to the equivalent single-shot `AutomatedStep(Llm)`. The
    // `agent_degenerate_lowers_byte_identical_to_llm_automated_step`
    // contract test (service/tests/agent_lowering.rs § 7) pins this.
    if *max_turns == 1 && stop_when.is_none() && !has_tool_children {
        return lower_agent_degenerate(cx);
    }

    // Context strategy gate: only `None` runs end-to-end in v1.
    // `DropOldest` requires a context-budget bookkeeping pass and
    // `SummarizeOldest` requires a summarisation sub-LLM call — both
    // deferred to a follow-up PR (docs/12 § 9). Compile-time reject so
    // mis-authored templates fail at publish, not at the first turn.
    if !matches!(context_strategy, ContextStrategy::None) {
        return Err(CompileError::Compilation(format!(
            "agent node '{}': context_strategy {:?} is not yet implemented \
             (v1 supports ContextStrategy::None only)",
            cx.node.id, context_strategy
        )));
    }

    // Deployment gate: the multi-turn loop path inlines one plain
    // `executor_lifecycle` per turn (`Executor { pool: None }`). Pooled
    // admission (`Executor { pool: Some }`) and external scheduling
    // (`Scheduled { .. }`) need that lease/claim topology interleaved with
    // the turn loop — a follow-up (docs/12). The degenerate single-shot path
    // ALREADY supports all of them (it routes through `lower_automated_step`),
    // so this gate only bites multi-turn / tool-bearing agents. Reject at
    // compile so a mis-authored template fails at publish, not mid-loop —
    // same idiom as the `context_strategy` gate above.
    if !matches!(deployment_model, DeploymentModel::Executor { pool: None }) {
        return Err(CompileError::Compilation(format!(
            "agent node '{}': deployment_model {:?} is not yet supported for \
             multi-turn / tool-bearing agents (v1 runs loop turns on the plain \
             executor pool only). Use a single-shot agent (maxTurns=1, no \
             stopWhen, no tools) for pooled/scheduled inference.",
            cx.node.id, deployment_model
        )));
    }

    lower_agent_loop(cx, &tool_children)
}

/// Byte-identical fall-through for the trivial agent shape. Synthesises the
/// equivalent `AutomatedStep(Llm)` and delegates, reusing the existing
/// lifecycle/retry/foundation plumbing. Keeps the same `id` / `slug` /
/// `parent_id` so every minted place/transition id lines up with what a
/// hand-authored single-shot LLM step would emit.
fn lower_agent_degenerate(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let WorkflowNodeData::Agent {
        label,
        description,
        model,
        system_prompt,
        user_prompt,
        response_format,
        images,
        retry_policy,
        deployment_model,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_agent_degenerate on non-Agent node")
    };

    let llm_config = crate::models::template::agent_to_llm_config(
        model,
        system_prompt.as_deref(),
        user_prompt,
        response_format.as_ref(),
        images,
        &[],
    );

    // Derive the output port from the LLM config (response_format) — NOT the
    // bare `default_output_port`. A `response_format: json_schema` agent must
    // unpack the schema's fields (e.g. `document_class`), exactly as a
    // hand-authored LLM step did (its server-derived `output` was cached in
    // the graph). Mirrors `nodes::agent::output_ports`; without it the
    // degenerate path emits the default `response/usage/...` envelope and
    // downstream `<agent>.<schema_field>` borrows dangle. Falls back to the
    // default envelope when no response_format is set.
    //
    // Resolve `{"$ref": "#/definitions/…"}` against the workflow `definitions`
    // on a COPY before deriving — `derive_output_port` can't see the ref
    // target otherwise and would silently fall back to the default envelope
    // (this is exactly what bit `classify-and-group-v1`, whose schema is a
    // bare `$ref`). The virtual node below keeps the UNRESOLVED config so the
    // delegated `lower_automated_step` inlines refs the same way a
    // hand-authored LLM step's config is inlined — preserving the
    // byte-identical contract. Ref-resolution failures here are non-fatal: the
    // derive falls back to the default envelope and `lower_automated_step`'s
    // own `inline_refs` surfaces the real error with a precise JSON path.
    let derived_output = {
        let mut resolved = llm_config.clone();
        let _ = crate::compiler::schema_refs::inline_refs(&mut resolved, cx.definitions);
        crate::backends::lookup(ExecutionBackendType::Llm)
            .and_then(|d| d.derive_output_port)
            .map(|f| f(&resolved))
            .unwrap_or_else(|| {
                crate::models::template::default_output_port(ExecutionBackendType::Llm)
            })
    };

    let virtual_node = WorkflowNode {
        id: cx.node.id.clone(),
        node_type: "automated_step".to_string(),
        slug: cx.node.slug.clone(),
        position: cx.node.position.clone(),
        data: WorkflowNodeData::AutomatedStep {
            label: label.clone(),
            description: description.clone(),
            execution_spec: crate::models::template::ExecutionSpecConfig {
                backend_type: ExecutionBackendType::Llm,
                entrypoint: None,
                config: llm_config,
            },
            input: crate::models::template::Port::empty_input(),
            output: derived_output,
            // Single-shot agents inherit the full AutomatedStep dispatch — the
            // author's `deployment_model` (Executor{pool} / Scheduled{lease})
            // and `retry_policy` flow straight through `lower_automated_step`.
            // This is what makes the degenerate Agent a complete replacement
            // for the retired hand-authored LLM step.
            retry_policy: *retry_policy,
            deployment_model: deployment_model.clone(),
        },
        parent_id: cx.node.parent_id.clone(),
        width: cx.node.width,
        height: cx.node.height,
    };

    let mut virtual_cx = LoweringCtx {
        node: &virtual_node,
        graph: cx.graph,
        outgoing_edges: cx.outgoing_edges,
        incoming_edges: cx.incoming_edges,
        children: cx.children,
        agent_tools: cx.agent_tools,
        ctx: &mut *cx.ctx,
        ports: &mut *cx.ports,
        fixups: &mut *cx.fixups,
        node_files: cx.node_files,
        sub_air: cx.sub_air,
        interfaces: &mut *cx.interfaces,
        definitions: cx.definitions,
        node_configs: &mut *cx.node_configs,
        config_storage: cx.config_storage,
        known_resources: cx.known_resources,
    };

    super::automated_step::lower_automated_step(&mut virtual_cx)
}

// LLM config projection lives in `models::template::agent_to_llm_config`
// — single source of truth shared with the resource borrow planner, the
// publish-time resource scan, and the `output_ports` deriver.

/// Full agent-loop lowering (docs/12 § 3). State is the single token
/// migrating through phase-named places — exactly one of {p_state,
/// p_state_in_flight, p_state_in_tool} holds it at any cycle point:
///
/// ```text
///   p_input ─► t_enter ─► p_state
///                            │
///                t_prepare_call (consume p_state)
///                            ├─► exec_inbox
///                            └─► p_state_in_flight
///                                  │
///                       (LLM call via executor lifecycle)
///                                  ▼
///                          t_to_response (lc.completed + p_state_in_flight)
///                                  │
///                                  ▼
///                              p_response
///                                  │
///                                  ├── t_route_final ──► p_final ─► t_exit ─► p_output
///                                  │     (tool_calls empty OR turn+1>=max OR stop_when)
///                                  │
///                                  ├── t_route_dispatch_<tn>  (per tool)
///                                  │     ├─► p_dispatch_<tn>  ─► t_invoke_<tn> ─► child input
///                                  │     │                                          (child runs)
///                                  │     │                                          child output
///                                  │     │                                              ▼
///                                  │     │                                       t_collect_<tn>
///                                  │     └─► p_state_in_tool ──────────────────────────┘
///                                  │                                                    │
///                                  │                                                    ▼
///                                  │                                                p_state (loop)
///                                  │
///                                  └── t_route_unknown ─► p_state (Feedback only;
///                                                        Bubble pre-rejected at
///                                                        compile via tool-name set)
/// ```
///
/// v1 scope: serial tool calls only (`tool_calls[0]`); both ToolErrorPolicy
/// variants; `ContextStrategy::None` only; history kept in-token (Vec<Map>),
/// S3-backed history deferred.
fn lower_agent_loop(
    cx: &mut LoweringCtx,
    tool_children: &[&WorkflowNode],
) -> Result<(), CompileError> {
    let WorkflowNodeData::Agent {
        label,
        model,
        system_prompt,
        user_prompt,
        response_format,
        images,
        max_turns,
        stop_when,
        on_tool_error,
        retry_policy,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_agent_loop on non-Agent node")
    };
    let id = cx.node.id.clone();
    let max_turns = *max_turns;
    let on_tool_error = *on_tool_error;
    // Per-turn executor retry budget. The loop's own failure path
    // (`t_call_failed`/`_timed_out`/`_dead` → p_error) still catches a turn
    // that exhausts its retries; this just lets a flaky single turn retry
    // before the whole agent bubbles an error.
    let per_turn_max_retries = retry_policy.max_retries;

    // Map-body-terminal gate: when this agent is the terminal child of a Map
    // body it must fork its FULL envelope (park data AND forward the whole
    // token) and preserve the __map_idx/__map_id correlation leaves so the
    // Map's t_collect can read body.detail.outputs.<resultVar> + correlate.
    // Computed here, before `cx.ctx` is reborrowed mutably below, and used
    // to GATE the t_enter/t_route_final correlation edits + park-vs-split so
    // the non-map agent AIR is byte-identical. Shared gate — see
    // `super::is_map_body_terminal`.
    let is_map_body_terminal =
        super::is_map_body_terminal(cx.graph, cx.node.parent_id.as_deref(), cx.outgoing_edges);

    // Per-tool derived metadata: tool_name = slugified node label,
    // tool_description = node description (verbatim). Single source of
    // truth — the canvas label IS what the LLM sees (after sanitisation).
    // Pre-computed so the uniqueness check, schema build, and per-tool
    // route emission all share the same name/description strings.
    let tool_meta: Vec<(String, String)> = tool_children
        .iter()
        .map(|c| {
            (
                crate::models::template::sanitize_slug(c.data.label()),
                c.data.description().unwrap_or("").to_string(),
            )
        })
        .collect();

    // Tool-name uniqueness — analog of `SlugConflict`. A duplicate would
    // make the per-tool dispatch route guards ambiguous; reject at
    // compile so the editor can ring the offending children. Names
    // collide post-slugification (e.g. "Order Lookup" and "order_lookup"
    // both slugify to the same identifier), so check on the derived form.
    let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (name, _) in &tool_meta {
        if !seen_names.insert(name.as_str()) {
            return Err(CompileError::Compilation(format!(
                "agent node '{id}': duplicate tool name '{name}' — two tool \
                 children have labels that slugify to the same identifier. \
                 Rename one of the tool nodes."
            )));
        }
    }

    // Build the tool schemas. Each tool node's declared input port becomes
    // the LLM-facing `input_schema` — the LLM addresses tool args by the
    // exact field names the node declares (e.g. `order_id`), and the
    // runner promotes those args to Python globals via `_AccessibleDict`,
    // so a mismatch surfaces as `AttributeError: '_AccessibleDict' object
    // has no attribute 'X'` at the first tool-call turn. With no declared
    // fields, the LLM gets a permissive `{type: object}` so it can call
    // but the platform doesn't pretend to validate.
    let sub_air = cx.sub_air;
    let tool_schemas: Vec<serde_json::Value> = tool_meta
        .iter()
        .zip(tool_children.iter())
        .map(|((name, description), child)| {
            // The tool contract comes from the callee's input boundary, not
            // its node kind. A leaf AutomatedStep declares it on its own
            // input port; a SubWorkflow declares it on its child's Start node
            // — carried on the resolved child as `input_contract` (extracted
            // in `resolve_subworkflow_air`). An unresolved SubWorkflow (no
            // `sub_air` entry, e.g. the back-compat `compile_to_air` path)
            // degrades to the permissive fallback.
            let contract_port = match &child.data {
                WorkflowNodeData::SubWorkflow { .. } => {
                    sub_air.get(&child.id).map(|rc| rc.input_contract.clone())
                }
                other => other.input_ports().into_iter().next(),
            };
            let input_schema = match contract_port {
                Some(port) if !port.fields.is_empty() => port_to_input_schema(&port),
                _ => serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": true,
                }),
            };
            serde_json::json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            })
        })
        .collect();

    let llm_config = crate::models::template::agent_to_llm_config(
        model,
        system_prompt.as_deref(),
        user_prompt,
        response_format.as_ref(),
        images,
        &tool_schemas,
    );

    // Side-channel the static config via `config_ref`. The Petri token
    // stays slim; the executor's `FetchConfigHook` materialises it.
    let storage_key = cx.config_storage.key(&id);
    cx.node_configs.insert(id.clone(), llm_config);
    // The conversation transcript lives OFF the token in per-turn S3 blobs
    // (`instances/{instance_id}/{node}/turn-{N}.json`, cumulative through
    // turn N's assistant message). The overlay carries only slim plumbing on
    // EXISTING wire fields (so the engine needs no new typed field — no
    // stale-engine field-drop): `history`/`pending` are `{{input:...}}`
    // placeholders the backend's `resolve_inputs` fills from staged inputs
    // (the base blob + this turn's delta), and `_history_write_key` is the
    // per-turn key the worker writes the new cumulative transcript to after
    // the model responds. `s` is in scope here — the prepare_call logic binds
    // `let s = state;` first.
    //
    // The instance id comes from the `__INSTANCE_ID__` AIR sentinel
    // (`parameterize_air` substitutes it at instance creation), NOT from the
    // control token — so the key is correct regardless of the agent's
    // position in the graph. Reading `input._instance_id` would be empty when
    // the agent sits downstream of an AutomatedStep/SubWorkflow (their output
    // is the executor envelope, not the control token), collapsing every such
    // instance onto a shared instance-less key.
    let storage_key_esc = rhai_str_escape(&storage_key);
    let write_key_expr =
        format!(r#""instances/__INSTANCE_ID__/{id}/turn-" + s.turn + ".json""#);
    // Build the overlay as a Rhai statement (`ov`) so we can conditionally
    // null `system_prompt` + `prompt` on turns > 0: by then those two opening
    // messages already sit at the head of `history` (read back from the prior
    // turn's cumulative blob), so re-sending them via the static config would
    // duplicate them. On turn 0 they stand — the conversation's system + user
    // turns. The worker persists the FULL transcript (system + user + history
    // + pending + assistant) by reading the resolved config from
    // `backend_state`, so the blob is the complete conversation, not just the
    // loop's tool turns.
    let overlay_build = format!(
        r#"let ov = #{{ "history": "{{{{input:history}}}}", "pending": "{{{{input:pending}}}}", "_history_write_key": {write_key_expr} }}; if s.turn > 0 {{ ov.system_prompt = (); ov.prompt = ""; }}"#
    );
    let config_ref_rhai = r#"#{ "storage_path": "STORAGE_KEY", "overlay": ov }"#
        .replace("STORAGE_KEY", &storage_key_esc);

    // Quote the optional stop_when as a Rhai sub-expression. Empty/None
    // canonicalises to `false` so the route guards can blindly OR it in
    // without a branch in the codegen.
    let stop_when_expr: String = stop_when
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| format!("({s})"))
        .unwrap_or_else(|| "false".to_string());

    // Known tool names — baked into the route_unknown guard so the
    // compiler decides at publish what counts as "known" rather than
    // shipping the set to runtime. List literal in Rhai map-key form.
    let known_names_rhai: String = if tool_meta.is_empty() {
        "[]".to_string()
    } else {
        let inner: Vec<String> = tool_meta
            .iter()
            .map(|(name, _)| format!("\"{}\"", rhai_str_escape(name)))
            .collect();
        format!("[{}]", inner.join(", "))
    };

    let ctx = &mut *cx.ctx;

    // ----- Places -----
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_state: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_state"),
        format!("{label} - Agent State (between turns)"),
    );
    let p_state_in_flight: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_state_in_flight"),
        format!("{label} - State (parked during LLM call)"),
    );
    let p_state_in_tool: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_state_in_tool"),
        format!("{label} - State (parked during tool call)"),
    );
    let p_response: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_response"),
        format!("{label} - LLM Response (state + turn_result merged)"),
    );
    let p_final: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_final"), format!("{label} - Final"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_error: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_error"), format!("{label} - Error"));

    // One dispatch place per declared tool. Stage the tool wiring data
    // here; the post-traversal `apply_agent_tool_wirings` fixup mints
    // the invoke + collect transitions once each child's NodePorts is in
    // node_ports.
    let mut dispatch_places: Vec<(String, PlaceHandle<DynamicToken>)> = Vec::new();
    let mut tool_entries: Vec<AgentToolEntry> = Vec::new();
    for (child, (tn, _desc)) in tool_children.iter().zip(tool_meta.iter()) {
        let tn = tn.clone();
        let pd: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_dispatch_{tn}"),
            format!("{label} - Dispatch {tn}"),
        );
        dispatch_places.push((tn.clone(), pd.clone()));
        tool_entries.push(AgentToolEntry {
            tool_name: tn,
            child_id: child.id.clone(),
            dispatch_place: pd,
            on_tool_error,
        });
    }

    // ----- t_enter: initialise state, hand to p_state -----
    // The parked envelope keeps the slim agent state: turn counter,
    // accumulating token totals, the `pending` delta (non-assistant turns
    // produced since the last LLM call — a tool result or feedback message,
    // bounded), and `final_response` (set on the terminal turn). The full
    // conversation transcript lives OFF the token in per-turn S3 blobs keyed
    // by the `__INSTANCE_ID__` AIR sentinel (substituted at instance creation,
    // so topology-independent) + turn. The user's inbound token rides through
    // as `state.input`.
    ctx.transition(format!("t_{id}_enter"), format!("{label} - Enter Agent"))
        .auto_input("input", &p_input)
        .auto_output("state", &p_state)
        // When a Map body terminal: capture the Map correlation leaves off the
        // inbound token into state so t_route_final can re-attach them. The
        // non-map branch keeps the byte-identical original state literal.
        .logic_rhai(if is_map_body_terminal {
            r#"let __mi = if type_of(input) == "map" && "__map_idx" in input { input.__map_idx } else { () }; let __mid = if type_of(input) == "map" && "__map_id" in input { input.__map_id } else { () }; #{ state: #{ turn: 0, message_count: 0, total_tokens_in: 0, total_tokens_out: 0, pending: [], pending_tool_call_id: (), input: input, final_response: (), __map_idx: __mi, __map_id: __mid } }"#
                .to_string()
        } else {
            r#"#{ state: #{ turn: 0, message_count: 0, total_tokens_in: 0, total_tokens_out: 0, pending: [], pending_tool_call_id: (), input: input, final_response: () } }"#
                .to_string()
        })
        .done();

    // ----- t_prepare_call: state → exec_inbox + p_state_in_flight -----
    // Consumes `p_state`, produces the executor job AND parks state on
    // `p_state_in_flight` for the LLM call's duration. Consumption gates
    // re-entry: t_prepare_call cannot fire again until t_collect_<tn>
    // (or t_route_unknown) re-deposits on `p_state`.
    let exec_inbox = ctx.state::<ExecutorSubmitInput>(
        format!("p_{id}_call_inbox"),
        format!("{label} - Call Inbox"),
    );
    let exec_inbox_for_lc = exec_inbox.clone();
    ctx.transition(
        format!("t_{id}_prepare_call"),
        format!("{label} - Prepare LLM Call"),
    )
    .auto_input("state", &p_state)
    .auto_output("job", &exec_inbox)
    .auto_output("state_in_flight", &p_state_in_flight)
    // Rhai variables are mutable by default — no `mut` keyword exists,
    // so `let mut d = ...` would parse `mut` as a fresh variable name
    // and then fail at the next token. Plain `let d` is mutable.
    //
    // The `/*__BORROWED_INPUTS__*/` marker is a Rhai block comment the
    // borrow apply phase splices ResourceEnvelope staging snippets into
    // (e.g. `job_inputs.push(#{ "name": "openai.json", ... })`). Without
    // it, an agent with `model.resource_alias` set deploys to staging,
    // which then fails with "resource '<alias>' not staged as
    // <alias>.json — compiler must emit a ResourceEnvelope borrow for
    // this step". Mirrors the marker site in
    // `lower_automated_step::t_<id>_prepare`.
    //
    // Transcript I/O rides as declared job inputs (reusing `StageInputsHook`
    // + `resolve_inputs`): `history` is the prior cumulative blob from the
    // global store (turn N-1) — or an inline `[]` on turn 0 so
    // `{{input:history}}` always resolves — and `pending` is this turn's
    // not-yet-persisted delta. After copying `s.pending` into the job, we
    // clear it on the parked state: the worker folds it into the turn-N blob
    // (which next turn reads as its base), so it must not be re-sent.
    .logic_rhai(format!(
        r#"let s = state; let d = #{{ }}; d.job_id = "{id}"; d.run = s.turn; d.retries = 0; d.max_retries = {per_turn_max_retries}; let job_inputs = []; if s.turn > 0 {{ job_inputs.push(#{{ "name": "history", "source": #{{ "type": "storage_path", "path": "instances/__INSTANCE_ID__/{id}/turn-" + (s.turn - 1) + ".json" }} }}); }} else {{ job_inputs.push(#{{ "name": "history", "source": #{{ "type": "inline", "value": [] }} }}); }} job_inputs.push(#{{ "name": "pending", "source": #{{ "type": "inline", "value": s.pending }} }}); /*__BORROWED_INPUTS__*/ {overlay_build} d.spec = #{{ "backend": "llm", "inputs": job_inputs, "outputs": [], "config_ref": {config_ref_rhai}, "stream_events": ["agent_turn", "metric", "log"] }}; d.metadata = #{{ "agent_node_id": "{id}" }}; s.pending = []; #{{ job: d, state_in_flight: s }}"#
    ))
    .done();

    // Scoped-prefix wrap mirrors `lower_automated_step`. Without it the
    // lifecycle's terminal places (`completed`, `dead_letter`,
    // `cancelled`) leak into the top-level namespace and (1) collide if
    // any other node calls `executor_lifecycle`, (2) clutter the
    // petri-net visualisation with free-floating terminals that look
    // like workflow exits. Inside the prefix they become
    // `{id}/completed`, `{id}/dead_letter`, etc. — same shape an LLM
    // AutomatedStep produces.
    let lc = ctx.scoped_prefix(id.as_str(), label.as_str(), |ctx| {
        executor_lifecycle(
            ctx,
            ExecutorBridges {
                inbox: exec_inbox_for_lc,
                result_out: None,
                failure_out: None,
                process_id: None,
                process_step: None,
                catalogue: false,
                process: false,
            },
        )
    });

    // ----- t_to_response: lc.completed + p_state_in_flight → p_response -----
    // Re-unites state with the LLM response and accumulates this turn's
    // token usage into the parked state envelope. Output token carries
    // both: `{state: ..., response: done}` so the route transitions can
    // consume a single token + see everything they need. The defensive
    // `type_of(...)` chain tolerates an absent / non-map `usage` field
    // (some adapters emit `null` until tokens land — never trust the
    // wire shape unconditionally).
    ctx.transition(
        format!("t_{id}_to_response"),
        format!("{label} - To Response"),
    )
    .auto_input("done", &lc.completed)
    .auto_input("state", &p_state_in_flight)
    .auto_output("response", &p_response)
    .logic_rhai(
        r#"let outs = if type_of(done.detail) == "map" && type_of(done.detail.outputs) == "map" { done.detail.outputs } else { #{} }; let usage = if type_of(outs.usage) == "map" { outs.usage } else { #{} }; let in_tok = if type_of(usage.input_tokens) == "i64" { usage.input_tokens } else { 0 }; let out_tok = if type_of(usage.output_tokens) == "i64" { usage.output_tokens } else { 0 }; state.total_tokens_in = state.total_tokens_in + in_tok; state.total_tokens_out = state.total_tokens_out + out_tok; state.message_count = state.message_count + 1; #{ response: #{ state: state, response: done } }"#
            .to_string(),
    )
    .done();

    // Executor-side failure paths drain state out of `p_state_in_flight`
    // too — otherwise it'd stay parked forever and block any retry path
    // a wrapping workflow might author. State is discarded on hard
    // executor failure (no good way to surface partial state).
    ctx.transition(format!("t_{id}_call_failed"), format!("{label} - LLM Call Failed"))
        .auto_input("dead", &lc.failed)
        .auto_input("state", &p_state_in_flight)
        .auto_output("error", &p_error)
        .logic_rhai("#{ error: dead }".to_string())
        .done();
    ctx.transition(format!("t_{id}_call_timed_out"), format!("{label} - LLM Call Timed Out"))
        .auto_input("dead", &lc.timed_out)
        .auto_input("state", &p_state_in_flight)
        .auto_output("error", &p_error)
        .logic_rhai("#{ error: dead }".to_string())
        .done();
    ctx.transition(format!("t_{id}_call_dead"), format!("{label} - LLM Call Dead Letter"))
        .auto_input("dead", &lc.dead_letter)
        .auto_input("state", &p_state_in_flight)
        .auto_output("error", &p_error)
        .logic_rhai("#{ error: dead }".to_string())
        .done();

    // ----- Route transitions: one per branch, each guarded -----
    //
    // Mirror lower_loop's t_continue / t_exit pattern: multiple
    // transitions on the same input place with complementary guards
    // (engine picks whichever is enabled). No single multi-output Rhai
    // script — each route's effect is one path.
    //
    // The Rhai `turn_result` extractor below tolerates either shape the
    // LLM backend may emit: `outputs.turn_result` (multi-turn / tool-call
    // path) or the canonical `{response, usage, finish_reason, model}`
    // single-shot fields. Whichever the backend gave us, `tr` ends up as
    // a map with `{content, tool_calls, stop_reason}` so the route guards
    // below can blindly index `tr.tool_calls`, `tr.content`, etc. Without
    // this normalisation, `tr.tool_calls` on a bare string would silently
    // fail every guard and stall the net.
    let extract_tr: &str = r#"let r = response.response; let outs = r.detail.outputs; let tr_raw = outs.turn_result; let tr = if type_of(tr_raw) == "map" { tr_raw } else { #{ content: if type_of(outs.response) == "string" { outs.response } else { () }, tool_calls: [], stop_reason: if type_of(outs.finish_reason) == "string" { outs.finish_reason } else { "end_turn" } } };"#;

    // t_route_final: terminate — tool_calls empty OR max_turns reached
    // OR stop_when satisfied. Produces the final envelope on p_final.
    //
    // The deposited token mirrors the executor envelope an
    // `AutomatedStep(Llm)` would produce — `{execution_id, job_id, run,
    // status, source, detail: {outputs, exit_code}}` — so the
    // `NodeKind::AutomatedStep` hoist path (`detail.outputs`) the borrow
    // planner uses resolves `<agent>.response`, `<agent>.usage`, etc.
    // exactly the way a plain LLM step's borrows resolve.
    //
    // We START from `outs` (the LLM backend's actual emit) so structured
    // `response_format: json_schema` outputs (each schema property
    // unpacked by `unpack_by_name`) flow through unchanged — matches what
    // `output_ports()`'s call to `LLM_DECL.derive_output_port` declares.
    // Then we OVERRIDE `usage` with the per-turn-accumulated totals from
    // state (the executor's `usage` is just the last turn's count, the
    // agent's is the conversation total) and add the agent-specific extras
    // (turn, history_ref, final_response, input). `history_ref` points at the
    // final cumulative transcript blob (`…/turn-{N}.json`) rather than
    // carrying the full conversation inline.
    ctx.transition(
        format!("t_{id}_route_final"),
        format!("{label} - Route: Final"),
    )
    .auto_input("response", &p_response)
    .auto_output("final", &p_final)
    .guard_rhai(format!(
        r#"let s = response.state; {extract_tr} let tc = if type_of(tr.tool_calls) == "array" {{ tr.tool_calls }} else {{ [] }}; tc.len() == 0 || s.turn + 1 >= {max_turns} || {stop_when_expr}"#
    ))
    // When a Map body terminal: copy the captured correlation leaves onto the
    // emitted envelope so the Map's gather can count/order this element. The
    // non-map branch keeps the byte-identical original envelope.
    .logic_rhai(if is_map_body_terminal {
        format!(
            r#"let s = response.state; {extract_tr} let outputs = outs; outputs.usage = #{{ input_tokens: s.total_tokens_in, output_tokens: s.total_tokens_out }}; outputs.turn = s.turn; outputs.history_ref = "instances/__INSTANCE_ID__/{id}/turn-" + s.turn + ".json"; outputs.final_response = tr; outputs.input = s.input; let env = #{{ execution_id: "agent-{id}", job_id: "{id}", run: s.turn, status: "succeeded", source: "agent_loop", detail: #{{ outputs: outputs, exit_code: 0 }} }}; if s.__map_idx != () {{ env.__map_idx = s.__map_idx; }} if s.__map_id != () {{ env.__map_id = s.__map_id; }} #{{ final: env }}"#
        )
    } else {
        format!(
            r#"let s = response.state; {extract_tr} let outputs = outs; outputs.usage = #{{ input_tokens: s.total_tokens_in, output_tokens: s.total_tokens_out }}; outputs.turn = s.turn; outputs.history_ref = "instances/__INSTANCE_ID__/{id}/turn-" + s.turn + ".json"; outputs.final_response = tr; outputs.input = s.input; let env = #{{ execution_id: "agent-{id}", job_id: "{id}", run: s.turn, status: "succeeded", source: "agent_loop", detail: #{{ outputs: outputs, exit_code: 0 }} }}; #{{ final: env }}"#
        )
    })
    .done();

    // t_route_dispatch_<tn>: one per declared tool. Guard fires only
    // when model picked this specific tool, max_turns isn't exhausted,
    // and stop_when is false. State migrates to p_state_in_tool (with
    // assistant turn appended to history, turn += 1); call args go to
    // p_dispatch_<tn>.
    for (tn, pd) in &dispatch_places {
        ctx.transition(
            format!("t_{id}_route_dispatch_{tn}"),
            format!("{label} - Route: Dispatch {tn}"),
        )
        .auto_input("response", &p_response)
        .auto_output("dispatch", pd)
        .auto_output("state_in_tool", &p_state_in_tool)
        .guard_rhai(format!(
            r#"let s = response.state; {extract_tr} let tc = if type_of(tr.tool_calls) == "array" {{ tr.tool_calls }} else {{ [] }}; tc.len() > 0 && tc[0].name == "{tn}" && s.turn + 1 < {max_turns} && !({stop_when_expr})"#
        ))
        // `call` is a Rhai reserved keyword (it's the indirect-call syntax
        // marker). Use `tcall` for the tool_calls[0] binding.
        //
        // The assistant turn (content + this tool_call) is NOT pushed here —
        // the executor worker writes it to this turn's transcript blob from
        // the model's `turn_result`. We only thread `pending_tool_call_id`
        // (so the collect transition can tag the tool result) and bump the
        // turn counter.
        .logic_rhai(format!(
            r#"let s = response.state; {extract_tr} let tcall = tr.tool_calls[0]; s.pending_tool_call_id = tcall.id; s.turn = s.turn + 1; s.message_count = s.message_count + 1; #{{ dispatch: #{{ call_id: tcall.id, tool_name: "{tn}", args: tcall.arguments }}, state_in_tool: s }}"#
        ))
        .done();
    }

    // t_route_unknown: both policies emit a transition for the
    // unknown-tool case, but the destination differs.
    //
    // - Feedback (default): append a synthetic `role: tool` failure
    //   message to history, re-deposit state on p_state, let the model
    //   retry on the next turn with the corrected tool list in context.
    // - Bubble: deposit a status-failed envelope on p_error so the agent
    //   exits via its error handle. Without this transition the token
    //   sat on p_response forever — a silent stall that looked exactly
    //   like a hung LLM call to anyone debugging.
    if !tool_children.is_empty() {
        match on_tool_error {
            ToolErrorPolicy::Feedback => {
                ctx.transition(
                    format!("t_{id}_route_unknown"),
                    format!("{label} - Route: Unknown Tool (feedback)"),
                )
                .auto_input("response", &p_response)
                .auto_output("state", &p_state)
                .guard_rhai(format!(
                    r#"let s = response.state; {extract_tr} let tc = if type_of(tr.tool_calls) == "array" {{ tr.tool_calls }} else {{ [] }}; let known = {known_names_rhai}; tc.len() > 0 && !(known.contains(tc[0].name)) && s.turn + 1 < {max_turns} && !({stop_when_expr})"#
                ))
                // The assistant turn (with the bad tool_call) is already
                // written to this turn's transcript blob by the worker; we
                // only stage the synthetic `role: tool` not-found message as
                // the `pending` delta so the next turn's request (and blob)
                // include it after that assistant turn.
                .logic_rhai(format!(
                    r#"let s = response.state; {extract_tr} let tcall = tr.tool_calls[0]; s.pending = [#{{ role: "tool", tool_call_id: tcall.id, content: "tool '" + tcall.name + "' not found — pick one of: " + {known_names_rhai} }}]; s.turn = s.turn + 1; s.message_count = s.message_count + 1; #{{ state: s }}"#
                ))
                .done();
            }
            ToolErrorPolicy::Bubble => {
                ctx.transition(
                    format!("t_{id}_route_unknown"),
                    format!("{label} - Route: Unknown Tool (bubble to error)"),
                )
                .auto_input("response", &p_response)
                .auto_output("error", &p_error)
                .guard_rhai(format!(
                    r#"let s = response.state; {extract_tr} let tc = if type_of(tr.tool_calls) == "array" {{ tr.tool_calls }} else {{ [] }}; let known = {known_names_rhai}; tc.len() > 0 && !(known.contains(tc[0].name)) && s.turn + 1 < {max_turns} && !({stop_when_expr})"#
                ))
                .logic_rhai(format!(
                    r#"let s = response.state; {extract_tr} let tcall = tr.tool_calls[0]; let msg = "agent picked unknown tool '" + tcall.name + "' (known: " + {known_names_rhai} + ")"; #{{ error: #{{ execution_id: "agent-{id}", job_id: "{id}", run: s.turn, status: "failed", source: "agent_loop", detail: #{{ outputs: #{{}}, exit_code: 1, error: #{{ kind: "unknown_tool", message: msg, tool_name: tcall.name }} }} }} }}"#
                ))
                .done();
            }
        }
    }

    // ----- t_exit -----
    ctx.transition(format!("t_{id}_exit"), format!("{label} - Exit"))
        .auto_input("final", &p_final)
        .auto_output("output", &p_output)
        .logic_rhai("#{ output: final }".to_string())
        .done();

    // Foundation tail. A Map body terminal forks the FULL envelope (park data
    // AND forward the whole token incl. detail.outputs + __map_* leaves) via
    // park_outputs; otherwise the slim split_outputs control token. Either way
    // `<agent_slug>.<field>` borrows resolve through the parked data place.
    let (data_place_id, p_ctrl) = if is_map_body_terminal {
        park_outputs(ctx, &id, label, &p_output)
    } else {
        split_outputs(ctx, &id, label, &p_output)
    };

    // Queue the agent → tool-child wiring fixup. Tool children's
    // NodePorts aren't populated yet — they'll be lowered later in the
    // topological pass. `apply_agent_tool_wirings` drains this after
    // the loop so every child's input/output places are addressable.
    if !tool_entries.is_empty() {
        cx.fixups.agent_tool_wirings.push(AgentToolWiring {
            agent_id: id.clone(),
            agent_label: label.clone(),
            p_state: p_state.clone(),
            p_state_in_tool: p_state_in_tool.clone(),
            p_error: p_error.clone(),
            tools: tool_entries,
        });
    }

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

/// Translate a tool node's declared input port into a JSON Schema fragment
/// for the LLM's tool-use call. The model addresses tool args by the exact
/// field names declared here (e.g. `order_id`); the runner then promotes
/// those args to Python globals via `_AccessibleDict`, so a name mismatch
/// surfaces as `AttributeError: '_AccessibleDict' object has no attribute
/// 'X'` at runtime. Keep the property list tight + `additionalProperties:
/// false` when fields are declared so the LLM can't invent unknown args.
fn port_to_input_schema(port: &crate::models::template::Port) -> serde_json::Value {
    use crate::models::template::FieldKind;
    use serde_json::json;
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();
    for f in &port.fields {
        let description = f
            .description
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| f.label.clone());
        // A rich schema override IS the property schema — the model is told the
        // exact (possibly deeply nested) shape to produce, not a flat scalar.
        let prop_value = if let Some(rich) = &f.schema {
            let mut rich = rich.clone();
            // Attach the field description if the rich schema is an object that
            // doesn't already carry one; leave an existing description intact.
            if !description.is_empty() {
                if let serde_json::Value::Object(map) = &mut rich {
                    if !map.contains_key("description") {
                        map.insert("description".to_string(), json!(description));
                    }
                }
            }
            rich
        } else {
            let type_str: &str = match f.kind {
                FieldKind::Number => "number",
                FieldKind::Bool => "boolean",
                FieldKind::Json => "object",
                _ => "string",
            };
            let mut prop = serde_json::Map::new();
            prop.insert("type".to_string(), json!(type_str));
            if !description.is_empty() {
                prop.insert("description".to_string(), json!(description));
            }
            serde_json::Value::Object(prop)
        };
        properties.insert(f.name.clone(), prop_value);
        if f.required {
            required.push(f.name.clone());
        }
    }
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_string(), json!("object"));
    schema.insert("properties".to_string(), serde_json::Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".to_string(), json!(required));
    }
    schema.insert("additionalProperties".to_string(), json!(false));
    serde_json::Value::Object(schema)
}
