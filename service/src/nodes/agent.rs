//! `Agent` node declaration. Subsumes single-shot LLM (degenerate path lowers
//! byte-identically to `AutomatedStep(Llm)`) plus the multi-turn agent loop
//! with optional tool children.
//!
//! **Kind mapping is asymmetric by path**:
//!
//! - Loop path → published `NodeKind::Agent`. Reflects the distinct subnet
//!   shape (`p_state`, `p_response`, `t_route_*`, per-tool dispatch).
//!   `NodeKind::Agent::hoist_path() == ["detail", "outputs"]` so the borrow
//!   planner resolves `<agent>.response` / `<agent>.usage` / `<agent>.turn`
//!   against the same `{detail: {outputs: …}}` envelope an `AutomatedStep(Llm)`
//!   would park.
//! - Degenerate path (`max_turns == 1`, no `stop_when`, no tool children) →
//!   published `NodeKind::AutomatedStep`. `lower_agent_degenerate` synthesises
//!   a virtual `WorkflowNodeData::AutomatedStep` node and delegates to
//!   `automated_step::lower_automated_step`; `publish_interface` reads the
//!   kind from `lookup_by_variant(virtual_node.data)` and gets AutomatedStep.
//!   This is what keeps `agent_degenerate_lowers_byte_identical_to_llm_automated_step`
//!   green: the published interface is byte-identical to an Llm step's.

use crate::compiler::interface::NodeKind;
use crate::compiler::lower::agent::{agent_extra_output_fields, agent_to_llm_config};
use crate::models::template::{default_output_port, ExecutionBackendType, Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static AGENT_DECL: NodeDecl = NodeDecl {
    wire_name: "agent",
    display_label: "Agent",
    description: Some(
        "LLM call optionally extended with tool children and a multi-turn \
         loop. Degenerate path (max_turns == 1, no stop_when, no tools) \
         lowers byte-identically to AutomatedStep(Llm).",
    ),
    // Loop-path kind. The degenerate path delegates via a virtual
    // `WorkflowNodeData::AutomatedStep` node, so `publish_interface`
    // reads kind from the *virtual* variant and stays AutomatedStep — the
    // byte-identical contract (`agent_degenerate_lowers_byte_identical_to_llm_automated_step`)
    // continues to hold. The loop path's envelope nesting matches
    // AutomatedStep's (`detail.outputs.*`), so `NodeKind::Agent::hoist_path()`
    // returns the same segments.
    kind: NodeKind::Agent,
    lowers_to_air: true,
    is_join: false,
    // Agent's loop path parks state in `p_<id>_data` (see
    // `lower_agent_loop` → `publish_interface().data_port = ...`). The
    // degenerate path delegates to AutomatedStep which also parks data.
    parks_data_envelope: true,
    lower: Some(crate::compiler::lower::agent::lower_agent),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    // No per-node structural rule. Agent shares AutomatedStep's outbound
    // executor-envelope shape (the byte-identical degenerate-path contract),
    // so its token_shape points at the same `out_shape_automated_step`.
    validate: None,
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_automated_step),
};

fn input_ports(_data: &WorkflowNodeData) -> Vec<Port> {
    // Agent accepts the single anonymous upstream token. The
    // user/system prompt templates `{{<slug>.<field>}}`-interpolate
    // against the parked-data envelopes of upstream producers, not
    // the inbound port; the input port is a Json pass-through.
    vec![Port::empty_input()]
}

fn output_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // Agent's success output starts with the canonical LLM fields
    // (`response`, `usage`, `finish_reason`, `model`) — same shape
    // a plain `AutomatedStep(Llm)` declares, so the degenerate
    // path's editor wiring matches an Llm step's. The loop path
    // additionally packs four agent-specific fields (`turn`,
    // `history`, `final_response`, `input`) under `detail.outputs`;
    // declared here only when the agent will actually take the loop
    // path so the degenerate-path byte-identical contract holds.
    let WorkflowNodeData::Agent {
        model,
        system_prompt,
        user_prompt,
        response_format,
        images,
        max_turns,
        stop_when,
        ..
    } = data
    else {
        unreachable!("agent::output_ports on non-Agent variant");
    };
    let cfg = agent_to_llm_config(
        model,
        system_prompt.as_deref(),
        user_prompt,
        response_format.as_ref(),
        images,
        &[],
    );
    let mut success = crate::backends::lookup(ExecutionBackendType::Llm)
        .and_then(|d| d.derive_output_port)
        .map(|f| f(&cfg))
        .unwrap_or_else(|| default_output_port(ExecutionBackendType::Llm));
    let takes_loop_path = *max_turns > 1 || stop_when.is_some();
    if takes_loop_path {
        success.fields.extend(agent_extra_output_fields());
    }
    vec![
        success,
        Port {
            id: "error".to_string(),
            label: "On error".to_string(),
            fields: vec![],
        },
    ]
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::Agent {
        model,
        system_prompt,
        user_prompt,
        response_format,
        images,
        max_turns,
        stop_when,
        context_strategy,
        on_tool_error,
        retry_policy,
        deployment_model,
        ..
    } = data
    else {
        unreachable!("agent::yjs_encode on non-Agent variant");
    };
    let model_val = serde_json::to_value(model).unwrap_or_default();
    config.insert(txn, "model", json_value_to_any(&model_val));
    if let Some(sp) = system_prompt {
        config.insert(txn, "systemPrompt", sp.clone());
    }
    config.insert(txn, "userPrompt", user_prompt.clone());
    if let Some(rf) = response_format {
        config.insert(txn, "responseFormat", json_value_to_any(rf));
    }
    if !images.is_empty() {
        let imgs_val = serde_json::Value::Array(images.clone());
        config.insert(txn, "images", json_value_to_any(&imgs_val));
    }
    config.insert(txn, "maxTurns", *max_turns as f64);
    if let Some(sw) = stop_when {
        config.insert(txn, "stopWhen", sw.clone());
    }
    let cs_val = serde_json::to_value(context_strategy).unwrap_or_default();
    config.insert(txn, "contextStrategy", json_value_to_any(&cs_val));
    let te_val = serde_json::to_value(on_tool_error).unwrap_or_default();
    config.insert(txn, "onToolError", json_value_to_any(&te_val));
    // Seed retry/deployment so the editor round-trips them through Yjs —
    // mirrors `automated_step::yjs_encode`. Without this, a demo authored with
    // a non-default deployment would lose it on the first collaborative save.
    let retry_val = serde_json::to_value(retry_policy).unwrap_or_default();
    config.insert(txn, "retryPolicy", json_value_to_any(&retry_val));
    let dm_val = serde_json::to_value(deployment_model).unwrap_or_default();
    config.insert(txn, "deploymentModel", json_value_to_any(&dm_val));
}
