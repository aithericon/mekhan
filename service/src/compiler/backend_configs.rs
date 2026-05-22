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
    file_ops::FileOpsConfig, http::HttpConfig, kreuzberg::KreuzbergConfig, llm::LlmConfig,
    process::ProcessConfig,
    python::{default_python, PythonConfig},
};
use aithericon_executor_domain::{InputDeclaration, InputSource};

use crate::models::template::ExecutionBackendType;

use super::CompileError;

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
pub struct CatalogueQueryConfig {
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
fn stage_all_files(node_files: &HashMap<String, InputSource>) -> Vec<InputDeclaration> {
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
fn format_available(node_files: &HashMap<String, InputSource>) -> String {
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
fn require_node_file(
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

/// Validate and transform an editor backend config into the executor's expected format.
///
/// Returns (validated config as Value, inputs to stage in the ExecutionSpec).
/// `node_files` is the per-node map of filename → source. Backends that take
/// files emit one `InputDeclaration` per entry; backends that don't (`file_ops`)
/// ignore it.
pub fn validate_and_transform(
    backend_type: &ExecutionBackendType,
    config: &Value,
    node_files: &HashMap<String, InputSource>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    match backend_type {
        ExecutionBackendType::Python => {
            let editor_config: EditorPythonConfig = serde_json::from_value(config.clone())
                .map_err(|e| CompileError::Validation(format!("invalid python config: {e}")))?;
            editor_config.to_executor_config(node_files)
        }

        ExecutionBackendType::Process => {
            let parsed: ProcessConfig = serde_json::from_value(config.clone())
                .map_err(|e| CompileError::Validation(format!("invalid process config: {e}")))?;
            if parsed.command.trim().is_empty() {
                return Err(CompileError::Validation(
                    "process config: command is required".into(),
                ));
            }
            Ok((config.clone(), stage_all_files(node_files)))
        }

        ExecutionBackendType::Docker => {
            let parsed: aithericon_executor_backend_configs::docker::DockerConfig =
                serde_json::from_value(config.clone())
                    .map_err(|e| CompileError::Validation(format!("invalid docker config: {e}")))?;
            if parsed.image.trim().is_empty() {
                return Err(CompileError::Validation(
                    "docker config: image is required".into(),
                ));
            }
            Ok((config.clone(), stage_all_files(node_files)))
        }

        ExecutionBackendType::Http => {
            let parsed: HttpConfig = serde_json::from_value(config.clone())
                .map_err(|e| CompileError::Validation(format!("invalid http config: {e}")))?;
            if parsed.url.trim().is_empty() {
                return Err(CompileError::Validation(
                    "http config: url is required".into(),
                ));
            }
            if parsed.body.is_some() && parsed.body_from_input.is_some() {
                return Err(CompileError::Validation(
                    "http config: body and body_from_input are mutually exclusive".into(),
                ));
            }
            if let Some(ref name) = parsed.body_from_input {
                require_node_file(name, "http config: body_from_input", node_files)?;
            }
            Ok((config.clone(), stage_all_files(node_files)))
        }

        ExecutionBackendType::Llm => {
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
            for (i, img) in parsed.images.iter().enumerate() {
                require_node_file(&img.path, &format!("llm config: images[{i}].path"), node_files)?;
            }
            Ok((config.clone(), stage_all_files(node_files)))
        }

        ExecutionBackendType::Kreuzberg => {
            let parsed: KreuzbergConfig = serde_json::from_value(config.clone())
                .map_err(|e| CompileError::Validation(format!("invalid kreuzberg config: {e}")))?;
            // Validate file references against node files.
            if let Some(ref name) = parsed.file {
                require_node_file(name, "kreuzberg config: file", node_files)?;
            }
            for (i, name) in parsed.files.iter().enumerate() {
                require_node_file(name, &format!("kreuzberg config: files[{i}]"), node_files)?;
            }
            if node_files.is_empty() {
                return Err(CompileError::Validation(
                    "kreuzberg config: node has no files; attach at least one document".into(),
                ));
            }
            Ok((config.clone(), stage_all_files(node_files)))
        }

        ExecutionBackendType::FileOps => {
            // Validates structure (operation tag + per-op required fields).
            // file_ops works on storage paths, not staged inputs — emits no
            // InputDeclarations.
            let _: FileOpsConfig = serde_json::from_value(config.clone())
                .map_err(|e| CompileError::Validation(format!("invalid file_ops config: {e}")))?;
            Ok((config.clone(), vec![]))
        }

        ExecutionBackendType::CatalogueQuery => {
            // Read-only catalogue lookup: no executor job, no staged inputs.
            // Validate the shape and emit the normalized `query` token the
            // `catalogue_lookup` effect handler consumes.
            let parsed: CatalogueQueryConfig = serde_json::from_value(config.clone())
                .map_err(|e| {
                    CompileError::Validation(format!("invalid catalogue_query config: {e}"))
                })?;
            let token = serde_json::to_value(&parsed).map_err(|e| {
                CompileError::Validation(format!("catalogue_query serialize: {e}"))
            })?;
            Ok((token, vec![]))
        }
    }
}

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
            validate_and_transform(&ExecutionBackendType::Python, &config, &files).unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "main.py");
    }

    #[test]
    fn python_rejects_missing_entrypoint() {
        let mut files = HashMap::new();
        files.insert("helper.py".to_string(), raw(""));

        let config = json!({"entrypoint": "main.py"});
        let err = validate_and_transform(&ExecutionBackendType::Python, &config, &files)
            .unwrap_err()
            .to_string();
        assert!(err.contains("entrypoint 'main.py' not found"));
        assert!(err.contains("'helper.py'"));
    }

    #[test]
    fn python_rejects_empty_files() {
        let files = HashMap::new();
        let config = json!({"entrypoint": "main.py"});
        let err = validate_and_transform(&ExecutionBackendType::Python, &config, &files)
            .unwrap_err()
            .to_string();
        assert!(err.contains("node has no files"));
    }

    #[test]
    fn process_rejects_empty_command() {
        let config = json!({"command": ""});
        let err = validate_and_transform(&ExecutionBackendType::Process, &config, &HashMap::new())
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
            validate_and_transform(&ExecutionBackendType::Process, &config, &files).unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "run.sh");
    }

    #[test]
    fn docker_rejects_empty_image() {
        let config = json!({"image": ""});
        let err = validate_and_transform(&ExecutionBackendType::Docker, &config, &HashMap::new())
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
        let err = validate_and_transform(&ExecutionBackendType::Http, &config, &HashMap::new())
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
        let err = validate_and_transform(&ExecutionBackendType::Http, &config, &HashMap::new())
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
            validate_and_transform(&ExecutionBackendType::Http, &config, &files).unwrap();
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
        let err = validate_and_transform(&ExecutionBackendType::Llm, &config, &HashMap::new())
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
        let err = validate_and_transform(&ExecutionBackendType::Llm, &config, &HashMap::new())
            .unwrap_err()
            .to_string();
        assert!(err.contains("model is required"));
    }

    #[test]
    fn kreuzberg_rejects_missing_file_reference() {
        let mut files = HashMap::new();
        files.insert("other.pdf".to_string(), raw(""));
        let config = json!({"mode": "single", "file": "missing.pdf"});
        let err = validate_and_transform(&ExecutionBackendType::Kreuzberg, &config, &files)
            .unwrap_err()
            .to_string();
        assert!(err.contains("kreuzberg config: file"));
        assert!(err.contains("'missing.pdf'"));
    }

    #[test]
    fn kreuzberg_rejects_empty_files() {
        let config = json!({"mode": "single"});
        let err =
            validate_and_transform(&ExecutionBackendType::Kreuzberg, &config, &HashMap::new())
                .unwrap_err()
                .to_string();
        assert!(err.contains("no files"));
    }

    #[test]
    fn file_ops_validates_operation_tag() {
        let bad = json!({"op": "stat"});
        let err = validate_and_transform(&ExecutionBackendType::FileOps, &bad, &HashMap::new())
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
        let (_, inputs) =
            validate_and_transform(&ExecutionBackendType::FileOps, &config, &HashMap::new())
                .unwrap();
        assert!(inputs.is_empty());
    }
}
