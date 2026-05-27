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

pub(super) fn lower_agent(cx: &mut LoweringCtx) -> Result<(), CompileError> {
    let WorkflowNodeData::Agent {
        max_turns,
        stop_when,
        context_strategy,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_agent on non-Agent node")
    };

    // Tool detection is structural — any child whose `tool_meta` is set
    // (docs/12 § 2.2). The picker / wire layer treats them like any other
    // child for layout; only the agent compiler reads `tool_meta`.
    let tool_children: Vec<&WorkflowNode> = cx
        .children
        .iter()
        .filter(|c| c.tool_meta.is_some())
        .copied()
        .collect();
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
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_agent_degenerate on non-Agent node")
    };

    let llm_config = build_llm_config_value(model, system_prompt, user_prompt, response_format, &[]);

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
            output: crate::models::template::default_output_port(ExecutionBackendType::Llm),
            retry_policy: Default::default(),
            deployment_model: Default::default(),
        },
        parent_id: cx.node.parent_id.clone(),
        width: cx.node.width,
        height: cx.node.height,
        tool_meta: cx.node.tool_meta.clone(),
    };

    let mut virtual_cx = LoweringCtx {
        node: &virtual_node,
        outgoing_edges: cx.outgoing_edges,
        incoming_edges: cx.incoming_edges,
        children: cx.children,
        ctx: &mut *cx.ctx,
        ports: &mut *cx.ports,
        fixups: &mut *cx.fixups,
        node_files: cx.node_files,
        sub_air: cx.sub_air,
        interfaces: &mut *cx.interfaces,
        definitions: cx.definitions,
        node_configs: &mut *cx.node_configs,
        config_storage: cx.config_storage,
    };

    super::automated_step::lower_automated_step(&mut virtual_cx)
}

/// Build the `LlmConfig` JSON the executor LLM backend deserializes.
/// Field names match `aithericon_executor_backend_configs::llm::LlmConfig`
/// 1:1 so `validate_and_transform`'s LLM arm round-trips this without
/// coercion. `tools` is empty for degenerate single-shot calls and
/// populated by the agent loop with one entry per tool child.
fn build_llm_config_value(
    model: &crate::models::template::ModelRef,
    system_prompt: &Option<String>,
    user_prompt: &str,
    response_format: &Option<serde_json::Value>,
    tools: &[serde_json::Value],
) -> serde_json::Value {
    let mut config = serde_json::Map::new();
    config.insert("provider".to_string(), json!(model.provider));
    config.insert("model".to_string(), json!(model.model));
    if let Some(k) = &model.api_key {
        config.insert("api_key".to_string(), json!(k));
    }
    if let Some(b) = &model.base_url {
        config.insert("base_url".to_string(), json!(b));
    }
    if let Some(a) = &model.resource_alias {
        config.insert("resource_alias".to_string(), json!(a));
    }
    config.insert("prompt".to_string(), json!(user_prompt));
    if let Some(sp) = system_prompt {
        config.insert("system_prompt".to_string(), json!(sp));
    }
    if let Some(t) = model.temperature {
        config.insert("temperature".to_string(), json!(t));
    }
    if let Some(m) = model.max_tokens {
        config.insert("max_tokens".to_string(), json!(m));
    }
    if let Some(rf) = response_format {
        config.insert("response_format".to_string(), rf.clone());
    }
    if !tools.is_empty() {
        config.insert(
            "tools".to_string(),
            serde_json::Value::Array(tools.to_vec()),
        );
    }
    serde_json::Value::Object(config)
}

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
        max_turns,
        stop_when,
        on_tool_error,
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_agent_loop on non-Agent node")
    };
    let id = cx.node.id.clone();
    let max_turns = *max_turns;
    let on_tool_error = *on_tool_error;

    // Tool-name uniqueness — analog of `SlugConflict`. A duplicate would
    // make the per-tool dispatch route guards ambiguous; reject at
    // compile so the editor can ring the offending children.
    let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for child in tool_children {
        let tm = child.tool_meta.as_ref().expect("filtered to tool children");
        if !seen_names.insert(tm.tool_name.as_str()) {
            return Err(CompileError::Compilation(format!(
                "agent node '{}': duplicate tool_name '{}' among tool children",
                id, tm.tool_name
            )));
        }
    }

    // Build the tool schemas. Each child's input port shape becomes the
    // tool's `input_schema`. v1: ports don't yet carry per-field JSON
    // Schema → fall back to a permissive `{type: object}` so the LLM
    // can call but the platform doesn't pretend to validate. Per-port
    // schemas are a separate concern from this PR.
    let tool_schemas: Vec<serde_json::Value> = tool_children
        .iter()
        .map(|child| {
            let tm = child.tool_meta.as_ref().unwrap();
            serde_json::json!({
                "name": tm.tool_name,
                "description": tm.tool_description,
                "input_schema": {"type": "object", "properties": {}, "additionalProperties": true},
            })
        })
        .collect();

    let llm_config = build_llm_config_value(
        model,
        system_prompt,
        user_prompt,
        response_format,
        &tool_schemas,
    );

    // Side-channel the static config via `config_ref`. The Petri token
    // stays slim; the executor's `FetchConfigHook` materialises it.
    let storage_key = cx.config_storage.key(&id);
    cx.node_configs.insert(id.clone(), llm_config);
    let config_ref_rhai = format!(
        "#{{ \"storage_path\": \"{}\" }}",
        rhai_str_escape(&storage_key)
    );

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
    let known_names_rhai: String = if tool_children.is_empty() {
        "[]".to_string()
    } else {
        let inner: Vec<String> = tool_children
            .iter()
            .map(|c| {
                let tm = c.tool_meta.as_ref().unwrap();
                format!("\"{}\"", rhai_str_escape(&tm.tool_name))
            })
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
    for child in tool_children {
        let tm = child.tool_meta.as_ref().unwrap();
        let tn = crate::models::template::sanitize_slug(&tm.tool_name);
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
    // accumulating token totals, conversation history (in-token Vec for
    // v1), and `final_response` (set on the terminal turn). The user's
    // inbound token rides through as `state.input`.
    ctx.transition(format!("t_{id}_enter"), format!("{label} - Enter Agent"))
        .auto_input("input", &p_input)
        .auto_output("state", &p_state)
        .logic_rhai(
            r#"#{ state: #{ turn: 0, message_count: 0, total_tokens_in: 0, total_tokens_out: 0, history: [], input: input, final_response: () } }"#
                .to_string(),
        )
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
    .logic_rhai(format!(
        r#"let s = state; let d = #{{ }}; d.job_id = "{id}"; d.run = s.turn; d.retries = 0; d.max_retries = 0; let job_inputs = []; /*__BORROWED_INPUTS__*/ d.spec = #{{ "backend": "llm", "inputs": job_inputs, "outputs": [], "config_ref": {config_ref_rhai}, "stream_events": ["agent_turn", "metric", "log"] }}; d.metadata = #{{ "agent_node_id": "{id}" }}; #{{ job: d, state_in_flight: s }}"#
    ))
    .done();

    let lc = executor_lifecycle(
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
    );

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
    // exactly the way a plain LLM step's borrows resolve. Agent-specific
    // extras (turn, history, final_response, input) ride along under
    // `detail.outputs` so an author who walks the picker can still see
    // them.
    let model_lit = rhai_str_escape(&model.model);
    ctx.transition(
        format!("t_{id}_route_final"),
        format!("{label} - Route: Final"),
    )
    .auto_input("response", &p_response)
    .auto_output("final", &p_final)
    .guard_rhai(format!(
        r#"let s = response.state; {extract_tr} let tc = if type_of(tr.tool_calls) == "array" {{ tr.tool_calls }} else {{ [] }}; tc.len() == 0 || s.turn + 1 >= {max_turns} || {stop_when_expr}"#
    ))
    .logic_rhai(format!(
        r#"let s = response.state; {extract_tr} let content = if type_of(tr.content) == "string" {{ tr.content }} else {{ () }}; let finish_reason = if type_of(tr.stop_reason) == "string" {{ tr.stop_reason }} else {{ "end_turn" }}; let usage = #{{ input_tokens: s.total_tokens_in, output_tokens: s.total_tokens_out }}; let outputs = #{{ response: content, usage: usage, finish_reason: finish_reason, model: "{model_lit}", turn: s.turn, history: s.history, final_response: tr, input: s.input }}; let env = #{{ execution_id: "agent-{id}", job_id: "{id}", run: s.turn, status: "succeeded", source: "agent_loop", detail: #{{ outputs: outputs, exit_code: 0 }} }}; #{{ final: env }}"#
    ))
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
        .logic_rhai(format!(
            r#"let s = response.state; {extract_tr} let tcall = tr.tool_calls[0]; let assistant_content = if type_of(tr.content) == "string" {{ tr.content }} else {{ "" }}; s.history.push(#{{ role: "assistant", content: assistant_content, tool_call_id: tcall.id, tool_name: "{tn}", tool_args: tcall.arguments }}); s.turn = s.turn + 1; s.message_count = s.message_count + 1; #{{ dispatch: #{{ call_id: tcall.id, tool_name: "{tn}", args: tcall.arguments }}, state_in_tool: s }}"#
        ))
        .done();
    }

    // t_route_unknown: ToolErrorPolicy::Feedback only. Fires when the
    // model picked a tool not in the known set, turn budget remains.
    // No dispatch — append a failure message and re-deposit state so
    // the next turn can correct the model. For Bubble policy this
    // transition is omitted; an unknown tool with no fallback then
    // can't satisfy any route guard and the net stalls — which is the
    // explicit "the model misbehaved and we want a noisy failure"
    // semantics of Bubble.
    if matches!(on_tool_error, ToolErrorPolicy::Feedback) && !tool_children.is_empty() {
        ctx.transition(
            format!("t_{id}_route_unknown"),
            format!("{label} - Route: Unknown Tool (feedback)"),
        )
        .auto_input("response", &p_response)
        .auto_output("state", &p_state)
        .guard_rhai(format!(
            r#"let s = response.state; {extract_tr} let tc = if type_of(tr.tool_calls) == "array" {{ tr.tool_calls }} else {{ [] }}; let known = {known_names_rhai}; tc.len() > 0 && !(known.contains(tc[0].name)) && s.turn + 1 < {max_turns} && !({stop_when_expr})"#
        ))
        .logic_rhai(format!(
            r#"let s = response.state; {extract_tr} let tcall = tr.tool_calls[0]; s.history.push(#{{ role: "tool", tool_name: tcall.name, tool_call_id: tcall.id, content: "tool '" + tcall.name + "' not found — pick one of: " + {known_names_rhai} }}); s.turn = s.turn + 1; s.message_count = s.message_count + 1; #{{ state: s }}"#
        ))
        .done();
    }

    // ----- t_exit -----
    ctx.transition(format!("t_{id}_exit"), format!("{label} - Exit"))
        .auto_input("final", &p_final)
        .auto_output("output", &p_output)
        .logic_rhai("#{ output: final }".to_string())
        .done();

    // Foundation split: park the agent's output envelope so downstream
    // `<agent_slug>.final_response` / `<agent_slug>.turn` reads resolve
    // via the read-arc synthesis pass.
    let (data_place_id, p_ctrl) = split_outputs(ctx, &id, label, &p_output);

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
