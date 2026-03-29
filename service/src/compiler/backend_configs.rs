//! Executor backend config validation and transformation.
//!
//! Validates the frontend's editor config against the executor's expected types
//! and transforms editor-specific fields (e.g., `scriptContent` for Python inline
//! code) into the executor's native format.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use aithericon_executor_backend_configs::python::{
    default_python, PythonConfig, INLINE_SCRIPT_NAME,
};

use super::CompileError;

/// Represents a staged input that the executor should receive alongside the config.
#[derive(Debug, Clone, Serialize)]
pub struct StagedInput {
    pub name: String,
    pub source: StagedInputSource,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StagedInputSource {
    Raw { content: String },
}

/// Editor-specific Python config that extends `PythonConfig` with
/// `script_content` (inline code) and `node_file` (Yjs file reference).
///
/// The frontend sends one of three modes:
/// - `script`: filename in inputs directory (pass through)
/// - `scriptContent`: inline code (transform to `__script__.py` + Raw input)
/// - `nodeFile`: Yjs CRDT file reference (resolved before compilation)
#[derive(Debug, Clone, Deserialize)]
pub struct EditorPythonConfig {
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default, alias = "scriptContent")]
    pub script_content: Option<String>,
    #[serde(default, alias = "nodeFile")]
    pub node_file: Option<String>,
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

fn default_true() -> bool {
    true
}

impl EditorPythonConfig {
    /// Transform editor config into executor-compatible (config Value, staged inputs).
    pub fn to_executor_config(self) -> Result<(Value, Vec<StagedInput>), CompileError> {
        let mut inputs = Vec::new();

        let script = if let Some(code) = &self.script_content {
            // Inline mode: stage code as __script__.py
            inputs.push(StagedInput {
                name: INLINE_SCRIPT_NAME.to_string(),
                source: StagedInputSource::Raw {
                    content: code.clone(),
                },
            });
            INLINE_SCRIPT_NAME.to_string()
        } else if let Some(node_file) = &self.node_file {
            // Node-file mode: the file content should have been resolved and
            // placed into script_content before reaching the compiler.
            // If we get here with node_file but no content, that's an error.
            return Err(CompileError::Validation(format!(
                "python config: nodeFile '{}' referenced but content not resolved. \
                 Ensure node files are resolved before compilation.",
                node_file
            )));
        } else if let Some(script) = &self.script {
            if script.is_empty() {
                return Err(CompileError::Validation(
                    "python config: 'script' is empty. Provide a script filename, \
                     inline code via 'scriptContent', or a node file via 'nodeFile'."
                        .into(),
                ));
            }
            script.clone()
        } else {
            return Err(CompileError::Validation(
                "python config: one of 'script', 'scriptContent', or 'nodeFile' is required"
                    .into(),
            ));
        };

        let executor_config = PythonConfig {
            script,
            python: self.python,
            requirements: self.requirements,
            virtualenv: self.virtualenv,
            env: self.env,
            working_dir: self.working_dir,
            inherit_env: self.inherit_env,
            sdk: self.sdk,
        };

        let config_value = serde_json::to_value(&executor_config)
            .map_err(|e| CompileError::Compilation(format!("failed to serialize python config: {e}")))?;

        Ok((config_value, inputs))
    }
}

/// Validate and transform an editor backend config into the executor's expected format.
///
/// Returns (validated config as Value, staged inputs to include in the ExecutionSpec).
pub fn validate_and_transform(
    backend_type: &str,
    config: &Value,
) -> Result<(Value, Vec<StagedInput>), CompileError> {
    match backend_type {
        "python" => {
            let editor_config: EditorPythonConfig =
                serde_json::from_value(config.clone()).map_err(|e| {
                    CompileError::Validation(format!("invalid python config: {e}"))
                })?;
            editor_config.to_executor_config()
        }
        "process" => {
            // Validate by deserializing, then pass through
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
        // Add validation when their configs are added to the configs crate.
        _ => Ok((config.clone(), vec![])),
    }
}
