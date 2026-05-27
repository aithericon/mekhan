//! `Agent` node declaration. Subsumes single-shot LLM (degenerate path lowers
//! byte-identically to `AutomatedStep(Llm)`) plus the multi-turn agent loop
//! with optional tool children.
//!
//! **Kind mapping is intentional**: `NodeKind::AutomatedStep`, NOT
//! `NodeKind::Agent`. The degenerate-path byte-identical contract is pinned
//! by `agent_degenerate_lowers_byte_identical_to_llm_automated_step` — the
//! emitted `NodeInterface` MUST publish `AutomatedStep` so downstream
//! consumers (causality, the borrow planner's `hoist_path`, `canonical_output_payload`)
//! treat the agent's degenerate path the same as a hand-authored Llm step.
//!
//! The registry **declares** this mapping; before PR2 it lived as a special
//! case in `compiler/lower/mod.rs::node_kind_of` (the "hack at line 412" the
//! plan calls out). With Agent in the registry, that fallback becomes dead
//! code at PR2's merge-time cleanup.

use crate::compiler::interface::NodeKind;
use crate::models::template::{
    agent_extra_output_fields, agent_to_llm_config, default_output_port, ExecutionBackendType,
    Port, WorkflowNodeData,
};
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
    // Declared, not derived. Replaces the `node_kind_of` carve-out at
    // `compiler/lower/mod.rs:412`. Preserves the byte-identical contract
    // (`agent_degenerate_lowers_byte_identical_to_llm_automated_step`).
    kind: NodeKind::AutomatedStep,
    lowers_to_air: true,
    is_join: false,
    // Agent's loop path parks state in `p_<id>_data` (see
    // `lower_agent_loop` → `publish_interface().data_port = ...`). The
    // degenerate path delegates to AutomatedStep which also parks data.
    parks_data_envelope: true,
    lower: Some(crate::compiler::lower::agent::lower_agent),
    input_ports: input_ports,
    output_ports: output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
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

fn yjs_encode(
    txn: &mut yrs::TransactionMut<'_>,
    config: &yrs::MapRef,
    data: &WorkflowNodeData,
) {
    use yrs::Map;
    let WorkflowNodeData::Agent {
        model,
        system_prompt,
        user_prompt,
        response_format,
        max_turns,
        stop_when,
        context_strategy,
        on_tool_error,
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
    config.insert(txn, "maxTurns", *max_turns as f64);
    if let Some(sw) = stop_when {
        config.insert(txn, "stopWhen", sw.clone());
    }
    let cs_val = serde_json::to_value(context_strategy).unwrap_or_default();
    config.insert(txn, "contextStrategy", json_value_to_any(&cs_val));
    let te_val = serde_json::to_value(on_tool_error).unwrap_or_default();
    config.insert(txn, "onToolError", json_value_to_any(&te_val));
}
