//! Typed-port definitions: [`Port`], [`PortField`], [`FieldKind`] and the
//! serde default-port constructors. Port schema emission + token validation
//! live with the compiler in `crate::compiler::token_shape::port`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use aithericon_backends::ExecutionBackendType;

use super::human_task::{deserialize_task_field_options, SelectOption};

/// Deserialization default for `SubWorkflow.output` — an empty `out` port
/// (Json pass-through of the child's terminal result). Authoring can prefill
/// fields from the child's End `terminal` port.
pub fn default_subworkflow_output_port() -> Port {
    Port {
        id: "out".to_string(),
        label: "Result".to_string(),
        fields: vec![],
    }
}

/// Deserialization default for `SubWorkflow.input_contract` — an empty `in`
/// port. Display-only; the real contract is filled by publish reconcile / the
/// editor's io-contract fetch. Existing graphs without the field load unchanged.
pub fn default_subworkflow_input_contract() -> Port {
    Port::empty_input()
}

/// Deserialization default for `Join.output` — an empty `out` port. The
/// editor or author fills in the fields the join exposes downstream via
/// `<slug>.<field>`.
pub fn default_join_output_port() -> Port {
    Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: vec![],
    }
}

/// Type kind for a typed port field. Superset of `TaskFieldKind`: adds `Bool`
/// (currently piggybacks on `Checkbox` in human-task forms), `Timestamp`
/// (needed for trigger fire times and audit fields), and `Json` (opaque
/// escape hatch for legacy / dynamic payloads). Snake-case wire values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Textarea,
    Number,
    Bool,
    Select,
    File,
    Signature,
    Timestamp,
    Json,
    /// First-class container marker for a nested JSON object. The recursive
    /// SHAPE lives in [`PortField::schema`] (a JSON Schema); this kind only
    /// marks the field as an object container. With a `schema` the emitted
    /// contract is that schema verbatim; without one it falls back to a
    /// permissive `{"type":"object","additionalProperties":true}`.
    Object,
    /// First-class container marker for a nested JSON array. The element/item
    /// SHAPE lives in [`PortField::schema`]; without one it falls back to a
    /// permissive `{"type":"array"}`.
    Array,
}

/// A single field within a typed `Port`. Identifier-like `name` is the wire
/// key in the token; `label` is for display.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortField {
    pub name: String,
    pub label: String,
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    /// Pre-fill value for launch surfaces: the instance Run form (and any
    /// other token-composing UI) seeds the field's input with this instead of
    /// the bare kind default, so a template with sensible defaults runs
    /// first-try from an untouched form. Display-side only — token
    /// validation never falls back to it (a submitted token must still carry
    /// the field), so an API caller omitting a required field is rejected
    /// regardless of any declared default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Choice list for `kind = Select`. Same `{value, label}` shape as
    /// [`TaskFieldConfig::options`]; the deserializer accepts either bare
    /// strings or `{value, label}` objects and normalizes to the rich
    /// form. See [`SelectOption`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_task_field_options"
    )]
    pub options: Option<Vec<SelectOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// For `File` kind: accepted formats as an HTML input `accept` list
    /// (comma-separated MIME types and/or extensions, e.g.
    /// `"image/png,image/jpeg,.pdf"`). The instance-launch upload widget
    /// uses this to filter the picker, reject mismatched files, and show
    /// the expected formats. Ignored for non-file kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<String>,
    /// Optional rich JSON Schema override for this field's value, for the cases
    /// where the flat `kind` vocabulary (`Json` being the only escape hatch)
    /// can't express the real structure. When present it is the field's
    /// authoritative schema everywhere a richer-than-scalar shape is needed:
    /// it becomes the agent-tool `input_schema` property (so a model calling a
    /// node as a tool is told the exact nested shape to produce), and it feeds
    /// the colored-token definition the runtime `SchemaRegistry` enforces. The
    /// canonical use is the dynamic-form HumanTask: its `steps` input field
    /// carries [`task_step_list_json_schema`] so the agent and the runtime
    /// share one single-sourced `TaskStepConfig[]` contract. Not author-facing
    /// — derived by node lowering, never hand-edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

/// A named bundle of typed fields exchanged at a block boundary. Two ports
/// type-match if their field sets are equal (same names, same kinds, with
/// `Json` as the escape hatch).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Port {
    /// Unique within the block (e.g. `"in"`, `"out"`, `"approved"`).
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub fields: Vec<PortField>,
}

impl Port {
    /// Empty input port — used as the deserialization default for `Start.initial`
    /// and similar so existing templates load unchanged.
    pub fn empty_input() -> Self {
        Self {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![],
        }
    }
}

/// One fold/scan slot on a [`WorkflowNodeData::Loop`]. Lives as an additional
/// field in the loop's parked `p_<id>_data` envelope (the iteration counter
/// generalized): `init` is evaluated once in the enter transition, `merge_expr`
/// is re-evaluated write-once-per-iteration in the continue transition. Both
/// are Rhai expressions. `merge_expr` may reference the prior accumulator value
/// as `<loop_slug>.<var>` and the body's output as `<body_slug>.<field>` — the
/// standard read-arc synthesis resolves those borrows automatically.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoopAccumulator {
    /// Rhai identifier the accumulator is addressed by, both inside the loop's
    /// own continue transition (`<loop_slug>.<var>`) and downstream. Must not
    /// be `iteration` (reserved) and must be unique within the loop.
    pub var: String,
    /// Rhai expression evaluated in the enter transition scope (the entering
    /// workflow token is bound as `input`). Keep simple — e.g. `"0"`, `"[]"`,
    /// `"#{}"`.
    pub init: String,
    /// Rhai expression evaluated in the continue transition scope, producing the
    /// next accumulator value. References the prior value as `<loop_slug>.<var>`
    /// and body output as `<body_slug>.<field>`.
    #[serde(rename = "mergeExpr")]
    pub merge_expr: String,
}

pub fn default_initial_port() -> Port {
    Port::empty_input()
}

pub fn default_terminal_port() -> Port {
    Port {
        id: "in".to_string(),
        label: "Terminal".to_string(),
        fields: vec![],
    }
}

pub fn default_automated_input_port() -> Port {
    Port::empty_input()
}

/// Canonical output-port shape for an `AutomatedStep` whose `output` field
/// hasn't been customized. Each backend declares the fields its executor
/// reliably surfaces via [`crate::backends::BackendDecl::default_output_fields`].
/// Editor exposes "Reset to default" by re-deriving against the current
/// `backendType`.
///
/// `BACKENDS` covers every `ExecutionBackendType` variant — the registry test
/// (`backend_registry_coverage.rs`) enforces it. No fallback needed.
pub fn default_output_port(backend: ExecutionBackendType) -> Port {
    let decl = crate::backends::lookup(backend)
        .expect("BACKENDS must cover every ExecutionBackendType; enforced by registry test");
    Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: decl
            .default_output_fields
            .iter()
            .map(|f| f.into_port_field())
            .collect(),
    }
}

pub fn default_automated_output_port() -> Port {
    // Serde default fires before we know the backend type, so we fall back to a
    // generic empty port. `AutomatedStep::ensure_output_default` (called from
    // the compiler and editor) re-derives backend-specific fields when the
    // stored `output` is the bare default and the user hasn't customized it.
    Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: vec![],
    }
}
