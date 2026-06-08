use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Types of form fields for human tasks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskFieldKind {
    Text,
    Textarea,
    Number,
    Select,
    Checkbox,
    File,
    Signature,
    Radio,
    Date,
    Range,
    Rating,
}

/// One choice in a `kind = "select"` (or `radio`) field. `value` is what
/// the form submits / what downstream guards compare against; `label` is
/// what the UI renders. Authors typically write `{value, label}`; the
/// deserializer on `TaskField::options` also accepts a bare string and
/// stretches it to `{value: s, label: s}`. Mirror of the same type in
/// `service/src/models/template.rs` — the engine's HumanTask effect
/// handler validates the wire payload against `TaskField`, so the shape
/// must match what the service compiles into AIR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

/// Definition of a single field in a human task form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct TaskField {
    pub name: String,
    pub label: String,
    pub kind: TaskFieldKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_mdsvex: Option<String>,
    /// Choice list for `kind = "select"` / `"radio"`. Authored as
    /// `[{"value": "approve", "label": "Approve"}, …]`; the deserializer
    /// also accepts bare string shorthand (`["approve", "reject"]`) and
    /// normalizes each entry to `{value, label}` where `label = value`.
    /// Keeps a uniform runtime representation regardless of which shape
    /// the author chose.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_task_field_options"
    )]
    pub options: Option<Vec<SelectOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pen_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rating: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_time: Option<bool>,
}

impl TaskField {
    fn new(name: impl Into<String>, label: impl Into<String>, kind: TaskFieldKind) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            kind,
            required: None,
            placeholder: None,
            description_mdsvex: None,
            options: None,
            accept: None,
            max_file_size: None,
            max_files: None,
            signature_mode: None,
            pen_color: None,
            min: None,
            max: None,
            step: None,
            max_rating: None,
            include_time: None,
        }
    }
    pub fn text(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::Text)
    }
    pub fn textarea(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::Textarea)
    }
    pub fn number(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::Number)
    }
    pub fn select(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::Select)
    }
    pub fn checkbox(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::Checkbox)
    }
    pub fn file(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::File)
    }
    pub fn signature(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::Signature)
    }
    pub fn radio(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::Radio)
    }
    pub fn date(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::Date)
    }
    pub fn range(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::Range)
    }
    pub fn rating(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self::new(name, label, TaskFieldKind::Rating)
    }
    pub fn required(mut self) -> Self {
        self.required = Some(true);
        self
    }
    pub fn placeholder(mut self, p: impl Into<String>) -> Self {
        self.placeholder = Some(p.into());
        self
    }
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description_mdsvex = Some(d.into());
        self
    }
    /// Bare-string shorthand: each entry becomes `{value: s, label: s}`.
    /// For distinct `value` / `label` pairs, set `self.options` directly
    /// or use the deserializer's rich shape.
    pub fn options(mut self, opts: &[&str]) -> Self {
        self.options = Some(
            opts.iter()
                .map(|s| SelectOption {
                    value: (*s).to_string(),
                    label: (*s).to_string(),
                })
                .collect(),
        );
        self
    }
    pub fn accept(mut self, a: impl Into<String>) -> Self {
        self.accept = Some(a.into());
        self
    }
    pub fn max_file_size(mut self, s: u64) -> Self {
        self.max_file_size = Some(s);
        self
    }
    pub fn max_files(mut self, n: u32) -> Self {
        self.max_files = Some(n);
        self
    }
    pub fn signature_mode(mut self, m: impl Into<String>) -> Self {
        self.signature_mode = Some(m.into());
        self
    }
    pub fn pen_color(mut self, c: impl Into<String>) -> Self {
        self.pen_color = Some(c.into());
        self
    }
    pub fn min(mut self, v: f64) -> Self {
        self.min = Some(v);
        self
    }
    pub fn max(mut self, v: f64) -> Self {
        self.max = Some(v);
        self
    }
    pub fn step(mut self, v: f64) -> Self {
        self.step = Some(v);
        self
    }
    pub fn max_rating(mut self, n: u32) -> Self {
        self.max_rating = Some(n);
        self
    }
    pub fn include_time(mut self) -> Self {
        self.include_time = Some(true);
        self
    }
}

/// Hand-rolled deserializer for `TaskField::options`. Accepts two authoring
/// shapes and normalizes to `Vec<SelectOption>`:
///
///   - `["approve", "reject"]` — bare string shorthand for the common case
///     where the canonical value doubles as the human-facing label.
///     Stretched to `{value: "approve", label: "approve"}` etc.
///   - `[{"value": "approve", "label": "Approve as-extracted"}, …]` — full
///     rich shape; `label` is optional and defaults to `value`.
///
/// Any other shape (numbers, bools, mixed arrays, objects without a string
/// `value`) is rejected with an actionable error that names the offending
/// index — better than serde's default "invalid type" surface, which is
/// what tripped the `Effect fatal error: invalid type: map, expected a
/// string` failure on the doc-pipeline-v1 review HumanTask before this
/// migration. Mirror of the same fn in
/// `service/src/models/template.rs::deserialize_task_field_options` —
/// both sides must accept the same shapes for round-tripping to close.
fn deserialize_task_field_options<'de, D>(de: D) -> Result<Option<Vec<SelectOption>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = Option::<serde_json::Value>::deserialize(de)?;
    let Some(value) = value else {
        return Ok(None);
    };
    let arr = value.as_array().ok_or_else(|| {
        D::Error::custom(
            "task field `options` must be a list (either of strings or of `{value,label}` objects)",
        )
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        match item {
            serde_json::Value::String(s) => out.push(SelectOption {
                value: s.clone(),
                label: s.clone(),
            }),
            serde_json::Value::Object(map) => {
                let value = map
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        D::Error::custom(format!(
                            "task field `options[{i}]` is an object but missing a string `value` key"
                        ))
                    })?
                    .to_string();
                let label = map
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| value.clone());
                out.push(SelectOption { value, label });
            }
            other => {
                return Err(D::Error::custom(format!(
                    "task field `options[{i}]` must be a string or `{{value,label}}` object; got {}",
                    match other {
                        serde_json::Value::Null => "null",
                        serde_json::Value::Bool(_) => "a boolean",
                        serde_json::Value::Number(_) => "a number",
                        serde_json::Value::Array(_) => "a list",
                        _ => "an unsupported value",
                    }
                )));
            }
        }
    }
    Ok(Some(out))
}

/// Severity level for callout blocks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CalloutSeverity {
    Info,
    Warning,
    Error,
    Success,
}

/// Column alignment for table blocks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TableAlignment {
    Left,
    Center,
    Right,
}

/// A downloadable file item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct DownloadItem {
    pub url: String,
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A render block inside a task step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskBlock {
    Input {
        field: TaskField,
    },
    Mdsvex {
        content: String,
    },
    Download {
        downloads: Vec<DownloadItem>,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alignments: Option<Vec<TableAlignment>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    Image {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    Callout {
        severity: CalloutSeverity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        content: String,
    },
    Pdf {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<String>,
    },
    Chart {
        chart_type: ChartType,
        data: Vec<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series: Option<Vec<ChartSeries>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y_label: Option<String>,
    },
    /// Feature B — array-iteration sub-task body. Renders one copy of
    /// `blocks` per element of the upstream array referenced by
    /// `items_ref` (which carries exactly one `[*]` boundary, e.g.
    /// `extract.tasks[*]`). Inner `blocks` are any TaskBlock variant
    /// except a nested Repeater — Input children declare the per-row
    /// form schema, display children render with placeholders resolved
    /// per row. The engine treats this as a pass-through — the
    /// structural shape is statically declared at compile time, the
    /// *count* of rows comes from runtime data the compiler stages
    /// into `HumanTaskRequest.payload`. Renderer-only; no engine-side
    /// semantics beyond accepting and forwarding it.
    Repeater {
        items_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_label_ref: Option<String>,
        blocks: Vec<TaskBlock>,
        output_slug: String,
    },
    Divider,
}

/// Chart type for data visualization blocks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChartType {
    Area,
    Bar,
    Line,
    Pie,
}

/// A series definition for chart rendering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct ChartSeries {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl TaskBlock {
    pub fn mdsvex(content: impl Into<String>) -> Self {
        Self::Mdsvex {
            content: content.into(),
        }
    }
    pub fn input(field: TaskField) -> Self {
        Self::Input { field }
    }
}

/// One step in a multi-step human task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct TaskStep {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_mdsvex: Option<String>,
    pub blocks: Vec<TaskBlock>,
}

impl TaskStep {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description_mdsvex: None,
            blocks: vec![],
        }
    }
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description_mdsvex = Some(d.into());
        self
    }
    pub fn block(mut self, block: TaskBlock) -> Self {
        self.blocks.push(block);
        self
    }
    pub fn input(mut self, field: TaskField) -> Self {
        self.blocks.push(TaskBlock::Input { field });
        self
    }
    pub fn mdsvex(mut self, content: impl Into<String>) -> Self {
        self.blocks.push(TaskBlock::Mdsvex {
            content: content.into(),
        });
        self
    }
    pub fn divider(mut self) -> Self {
        self.blocks.push(TaskBlock::Divider);
        self
    }
}

/// Request to create a new human task.
/// Published to `petri.human.request.{net_id}.{place}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema, JsonSchema)]
pub struct HumanTaskRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Opt-in caller-supplied task identity. Honored ONLY by the human_task
    /// effect handler when `Some` (the pooled human-task lowering sets it to
    /// the capacity grant_id so `hpi_tasks.id == task_id == grant_id`); when
    /// `None` the handler mints a UUID as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_id: Option<String>,
    /// Organization ID for routing this task to the correct HPI org.
    /// Set from token data (dynamic) or engine config fallback (HUMAN_ORG_ID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions_mdsvex: Option<String>,
    pub steps: Vec<TaskStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// NATS subject where the UI should publish outcome signals (ExternalSignal envelope).
    /// Set by the engine before publishing to `human.request.*`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_subject: Option<String>,
    /// Links this task to a process (set automatically from read-arc process token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    /// Identifies which step in the process this task represents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_step: Option<String>,
}

/// Completion signal from a human task.
/// Received on `human.completed.{net_id}.{place}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HumanTaskCompletion {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr_id: Option<String>,
    pub data: serde_json::Value,
    pub completed_at: chrono::DateTime<chrono::Utc>,
}

/// Cancellation request for a human task.
/// Published to `human.cancel.{net_id}.{place}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HumanTaskCancellation {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub cancelled_at: chrono::DateTime<chrono::Utc>,
}

/// Failure signal from a human task (user rejection).
/// Received on `human.failed.{net_id}.{place}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HumanTaskFailure {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub failed_at: chrono::DateTime<chrono::Utc>,
}

/// Trait for clients that can manage human tasks (usually via NATS).
#[async_trait::async_trait]
pub trait HumanTaskClient: Send + Sync + std::fmt::Debug {
    /// Submit a new human task request.
    async fn submit_task(&self, request: HumanTaskRequest) -> Result<String, String>;

    /// Cancel a human task.
    async fn cancel_task(
        &self,
        task_id: &str,
        place: &str,
        reason: Option<&str>,
    ) -> Result<(), String>;

    /// Get the client name for logging.
    fn name(&self) -> &str;

    /// Get the net_id this client is scoped to.
    fn net_id(&self) -> &str;

    /// Get the org_id this client routes tasks to (if configured).
    fn org_id(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rich_field() -> serde_json::Value {
        json!({
            "name": "decision",
            "label": "Decision",
            "kind": "select",
            "options": [
                {"value": "approve", "label": "Approve as-extracted"},
                {"value": "edit", "label": "Approve with edits"}
            ]
        })
    }

    fn bare_field() -> serde_json::Value {
        json!({
            "name": "decision",
            "label": "Decision",
            "kind": "select",
            "options": ["approve", "reject"]
        })
    }

    #[test]
    fn rich_options_shape_preserves_value_and_label() {
        let field: TaskField = serde_json::from_value(rich_field()).expect("parse rich");
        let opts = field.options.expect("options present");
        assert_eq!(opts.len(), 2);
        assert_eq!(
            opts[0],
            SelectOption {
                value: "approve".into(),
                label: "Approve as-extracted".into(),
            }
        );
        assert_eq!(opts[1].value, "edit");
        assert_eq!(opts[1].label, "Approve with edits");
    }

    #[test]
    fn bare_string_options_normalize_to_value_equals_label() {
        let field: TaskField = serde_json::from_value(bare_field()).expect("parse bare");
        let opts = field.options.expect("options present");
        assert_eq!(
            opts,
            vec![
                SelectOption {
                    value: "approve".into(),
                    label: "approve".into()
                },
                SelectOption {
                    value: "reject".into(),
                    label: "reject".into()
                },
            ]
        );
    }

    #[test]
    fn rich_options_round_trip_through_json() {
        let original = SelectOption {
            value: "approve".into(),
            label: "Approve as-extracted".into(),
        };
        let field = TaskField {
            name: "decision".into(),
            label: "Decision".into(),
            kind: TaskFieldKind::Select,
            required: None,
            placeholder: None,
            description_mdsvex: None,
            options: Some(vec![original.clone()]),
            accept: None,
            max_file_size: None,
            max_files: None,
            signature_mode: None,
            pen_color: None,
            min: None,
            max: None,
            step: None,
            max_rating: None,
            include_time: None,
        };
        let wire = serde_json::to_value(&field).expect("serialize");
        let back: TaskField = serde_json::from_value(wire).expect("deserialize");
        assert_eq!(back.options, Some(vec![original]));
    }

    #[test]
    fn options_object_missing_value_is_rejected_with_index() {
        let raw = json!({
            "name": "x", "label": "X", "kind": "select",
            "options": [{"label": "no value here"}]
        });
        let err = serde_json::from_value::<TaskField>(raw).expect_err("must fail");
        let msg = err.to_string();
        assert!(msg.contains("options[0]"), "error names index: {msg}");
        assert!(
            msg.contains("value"),
            "error mentions missing `value` key: {msg}"
        );
    }

    #[test]
    fn options_with_non_string_item_is_rejected_with_index() {
        let raw = json!({
            "name": "x", "label": "X", "kind": "select",
            "options": ["ok", 42, "also_ok"]
        });
        let err = serde_json::from_value::<TaskField>(raw).expect_err("must fail");
        let msg = err.to_string();
        assert!(msg.contains("options[1]"), "error names index: {msg}");
        assert!(msg.contains("number"), "error names value type: {msg}");
    }

    #[test]
    fn options_object_only_value_defaults_label_to_value() {
        let raw = json!({
            "name": "x", "label": "X", "kind": "select",
            "options": [{"value": "approve"}]
        });
        let field: TaskField = serde_json::from_value(raw).expect("parse");
        let opts = field.options.expect("options present");
        assert_eq!(opts[0].value, "approve");
        assert_eq!(opts[0].label, "approve");
    }

    #[test]
    fn options_absent_yields_none() {
        let raw = json!({"name": "x", "label": "X", "kind": "text"});
        let field: TaskField = serde_json::from_value(raw).expect("parse");
        assert!(field.options.is_none());
    }

    // ── Feature B: Repeater block variant ─────────────────────────────
    //
    // The Repeater block is the renderer-side leg of array iteration —
    // mekhan-service's compiler emits it inside `HumanTaskRequest.steps`
    // and the engine's effect handler must accept + forward it as-is.
    // These tests pin the wire shape so a regression in either side's
    // serde tagging or field naming fails here, not at runtime with
    // "unknown variant `repeater`" surfaced from a wedged net.

    /// The exact JSON the compiler emits for a well-formed Repeater
    /// (taken from a live invoice-processing demo run) must round-trip
    /// through `TaskBlock` without dropping or renaming fields. The
    /// fixture mirrors the snake_case wire format the compiler emits.
    #[test]
    fn repeater_block_round_trips_through_serde() {
        let raw = json!({
            "type": "repeater",
            "items_ref": "extract.line_items[*]",
            "item_label_ref": "extract.line_items[*].description",
            "output_slug": "line_approvals",
            "blocks": [
                {"type": "input", "field": {"name": "approved", "label": "Approved", "kind": "checkbox", "required": true}},
                {"type": "input", "field": {"name": "notes", "label": "Notes", "kind": "textarea", "required": false}}
            ]
        });
        let block: TaskBlock = serde_json::from_value(raw.clone()).expect("parse repeater block");
        match &block {
            TaskBlock::Repeater {
                items_ref,
                item_label_ref,
                blocks,
                output_slug,
            } => {
                assert_eq!(items_ref, "extract.line_items[*]");
                assert_eq!(
                    item_label_ref.as_deref(),
                    Some("extract.line_items[*].description"),
                );
                assert_eq!(output_slug, "line_approvals");
                assert_eq!(blocks.len(), 2);
                let TaskBlock::Input { field: f0 } = &blocks[0] else {
                    panic!("expected Input child[0], got {:?}", blocks[0]);
                };
                assert_eq!(f0.name, "approved");
                assert_eq!(f0.kind, TaskFieldKind::Checkbox);
                let TaskBlock::Input { field: f1 } = &blocks[1] else {
                    panic!("expected Input child[1], got {:?}", blocks[1]);
                };
                assert_eq!(f1.name, "notes");
                assert_eq!(f1.kind, TaskFieldKind::Textarea);
            }
            other => panic!("expected TaskBlock::Repeater, got {other:?}"),
        }
        // Round-trip: re-serialize and confirm the wire shape is stable.
        let back = serde_json::to_value(&block).expect("serialize");
        assert_eq!(back["type"], "repeater");
        assert_eq!(back["items_ref"], "extract.line_items[*]");
        assert_eq!(back["item_label_ref"], "extract.line_items[*].description");
        assert_eq!(back["output_slug"], "line_approvals");
    }

    /// `item_label_ref` is the only optional Repeater field — the
    /// renderer falls back to "Item N" when absent. The compiler skips
    /// the key entirely when not set; deserialization must accept that.
    #[test]
    fn repeater_block_without_item_label_ref_parses() {
        let raw = json!({
            "type": "repeater",
            "items_ref": "extract.line_items[*]",
            "output_slug": "line_approvals",
            "blocks": []
        });
        let block: TaskBlock = serde_json::from_value(raw).expect("parse minimal repeater");
        match block {
            TaskBlock::Repeater {
                item_label_ref,
                blocks,
                ..
            } => {
                assert!(
                    item_label_ref.is_none(),
                    "item_label_ref must be None when omitted"
                );
                assert!(blocks.is_empty());
            }
            other => panic!("expected TaskBlock::Repeater, got {other:?}"),
        }
    }

    /// Repeater is just one variant in `TaskBlock` — the dispatch tag
    /// must still discriminate cleanly between Repeater and the other
    /// block types. Guards against an accidental flat-variant rename
    /// (e.g. dropping `#[serde(tag = "type", rename_all = "snake_case")]`)
    /// that would let `{"type": "repeater"}` match the wrong arm.
    #[test]
    fn repeater_block_tag_does_not_collide_with_other_variants() {
        // Each block type must dispatch to its own variant under the
        // shared `#[serde(tag = "type", rename_all = "snake_case")]`.
        let cases = [
            (
                json!({"type": "divider"}),
                matches!(
                    serde_json::from_value::<TaskBlock>(json!({"type": "divider"})).unwrap(),
                    TaskBlock::Divider
                ),
            ),
            (
                json!({"type": "mdsvex", "content": "hello"}),
                matches!(
                    serde_json::from_value::<TaskBlock>(
                        json!({"type": "mdsvex", "content": "hello"})
                    )
                    .unwrap(),
                    TaskBlock::Mdsvex { .. }
                ),
            ),
            (
                json!({
                    "type": "repeater",
                    "items_ref": "x.y[*]",
                    "output_slug": "z",
                    "blocks": []
                }),
                matches!(
                    serde_json::from_value::<TaskBlock>(json!({
                        "type": "repeater",
                        "items_ref": "x.y[*]",
                        "output_slug": "z",
                        "blocks": []
                    }))
                    .unwrap(),
                    TaskBlock::Repeater { .. }
                ),
            ),
        ];
        for (raw, ok) in cases {
            assert!(ok, "variant tag dispatch failed for {raw}");
        }
    }
}
