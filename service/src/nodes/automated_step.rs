//! `AutomatedStep` node declaration. Carries the declared `input` / `output`
//! ports verbatim; output is the declared success port plus an always-present
//! `error` port the compiler maps to `p_{id}_error`.
//!
//! The `BackendConfig` lives inside `execution_spec.config` and is encoded as
//! a JSON blob under the `executionSpec` Y.Map key — mirroring the existing
//! `yjs/doc_ops.rs::write_node_config` arm verbatim. The runtime backend
//! selection happens later (see `compiler/lower/automated_step.rs`'s three
//! dispatch arms for Inline / Scheduled / EngineEffect).

use crate::compiler::interface::NodeKind;
use crate::models::template::{ChannelDirection, Port, WorkflowNodeData};
use crate::nodes::{NodeDecl, YjsEncodeFn};
use crate::yjs::persistence::json_value_to_any;

pub(crate) static AUTOMATED_STEP_DECL: NodeDecl = NodeDecl {
    wire_name: "automated_step",
    display_label: "Automated Step",
    description: Some(
        "Run a job through one of the executor backends (Python / Docker / HTTP / \
         File Ops / LLM / Kreuzberg / Catalogue / Postgres / SMTP). Parks the \
         job's output as a write-once envelope downstream borrows can read via \
         `<slug>.<field>`.",
    ),
    kind: NodeKind::AutomatedStep,
    lowers_to_air: true,
    is_join: false,
    parks_data_envelope: true,
    lower: Some(crate::compiler::lower::automated_step::lower_automated_step),
    input_ports,
    output_ports,
    wiring_logic: None,
    yjs_encode: yjs_encode as YjsEncodeFn,
    // The unmerged-fan-in warning (shared with HumanTask) — never errors.
    validate: Some(crate::compiler::validate::validate_automated_step),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_automated_step),
};

fn input_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // AutomatedStep carries its declared input Port verbatim — `default_*`
    // serde defaults give an empty pass-through for templates that never
    // authored an input shape. Matches the central
    // `WorkflowNodeData::input_ports` arm.
    let WorkflowNodeData::AutomatedStep {
        input, channels, ..
    } = data
    else {
        unreachable!("automated_step::input_ports on non-AutomatedStep variant");
    };
    // Plus one pass-through input port per `In` channel (docs/25) — an upstream
    // edge wires to it by `targetHandle == <name>` (the compiler registers the
    // synthesized inbound place in `NodePorts.input_handles` under the same
    // name). Empty fields ⇒ pass-through so the dynamic channel token wires
    // without a static field contract. Channel-less steps add nothing, so the
    // port list is byte-stable.
    let mut ports = vec![input.clone()];
    ports.extend(
        channels
            .iter()
            .filter(|c| matches!(c.direction, ChannelDirection::In))
            .map(|c| Port {
                id: c.name.clone(),
                label: c.name.clone(),
                fields: vec![],
            }),
    );
    ports
}

fn output_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // Declared success output + an always-present `error` output (retries
    // exhausted / infra failure). Empty fields ⇒ pass-through so wiring it
    // to any handler/End type-checks. The compiler maps `error` to
    // `p_{id}_error`. Matches the central `WorkflowNodeData::output_ports`
    // arm.
    let WorkflowNodeData::AutomatedStep {
        output, channels, ..
    } = data
    else {
        unreachable!("automated_step::output_ports on non-AutomatedStep variant");
    };
    // Plus one pass-through output port per `Out` channel (docs/25) — a
    // downstream edge wires off it by `sourceHandle == <name>` (the compiler
    // registers the synthesized deposit/gathered place in
    // `NodePorts.output_places` under the same name; `find_output_place` routes
    // there). Empty fields ⇒ pass-through so the dynamic channel token (a
    // `Signal` emission or the `Scatter` gathered `{ output: [..] }` envelope)
    // wires without a static field contract. Channel-less steps add nothing, so
    // the port list is byte-stable.
    let mut ports = vec![
        output.clone(),
        Port {
            id: "error".to_string(),
            label: "On error".to_string(),
            fields: vec![],
        },
    ];
    ports.extend(
        channels
            .iter()
            .filter(|c| matches!(c.direction, ChannelDirection::Out))
            .map(|c| Port {
                id: c.name.clone(),
                label: c.name.clone(),
                fields: vec![],
            }),
    );
    ports
}

fn yjs_encode(txn: &mut yrs::TransactionMut<'_>, config: &yrs::MapRef, data: &WorkflowNodeData) {
    use yrs::Map;
    let WorkflowNodeData::AutomatedStep {
        execution_spec,
        input,
        output,
        retry_policy,
        deployment_model,
        channels,
        requirements,
        ..
    } = data
    else {
        unreachable!("automated_step::yjs_encode on non-AutomatedStep variant");
    };
    // `execution_spec` carries the per-backend `BackendConfig` enum nested as
    // `config`. We serialize the whole spec as a JSON blob into the `executionSpec`
    // Y.Map key (matches the legacy arm verbatim) so the round-trip stays
    // schema-agnostic — adding a new backend variant doesn't require a Y.Doc
    // schema migration.
    let spec_val = serde_json::to_value(execution_spec).unwrap_or_default();
    config.insert(txn, "executionSpec", json_value_to_any(&spec_val));
    // `input`/`output`/`retry_policy`/`deployment_model` are all
    // `#[serde(default)]` on AutomatedStep, so omitting any of them here
    // makes the graph→Y.Doc seed (createTemplate / seeded demos) + the
    // Y.Doc→graph reconstruction (`doc_to_graph`) silently reset them.
    // Without input/output the editor's "Output port — Fields" panel reads
    // back empty; without retry/deployment we'd lose authored retries and
    // collapse a Scheduled step to Inline (never reaches external cluster dispatch).
    let in_val = serde_json::to_value(input).unwrap_or_default();
    config.insert(txn, "input", json_value_to_any(&in_val));
    let out_val = serde_json::to_value(output).unwrap_or_default();
    config.insert(txn, "output", json_value_to_any(&out_val));
    let retry_val = serde_json::to_value(retry_policy).unwrap_or_default();
    config.insert(txn, "retryPolicy", json_value_to_any(&retry_val));
    let dm_val = serde_json::to_value(deployment_model).unwrap_or_default();
    config.insert(txn, "deploymentModel", json_value_to_any(&dm_val));
    // `channels` is `#[serde(default, skip_serializing_if = Vec::is_empty)]`;
    // like the other fields above it must be written explicitly (when non-empty)
    // or the graph→Y.Doc seed + Y.Doc→graph reconstruction would silently drop
    // the declared streaming channels. Empty ⇒ absent key (round-trips to `[]`).
    if !channels.is_empty() {
        let ch_val = serde_json::to_value(channels).unwrap_or_default();
        config.insert(txn, "channels", json_value_to_any(&ch_val));
    }
    // `requirements` is `Option<Requirements>` (`#[serde(default,
    // skip_serializing_if = Option::is_none)]`). Like deploymentModel it must be
    // written explicitly or the graph→Y.Doc seed (createTemplate / seeded demos)
    // + the Y.Doc→graph reconstruction (`doc_to_graph`) would silently drop the
    // authored placement constraints. Mirror the serde `skip_if_none`: write the
    // serialized requirements ONLY when present so `None` round-trips as an
    // absent key (which `doc_to_graph` reads back as `None`) — byte-identical to
    // a step that never authored requirements.
    if let Some(req) = requirements {
        let req_val = serde_json::to_value(req).unwrap_or_default();
        config.insert(txn, "requirements", json_value_to_any(&req_val));
    }
}
