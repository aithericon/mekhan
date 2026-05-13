use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use aithericon_executor_domain::{
    ExecutionSpec, ExecutorError, InputDeclaration, InputSource, OutputDeclaration,
};

/// Configuration for the Python execution backend.
///
/// The `script` field names the Python file to execute, relative to the inputs
/// directory. For inline code, use [`PythonConfig::inline_spec`] which stages
/// the code as a `Raw` input automatically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PythonConfig {
    /// Name of the Python script file in the inputs directory.
    pub script: String,

    /// Python command/binary to use (e.g., "python3", "python3.11").
    #[serde(default = "default_python")]
    pub python: String,

    /// Pip packages to install before execution.
    #[serde(default)]
    pub requirements: Vec<String>,

    /// Whether to create an isolated virtualenv for this execution.
    #[serde(default)]
    pub virtualenv: bool,

    /// Additional environment variables.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Working directory (defaults to run_dir root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,

    /// Whether to inherit the executor process's environment variables.
    #[serde(default = "default_true")]
    pub inherit_env: bool,

    /// Whether to auto-install the aithericon SDK in the virtualenv.
    #[serde(default = "default_true")]
    pub sdk: bool,
}

pub fn default_python() -> String {
    "python3".to_string()
}

fn default_true() -> bool {
    true
}

/// The standard filename used for inline code staged as a script input.
pub const INLINE_SCRIPT_NAME: &str = "__script__.py";

impl PythonConfig {
    pub fn into_spec(self) -> ExecutionSpec {
        self.into_spec_with_io(vec![], vec![])
    }

    pub fn into_spec_with_io(
        self,
        inputs: Vec<InputDeclaration>,
        outputs: Vec<OutputDeclaration>,
    ) -> ExecutionSpec {
        ExecutionSpec {
            backend: "python".into(),
            inputs,
            outputs,
            config: serde_json::to_value(self).expect("PythonConfig serialization cannot fail"),
        }
    }

    pub fn inline_spec(code: impl Into<String>) -> ExecutionSpec {
        Self::inline_spec_with_io(code, vec![], vec![])
    }

    pub fn inline_spec_with_io(
        code: impl Into<String>,
        mut inputs: Vec<InputDeclaration>,
        outputs: Vec<OutputDeclaration>,
    ) -> ExecutionSpec {
        let config = PythonConfig {
            script: INLINE_SCRIPT_NAME.into(),
            python: default_python(),
            requirements: vec![],
            virtualenv: false,
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
            sdk: false,
        };
        inputs.insert(
            0,
            InputDeclaration {
                name: INLINE_SCRIPT_NAME.into(),
                source: InputSource::Raw {
                    content: code.into(),
                },
                required: true,
            },
        );
        config.into_spec_with_io(inputs, outputs)
    }

    pub fn from_spec(spec: &ExecutionSpec) -> Result<Self, ExecutorError> {
        serde_json::from_value(spec.config.clone())
            .map_err(|e| ExecutorError::Config(format!("invalid python backend config: {e}")))
    }
}
