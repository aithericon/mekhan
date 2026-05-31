//! Executor backend config validation and transformation.
//!
//! Validates the frontend's editor config against the executor's expected types
//! and produces the executor-side config plus the list of inputs to stage.
//!
//! Files attached to a node (managed via the IDE FileTree, stored as Y.Text in
//! the Y.Doc, uploaded to S3 at publish time) are the single source for staged
//! inputs. The caller passes in a per-node `name -> InputSource` map and the
//! compiler emits one `InputDeclaration` per entry.

use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

use aithericon_executor_backend_configs::{
    python::{default_python, PythonConfig},
    smtp::{AttachmentSpec as SmtpAttachmentSpec, TemplateSource},
};
use aithericon_executor_domain::{InputDeclaration, InputSource};

use crate::compiler::rhai_gen::parse_placeholder_segments;
use crate::models::template::ExecutionBackendType;

use super::CompileError;

/// Walk a free-form string looking for `{{ … }}` placeholder bodies; for each
/// one, run the shared `parse_placeholder_segments` validator and surface a
/// [`CompileError::BackendPlaceholderSyntax`] if the body isn't a dotted
/// identifier path. Returns `true` if the string contains at least one
/// well-formed `{{...}}` placeholder (so the caller can decide whether to
/// skip `require_node_file`).
///
/// Caller passes `node_id`, `backend` and `site` for error attribution.
pub(crate) fn validate_placeholders(
    s: &str,
    node_id: &str,
    backend: &str,
    site: &str,
) -> Result<bool, CompileError> {
    let mut rest = s;
    let mut had_placeholder = false;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close_rel) = after.find("}}") else {
            // Unterminated `{{` — keep author-friendly: it's not a placeholder.
            return Ok(had_placeholder);
        };
        let inner = &after[..close_rel];
        if parse_placeholder_segments(inner).is_none() {
            return Err(CompileError::BackendPlaceholderSyntax {
                node_id: node_id.to_string(),
                backend: backend.to_string(),
                site: site.to_string(),
                body: inner.trim().to_string(),
            });
        }
        had_placeholder = true;
        rest = &after[close_rel + 2..];
    }
    Ok(had_placeholder)
}

/// Editor-side Python config. The script is selected by `entrypoint`, which
/// must name one of the node's files.
#[derive(Debug, Clone, Deserialize)]
pub struct EditorPythonConfig {
    /// Filename of the script to execute (must exist in the node's files).
    #[serde(default = "default_entrypoint")]
    pub entrypoint: String,
    #[serde(default = "default_python")]
    pub python: String,
    #[serde(default)]
    pub requirements: Vec<String>,
    #[serde(default)]
    pub virtualenv: bool,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default = "default_true")]
    pub inherit_env: bool,
    #[serde(default = "default_true")]
    pub sdk: bool,
}

fn default_entrypoint() -> String {
    "main.py".to_string()
}

/// Editor-side catalogue-query config. Deserialized purely to validate shape;
/// re-serialized as the `query` token the engine's `catalogue_lookup` handler
/// accepts (ADR-17 convenience format: top-level `category` / `source_net` /
/// `source_process_id` / `sort_by` / `limit` / `page` / `search` / `filters`).
/// Maps directly onto the service catalogue filter grammar
/// (`service/src/catalogue/queries.rs::list_entries`).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub(crate) struct CatalogueQueryConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_net: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_process_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    /// Generic typed filters: `{ field: { op: value } }` (eq/neq/lt/gt/...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filters: Option<HashMap<String, HashMap<String, Value>>>,
}

fn default_true() -> bool {
    true
}

impl EditorPythonConfig {
    /// Build the executor-side `PythonConfig` plus the list of staged inputs.
    pub fn to_executor_config(
        self,
        node_files: &HashMap<String, InputSource>,
    ) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
        if node_files.is_empty() {
            return Err(CompileError::Validation(format!(
                "python config: node has no files; add at least one file (entrypoint default is '{}')",
                self.entrypoint
            )));
        }
        if !node_files.contains_key(&self.entrypoint) {
            return Err(CompileError::Validation(format!(
                "python config: entrypoint '{}' not found in node files (have: {})",
                self.entrypoint,
                format_available(node_files)
            )));
        }

        let inputs = stage_all_files(node_files);

        let executor_config = PythonConfig {
            script: self.entrypoint,
            python: self.python,
            requirements: self.requirements,
            virtualenv: self.virtualenv,
            env: self.env,
            working_dir: self.working_dir,
            inherit_env: self.inherit_env,
            sdk: self.sdk,
        };

        let config_value = serde_json::to_value(&executor_config).map_err(|e| {
            CompileError::Compilation(format!("failed to serialize python config: {e}"))
        })?;

        Ok((config_value, inputs))
    }
}

/// Stage all node files as required `InputDeclaration`s, sorted by name for
/// deterministic AIR output. Used by backends whose files are passed through
/// without per-name validation (Python, Process, Docker, generic LLM/Kreuzberg
/// inputs).
pub(crate) fn stage_all_files(node_files: &HashMap<String, InputSource>) -> Vec<InputDeclaration> {
    let mut inputs: Vec<InputDeclaration> = node_files
        .iter()
        .map(|(name, source)| InputDeclaration {
            name: name.clone(),
            source: source.clone(),
            required: true,
        })
        .collect();
    inputs.sort_by(|a, b| a.name.cmp(&b.name));
    inputs
}

/// Format the available filenames for an error message.
pub(crate) fn format_available(node_files: &HashMap<String, InputSource>) -> String {
    if node_files.is_empty() {
        return "(none)".to_string();
    }
    let mut names: Vec<&String> = node_files.keys().collect();
    names.sort();
    names
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Check that a referenced filename exists in the node's files; otherwise emit
/// a validation error attributing the failure to a specific config field.
pub(crate) fn require_node_file(
    filename: &str,
    field: &str,
    node_files: &HashMap<String, InputSource>,
) -> Result<(), CompileError> {
    if node_files.contains_key(filename) {
        return Ok(());
    }
    Err(CompileError::Validation(format!(
        "{field} references file '{filename}' which is not attached to this node (available: {})",
        format_available(node_files)
    )))
}

/// Validate and transform an editor backend config into the executor's
/// expected format. Pure registry dispatch — every backend has a decl in
/// `crate::backends`, so this is a single trampoline call.
///
/// `node_files` is the per-node map of filename → source. Backends that
/// take files emit one `InputDeclaration` per entry; backends that don't
/// (`file_ops`, `catalogue_query`) ignore it.
///
/// `node_id` is used for attribution in placeholder-syntax errors raised
/// by backends whose author-supplied strings carry `{{<slug>.<field>}}`
/// placeholders (LLM, Kreuzberg, SMTP). Callers without a meaningful id
/// (test harnesses) can pass `""` — the error message just shows blank.
pub fn validate_and_transform(
    backend_type: &ExecutionBackendType,
    config: &Value,
    node_files: &HashMap<String, InputSource>,
    node_id: &str,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    let decl = crate::backends::lookup(*backend_type).ok_or_else(|| {
        CompileError::Compilation(format!("backend {:?} has no registered decl", backend_type))
    })?;
    let ctx = crate::backends::ValidationCtx {
        node_id,
        node_files,
    };
    (decl.validate)(config, &ctx)
}

// Aliases used by handlers/tests that want to construct an SMTP config
// without reaching into the executor configs crate.
pub type SmtpAttachment = SmtpAttachmentSpec;
pub type SmtpTemplateSource = TemplateSource;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(content: &str) -> InputSource {
        InputSource::Raw {
            content: content.to_string(),
        }
    }

    #[test]
    fn python_validates_entrypoint_exists() {
        let mut files = HashMap::new();
        files.insert("main.py".to_string(), raw("print(1)"));

        let config = json!({"entrypoint": "main.py"});
        let (_, inputs) =
            validate_and_transform(&ExecutionBackendType::Python, &config, &files, "test_node")
                .unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "main.py");
    }

    #[test]
    fn python_rejects_missing_entrypoint() {
        let mut files = HashMap::new();
        files.insert("helper.py".to_string(), raw(""));

        let config = json!({"entrypoint": "main.py"});
        let err =
            validate_and_transform(&ExecutionBackendType::Python, &config, &files, "test_node")
                .unwrap_err()
                .to_string();
        assert!(err.contains("entrypoint 'main.py' not found"));
        assert!(err.contains("'helper.py'"));
    }

    #[test]
    fn python_rejects_empty_files() {
        let files = HashMap::new();
        let config = json!({"entrypoint": "main.py"});
        let err =
            validate_and_transform(&ExecutionBackendType::Python, &config, &files, "test_node")
                .unwrap_err()
                .to_string();
        assert!(err.contains("node has no files"));
    }

    #[test]
    fn process_rejects_empty_command() {
        let config = json!({"command": ""});
        let err = validate_and_transform(
            &ExecutionBackendType::Process,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("command is required"));
    }

    #[test]
    fn process_stages_files() {
        let mut files = HashMap::new();
        files.insert("run.sh".to_string(), raw("echo hi"));
        let config = json!({"command": "bash", "args": ["run.sh"]});
        let (_, inputs) =
            validate_and_transform(&ExecutionBackendType::Process, &config, &files, "test_node")
                .unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "run.sh");
    }

    #[test]
    fn docker_rejects_empty_image() {
        let config = json!({"image": ""});
        let err = validate_and_transform(
            &ExecutionBackendType::Docker,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("image is required"));
    }

    #[test]
    fn http_rejects_missing_body_from_input_file() {
        let config = json!({
            "url": "https://api.example.com",
            "method": "POST",
            "body_from_input": "payload.json"
        });
        let err = validate_and_transform(
            &ExecutionBackendType::Http,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("body_from_input"));
        assert!(err.contains("'payload.json'"));
    }

    #[test]
    fn http_rejects_body_and_body_from_input() {
        let config = json!({
            "url": "https://api.example.com",
            "body": {"k": "v"},
            "body_from_input": "payload.json"
        });
        let err = validate_and_transform(
            &ExecutionBackendType::Http,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn http_accepts_body_from_input_when_file_present() {
        let mut files = HashMap::new();
        files.insert("payload.json".to_string(), raw("{}"));
        let config = json!({
            "url": "https://api.example.com",
            "method": "POST",
            "body_from_input": "payload.json"
        });
        let (_, inputs) =
            validate_and_transform(&ExecutionBackendType::Http, &config, &files, "test_node")
                .unwrap();
        assert_eq!(inputs.len(), 1);
    }

    #[test]
    fn llm_rejects_missing_image_file() {
        let config = json!({
            "provider": "openai",
            "model": "gpt-4o",
            "prompt": "describe",
            "images": [{"path": "diagram.png"}]
        });
        let err = validate_and_transform(
            &ExecutionBackendType::Llm,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("images[0].path"));
        assert!(err.contains("'diagram.png'"));
    }

    #[test]
    fn llm_rejects_empty_model() {
        let config = json!({
            "provider": "openai",
            "model": "",
            "prompt": "hi"
        });
        let err = validate_and_transform(
            &ExecutionBackendType::Llm,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("model is required"));
    }

    #[test]
    fn kreuzberg_rejects_missing_file_reference() {
        let mut files = HashMap::new();
        files.insert("other.pdf".to_string(), raw(""));
        let config = json!({"mode": "single", "file": "missing.pdf"});
        let err = validate_and_transform(
            &ExecutionBackendType::Kreuzberg,
            &config,
            &files,
            "test_node",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("kreuzberg config: file"));
        assert!(err.contains("'missing.pdf'"));
    }

    #[test]
    fn kreuzberg_rejects_empty_files() {
        let config = json!({"mode": "single"});
        let err = validate_and_transform(
            &ExecutionBackendType::Kreuzberg,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no files"));
    }

    #[test]
    fn file_ops_validates_operation_tag() {
        let bad = json!({"op": "stat"});
        let err = validate_and_transform(
            &ExecutionBackendType::FileOps,
            &bad,
            &HashMap::new(),
            "test_node",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("invalid file_ops config"));
    }

    #[test]
    fn file_ops_accepts_stat_with_storage() {
        let config = json!({
            "operation": "stat",
            "path": "data/x.csv",
            "storage": {"backend": "local", "endpoint": "/tmp"}
        });
        let (_, inputs) = validate_and_transform(
            &ExecutionBackendType::FileOps,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .unwrap();
        assert!(inputs.is_empty());
    }

    // ──────────────────────────────────────────────────────────────────
    // Placeholder validation: `{{...}}` bodies in LLM / Kreuzberg strings
    // must parse as dotted identifier paths. Slug resolution itself runs
    // later in apply_control_data_foundation (it needs graph access).
    // ──────────────────────────────────────────────────────────────────

    #[test]
    fn llm_rejects_malformed_placeholder_in_prompt() {
        let config = json!({
            "provider": "openai",
            "model": "gpt-4o",
            "prompt": "Sum: {{ a + b }}"
        });
        let err = validate_and_transform(
            &ExecutionBackendType::Llm,
            &config,
            &HashMap::new(),
            "node-classify",
        )
        .unwrap_err();
        match err {
            CompileError::BackendPlaceholderSyntax {
                node_id,
                backend,
                site,
                body,
            } => {
                assert_eq!(node_id, "node-classify");
                assert_eq!(backend, "llm");
                assert_eq!(site, "prompt");
                assert_eq!(body, "a + b");
            }
            other => panic!("expected BackendPlaceholderSyntax, got {other:?}"),
        }
    }

    #[test]
    fn llm_accepts_well_formed_prompt_placeholders() {
        // Slug resolution is deferred — at this stage we only check syntax.
        let config = json!({
            "provider": "openai",
            "model": "gpt-4o",
            "prompt": "Classify: {{ ocr.content }} for {{ review.vendor_name }}"
        });
        let (_, inputs) = validate_and_transform(
            &ExecutionBackendType::Llm,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .expect("well-formed placeholders pass validation");
        assert!(inputs.is_empty()); // No attached node files in this test.
    }

    #[test]
    fn llm_images_path_skips_node_file_check_for_placeholder() {
        // `images[0].path` may be either an attached file OR an upstream
        // ref. The latter case skips `require_node_file` since the
        // foundation pass resolves it from the producer's parked envelope.
        let config = json!({
            "provider": "openai",
            "model": "gpt-4o",
            "prompt": "Caption this image",
            "images": [{"path": "{{ uploader.photo }}"}]
        });
        let (_, _) = validate_and_transform(
            &ExecutionBackendType::Llm,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .expect("upstream image refs don't require an attached file");
    }

    #[test]
    fn kreuzberg_accepts_upstream_ref_without_attached_files() {
        // A Kreuzberg node with ONLY an upstream ref and no attached
        // files is legal — the foundation pass produces the staging.
        let config = json!({
            "mode": "single",
            "file": "{{ uploader.pdf }}"
        });
        let (_, _) = validate_and_transform(
            &ExecutionBackendType::Kreuzberg,
            &config,
            &HashMap::new(),
            "test_node",
        )
        .expect("upstream ref alone satisfies the no-files gate");
    }

    #[test]
    fn kreuzberg_rejects_malformed_placeholder_in_file() {
        let config = json!({
            "mode": "single",
            "file": "{{ a + b }}"
        });
        let err = validate_and_transform(
            &ExecutionBackendType::Kreuzberg,
            &config,
            &HashMap::new(),
            "ocr",
        )
        .unwrap_err();
        match err {
            CompileError::BackendPlaceholderSyntax {
                node_id,
                backend,
                site,
                body,
            } => {
                assert_eq!(node_id, "ocr");
                assert_eq!(backend, "kreuzberg");
                assert_eq!(site, "file");
                assert_eq!(body, "a + b");
            }
            other => panic!("expected BackendPlaceholderSyntax, got {other:?}"),
        }
    }

    // ─── SMTP arm ─────────────────────────────────────────────────────────

    fn smtp_minimal_config() -> serde_json::Value {
        json!({
            "to": ["{{ intake.email }}"],
            "subject": { "label": "subject.tera", "source": "Welcome, {{ intake.name }}!" },
            "body_text": { "label": "body.txt.tera", "source": "Hi {{ intake.name }}." },
            "resource_alias": "mail",
        })
    }

    #[test]
    fn smtp_minimal_config_compiles() {
        let (canonical, inputs) = validate_and_transform(
            &ExecutionBackendType::Smtp,
            &smtp_minimal_config(),
            &HashMap::new(),
            "send",
        )
        .unwrap();
        // SMTP doesn't pull node files for templates (they're embedded);
        // attachments would be the only InputDeclaration source — none here.
        assert!(inputs.is_empty());
        // Canonical re-serialization preserves the inline source strings.
        assert_eq!(
            canonical["subject"]["source"],
            "Welcome, {{ intake.name }}!"
        );
        assert_eq!(canonical["body_text"]["label"], "body.txt.tera");
        assert_eq!(canonical["resource_alias"], "mail");
    }

    #[test]
    fn smtp_rejects_missing_recipients() {
        let mut cfg = smtp_minimal_config();
        cfg["to"] = json!([]);
        let err =
            validate_and_transform(&ExecutionBackendType::Smtp, &cfg, &HashMap::new(), "send")
                .unwrap_err()
                .to_string();
        assert!(err.contains("at least one recipient"), "got: {err}");
    }

    #[test]
    fn smtp_rejects_missing_body() {
        let mut cfg = smtp_minimal_config();
        cfg.as_object_mut().unwrap().remove("body_text");
        let err =
            validate_and_transform(&ExecutionBackendType::Smtp, &cfg, &HashMap::new(), "send")
                .unwrap_err()
                .to_string();
        assert!(err.contains("body_text or body_html"), "got: {err}");
    }

    #[test]
    fn smtp_rejects_empty_subject_source() {
        let mut cfg = smtp_minimal_config();
        cfg["subject"]["source"] = json!("");
        let err =
            validate_and_transform(&ExecutionBackendType::Smtp, &cfg, &HashMap::new(), "send")
                .unwrap_err()
                .to_string();
        assert!(err.contains("subject"), "got: {err}");
    }

    #[test]
    fn smtp_rejects_malformed_placeholder_in_subject() {
        let mut cfg = smtp_minimal_config();
        // `{{ user.name + 1 }}` is not a valid dotted-path placeholder.
        cfg["subject"]["source"] = json!("Hi {{ user.name + 1 }}");
        let err =
            validate_and_transform(&ExecutionBackendType::Smtp, &cfg, &HashMap::new(), "send")
                .unwrap_err();
        match err {
            CompileError::BackendPlaceholderSyntax {
                node_id,
                backend,
                site,
                ..
            } => {
                assert_eq!(node_id, "send");
                assert_eq!(backend, "smtp");
                assert!(site.contains("subject"), "site was {site}");
            }
            other => panic!("expected BackendPlaceholderSyntax, got {other:?}"),
        }
    }

    #[test]
    fn smtp_rejects_malformed_placeholder_in_recipient() {
        let mut cfg = smtp_minimal_config();
        cfg["to"] = json!(["{{ user.name + 1 }}"]);
        let err =
            validate_and_transform(&ExecutionBackendType::Smtp, &cfg, &HashMap::new(), "send")
                .unwrap_err();
        match err {
            CompileError::BackendPlaceholderSyntax { site, .. } => {
                assert!(site.starts_with("to["), "site was {site}");
            }
            other => panic!("expected BackendPlaceholderSyntax, got {other:?}"),
        }
    }

    #[test]
    fn smtp_rejects_duplicate_attachment_input_names() {
        let mut cfg = smtp_minimal_config();
        cfg["attachments"] = json!([
            { "filename": "a.pdf", "input_name": "_att_0" },
            { "filename": "b.pdf", "input_name": "_att_0" }, // duplicate
        ]);
        let err =
            validate_and_transform(&ExecutionBackendType::Smtp, &cfg, &HashMap::new(), "send")
                .unwrap_err()
                .to_string();
        assert!(err.contains("duplicate attachment"), "got: {err}");
    }
}
