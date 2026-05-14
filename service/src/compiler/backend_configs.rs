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

use aithericon_executor_backend_configs::python::{default_python, PythonConfig};
use aithericon_executor_domain::{InputDeclaration, InputSource};

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

fn default_true() -> bool {
    true
}

impl EditorPythonConfig {
    /// Build the executor-side `PythonConfig` plus the list of staged inputs.
    ///
    /// `node_files` maps filename -> source (caller decides whether each file
    /// is staged from storage, embedded raw, etc).
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
            let mut available: Vec<&String> = node_files.keys().collect();
            available.sort();
            return Err(CompileError::Validation(format!(
                "python config: entrypoint '{}' not found in node files (have: {:?})",
                self.entrypoint, available
            )));
        }

        let mut inputs: Vec<InputDeclaration> = node_files
            .iter()
            .map(|(name, source)| InputDeclaration {
                name: name.clone(),
                source: source.clone(),
                required: true,
            })
            .collect();
        // Deterministic ordering so the AIR doesn't churn between compiles.
        inputs.sort_by(|a, b| a.name.cmp(&b.name));

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

/// Validate and transform an editor backend config into the executor's expected format.
///
/// Returns (validated config as Value, inputs to stage in the ExecutionSpec).
/// `node_files` is the per-node map of filename → source (consulted by backends
/// that stage files; ignored otherwise).
pub fn validate_and_transform(
    backend_type: &str,
    config: &Value,
    node_files: &HashMap<String, InputSource>,
) -> Result<(Value, Vec<InputDeclaration>), CompileError> {
    match backend_type {
        "python" => {
            let editor_config: EditorPythonConfig = serde_json::from_value(config.clone())
                .map_err(|e| CompileError::Validation(format!("invalid python config: {e}")))?;
            editor_config.to_executor_config(node_files)
        }
        "process" => {
            let _: aithericon_executor_backend_configs::process::ProcessConfig =
                serde_json::from_value(config.clone()).map_err(|e| {
                    CompileError::Validation(format!("invalid process config: {e}"))
                })?;
            Ok((config.clone(), vec![]))
        }
        "docker" => {
            let _: aithericon_executor_backend_configs::docker::DockerConfig =
                serde_json::from_value(config.clone()).map_err(|e| {
                    CompileError::Validation(format!("invalid docker config: {e}"))
                })?;
            Ok((config.clone(), vec![]))
        }
        "http" => {
            let _: aithericon_executor_backend_configs::http::HttpConfig =
                serde_json::from_value(config.clone()).map_err(|e| {
                    CompileError::Validation(format!("invalid http config: {e}"))
                })?;
            Ok((config.clone(), vec![]))
        }
        // LLM, file_ops, kreuzberg — pass through unvalidated for now.
        _ => Ok((config.clone(), vec![])),
    }
}
