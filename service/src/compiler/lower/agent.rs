//! `WorkflowNodeData::Agent` lowering — `docs/12-agent-node-design.md`.
//!
//! - Degenerate path (`max_turns == 1` + no `stop_when` + no tool children)
//!   synthesizes the equivalent `AutomatedStep(Llm)` and delegates so the
//!   compiled net is byte-identical to a hand-authored single-shot LLM step.
//! - Full agent loop emits the parked-state + LLM call + route topology
//!   described in `docs/12` § 3 (v1: ContextStrategy::None only, tool
//!   subnets are pinned as scaffolding without yet being wired).

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

/// Full agent-loop lowering — docs/12 § 3. Emits:
///
///   p_input ─► t_enter ─► p_state ─► t_call_llm ─► p_response ─► t_route ┬─► p_dispatch_<tool> (per tool)
///                            ▲                                           └─► p_final ─► t_exit ─► p_output
///                            │
///                            └── (re-entered on tool collect — wiring in
///                                 a follow-up PR; v1 routes to p_final)
///
/// v1 scope cut (per the PR plan): both `ToolErrorPolicy` variants compile
/// to the same structural shape (the policy lives in `t_route`'s Rhai
/// branch logic at runtime); `ContextStrategy::None` only; per-tool
/// `t_collect_<tn>` transitions are emitted as compile-time scaffolding
/// but their child-output wiring is delegated to a follow-up PR. The
/// route currently always deposits on `p_final` — once the tool subnet
/// wiring lands, the route gets the data-driven branch on
/// `response.tool_calls[0].name`.
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
        ..
    } = &cx.node.data
    else {
        unreachable!("lower_agent_loop on non-Agent node")
    };
    let id = cx.node.id.clone();

    // Tool-name uniqueness — analog of `SlugConflict` (error.rs:54). A
    // duplicate would make the `t_route` branch guard ambiguous; reject
    // at compile so the editor can ring the offending children.
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

    // Side-channel the static config via `config_ref` (same as
    // `lower_automated_step:1124`). The Petri token stays slim; the
    // executor's `FetchConfigHook` materialises it.
    let storage_key = cx.config_storage.key(&id);
    cx.node_configs.insert(id.clone(), llm_config);
    let config_ref_rhai = format!(
        "#{{ \"storage_path\": \"{}\" }}",
        rhai_str_escape(&storage_key)
    );

    let ctx = &mut *cx.ctx;

    // ----- Places -----
    let p_input: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_input"), format!("{label} - Input"));
    let p_state: PlaceHandle<DynamicToken> = ctx.state(
        format!("p_{id}_state"),
        format!("{label} - Agent State (parked)"),
    );
    let p_response: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_response"), format!("{label} - LLM Response"));
    let p_final: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_final"), format!("{label} - Final"));
    let p_output: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_output"), format!("{label} - Output"));
    let p_error: PlaceHandle<DynamicToken> =
        ctx.state(format!("p_{id}_error"), format!("{label} - Error"));

    // One dispatch place per declared tool (sources for t_route's tool
    // branches). v1 has no consumer for these — the route always picks
    // p_final — but minting them now pins the AIR shape so the follow-up
    // PR that wires tool subnets doesn't have to renumber places.
    let mut dispatch_places: Vec<(String, PlaceHandle<DynamicToken>)> = Vec::new();
    for child in tool_children {
        let tm = child.tool_meta.as_ref().unwrap();
        let tn = crate::models::template::sanitize_slug(&tm.tool_name);
        let pd: PlaceHandle<DynamicToken> = ctx.state(
            format!("p_{id}_dispatch_{tn}"),
            format!("{label} - Dispatch {tn}"),
        );
        dispatch_places.push((tn, pd));
    }

    // ----- t_enter: initialize parked state, hand off to call_llm -----
    // The parked envelope keeps the slim agent state: turn counter,
    // accumulating token totals, and `final_response` (set on the
    // terminal turn). The user's inbound token rides through as the
    // initial conversation input under `state.input`.
    ctx.transition(format!("t_{id}_enter"), format!("{label} - Enter Agent"))
        .auto_input("input", &p_input)
        .auto_output("state", &p_state)
        .logic_rhai(
            r#"#{ state: #{ turn: 0, message_count: 0, total_tokens_in: 0, total_tokens_out: 0, input: input, final_response: () } }"#
                .to_string(),
        )
        .done();

    // ----- t_call_llm: executor lifecycle for one LLM turn -----
    // Consumes the parked state, submits an LLM job, returns the
    // response envelope on lc.completed (or routes to lc.failed for
    // executor-side errors). `stream_events` includes `agent_turn` so
    // the executor emits per-turn observability events on the
    // `executor.events.{exec_id}.agent_turn` subject (docs/12 § 5).
    // `metadata.agent_node_id` flags the LLM job as part of an agent
    // context so the executor side gates the AgentTurn emission.
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
    .auto_output("state", &p_state)
    .logic_rhai(format!(
        r#"let s = state; let mut d = #{{ }}; d.job_id = "{id}"; d.run = s.turn; d.retries = 0; d.max_retries = 0; let job_inputs = []; d.spec = #{{ "backend": "llm", "inputs": job_inputs, "outputs": [], "config_ref": {config_ref_rhai}, "stream_events": ["agent_turn", "metric", "log"] }}; d.metadata = #{{ "agent_node_id": "{id}" }}; #{{ job: d, state: s }}"#
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

    // Bridge lifecycle outputs to the route's input.
    ctx.transition(
        format!("t_{id}_to_response"),
        format!("{label} - To Response"),
    )
    .auto_input("done", &lc.completed)
    .auto_output("response", &p_response)
    .logic_rhai("#{ response: done }".to_string())
    .done();

    // Executor-side failure paths drain to the agent's error output —
    // there's no per-turn retry in v1 (the LLM call has its own provider
    // retries inside the adapter; agent-level retry composes via the
    // standard error edge user pattern).
    ctx.transition(format!("t_{id}_call_failed"), format!("{label} - LLM Call Failed"))
        .auto_input("dead", &lc.failed)
        .auto_output("error", &p_error)
        .logic_rhai("#{ error: dead }".to_string())
        .done();
    ctx.transition(format!("t_{id}_call_timed_out"), format!("{label} - LLM Call Timed Out"))
        .auto_input("dead", &lc.timed_out)
        .auto_output("error", &p_error)
        .logic_rhai("#{ error: dead }".to_string())
        .done();
    ctx.transition(format!("t_{id}_call_dead"), format!("{label} - LLM Call Dead Letter"))
        .auto_input("dead", &lc.dead_letter)
        .auto_output("error", &p_error)
        .logic_rhai("#{ error: dead }".to_string())
        .done();

    // ----- t_route: branch on LlmTurnResult shape -----
    // v1: always routes to p_final. The follow-up PR adds the
    // tool-branching script — once `p_dispatch_<tn>` has a consumer, the
    // route's Rhai picks which port to populate by inspecting
    // `response.detail.outputs.turn_result.tool_calls[0].name`. Until
    // then the dispatch places exist as named outputs but receive no
    // tokens.
    let mut route = ctx.transition(format!("t_{id}_route"), format!("{label} - Route"))
        .auto_input("response", &p_response)
        .auto_output("final", &p_final);
    for (tn, pd) in &dispatch_places {
        route = route.auto_output(format!("dispatch_{tn}"), pd);
    }
    route
        .logic_rhai(
            r#"let resp = response; let outs = resp.detail.outputs; let tr = outs.turn_result ?? outs.response; #{ final: #{ content: tr, turn: 1, final_response: tr } }"#
                .to_string(),
        )
        .done();

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
