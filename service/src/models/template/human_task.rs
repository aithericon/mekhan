//! Human-task form configuration: steps, blocks, field kinds and the
//! machinery that derives a HumanTask's typed output port from them.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::ports::{FieldKind, Port, PortField};

/// Derive a HumanTask's output port from the union of Input fields across all
/// task steps. Duplicate field names (first-wins) and non-input blocks are
/// silently ignored — the editor enforces uniqueness during authoring, and
/// the human-task form UI is the source of truth for behavior.
pub(crate) fn derive_human_task_output_port(steps: &[TaskStepConfig]) -> Port {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut fields: Vec<PortField> = Vec::new();
    for step in steps {
        for block in &step.blocks {
            if let TaskBlockConfig::Input { field } = block {
                if seen.insert(field.name.clone()) {
                    fields.push(PortField {
                        default: None,
                        schema: None,
                        name: field.name.clone(),
                        label: field.label.clone(),
                        kind: FieldKind::from(field.kind),
                        required: field.required.unwrap_or(false),
                        options: field.options.clone(),
                        description: None,
                        accept: None,
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

/// Single-source JSON Schema for a dynamic-form step list — `Vec<TaskStepConfig>`,
/// the runtime shape an upstream producer (agent or script) must emit to drive a
/// HumanTask whose `stepsRef` points at it.
///
/// This ONE artifact is reused so the two consumers can't drift:
///   1. **Advertised** to an agent — surfaced on the HumanTask input port (via
///      `PortField::schema`) and thence into the agent tool's `input_schema`, so
///      the model is told the exact block grammar to produce.
///   2. **Enforced** at runtime — emitted as a colored-token `definition` bound
///      to the place the produced blocks land in, so the engine's
///      `SchemaRegistry` validates them like any other typed token.
///
/// Generated from the `#[derive(schemars::JsonSchema)]` on `TaskStepConfig` /
/// `TaskBlockConfig` / `TaskFieldConfig` and friends — the Rust types are the
/// single source of truth, mirroring the `ResourceType` schema precedent in
/// `shared/resources`.
pub fn task_step_list_json_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(Vec<TaskStepConfig>);
    serde_json::to_value(schema).expect("TaskStepConfig JsonSchema must serialize cleanly")
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
            // Radio is a Select with inline option rendering — wire kind is
            // identical so downstream borrow-checking treats them the same.
            TaskFieldKind::Radio => FieldKind::Select,
            // Date is an ISO 8601 string on the wire; reuse Text so guards
            // can do lexicographic comparison (`step.due < "2026-01-01"`).
            // A dedicated `FieldKind::Date` could come later if we want
            // typed-date guard helpers; for now Text-with-format is enough.
            TaskFieldKind::Date => FieldKind::Text,
            // Range / Rating both emit numbers; min/max/step/max_rating are
            // renderer hints, not wire-shape constraints.
            TaskFieldKind::Range => FieldKind::Number,
            TaskFieldKind::Rating => FieldKind::Number,
        }
    }
}

// --- Task step configuration (maps to human-ui TaskStep) ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskStepConfig {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_mdsvex: Option<String>,
    pub blocks: Vec<TaskBlockConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
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
    /// `filenames` references compile-time staged assets; `url` is a direct
    /// (often `{{ <slug>.<field> }}`-interpolated) source resolved at instance
    /// time. When `url` is set the human-task UI renders it as the image
    /// source (matching the frontend `{type:"image",url,alt?,caption?}`).
    #[serde(rename = "image")]
    Image {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        filenames: Vec<String>,
        #[serde(default)]
        display: ImageDisplay,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    #[serde(rename = "file")]
    File { filename: String },
    /// Embedded PDF viewer (rendered inline in the task UI). `height` is a
    /// CSS length string, default ~"400px"; `caption` is rendered above the
    /// viewer. `url`, when set (typically via `{{ <slug>.<field> }}`
    /// interpolation), is the direct PDF source.
    #[serde(rename = "pdf")]
    Pdf {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<String>,
    },
    /// Downloadable artifact list. Serializes to the frontend
    /// `{type:"download",downloads:[{url,filename,...}]}` shape. `url` is
    /// typically `{{ <slug>.<field> }}`-interpolated to an uploaded file.
    #[serde(rename = "download")]
    Download { downloads: Vec<DownloadItemConfig> },
    /// Sortable data table for rich report display. Serializes to the
    /// frontend `{type:"table",headers,rows,rows_ref?,alignments?,caption?}`
    /// hpi block (`app/src/lib/hpi/types.ts`).
    ///
    /// `rows` is a static `string[][]` literal — each cell may carry
    /// `{{ <slug>.<field> }}` text interpolation like any other authored
    /// string. `rows_ref`, when set, is a producer-namespaced
    /// `<slug>.<field>[.<more>…]` whole-array reference (same grammar as
    /// `steps_ref` — no `[*]`): the compiler synthesizes a read-arc on the
    /// upstream parked place and stages the resolved array into the task
    /// payload on the same Feature-B rails as `Repeater.items_ref`; the
    /// task UI resolves it against `task.payload` at render time and it
    /// wins over `rows`. A malformed or unresolvable ref silently degrades
    /// to the static `rows` (an empty table when none are authored) —
    /// mirroring the Repeater's render-side degrade, not the hard
    /// `steps_ref` validator.
    #[serde(rename = "table")]
    Table {
        headers: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rows: Vec<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alignments: Option<Vec<TableAlignment>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    /// Feature B — render N copies of a sub-task body, one per element of an
    /// upstream array. `items_ref` is a Feature-B `<slug>.<field>[*]`
    /// reference; the compiler synthesizes a read-arc on the parked array
    /// and the frontend renderer iterates `task.data[<items_ref>]`,
    /// instantiating the sub-`blocks` per element. The block's typed
    /// output is `<output_slug>.results: array<{<inputs>}>`, where
    /// `<inputs>` is the union of every `Input` child block's field —
    /// visible to downstream pickers via the standard `TyDescriptor::Array`
    /// machinery. Non-Input children (Mdsvex, Callout, Divider, Image,
    /// Pdf, File, Download) are render-only and contribute nothing to the
    /// per-row schema.
    ///
    /// `item_label_ref`, when set, names a `<slug>.<field>[*].<label>`
    /// ref whose per-element string is used as the row header (e.g. the
    /// task title from an LLM-extracted task list). Static-only: B v1
    /// rejects `[*]` chained twice (`NestedIterationUnsupported`) and
    /// rejects a Repeater nested inside another Repeater.
    #[serde(rename = "repeater")]
    Repeater {
        /// Producer-namespaced ref carrying exactly one `[*]` boundary,
        /// e.g. `extract.tasks[*]`. The pre-`[*]` segments address an
        /// upstream parked array; iteration happens consumer-side.
        items_ref: String,
        /// Optional per-element row label ref, e.g.
        /// `extract.tasks[*].title`. Must share the same iteration prefix
        /// as `items_ref`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_label_ref: Option<String>,
        /// The sub-task body rendered per element. Any TaskBlockConfig
        /// variant except a nested Repeater. `Input` children declare the
        /// per-row form schema and contribute to `<output_slug>.results`
        /// element shape; display blocks (Mdsvex/Callout/Image/Pdf/File/
        /// Download/Divider) are rendered per row with placeholders
        /// resolved against the current row's element.
        ///
        /// `no_recursion` breaks the recursive schema cycle for
        /// utoipa — the wire schema still references `TaskBlockConfig`
        /// via `$ref`, but the generator stops descending here.
        #[schema(no_recursion)]
        blocks: Vec<TaskBlockConfig>,
        /// Rhai-safe slug under which the Repeater's typed output is
        /// addressable downstream as `<output_slug>.results`. Defaults to
        /// the parent HumanTask's slug when empty; must be unique within
        /// the graph (the compiler's existing slug-collision check
        /// covers it).
        output_slug: String,
    },
}

/// One entry in a `download` task block. Mirrors the frontend `DownloadItem`
/// (`app/src/lib/hpi/types.ts`) field-for-field on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct DownloadItemConfig {
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

/// Severity for callout blocks. Snake-case on the wire (`"info"`,
/// `"warning"`, `"error"`, `"success"`) to keep the byte-for-byte shape that
/// the editor and human-task UI already produce/consume.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CalloutSeverity {
    Info,
    Warning,
    Error,
    Success,
}

/// Per-column text alignment for table blocks. Snake-case wire values
/// (`"left"`, `"center"`, `"right"`) matching the frontend hpi
/// `alignments?: ('left' | 'center' | 'right')[]` contract.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TableAlignment {
    Left,
    Center,
    Right,
}

/// Layout mode for image blocks. Snake-case wire values: `"single"`,
/// `"grid"`, `"gallery"`.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ToSchema,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ImageDisplay {
    #[default]
    Single,
    Grid,
    Gallery,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct TaskFieldConfig {
    pub name: String,
    pub label: String,
    pub kind: TaskFieldKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Per-field helper text shown under the input. Mdsvex source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_mdsvex: Option<String>,
    /// Choice list for `kind = "select"` / `"radio"`. Authored as
    /// `[{"value": "approve", "label": "Approve"}, …]` — `value` is the
    /// canonical wire value submitted by the form, `label` is the
    /// human-facing display string. A bare string shorthand
    /// (`["approve", "reject"]`) is accepted at deserialize time and
    /// normalized to `{value, label}` where `label = value` — convenient
    /// for trivial sets while keeping the runtime representation uniform.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_task_field_options"
    )]
    pub options: Option<Vec<SelectOption>>,
    /// For `File` kind: accepted file types as an HTML input `accept`
    /// attribute (e.g. `"image/png,image/jpeg,.pdf"`). Ignored for
    /// non-file kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<String>,
    /// For `File` kind: maximum file size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size: Option<u64>,
    /// For `File` kind: maximum number of files (defaults to 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u32>,
    /// For `Signature` kind: capture mode (currently only `"draw"` is
    /// implemented).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_mode: Option<String>,
    /// For `Signature` kind: ink color (CSS color string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pen_color: Option<String>,
    /// For `Number` / `Range` kinds: minimum allowed value (inclusive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// For `Number` / `Range` kinds: maximum allowed value (inclusive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// For `Number` / `Range` kinds: step increment (defaults to 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// For `Rating` kind: number of stars (defaults to 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rating: Option<u32>,
    /// For `Date` kind: when true, capture date + time (`YYYY-MM-DDTHH:MM`);
    /// otherwise capture date only (`YYYY-MM-DD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_time: Option<bool>,
}

/// One choice in a `kind = "select"` field. `value` is what the form
/// submits / what guards downstream compare against; `label` is what the
/// UI renders. Authors typically write `{value, label}`; the deserializer
/// also accepts a bare string and stretches it to `{value: s, label: s}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

/// Hand-rolled deserializer for `TaskFieldConfig::options`. Accepts two
/// authoring shapes and normalizes to `Vec<SelectOption>`:
///
///   - `["approve", "reject"]` — bare string shorthand for the common
///     case where the value doubles as the label. Stretched to
///     `{value: "approve", label: "approve"}` etc.
///   - `[{"value": "approve", "label": "Approve as-extracted"}, …]` —
///     full rich shape.
///
/// Any other shape (numbers, bools, mixed arrays without those exact
/// keys) is rejected with an actionable error that names the field
/// index — much better than serde's default "invalid type" surface that
/// doesn't point at the offending entry.
pub(crate) fn deserialize_task_field_options<'de, D>(
    de: D,
) -> Result<Option<Vec<SelectOption>>, D::Error>
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
                // `label` is optional — defaults to `value` so trivial
                // entries can be authored as `{"value": "approve"}`.
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

/// Form-field control kind for `input` task blocks. Snake-case wire values
/// such as `"text"`, `"textarea"`, `"number"`, `"select"`, `"checkbox"`,
/// `"file"`, `"signature"`, `"radio"`, `"date"`, `"range"`, `"rating"`.
/// Must stay in sync with the engine's `TaskFieldKind` in
/// `engine/core-engine/crates/domain/src/human.rs` and the frontend's
/// `TASK_FIELD_KINDS` in `app/src/lib/hpi/types.ts` — drift means the
/// compiler accepts an author's choice that the engine rejects (or
/// vice-versa).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    Serialize,
    Deserialize,
    ToSchema,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskFieldKind {
    #[default]
    Text,
    Textarea,
    Number,
    Select,
    Checkbox,
    File,
    Signature,
    /// Radio button group — same `{value, label}` options as `Select`,
    /// rendered inline (all options visible at once) instead of as a
    /// dropdown. Picked-value wire shape matches `Select`.
    Radio,
    /// Date picker (`YYYY-MM-DD`) or datetime picker (`YYYY-MM-DDTHH:MM`)
    /// when the field carries `include_time = true`. Wire value is a
    /// plain ISO string; downstream comparisons can use lexicographic
    /// ordering up to minute precision.
    Date,
    /// Slider control — emits a `number` on the wire. Customize the
    /// span via `min` / `max` / `step` on the field config.
    Range,
    /// Star-rating control — emits a `number` from 0 to `max_rating`
    /// (default 5) on the wire.
    Rating,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::template::{ExecutionBackendType, ExecutionSpecConfig};

    // ── TaskFieldKind / TaskFieldConfig type-parity tests ─────────────
    //
    // These pin the wire-shape sync between the compiler-side
    // `TaskFieldKind` (this file) and the engine-side equivalent in
    // `engine/core-engine/crates/domain/src/human.rs`. The two have to
    // agree exactly — a kind the compiler accepts but the engine
    // rejects wedges a live net at the human-task effect handler.
    //
    // The frontend's `TASK_FIELD_KINDS` in
    // `app/src/lib/hpi/types.ts` is the third leg; it isn't
    // auto-generated but is asserted against the OpenAPI schema in CI's
    // `openapi-drift` check.

    #[test]
    fn task_field_kind_all_variants_round_trip_through_json() {
        // Every TaskFieldKind round-trips through serde with its
        // snake_case wire form. Adding a new variant without serde
        // tagging or with a wrong rename_all here would slip past type
        // checks but fail at compile-to-air time.
        let cases = [
            (TaskFieldKind::Text, "\"text\""),
            (TaskFieldKind::Textarea, "\"textarea\""),
            (TaskFieldKind::Number, "\"number\""),
            (TaskFieldKind::Select, "\"select\""),
            (TaskFieldKind::Checkbox, "\"checkbox\""),
            (TaskFieldKind::File, "\"file\""),
            (TaskFieldKind::Signature, "\"signature\""),
            (TaskFieldKind::Radio, "\"radio\""),
            (TaskFieldKind::Date, "\"date\""),
            (TaskFieldKind::Range, "\"range\""),
            (TaskFieldKind::Rating, "\"rating\""),
        ];
        for (kind, wire) in cases {
            let ser = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(ser, wire, "wire form drift for {kind:?}");
            let back: TaskFieldKind = serde_json::from_str(wire).expect("deserialize");
            assert_eq!(back, kind, "round-trip drift for {wire}");
        }
    }

    #[test]
    fn task_field_kind_maps_to_typed_port_field_kind() {
        // The compiler emits a typed `Port` for the HumanTask's parked
        // output by mapping each Input block's field kind through this
        // From impl. Pin the mapping so downstream borrow-checking can
        // rely on the typed-ports superset (`Bool` for checkbox, etc.).
        assert_eq!(FieldKind::from(TaskFieldKind::Text), FieldKind::Text);
        assert_eq!(
            FieldKind::from(TaskFieldKind::Textarea),
            FieldKind::Textarea
        );
        assert_eq!(FieldKind::from(TaskFieldKind::Number), FieldKind::Number);
        assert_eq!(FieldKind::from(TaskFieldKind::Select), FieldKind::Select);
        assert_eq!(FieldKind::from(TaskFieldKind::Checkbox), FieldKind::Bool);
        assert_eq!(FieldKind::from(TaskFieldKind::File), FieldKind::File);
        assert_eq!(
            FieldKind::from(TaskFieldKind::Signature),
            FieldKind::Signature
        );
        // New in Feature B parity sync: Radio borrows Select's option
        // semantics, Date is wire-text (ISO string), Range/Rating emit
        // plain numbers.
        assert_eq!(FieldKind::from(TaskFieldKind::Radio), FieldKind::Select);
        assert_eq!(FieldKind::from(TaskFieldKind::Date), FieldKind::Text);
        assert_eq!(FieldKind::from(TaskFieldKind::Range), FieldKind::Number);
        assert_eq!(FieldKind::from(TaskFieldKind::Rating), FieldKind::Number);
    }

    #[test]
    fn task_field_config_renderer_metadata_round_trips() {
        // Authors set min/max/step on a Range field (or max_rating on a
        // Rating field, or include_time on a Date field). The compiler
        // must serialize these so the engine can forward them to the
        // renderer — otherwise the per-field customization disappears
        // between editor and run.
        let raw = serde_json::json!({
            "name": "score",
            "label": "Score",
            "kind": "range",
            "required": true,
            "min": 0,
            "max": 10,
            "step": 0.5,
        });
        let field: TaskFieldConfig =
            serde_json::from_value(raw.clone()).expect("range field parses");
        assert_eq!(field.kind, TaskFieldKind::Range);
        assert_eq!(field.min, Some(0.0));
        assert_eq!(field.max, Some(10.0));
        assert_eq!(field.step, Some(0.5));
        // And round-trip: re-serializing must preserve the metadata.
        let back = serde_json::to_value(&field).expect("serialize");
        assert_eq!(back["min"], 0.0);
        assert_eq!(back["max"], 10.0);
        assert_eq!(back["step"], 0.5);
    }

    #[test]
    fn task_field_config_omits_unset_metadata_from_wire() {
        // skip_serializing_if = "Option::is_none" must hold for every
        // optional metadata key — otherwise byte-identity guards for
        // legacy TaskField shapes break and OpenAPI drift cycles
        // forever as tests churn the spec.
        let field = TaskFieldConfig {
            name: "x".into(),
            label: "X".into(),
            kind: TaskFieldKind::Text,
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
        };
        let wire = serde_json::to_value(&field).expect("serialize");
        let obj = wire.as_object().expect("object");
        // Only name + label + kind survive when nothing else is set.
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["kind", "label", "name"]);
    }

    #[test]
    fn image_block_roundtrip() {
        let block = TaskBlockConfig::Image {
            filenames: vec!["photo.png".to_string(), "diagram.svg".to_string()],
            display: ImageDisplay::Grid,
            url: None,
            alt: None,
            caption: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["filenames"][0], "photo.png");
        assert_eq!(json["filenames"][1], "diagram.svg");
        assert_eq!(json["display"], "grid");
        // Additive url/alt/caption stay absent from the wire when unset.
        assert!(json.get("url").is_none());

        let back: TaskBlockConfig = serde_json::from_value(json).unwrap();
        if let TaskBlockConfig::Image {
            filenames,
            display,
            url,
            ..
        } = back
        {
            assert_eq!(filenames.len(), 2);
            assert_eq!(display, ImageDisplay::Grid);
            assert_eq!(url, None);
        } else {
            panic!("expected Image variant");
        }
    }

    #[test]
    fn url_image_and_download_blocks_match_frontend_shape() {
        // url-driven image: filenames empty (omitted), url present.
        let img = TaskBlockConfig::Image {
            filenames: vec![],
            display: ImageDisplay::Single,
            url: Some("/api/v1/files/k.png".to_string()),
            alt: Some("Invoice".to_string()),
            caption: None,
        };
        let j = serde_json::to_value(&img).unwrap();
        assert_eq!(j["type"], "image");
        assert_eq!(j["url"], "/api/v1/files/k.png");
        assert!(j.get("filenames").is_none(), "empty filenames omitted: {j}");
        assert!(j.get("caption").is_none());

        let dl = TaskBlockConfig::Download {
            downloads: vec![DownloadItemConfig {
                url: "/api/v1/files/k.pdf".to_string(),
                filename: "invoice.pdf".to_string(),
                size: None,
                mime_type: Some("application/pdf".to_string()),
                thumbnail_url: None,
                description: Some("Original invoice".to_string()),
            }],
        };
        let j = serde_json::to_value(&dl).unwrap();
        assert_eq!(j["type"], "download");
        assert_eq!(j["downloads"][0]["url"], "/api/v1/files/k.pdf");
        assert_eq!(j["downloads"][0]["mime_type"], "application/pdf");
        assert!(j["downloads"][0].get("size").is_none());

        // Round-trips back to the same variants.
        let back: TaskBlockConfig = serde_json::from_value(j).unwrap();
        assert!(matches!(back, TaskBlockConfig::Download { .. }));
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
        let blocks = [
            serde_json::json!({"type": "input", "field": {"name": "f", "label": "F", "kind": "text"}}),
            serde_json::json!({"type": "mdsvex", "content": "# Hello"}),
            serde_json::json!({"type": "callout", "severity": "warning", "content": "Watch out"}),
            serde_json::json!({"type": "divider"}),
            serde_json::json!({"type": "image", "filenames": ["a.png"], "display": "single"}),
            serde_json::json!({"type": "file", "filename": "data.csv"}),
        ];
        for (i, json) in blocks.iter().enumerate() {
            let result: Result<TaskBlockConfig, _> = serde_json::from_value(json.clone());
            assert!(
                result.is_ok(),
                "block type {} failed to deserialize: {:?}",
                i,
                result.err()
            );
        }
    }
}
