use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::event::StagedEvent;
use crate::job::ExecutionSpec;
use crate::run_dir::RunDirectory;

/// Context passed to backends for execution. Accumulated by staging hooks.
///
/// This is pure data — no I/O methods. Backends read from this to configure
/// the execution environment (env vars, working dir, timeout, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunContext {
    /// Execution identifier.
    pub execution_id: String,

    /// What to execute.
    pub spec: ExecutionSpec,

    /// Structured run directory paths.
    pub run_dir: RunDirectory,

    /// Execution timeout.
    #[serde(with = "crate::serde_duration")]
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub timeout: Duration,

    /// Accumulated environment variables (from spec + hooks + backend).
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Metadata echoed through status updates.
    #[serde(default)]
    pub metadata: HashMap<String, String>,

    /// Staged input files: name → local path.
    #[serde(default)]
    pub staged_inputs: HashMap<String, PathBuf>,

    /// Expected output files: name → relative path in outputs_dir.
    #[serde(default)]
    pub expected_outputs: HashMap<String, PathBuf>,

    /// Events collected during staging, flushed after StreamContext is built.
    #[serde(default)]
    pub staged_events: Vec<StagedEvent>,

    /// Opaque backend-specific state.
    #[serde(default)]
    pub backend_state: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_context_serde_roundtrip() {
        let ctx = RunContext {
            execution_id: "exec-789".into(),
            spec: ExecutionSpec {
                backend: "process".into(),
                inputs: vec![],
                outputs: vec![],
                config: serde_json::json!({
                    "command": "python3",
                    "args": ["train.py"],
                    "inherit_env": true
                }),
                    config_ref: None,
            },
            run_dir: RunDirectory::new(&PathBuf::from("/tmp"), "exec-789"),
            timeout: Duration::from_secs(3600),
            env: HashMap::from([("AITHERICON_EXECUTION_ID".into(), "exec-789".into())]),
            metadata: HashMap::from([("user".into(), "alice".into())]),
            staged_inputs: Default::default(),
            expected_outputs: Default::default(),
            staged_events: vec![],
            backend_state: serde_json::Value::Null,
        };

        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: RunContext = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.execution_id, "exec-789");
        assert_eq!(deserialized.timeout, Duration::from_secs(3600));
    }
}
