use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// --- Database row ---

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct WorkflowTemplate {
    pub id: Uuid,
    pub name: String,
    pub description: String,

    // Version chain
    pub base_template_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub version: i32,
    pub is_latest: bool,

    // Publishing
    pub published: bool,
    pub published_at: Option<DateTime<Utc>>,
    pub published_by: Option<Uuid>,

    // Graph data
    pub graph: serde_json::Value,

    // Compiled AIR (populated on publish)
    pub air_json: Option<serde_json::Value>,

    // Metadata
    pub author_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// --- Visual editor data model (Section 2) ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowGraph {
    pub nodes: Vec<WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<Viewport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Viewport {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub position: Position,
    pub data: WorkflowNodeData,
    /// Parent scope node id — child positions are relative to the parent.
    #[serde(rename = "parentId", skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Explicit width (used by scope nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// Explicit height (used by scope nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkflowNodeData {
    #[serde(rename = "start")]
    Start {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Declared input schema. The token this Start emits has this shape.
        /// Defaults to an empty-fields port so pre-typed-ports templates load
        /// unchanged. Replaces the previous opaque `initialData` blob; initial
        /// tokens are now seeded per-Start at instance creation time via the
        /// `start_tokens` field of `CreateInstanceRequest`.
        #[serde(default = "default_initial_port")]
        initial: Port,
    },
    #[serde(rename = "end")]
    End {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Declared terminal token shape. Defaults to an empty port (accepts
        /// any incoming token) so existing End nodes keep working. UI editor
        /// for `terminal` lands in Phase 4.
        #[serde(default = "default_terminal_port")]
        terminal: Port,
    },
    #[serde(rename = "human_task")]
    HumanTask {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(rename = "taskTitle")]
        task_title: String,
        #[serde(rename = "instructionsMdsvex", skip_serializing_if = "Option::is_none")]
        instructions_mdsvex: Option<String>,
        steps: Vec<TaskStepConfig>,
    },
    #[serde(rename = "automated_step")]
    AutomatedStep {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(rename = "executionSpec")]
        execution_spec: ExecutionSpecConfig,
        /// Declared input shape. Empty by default — empty `fields` means
        /// "accepts any token" (Json pass-through), preserving back-compat
        /// for templates that wire arbitrary upstream output into an
        /// automated step. Phase 4 may derive richer defaults.
        #[serde(default = "default_automated_input_port")]
        input: Port,
        /// Declared output shape. Persisted as-is once authored; the bare
        /// default has no fields and should be re-derived from
        /// `execution_spec.backend_type` via `default_output_port` whenever a
        /// caller needs the canonical backend shape.
        #[serde(default = "default_automated_output_port")]
        output: Port,
    },
    #[serde(rename = "decision")]
    Decision {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        conditions: Vec<BranchCondition>,
        #[serde(rename = "defaultBranch", skip_serializing_if = "Option::is_none")]
        default_branch: Option<String>,
    },
    #[serde(rename = "parallel_split")]
    ParallelSplit {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    #[serde(rename = "parallel_join")]
    ParallelJoin {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// How tokens arriving on the joined branches are merged into the
        /// single output token. `ShallowLastWins` (default) preserves the
        /// historical behaviour; `DeepMerge` recursively merges nested maps.
        #[serde(rename = "mergeStrategy", default)]
        merge_strategy: MergeStrategy,
    },
    #[serde(rename = "loop")]
    Loop {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(rename = "maxIterations")]
        max_iterations: i32,
        #[serde(rename = "loopCondition")]
        loop_condition: String,
    },
    #[serde(rename = "scope")]
    Scope {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Trigger node (Phase 5). Lives at the template level and connects to a
    /// target input port via a single outgoing edge. Triggers are never edge
    /// targets; they are *inputs to the workflow*, not part of it. AIR
    /// compilation skips trigger nodes — they are a pre-compile concern owned
    /// by the dispatcher (`service::triggers`).
    #[serde(rename = "trigger")]
    Trigger {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Tagged source describing what event fires this trigger.
        source: TriggerSource,
        /// Concurrency / dedup policy applied by the dispatcher.
        #[serde(default)]
        concurrency: ConcurrencyPolicy,
        /// Per-target-field mapping. Each entry's `expression` is a Rhai
        /// expression evaluated against the trigger source's event payload.
        #[serde(rename = "payloadMapping", default)]
        payload_mapping: Vec<FieldMapping>,
        /// Disabled triggers are stored but the dispatcher ignores them.
        #[serde(default)]
        enabled: bool,
    },
}

impl WorkflowNodeData {
    pub fn label(&self) -> &str {
        match self {
            Self::Start { label, .. }
            | Self::End { label, .. }
            | Self::HumanTask { label, .. }
            | Self::AutomatedStep { label, .. }
            | Self::Decision { label, .. }
            | Self::ParallelSplit { label, .. }
            | Self::ParallelJoin { label, .. }
            | Self::Loop { label, .. }
            | Self::Scope { label, .. }
            | Self::Trigger { label, .. } => label,
        }
    }

    pub fn type_name(&self) -> &str {
        match self {
            Self::Start { .. } => "start",
            Self::End { .. } => "end",
            Self::HumanTask { .. } => "human_task",
            Self::AutomatedStep { .. } => "automated_step",
            Self::Decision { .. } => "decision",
            Self::ParallelSplit { .. } => "parallel_split",
            Self::ParallelJoin { .. } => "parallel_join",
            Self::Loop { .. } => "loop",
            Self::Scope { .. } => "scope",
            Self::Trigger { .. } => "trigger",
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            Self::Start { description, .. }
            | Self::End { description, .. }
            | Self::HumanTask { description, .. }
            | Self::AutomatedStep { description, .. }
            | Self::Decision { description, .. }
            | Self::ParallelSplit { description, .. }
            | Self::ParallelJoin { description, .. }
            | Self::Loop { description, .. }
            | Self::Scope { description, .. }
            | Self::Trigger { description, .. } => description.as_deref(),
        }
    }

    /// Typed input ports declared or derived for this node. Returns owned
    /// ports because some variants (HumanTask, Decision, ...) derive their
    /// ports from inner config rather than carrying a stored `Port`. The
    /// returned list is small (1-2 entries) so allocation is negligible.
    ///
    /// An empty list means "single anonymous input" — edges with
    /// `target_handle: "in"` still resolve via the pass-through path in
    /// `validate_edges_typed`.
    pub fn input_ports(&self) -> Vec<Port> {
        match self {
            Self::Start { .. } => vec![],
            Self::End { terminal, .. } => vec![terminal.clone()],
            Self::AutomatedStep { input, .. } => vec![input.clone()],

            // Phase 4: derived inputs. Each control-flow block accepts a
            // single anonymous "in" port that's a Json pass-through — the
            // typed-edge check treats empty target fields as compatible with
            // anything, which matches the proposal §3.3 semantics for these
            // blocks ("they don't transform the token, they route or fan it").
            Self::HumanTask { .. }
            | Self::Decision { .. }
            | Self::ParallelSplit { .. }
            | Self::ParallelJoin { .. }
            | Self::Loop { .. }
            | Self::Scope { .. } => vec![Port::empty_input()],

            // Trigger nodes are never edge targets — the editor refuses to draw
            // an edge into a Trigger node. Return empty so any malformed graph
            // that does attempt it surfaces as `UnknownTargetPort` during
            // `validate_edges_typed`.
            Self::Trigger { .. } => vec![],
        }
    }

    /// Typed output ports declared or derived for this node.
    ///
    /// Derived ports (Phase 4):
    /// - `HumanTask` → single `out` port whose fields are the union of every
    ///   Input block's `TaskFieldConfig` across all steps, mapped via
    ///   `FieldKind::from(TaskFieldKind)`.
    /// - `Decision` → one port per branch (id = `BranchCondition.edge_id`,
    ///   label = branch label) plus a `default` port for the catch-all.
    ///   Phase 4 stub: each branch port has empty fields (pass-through), so
    ///   downstream type-checking flows through unchanged.
    /// - `ParallelSplit` / `ParallelJoin` / `Loop` → single `out` port,
    ///   empty fields (pass-through).
    /// - `Scope` → single `out` port, empty fields (pass-through). The
    ///   scope's *boundary* port editor lands separately.
    pub fn output_ports(&self) -> Vec<Port> {
        match self {
            Self::Start { initial, .. } => vec![initial.clone()],
            Self::AutomatedStep { output, .. } => vec![output.clone()],

            Self::HumanTask { steps, .. } => vec![derive_human_task_output_port(steps)],

            Self::Decision { conditions, default_branch, .. } => {
                let mut out: Vec<Port> = conditions
                    .iter()
                    .map(|c| Port {
                        id: c.edge_id.clone(),
                        label: c.label.clone(),
                        fields: vec![],
                    })
                    .collect();
                if let Some(default_id) = default_branch {
                    out.push(Port {
                        id: default_id.clone(),
                        label: "Default".to_string(),
                        fields: vec![],
                    });
                }
                out
            }

            Self::ParallelSplit { .. }
            | Self::ParallelJoin { .. }
            | Self::Loop { .. }
            | Self::Scope { .. } => vec![Port {
                id: "out".to_string(),
                label: "Output".to_string(),
                fields: vec![],
            }],

            // End has no output port — tokens terminate here.
            Self::End { .. } => vec![],

            // Trigger nodes "wear the shape" of whatever they target. The
            // resolved shape is computed at compile / fire time by
            // looking up the outgoing edge's target port; statically here we
            // emit an empty pass-through port. `validate_edges_typed` skips
            // type-checking when the source is a Trigger; payload-mapping
            // validation handles the field-level contract instead.
            Self::Trigger { .. } => vec![Port {
                id: "out".to_string(),
                label: "Output".to_string(),
                fields: vec![],
            }],
        }
    }
}

/// Derive a HumanTask's output port from the union of Input fields across all
/// task steps. Duplicate field names (first-wins) and non-input blocks are
/// silently ignored — the editor enforces uniqueness during authoring, and
/// the human-task form UI is the source of truth for behavior.
fn derive_human_task_output_port(steps: &[TaskStepConfig]) -> Port {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut fields: Vec<PortField> = Vec::new();
    for step in steps {
        for block in &step.blocks {
            if let TaskBlockConfig::Input { field } = block {
                if seen.insert(field.name.clone()) {
                    fields.push(PortField {
                        name: field.name.clone(),
                        label: field.label.clone(),
                        kind: FieldKind::from(field.kind),
                        required: field.required.unwrap_or(false),
                        options: field.options.clone(),
                        description: None,
                    });
                }
            }
        }
    }
    Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields,
    }
}

impl From<TaskFieldKind> for FieldKind {
    fn from(k: TaskFieldKind) -> Self {
        match k {
            TaskFieldKind::Text => FieldKind::Text,
            TaskFieldKind::Textarea => FieldKind::Textarea,
            TaskFieldKind::Number => FieldKind::Number,
            TaskFieldKind::Select => FieldKind::Select,
            // Checkbox → Bool (the typed-ports superset). HumanTask form UI
            // still renders a checkbox; the wire kind on the derived port is
            // a proper Bool so guards can use `step.flag == true`.
            TaskFieldKind::Checkbox => FieldKind::Bool,
            TaskFieldKind::File => FieldKind::File,
            TaskFieldKind::Signature => FieldKind::Signature,
        }
    }
}

// --- Task step configuration (maps to human-ui TaskStep) ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskStepConfig {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_mdsvex: Option<String>,
    pub blocks: Vec<TaskBlockConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type")]
pub enum TaskBlockConfig {
    #[serde(rename = "input")]
    Input { field: TaskFieldConfig },
    #[serde(rename = "mdsvex")]
    Mdsvex { content: String },
    #[serde(rename = "callout")]
    Callout {
        severity: CalloutSeverity,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        content: String,
    },
    #[serde(rename = "divider")]
    Divider,
    #[serde(rename = "image")]
    Image {
        filenames: Vec<String>,
        display: ImageDisplay,
    },
    #[serde(rename = "file")]
    File { filename: String },
    /// Embedded PDF viewer (rendered inline in the task UI). `height` is a
    /// CSS length string, default ~"400px"; `caption` is rendered above the
    /// viewer. Added so the editor's PDF blocks round-trip through publish.
    #[serde(rename = "pdf")]
    Pdf {
        filename: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<String>,
    },
}

/// Severity for callout blocks. Snake-case on the wire (`"info"`,
/// `"warning"`, `"error"`, `"success"`) to keep the byte-for-byte shape that
/// the editor and human-task UI already produce/consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CalloutSeverity {
    Info,
    Warning,
    Error,
    Success,
}

/// Layout mode for image blocks. Snake-case wire values: `"single"`,
/// `"grid"`, `"gallery"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageDisplay {
    Single,
    Grid,
    Gallery,
}

/// How a `ParallelJoin` merges the tokens arriving on its joined branches.
///
/// `ShallowLastWins` is the historical behaviour (top-level keys overwrite,
/// last branch to arrive wins on a key collision). `DeepMerge` recursively
/// merges nested object values instead of overwriting them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    #[default]
    ShallowLastWins,
    DeepMerge,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TaskFieldConfig {
    pub name: String,
    pub label: String,
    pub kind: TaskFieldKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

/// Form-field control kind for `input` task blocks. Snake-case wire values
/// such as `"text"`, `"textarea"`, `"number"`, `"select"`, `"checkbox"`,
/// `"file"`, `"signature"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskFieldKind {
    Text,
    Textarea,
    Number,
    Select,
    Checkbox,
    File,
    Signature,
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
}

impl FieldKind {
    /// Best-effort runtime check that a JSON value is acceptable for this kind.
    /// `Json` accepts anything. Used by `parameterize_air` to validate start
    /// tokens against the declared Start `initial` port.
    pub fn accepts(&self, value: &serde_json::Value) -> bool {
        match self {
            Self::Json => true,
            Self::Bool => value.is_boolean(),
            Self::Number => value.is_number(),
            Self::Text
            | Self::Textarea
            | Self::Select
            | Self::Signature
            | Self::Timestamp => value.is_string(),
            // File is a catalog reference (`file_metadata::StoragePath`); accept
            // any string or object, validation happens deeper.
            Self::File => value.is_string() || value.is_object(),
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
        Self { id: "in".to_string(), label: "Input".to_string(), fields: vec![] }
    }

    /// Validate a candidate token against this port's declared fields.
    ///
    /// Validation only — never coerces. `Json`/`File` kinds are permissive
    /// escape hatches (see [`FieldKind::accepts`]). A port with no `fields`
    /// accepts any object (pass-through ports). This is the *single* rule
    /// enforced for every token entering any port: a Start block's `initial`
    /// port (via `petri::instance::parameterize_air`) and in-flight signal
    /// ports (via the trigger dispatcher's signal path). Keeping one
    /// implementation guarantees the spawn and signal paths can't diverge.
    pub fn validate_token(&self, token: &serde_json::Value) -> Result<(), PortValidationError> {
        let obj = token
            .as_object()
            .ok_or(PortValidationError::NotObject)?;
        for field in &self.fields {
            match obj.get(&field.name) {
                None if field.required => {
                    return Err(PortValidationError::MissingRequiredField {
                        field: field.name.clone(),
                    });
                }
                None => {} // optional and absent — fine
                Some(v) if v.is_null() && field.required => {
                    return Err(PortValidationError::MissingRequiredField {
                        field: field.name.clone(),
                    });
                }
                Some(v) if v.is_null() => {} // optional null — fine
                Some(v) if !field.kind.accepts(v) => {
                    return Err(PortValidationError::FieldKindMismatch {
                        field: field.name.clone(),
                        kind: field.kind,
                    });
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

/// Why a token failed [`Port::validate_token`]. Context-free by design — the
/// caller adds the block / trigger identity (`parameterize_air` maps these into
/// its `ParameterizeError`; the dispatcher maps them into a dropped-fire
/// reason).
#[derive(Debug, thiserror::Error)]
pub enum PortValidationError {
    /// Token isn't a JSON object — every port is field-keyed.
    #[error("token must be a JSON object")]
    NotObject,
    /// A required field is absent (or explicitly null).
    #[error("field '{field}' is required but missing")]
    MissingRequiredField { field: String },
    /// A field is present but its JSON kind doesn't match the declared
    /// `FieldKind` (e.g. a string supplied for a `Number` field).
    #[error("field '{field}' has wrong type for kind {kind:?}")]
    FieldKindMismatch { field: String, kind: FieldKind },
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
/// reliably surfaces. Editor exposes "Reset to default" by re-deriving against
/// the current `backendType`.
pub fn default_output_port(backend: ExecutionBackendType) -> Port {
    let fields = match backend {
        ExecutionBackendType::Python => vec![port_field("result", "Result", FieldKind::Json)],
        ExecutionBackendType::Process => vec![
            port_field("stdout", "Stdout", FieldKind::Textarea),
            port_field("stderr", "Stderr", FieldKind::Textarea),
            port_field("exit_code", "Exit Code", FieldKind::Number),
        ],
        ExecutionBackendType::Docker => vec![
            port_field("stdout", "Stdout", FieldKind::Textarea),
            port_field("stderr", "Stderr", FieldKind::Textarea),
            port_field("exit_code", "Exit Code", FieldKind::Number),
            port_field("image", "Image", FieldKind::Text),
        ],
        ExecutionBackendType::Http => vec![
            port_field("status_code", "Status Code", FieldKind::Number),
            port_field("body", "Body", FieldKind::Json),
            port_field("headers", "Headers", FieldKind::Json),
        ],
        ExecutionBackendType::Llm => vec![
            port_field("text", "Text", FieldKind::Textarea),
            port_field("usage", "Usage", FieldKind::Json),
        ],
        ExecutionBackendType::FileOps => vec![port_field("files", "Files", FieldKind::Json)],
        ExecutionBackendType::Kreuzberg => vec![
            port_field("text", "Text", FieldKind::Textarea),
            port_field("metadata", "Metadata", FieldKind::Json),
        ],
    };
    Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields,
    }
}

fn port_field(name: &str, label: &str, kind: FieldKind) -> PortField {
    PortField {
        name: name.to_string(),
        label: label.to_string(),
        kind,
        required: false,
        options: None,
        description: None,
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

// --- Trigger nodes (Phase 5) ---

/// What event source fires a `Trigger` node. Tagged enum on the wire
/// (`{"kind": "cron", ...}`). Phase 5a only wires `Manual` into the dispatcher
/// end-to-end; the other variants are stored as data and surfaced through the
/// API for the editor to round-trip, but firing logic for each lands in 5b–5e.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerSource {
    Cron(CronTrigger),
    Catalog(CatalogTrigger),
    NetCompletion(NetCompletionTrigger),
    Webhook(WebhookTrigger),
    Manual(ManualTrigger),
}

impl TriggerSource {
    /// Discriminant string used for routing in the dispatcher and metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Cron(_) => "cron",
            Self::Catalog(_) => "catalog",
            Self::NetCompletion(_) => "net_completion",
            Self::Webhook(_) => "webhook",
            Self::Manual(_) => "manual",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CronTrigger {
    /// Standard cron expression (5- or 6-field). Validated at compile time.
    pub schedule: String,
    /// IANA timezone (e.g. `"Europe/Berlin"`). Defaults to `"UTC"` if absent.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Optional jitter window in seconds; the dispatcher fires within
    /// `[scheduled, scheduled + jitter_secs]` to spread load.
    #[serde(default)]
    pub jitter_secs: u32,
    /// What to do after a service restart with missed fire windows.
    #[serde(default)]
    pub catchup: CronCatchup,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum CronCatchup {
    /// Fire every missed window from the last-fire timestamp.
    FireMissed,
    /// Discard missed windows; only fire the next one.
    #[default]
    SkipMissed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogTrigger {
    /// Same filter shape as `CatalogueSubscription.filters`.
    #[serde(default)]
    pub filters: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    /// If true, the dispatcher walks existing catalogue entries matching the
    /// filters when the trigger is first registered.
    #[serde(default)]
    pub backfill: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetCompletionTrigger {
    /// Source template whose instance completion fires this trigger.
    pub source_template_id: Uuid,
    /// Specific version, or `None` for any published version.
    #[serde(default)]
    pub source_version: Option<i32>,
    /// Which terminal status counts as a fire.
    pub on: CompletionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompletionStatus {
    Success,
    Failure,
    Cancelled,
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebhookTrigger {
    /// Stable slug appended to `/api/triggers/webhook/{slug}`. Must be unique
    /// across published templates — the editor reserves it at publish.
    pub slug: String,
    pub auth: WebhookAuth,
    #[serde(default)]
    pub require_method: Option<HttpMethod>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WebhookAuth {
    /// No auth — endpoint is publicly fireable. Sane only for staging or
    /// trusted networks; the editor surfaces a warning.
    None,
    /// Compare a header (typically `Authorization` or `X-Webhook-Token`) to a
    /// static shared secret. Secret is stored encrypted at rest.
    SharedSecret {
        header: String,
        secret_ref: String,
    },
    /// HMAC-SHA256 signature over the request body, with the signing key
    /// stored encrypted at rest and the signature read from `header`.
    SignedHmac {
        header: String,
        secret_ref: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualTrigger {
    /// Form schema for the "Run with parameters" dialog. Reuses the same
    /// `TaskFieldConfig` shape as human-task forms.
    #[serde(default)]
    pub form: Vec<TaskFieldConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyPolicy {
    /// Every fire produces an event (default).
    #[default]
    Allow,
    /// At most one fire in flight; subsequent fires are dropped while running.
    Skip,
    /// Queue fires behind the active one; drained when it completes.
    Queue,
    /// Dedup by hashing the result of a Rhai `expression` over the event scope;
    /// fires whose key has been seen within `window_secs` are dropped.
    DedupKey { expression: String, window_secs: u32 },
}

/// A single field mapping for `Trigger.payload_mapping`. Each entry projects
/// an event scope into a typed token field via a Rhai expression. The compiler
/// validates that `target_field` exists in the resolved target port and that
/// `expression` parses.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldMapping {
    /// Name of the target port field this mapping fills.
    pub target_field: String,
    /// Rhai expression evaluated against the trigger source's event scope
    /// (`payload`, `fire_time`, etc. — varies by source kind).
    pub expression: String,
}

// --- Branch conditions ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BranchCondition {
    pub edge_id: String,
    pub label: String,
    pub guard: String,
}

// --- Execution spec configuration ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSpecConfig {
    pub backend_type: ExecutionBackendType,
    /// Filename of the entrypoint script within the node's staged files.
    /// Backends that don't run a user script (e.g. `http`) ignore this; the
    /// editor still surfaces it for python/process/docker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    pub config: serde_json::Value,
}

/// Discriminator selecting which executor backend handles an automated step.
/// Snake-case wire values: `"python"`, `"process"`, `"docker"`, `"http"`,
/// `"llm"`, `"file_ops"`, `"kreuzberg"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackendType {
    Python,
    Process,
    Docker,
    Http,
    Llm,
    FileOps,
    Kreuzberg,
}

impl ExecutionBackendType {
    /// Canonical snake_case wire string. Keep in lockstep with the
    /// `#[serde(rename_all = "snake_case")]` derive — these strings are what
    /// the executor and editor pass around at runtime.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Process => "process",
            Self::Docker => "docker",
            Self::Http => "http",
            Self::Llm => "llm",
            Self::FileOps => "file_ops",
            Self::Kreuzberg => "kreuzberg",
        }
    }
}

impl std::fmt::Display for ExecutionBackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

// --- Edge types ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_handle: Option<String>,
    /// Required at publish time (Phase 2 typed-ports). Stays optional in
    /// serde so pre-typed-ports edges still deserialize, but the compiler
    /// rejects `None` with `CompileError::MissingTargetHandle` so existing
    /// templates need an editor pass before they republish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(rename = "type")]
    pub edge_type: String,
}

// --- API request/response types ---

/// Request body for stateless compilation. Used by `POST /api/compile` and
/// `POST /api/templates/{id}/compile`. `files` is a per-node, per-filename map
/// of inline contents; the preview compile emits `InputSource::Raw` entries so
/// the AIR matches the StoragePath-keyed shape produced by publish.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CompileRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub graph: WorkflowGraph,
    #[serde(default)]
    pub files: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTemplateRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub graph: Option<WorkflowGraph>,
    /// Optional per-node file map (filename → inline contents). Files are
    /// seeded as Y.Text entries inside each node's `files` Y.Map so that the
    /// new template lands ready-to-publish for backends that require
    /// attached scripts (e.g. Python's entrypoint).
    #[serde(default)]
    pub files: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTemplateRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub graph: Option<WorkflowGraph>,
}

#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct ListTemplatesQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
    pub published: Option<bool>,
    pub search: Option<String>,
    pub base_template_id: Option<Uuid>,
}

fn default_page() -> i64 {
    1
}
fn default_per_page() -> i64 {
    20
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedResponse<T: ToSchema> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

impl WorkflowGraph {
    /// Create a default graph with just a Start and End node connected by an edge.
    pub fn default_graph() -> Self {
        Self {
            nodes: vec![
                WorkflowNode {
                    id: "start".to_string(),
                    node_type: "start".to_string(),
                    position: Position { x: 250.0, y: 100.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: Port::empty_input(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "end".to_string(),
                    node_type: "end".to_string(),
                    position: Position { x: 250.0, y: 300.0 },
                    data: WorkflowNodeData::End {
                        label: "End".to_string(),
                        description: None,
                        terminal: default_terminal_port(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
            ],
            edges: vec![WorkflowEdge {
                id: "edge_start_to_end".to_string(),
                source: "start".to_string(),
                target: "end".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            }],
            viewport: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pf(name: &str, kind: FieldKind, required: bool) -> PortField {
        PortField {
            name: name.to_string(),
            label: name.to_string(),
            kind,
            required,
            options: None,
            description: None,
        }
    }

    #[test]
    fn validate_token_accepts_well_typed_object() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![
                pf("name", FieldKind::Text, true),
                pf("count", FieldKind::Number, false),
                pf("blob", FieldKind::Json, false),
            ],
        };
        let ok = serde_json::json!({ "name": "a", "count": 3, "blob": [1, 2] });
        assert!(port.validate_token(&ok).is_ok());
        let ok2 = serde_json::json!({ "name": "a" });
        assert!(port.validate_token(&ok2).is_ok());
    }

    #[test]
    fn validate_token_rejects_missing_required_and_kind_mismatch() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![pf("name", FieldKind::Text, true), pf("n", FieldKind::Number, false)],
        };
        match port.validate_token(&serde_json::json!({ "n": 1 })) {
            Err(PortValidationError::MissingRequiredField { field }) => assert_eq!(field, "name"),
            other => panic!("expected MissingRequiredField, got {other:?}"),
        }
        match port.validate_token(&serde_json::json!({ "name": "a", "n": "5" })) {
            Err(PortValidationError::FieldKindMismatch { field, kind }) => {
                assert_eq!(field, "n");
                assert!(matches!(kind, FieldKind::Number));
            }
            other => panic!("expected FieldKindMismatch, got {other:?}"),
        }
        assert!(matches!(
            port.validate_token(&serde_json::json!([1, 2])),
            Err(PortValidationError::NotObject)
        ));
    }

    #[test]
    fn validate_token_fieldless_port_accepts_any_object() {
        let port = Port::empty_input();
        assert!(port.validate_token(&serde_json::json!({ "anything": 1 })).is_ok());
        assert!(port.validate_token(&serde_json::json!({})).is_ok());
        assert!(matches!(
            port.validate_token(&serde_json::json!("nope")),
            Err(PortValidationError::NotObject)
        ));
    }

    #[test]
    fn scope_node_data_roundtrip() {
        let data = WorkflowNodeData::Scope {
            label: "My Scope".to_string(),
            description: Some("A visual container".to_string()),
        };
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["type"], "scope");
        assert_eq!(json["label"], "My Scope");
        assert_eq!(json["description"], "A visual container");

        let back: WorkflowNodeData = serde_json::from_value(json).unwrap();
        assert_eq!(back.type_name(), "scope");
        assert_eq!(back.label(), "My Scope");
        assert_eq!(back.description(), Some("A visual container"));
    }

    #[test]
    fn workflow_node_with_parent_id_roundtrip() {
        let node = WorkflowNode {
            id: "child".to_string(),
            node_type: "human_task".to_string(),
            position: Position { x: 10.0, y: 20.0 },
            data: WorkflowNodeData::HumanTask {
                label: "Task".to_string(),
                description: None,
                task_title: "Do it".to_string(),
                instructions_mdsvex: None,
                steps: vec![],
            },
            parent_id: Some("scope1".to_string()),
            width: None,
            height: None,
        };
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["parentId"], "scope1");

        let back: WorkflowNode = serde_json::from_value(json).unwrap();
        assert_eq!(back.parent_id, Some("scope1".to_string()));
    }

    #[test]
    fn scope_node_with_dimensions_roundtrip() {
        let node = WorkflowNode {
            id: "s1".to_string(),
            node_type: "scope".to_string(),
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Scope {
                label: "Container".to_string(),
                description: None,
            },
            parent_id: None,
            width: Some(500.0),
            height: Some(300.0),
        };
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["width"], 500.0);
        assert_eq!(json["height"], 300.0);
        assert!(json.get("parentId").is_none());

        let back: WorkflowNode = serde_json::from_value(json).unwrap();
        assert_eq!(back.width, Some(500.0));
        assert_eq!(back.height, Some(300.0));
        assert_eq!(back.parent_id, None);
    }

    #[test]
    fn parent_id_omitted_when_none() {
        let node = WorkflowNode {
            id: "n".to_string(),
            node_type: "end".to_string(),
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::End {
                label: "End".to_string(),
                description: None,
                terminal: default_terminal_port(),
            },
            parent_id: None,
            width: None,
            height: None,
        };
        let json = serde_json::to_string(&node).unwrap();
        assert!(!json.contains("parentId"), "parentId should be omitted when None");
        assert!(!json.contains("width"), "width should be omitted when None");
        assert!(!json.contains("height"), "height should be omitted when None");
    }

    #[test]
    fn image_block_roundtrip() {
        let block = TaskBlockConfig::Image {
            filenames: vec!["photo.png".to_string(), "diagram.svg".to_string()],
            display: ImageDisplay::Grid,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["filenames"][0], "photo.png");
        assert_eq!(json["filenames"][1], "diagram.svg");
        assert_eq!(json["display"], "grid");

        let back: TaskBlockConfig = serde_json::from_value(json).unwrap();
        if let TaskBlockConfig::Image { filenames, display } = back {
            assert_eq!(filenames.len(), 2);
            assert_eq!(display, ImageDisplay::Grid);
        } else {
            panic!("expected Image variant");
        }
    }

    /// Canary: each newly-enumified field must serialize to the same wire
    /// strings it produced when typed as `String`. If this fails, the JSON in
    /// the database / on the network has diverged from the Rust type.
    #[test]
    fn enumified_fields_preserve_wire_format() {
        // Callout severity
        let json = serde_json::json!({
            "type": "callout",
            "severity": "warning",
            "content": "hi",
        });
        let block: TaskBlockConfig = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&block).unwrap(), json);

        // Image display + nested field kind for full coverage
        let json = serde_json::json!({
            "type": "image",
            "filenames": ["a.png"],
            "display": "gallery",
        });
        let block: TaskBlockConfig = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&block).unwrap(), json);

        // TaskFieldKind via the input variant
        let json = serde_json::json!({
            "type": "input",
            "field": {
                "name": "n",
                "label": "L",
                "kind": "textarea",
            },
        });
        let block: TaskBlockConfig = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&block).unwrap(), json);

        // ExecutionBackendType snake_case rename (file_ops is the canary —
        // PascalCase would emit `"fileOps"` and break the wire).
        let spec = ExecutionSpecConfig {
            backend_type: ExecutionBackendType::FileOps,
            entrypoint: None,
            config: serde_json::json!({}),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["backendType"], "file_ops");
        let back: ExecutionSpecConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.backend_type, ExecutionBackendType::FileOps);
    }

    #[test]
    fn file_block_roundtrip() {
        let block = TaskBlockConfig::File {
            filename: "report.pdf".to_string(),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "file");
        assert_eq!(json["filename"], "report.pdf");

        let back: TaskBlockConfig = serde_json::from_value(json).unwrap();
        if let TaskBlockConfig::File { filename } = back {
            assert_eq!(filename, "report.pdf");
        } else {
            panic!("expected File variant");
        }
    }

    #[test]
    fn all_block_types_deserialize() {
        // Verify all block types roundtrip through JSON
        let blocks = vec![
            serde_json::json!({"type": "input", "field": {"name": "f", "label": "F", "kind": "text"}}),
            serde_json::json!({"type": "mdsvex", "content": "# Hello"}),
            serde_json::json!({"type": "callout", "severity": "warning", "content": "Watch out"}),
            serde_json::json!({"type": "divider"}),
            serde_json::json!({"type": "image", "filenames": ["a.png"], "display": "single"}),
            serde_json::json!({"type": "file", "filename": "data.csv"}),
        ];
        for (i, json) in blocks.iter().enumerate() {
            let result: Result<TaskBlockConfig, _> = serde_json::from_value(json.clone());
            assert!(result.is_ok(), "block type {} failed to deserialize: {:?}", i, result.err());
        }
    }
}
