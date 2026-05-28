use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use aithericon_executor_domain::{
    ExecutionSpec, ExecutorError, InputDeclaration, OutputDeclaration,
};

/// Configuration for the process execution backend.
///
/// Spawns a local process with the given command and arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct ProcessConfig {
    /// The command to run (e.g., "python3", "/usr/bin/train.sh").
    pub command: String,

    /// Command-line arguments.
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables to set.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Working directory. If None, inherits from the executor process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,

    /// Whether to inherit the executor process's environment variables.
    #[serde(default = "default_true")]
    #[cfg_attr(feature = "schema", schema(default = true))]
    pub inherit_env: bool,
}

fn default_true() -> bool {
    true
}

impl ProcessConfig {
    pub fn into_spec(self) -> ExecutionSpec {
        self.into_spec_with_io(vec![], vec![])
    }

    pub fn into_spec_with_io(
        self,
        inputs: Vec<InputDeclaration>,
        outputs: Vec<OutputDeclaration>,
    ) -> ExecutionSpec {
        ExecutionSpec {
            backend: "process".into(),
            inputs,
            outputs,
            config: serde_json::to_value(self).expect("ProcessConfig serialization cannot fail"),
            config_ref: None,
        }
    }

    pub fn from_spec(spec: &ExecutionSpec) -> Result<Self, ExecutorError> {
        crate::from_spec(spec, "process")
    }
}
