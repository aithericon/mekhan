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
use crate::models::template::{Port, WorkflowNodeData};
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
    validate: Some(crate::compiler::validate::warn_unmerged_fan_in),
    token_shape: Some(crate::compiler::token_shape::analyze::out_shape_automated_step),
};

fn input_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // AutomatedStep carries its declared input Port verbatim — `default_*`
    // serde defaults give an empty pass-through for templates that never
    // authored an input shape. Matches the central
    // `WorkflowNodeData::input_ports` arm.
    let WorkflowNodeData::AutomatedStep { input, .. } = data else {
        unreachable!("automated_step::input_ports on non-AutomatedStep variant");
    };
    vec![input.clone()]
}

fn output_ports(data: &WorkflowNodeData) -> Vec<Port> {
    // Declared success output + an always-present `error` output (retries
    // exhausted / infra failure). Empty fields ⇒ pass-through so wiring it
    // to any handler/End type-checks. The compiler maps `error` to
    // `p_{id}_error`. Matches the central `WorkflowNodeData::output_ports`
    // arm.
    let WorkflowNodeData::AutomatedStep {
        output,
        stream_output,
        ..
    } = data
    else {
        unreachable!("automated_step::output_ports on non-AutomatedStep variant");
    };
    let mut ports = vec![
        output.clone(),
        Port {
            id: "error".to_string(),
            label: "On error".to_string(),
            fields: vec![],
        },
    ];
    // PROTOTYPE — `stream_output` exposes a SECOND output handle "stream"
    // alongside the control "out" and the "error" port. The compiler maps it to
    // the Signal place `p_{id}_stream` (one token per executor Log event). Empty
    // `fields` ⇒ pass-through so wiring it to any downstream node type-checks.
    if *stream_output {
        ports.push(Port {
            id: "stream".to_string(),
            label: "Stream".to_string(),
            fields: vec![],
        });
    }
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
        stream_output,
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
    // collapse a Scheduled step to Inline (never reaches scheduler-net).
    let in_val = serde_json::to_value(input).unwrap_or_default();
    config.insert(txn, "input", json_value_to_any(&in_val));
    let out_val = serde_json::to_value(output).unwrap_or_default();
    config.insert(txn, "output", json_value_to_any(&out_val));
    let retry_val = serde_json::to_value(retry_policy).unwrap_or_default();
    config.insert(txn, "retryPolicy", json_value_to_any(&retry_val));
    let dm_val = serde_json::to_value(deployment_model).unwrap_or_default();
    config.insert(txn, "deploymentModel", json_value_to_any(&dm_val));
    // `stream_output` is `#[serde(default)]`; like the other fields above it must
    // be written explicitly or the graph→Y.Doc seed + Y.Doc→graph reconstruction
    // would silently reset the prototype streaming flag to `false`.
    config.insert(
        txn,
        "streamOutput",
        json_value_to_any(&serde_json::Value::Bool(*stream_output)),
    );
}
