//! LLM backend declaration.
//!
//! Wraps the executor's LLM backend (OpenAI / Ollama / Anthropic /
//! local). Scans every author-string surface for `{{<slug>.<attr>}}`
//! placeholders and emits per-field `BackendFieldStage` borrows so the
//! executor's input resolver substitutes `{{input:NAME}}` /
//! `{{input_path:NAME}}` at run time.
//!
//! `borrow_shape: PerField` replaces the dedicated `llm_borrow_plan` that
//! used to live in `compiler/token_shape.rs`. The per-site kind constraint
//! (images must be File, content must not be File) moves into
//! [`validate_ref_kind`] on this decl.
//!
//! Resource binding: `resource_alias` names a workspace `openai` /
//! `anthropic` / `ollama` resource. The runtime channel is
//! `ResourceChannel::ConfigOverlay` — LLM's `prepare()` reads
//! `<alias>.json` and merges the resource fields into the resolved
//! config (per-step values win). See
//! [[project_llm_resource_binding]].

use serde_json::{json, Value};

use aithericon_executor_backend_configs::llm::LlmConfig;
use aithericon_executor_domain::InputDeclaration;

use crate::compiler::backend_configs::{require_node_file, stage_all_files, validate_placeholders};
use crate::compiler::placeholder_refs::scan_placeholders;
use crate::compiler::CompileError;
use crate::models::template::{ExecutionBackendType, FieldKind, Port, PortField};

use super::{
    BackendDecl, BorrowShape, DefaultPortField, OutputAuthoring, RefKindCtx, RefSite, ScanCtx,
    ValidationCtx, LLM_META,
};

/// Fallback shape used before the editor's config has any `response_format`
/// set. Mirrors the text-mode default the deriver returns. Keeps
/// [[reference_openapi_schema_regen]]'s `Reset to default` button useful for
/// the (very brief) authoring window before a format is picked.
///
/// Field names match what `executor-llm/src/backend.rs:238-247` puts into
/// the run outputs (`response` / `usage` / `finish_reason` / `model`) so a
/// step with the default port shape lines up with the runtime envelope.
const DEFAULT_OUTPUT_FIELDS: &[DefaultPortField] = &[
    DefaultPortField {
        name: "response",
        label: "Response",
        kind: FieldKind::Textarea,
    },
    DefaultPortField {
        name: "usage",
        label: "Token usage",
        kind: FieldKind::Json,
    },
    DefaultPortField {
        name: "finish_reason",
        label: "Finish reason",
        kind: FieldKind::Text,
    },
    DefaultPortField {
        name: "model",
        label: "Model",
        kind: FieldKind::Text,
    },
];

const RESOURCE_ALIAS_PATHS: &[&[&str]] = &[&["resource_alias"]];

/// The graph-level `provider` value the editor persists for an in-cluster
/// inference-router binding. Kept verbatim in the stored graph so the editor
/// round-trips (`LlmCommonFields.svelte` keys `isInternal` off
/// `provider === "internal"`), but it is NEVER emitted to the executor wire —
/// the executor's `Provider` enum only knows `openai`/`anthropic`/`ollama`.
pub const INTERNAL_PROVIDER: &str = "internal";

/// Remap the editor-only `internal` provider value to the OpenAI-compatible
/// wire provider. An internal binding IS the OpenAI adapter pointed at the
/// router `base_url` supplied by the bound `internal_llm` resource overlay
/// (`executor/crates/executor-llm/src/backend.rs::overlay_resource`), so the
/// wire provider is `openai`. Every other provider passes through unchanged.
///
/// Single source of truth for the two emission seams (the plain-LLM
/// AutomatedStep validator in this module and `agent_to_llm_config`) so they
/// cannot drift.
pub fn remap_internal_provider(provider: &str) -> &str {
    if provider == INTERNAL_PROVIDER {
        "openai"
    } else {
        provider
    }
}

/// Enforce the internal-binding contract on a raw provider + its binding
/// fields. Shared between the plain-LLM AutomatedStep validator and the agent
/// lowering paths so the rule cannot drift.
///
/// No-op when `provider` is not `internal`. When it is, `resource_alias` MUST
/// be present + non-empty — the in-cluster router endpoint lives only in the
/// bound `internal_llm` resource overlay — else [`CompileError::Validation`].
///
/// The GDPR off-router lock (no per-step `base_url`/`api_key` reaching the wire)
/// is NOT enforced here by rejection: a per-step `base_url`/`api_key` on an
/// internal binding is almost always vestigial (the editor hides those inputs
/// for `internal`, so any value is leftover from a prior provider) and erroring
/// on an *invisible* field is a dead-end for the author. Instead every emission
/// seam STRIPS `base_url`/`api_key` for an internal binding (see
/// [`strip_internal_overrides`]) so they never reach the executor wire — which
/// also satisfies the lock, because the executor overlay only fills `base_url`
/// from the resource when the config's own `base_url` is absent.
pub fn check_internal_binding(
    provider: &str,
    resource_alias: Option<&str>,
    _base_url: Option<&str>,
    _api_key: Option<&str>,
) -> Result<(), CompileError> {
    if provider != INTERNAL_PROVIDER {
        return Ok(());
    }
    let present = |v: Option<&str>| v.map(|s| !s.trim().is_empty()).unwrap_or(false);
    if !present(resource_alias) {
        return Err(CompileError::Validation(
            "llm config: provider \"internal\" requires a non-empty resource_alias \
             bound to an internal_llm resource (the in-cluster router endpoint is \
             supplied by that resource's base_url overlay)"
                .into(),
        ));
    }
    Ok(())
}

/// Drop any per-step `base_url`/`api_key` from an internal binding's config
/// object. For `provider: "internal"` the endpoint + credentials come solely
/// from the bound `internal_llm` resource overlay, so a per-step value is both
/// vestigial AND unsafe (it would win over the overlay and could escape the
/// in-cluster router). Stripping it here is the off-router lock. No-op for any
/// other provider, where `base_url`/`api_key` are legitimate authoring fields.
pub fn strip_internal_overrides(config: &mut serde_json::Map<String, Value>) {
    if config.get("provider").and_then(|v| v.as_str()) == Some(INTERNAL_PROVIDER) {
        config.remove("base_url");
        config.remove("api_key");
    }
}

pub static LLM_DECL: BackendDecl = BackendDecl {
    meta: &LLM_META,
    backend_type: ExecutionBackendType::Llm,
    default_output_fields: DEFAULT_OUTPUT_FIELDS,
    default_editor_config,
    validate,
    ref_scanner: Some(ref_scanner),
    resource_alias_paths: RESOURCE_ALIAS_PATHS,
    consumes_declared_outputs: true,
    pyi_introspection: false,
    borrow_shape: BorrowShape::PerField,
    validate_ref_kind,
    output_authoring: OutputAuthoring::Derived,
    derive_output_port: Some(derive_output_port),
    config_schema_fn: config_schema,
    secret_fields: &["api_key"],
};

fn config_schema() -> Value {
    super::self_contained_config_schema::<LlmConfig>()
}

/// Seed config the editor inserts when a step's backend is first set to
/// LLM. Mirrors `AutomatedStepSection.svelte::defaultConfigs.llm`.
fn default_editor_config() -> Value {
    json!({
        "provider": "openai",
        "model": "",
        "prompt": "",
    })
}

/// Validate the editor's LLM config. Moved verbatim from
/// `compiler/backend_configs.rs::validate_and_transform` LLM arm.
fn validate(
    config: &Value,
    ctx: &ValidationCtx<'_>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    // Internal-binding normalization (GAP F keystone). The editor persists
    // `provider: "internal"` so it round-trips as an in-cluster router
    // binding, but the executor's `Provider` enum rejects that string. An
    // internal binding IS the OpenAI adapter pointed at the `internal_llm`
    // resource's router `base_url` (overlaid at fire time). So before serde
    // parses into `LlmConfig`, detect `internal`, enforce the off-router lock,
    // and rewrite the wire `provider` to `openai`.
    let provider = config
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let config = if provider == INTERNAL_PROVIDER {
        let str_field = |k: &str| config.get(k).and_then(|v| v.as_str());
        // (a) alias required (the router endpoint lives only in the bound
        //     internal_llm resource overlay).
        check_internal_binding(
            provider,
            str_field("resource_alias"),
            str_field("base_url"),
            str_field("api_key"),
        )?;
        let mut normalized = config.clone();
        if let Some(obj) = normalized.as_object_mut() {
            // (b) off-router lock: strip any vestigial per-step base_url/api_key
            //     BEFORE the provider rewrite (strip keys off `internal`), so they
            //     never reach the wire and the overlay's router base_url is used.
            strip_internal_overrides(obj);
            // (c) rewrite the wire provider to `openai` and validate THAT
            //     normalized config, so the emitted wire carries `openai`.
            obj.insert(
                "provider".to_string(),
                Value::String(remap_internal_provider(INTERNAL_PROVIDER).to_string()),
            );
        }
        normalized
    } else {
        config.clone()
    };
    let config = &config;

    let parsed: LlmConfig = serde_json::from_value(config.clone())
        .map_err(|e| CompileError::Validation(format!("invalid llm config: {e}")))?;
    if parsed.model.trim().is_empty() {
        return Err(CompileError::Validation(
            "llm config: model is required".into(),
        ));
    }
    if parsed.prompt.trim().is_empty() {
        return Err(CompileError::Validation(
            "llm config: prompt is required".into(),
        ));
    }
    // Placeholder syntax validation. Graph-aware slug resolution happens
    // later in the foundation pass (via the unified borrow planner —
    // `automated_step_borrow_plan` calls our `ref_scanner` + then
    // `validate_ref_kind` once the producer's field kind is resolved).
    validate_placeholders(&parsed.prompt, ctx.node_id, "llm", "prompt")?;
    if let Some(ref sys) = parsed.system_prompt {
        validate_placeholders(sys, ctx.node_id, "llm", "system_prompt")?;
    }
    for (i, m) in parsed.history.iter().enumerate() {
        // `content` is a JSON value (tool-result turns carry structured
        // output); only text turns can hold `{{...}}` placeholders.
        if let Some(s) = m.content.as_str() {
            validate_placeholders(s, ctx.node_id, "llm", &format!("history[{i}].content"))?;
        }
    }
    for (i, img) in parsed.images.iter().enumerate() {
        let site = format!("images[{i}].path");
        let has_placeholder = validate_placeholders(&img.path, ctx.node_id, "llm", &site)?;
        // Attached-file paths get the node-file gate; upstream refs
        // (`{{...}}`) are resolved by the foundation pass.
        if !has_placeholder {
            require_node_file(&img.path, &format!("llm config: {site}"), ctx.node_files)?;
        }
    }
    Ok((config.clone(), stage_all_files(ctx.node_files)))
}

/// Scan every LLM string surface (`prompt`, `system_prompt`,
/// `history[i].content`, `images[i].path`) for `{{<head>.<attr>}}`
/// placeholders. `images[i].path` is the only path site; everything else
/// is a content site.
fn ref_scanner(ctx: &ScanCtx<'_>) -> Vec<RefSite> {
    let mut out: Vec<RefSite> = Vec::new();
    let Some(obj) = ctx.config.as_object() else {
        return out;
    };

    let push_content = |label: String, text: &str, out: &mut Vec<RefSite>| {
        for r in scan_placeholders(text) {
            out.push(RefSite {
                head: r.head,
                attr: r.attr,
                is_path_site: false,
                site_label: label.clone(),
            });
        }
    };

    if let Some(s) = obj.get("prompt").and_then(|v| v.as_str()) {
        push_content("prompt".to_string(), s, &mut out);
    }
    if let Some(s) = obj.get("system_prompt").and_then(|v| v.as_str()) {
        push_content("system_prompt".to_string(), s, &mut out);
    }
    if let Some(arr) = obj.get("history").and_then(|v| v.as_array()) {
        for (i, msg) in arr.iter().enumerate() {
            if let Some(s) = msg.get("content").and_then(|v| v.as_str()) {
                push_content(format!("history[{i}].content"), s, &mut out);
            }
        }
    }
    if let Some(arr) = obj.get("images").and_then(|v| v.as_array()) {
        for (i, img) in arr.iter().enumerate() {
            if let Some(s) = img.get("path").and_then(|v| v.as_str()) {
                for r in scan_placeholders(s) {
                    out.push(RefSite {
                        head: r.head,
                        attr: r.attr,
                        is_path_site: true,
                        site_label: format!("images[{i}].path"),
                    });
                }
            }
        }
    }
    out
}

/// LLM-specific kind constraint:
/// - Path sites (`images[].path`) MUST be `FieldKind::File`. LLM vision
///   needs real image bytes; non-File producers don't have a usable
///   binary representation.
/// - Content sites (`prompt`, `system_prompt`, `history[].content`)
///   MUST NOT be `FieldKind::File`. Interpolating a File envelope
///   (URL + filename JSON) into a prompt would emit structural garbage;
///   the author should add a Kreuzberg step to OCR the file first.
fn validate_ref_kind(ctx: &RefKindCtx<'_>) -> Result<(), CompileError> {
    if ctx.is_path_site && ctx.kind != FieldKind::File {
        return Err(CompileError::LlmImageRefNotFileKind {
            node_id: ctx.node_id.to_string(),
            site: ctx.site_label.to_string(),
            slug: ctx.slug.to_string(),
            field: ctx.attr.to_string(),
            actual_kind: format!("{:?}", ctx.kind).to_lowercase(),
        });
    }
    if !ctx.is_path_site && ctx.kind == FieldKind::File {
        return Err(CompileError::LlmImageRefNotFileKind {
            node_id: ctx.node_id.to_string(),
            site: ctx.site_label.to_string(),
            slug: ctx.slug.to_string(),
            field: ctx.attr.to_string(),
            actual_kind: "file (only valid in images[].path; add a Kreuzberg step to OCR)"
                .to_string(),
        });
    }
    Ok(())
}

/// Derive the LLM step's output port from its config. The runtime emits a
/// fixed envelope (`executor-llm/src/backend.rs:238-247`):
///
/// - `response` — either the structured JSON object (when `response_format
///   = json_schema`) or the raw assistant text.
/// - `usage`, `finish_reason`, `model` — call metadata, always present.
///
/// In structured mode the runner additionally unpacks each top-level
/// schema property to a same-named output (`unpack_by_name`). So the
/// editor port shape is: schema properties (or a single `response` field
/// in text mode) + the three metadata fields.
///
/// Permissive at edit time: a half-typed schema with no `properties` falls
/// back to the text-mode shape rather than erroring. Strict validation is
/// `validate`'s job, run on publish.
fn derive_output_port(config: &Value) -> Port {
    let fmt = config.get("response_format");
    let fmt_type = fmt.and_then(|v| v.get("type")).and_then(|v| v.as_str());

    let mut fields: Vec<PortField> = Vec::new();

    match fmt_type {
        Some("json_schema") => {
            let schema = fmt.and_then(|v| v.get("schema"));
            let schema_type = schema.and_then(|s| s.get("type")).and_then(|v| v.as_str());
            let props = schema
                .and_then(|s| s.get("properties"))
                .and_then(|p| p.as_object());
            let required: std::collections::HashSet<&str> = schema
                .and_then(|s| s.get("required"))
                .and_then(|r| r.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            match (schema_type, props) {
                // Object with explicit properties → one field per property.
                // This is the canonical "structured output" shape: the
                // executor's `unpack_by_name` walks the same property set
                // and routes each to a same-named declared output port.
                (Some("object"), Some(props)) => {
                    for (name, prop) in props.iter() {
                        fields.push(PortField {
                            default: None,
                            schema: None,
                            name: name.clone(),
                            label: prop
                                .get("title")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| name.clone()),
                            kind: kind_from_json_schema(prop),
                            required: required.contains(name.as_str()),
                            options: None,
                            description: prop
                                .get("description")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            accept: None,
                        });
                    }
                }
                // Root-level scalar/array schema → a single `response`
                // field whose kind matches the schema's `type`. The
                // runner returns the parsed value (string / number / bool
                // / array) under `outputs.response` (it never unpacks a
                // non-object); the label comes from the schema's `title`
                // if the author gave one, so a `{title:"Sentiment",
                // type:"string"}` schema shows up as a single
                // "Sentiment"-labeled Text field instead of the generic
                // text-mode placeholder.
                (Some("string"), _)
                | (Some("integer"), _)
                | (Some("number"), _)
                | (Some("boolean"), _)
                | (Some("array"), _) => {
                    let label = schema
                        .and_then(|s| s.get("title"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "Response".to_string());
                    let description = schema
                        .and_then(|s| s.get("description"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    fields.push(PortField {
                        default: None,
                        schema: None,
                        name: "response".into(),
                        label,
                        kind: schema
                            .map(kind_from_json_schema)
                            .unwrap_or(FieldKind::Textarea),
                        required: false,
                        options: None,
                        description,
                        accept: None,
                    });
                }
                // Object with no declared properties, missing `type`, or
                // anything we don't recognize → text-mode fallback. The
                // runner emits the assistant's content as the `response`
                // string; declaring more shape here would just mislead
                // downstream consumers.
                _ => fields.push(text_response_field()),
            }
        }
        _ => {
            fields.push(text_response_field());
        }
    }

    // Metadata fields — always present in the runtime envelope.
    fields.push(PortField {
        default: None,
        schema: None,
        name: "usage".into(),
        label: "Token usage".into(),
        kind: FieldKind::Json,
        required: false,
        options: None,
        description: None,
        accept: None,
    });
    fields.push(PortField {
        default: None,
        schema: None,
        name: "finish_reason".into(),
        label: "Finish reason".into(),
        kind: FieldKind::Text,
        required: false,
        options: None,
        description: None,
        accept: None,
    });
    fields.push(PortField {
        default: None,
        schema: None,
        name: "model".into(),
        label: "Model".into(),
        kind: FieldKind::Text,
        required: false,
        options: None,
        description: None,
        accept: None,
    });

    Port {
        id: "out".into(),
        label: "Output".into(),
        fields,
    }
}

fn text_response_field() -> PortField {
    PortField {
        default: None,
        schema: None,
        name: "response".into(),
        label: "Response".into(),
        kind: FieldKind::Textarea,
        required: false,
        options: None,
        description: None,
        accept: None,
    }
}

/// Map a JSON Schema property to the closest [`FieldKind`]. Conservative:
/// anything we can't classify falls to `Json` so downstream consumers
/// don't get a misleadingly narrow shape.
fn kind_from_json_schema(prop: &Value) -> FieldKind {
    let ty = prop.get("type").and_then(|v| v.as_str());
    match ty {
        Some("string") => {
            // Hint: explicit `format: "textarea"` or long-text contentMediaType
            // gets the textarea kind; everything else stays single-line.
            let format = prop.get("format").and_then(|v| v.as_str());
            if matches!(format, Some("textarea") | Some("multi-line")) {
                FieldKind::Textarea
            } else {
                FieldKind::Text
            }
        }
        Some("integer") | Some("number") => FieldKind::Number,
        Some("boolean") => FieldKind::Bool,
        Some("object") | Some("array") => FieldKind::Json,
        _ => FieldKind::Json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_text_mode() {
        let port = derive_output_port(&json!({}));
        let names: Vec<_> = port.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["response", "usage", "finish_reason", "model"]);
    }

    #[test]
    fn derive_json_schema_mode() {
        // Note: schema property iteration order follows `serde_json::Map`'s
        // ordering. This workspace builds with `serde_json/preserve_order`
        // (enabled transitively by `zarrs`), so schema-property ports come
        // out in declaration order (`summary`, `score`, `ok`), then the
        // metadata tail in fixed order.
        let cfg = json!({
            "response_format": {
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": {
                        "summary": { "type": "string", "title": "Summary" },
                        "score": { "type": "number" },
                        "ok": { "type": "boolean" }
                    },
                    "required": ["summary"]
                }
            }
        });
        let port = derive_output_port(&cfg);
        let names: Vec<_> = port.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["summary", "score", "ok", "usage", "finish_reason", "model"]
        );
        let summary = port.fields.iter().find(|f| f.name == "summary").unwrap();
        assert_eq!(summary.kind, FieldKind::Text);
        assert_eq!(summary.label, "Summary");
        assert!(summary.required);
        let score = port.fields.iter().find(|f| f.name == "score").unwrap();
        assert_eq!(score.kind, FieldKind::Number);
        let ok = port.fields.iter().find(|f| f.name == "ok").unwrap();
        assert_eq!(ok.kind, FieldKind::Bool);
    }

    #[test]
    fn derive_json_schema_no_properties_falls_back_to_text() {
        let cfg = json!({
            "response_format": { "type": "json_schema", "schema": { "type": "object" } }
        });
        let port = derive_output_port(&cfg);
        let names: Vec<_> = port.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["response", "usage", "finish_reason", "model"]);
    }

    #[test]
    fn derive_root_scalar_string_schema() {
        // Root-level scalar schema — the case the user hit. The LLM
        // returns a single string; we expose it as one `response` field
        // labeled from the schema's `title`, kind Text (not Textarea —
        // root scalars are usually short answers).
        let cfg = json!({
            "response_format": {
                "type": "json_schema",
                "schema": { "type": "string", "title": "Sentiment" }
            }
        });
        let port = derive_output_port(&cfg);
        let names: Vec<_> = port.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["response", "usage", "finish_reason", "model"]);
        let response = port.fields.iter().find(|f| f.name == "response").unwrap();
        assert_eq!(response.kind, FieldKind::Text);
        assert_eq!(response.label, "Sentiment");
    }

    #[test]
    fn derive_root_number_schema() {
        let cfg = json!({
            "response_format": {
                "type": "json_schema",
                "schema": { "type": "number" }
            }
        });
        let port = derive_output_port(&cfg);
        let response = port.fields.iter().find(|f| f.name == "response").unwrap();
        assert_eq!(response.kind, FieldKind::Number);
        assert_eq!(response.label, "Response");
    }

    #[test]
    fn derive_root_array_schema() {
        let cfg = json!({
            "response_format": {
                "type": "json_schema",
                "schema": { "type": "array", "title": "Tags" }
            }
        });
        let port = derive_output_port(&cfg);
        let response = port.fields.iter().find(|f| f.name == "response").unwrap();
        assert_eq!(response.kind, FieldKind::Json);
        assert_eq!(response.label, "Tags");
    }

    // --- GAP F: internal-provider remap (keystone) -------------------------

    fn validate_cfg(config: &Value) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
        let node_files = std::collections::HashMap::new();
        let ctx = ValidationCtx {
            node_id: "n1",
            node_files: &node_files,
        };
        validate(config, &ctx)
    }

    #[test]
    fn internal_remaps_to_openai() {
        // The editor persists `provider: "internal"`; the emitted WIRE config
        // must carry `openai` so the executor's `Provider` enum accepts it.
        let cfg = json!({
            "provider": "internal",
            "model": "llama3.2:1b",
            "prompt": "hi",
            "resource_alias": "internal_pool_router",
        });
        let (wire, _inputs) = validate_cfg(&cfg).expect("internal should normalize");
        assert_eq!(
            wire.get("provider").and_then(|v| v.as_str()),
            Some("openai"),
            "internal must remap to openai on the wire"
        );
        // Round-trips through serde into the executor's LlmConfig.
        let parsed: LlmConfig = serde_json::from_value(wire).unwrap();
        assert_eq!(parsed.model, "llama3.2:1b");
    }

    #[test]
    fn internal_requires_resource_alias() {
        let cfg = json!({
            "provider": "internal",
            "model": "llama3.2:1b",
            "prompt": "hi",
        });
        let err = validate_cfg(&cfg).expect_err("missing alias must error");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("resource_alias") && msg.contains("internal_llm"),
            "error should name resource_alias + internal_llm, got: {msg}"
        );
    }

    #[test]
    fn internal_strips_per_step_base_url_and_api_key() {
        // Off-router lock: a per-step base_url/api_key on an internal binding is
        // vestigial (the editor hides those inputs for `internal`). Validation
        // must SUCCEED and STRIP them from the wire — never let them reach the
        // executor (where a per-step base_url would win over the resource overlay
        // and could escape the in-cluster router).
        let cfg = json!({
            "provider": "internal",
            "model": "llama3.2:1b",
            "prompt": "hi",
            "resource_alias": "internal_pool_router",
            "base_url": "https://evil.example.com/v1",
            "api_key": "sk-leak",
        });
        let (wire, _inputs) =
            validate_cfg(&cfg).expect("internal with vestigial overrides must pass");
        assert_eq!(
            wire.get("provider").and_then(|v| v.as_str()),
            Some("openai"),
            "internal must remap to openai"
        );
        assert!(
            wire.get("base_url").is_none(),
            "per-step base_url must be stripped from the internal wire config"
        );
        assert!(
            wire.get("api_key").is_none(),
            "per-step api_key must be stripped from the internal wire config"
        );
        assert_eq!(
            wire.get("resource_alias").and_then(|v| v.as_str()),
            Some("internal_pool_router"),
            "the internal_llm binding must be preserved"
        );
    }
}
