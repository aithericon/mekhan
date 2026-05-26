use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use aithericon_executor_domain::{
    ExecutionSpec, ExecutorError, InputDeclaration, OutputDeclaration,
};

/// Configuration for the Docker execution backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerConfig {
    /// Docker image to use (e.g., "python:3.12-slim", "alpine:3.19").
    pub image: String,

    /// Command to run in the container (Docker CMD).
    #[serde(default)]
    pub command: Vec<String>,

    /// Override the image entrypoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,

    /// Environment variables to set in the container.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Image pull policy.
    #[serde(default)]
    pub pull_policy: PullPolicy,

    /// Optional resource limits for the container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_limits: Option<ResourceLimits>,

    /// Docker network mode (e.g., "host", "bridge", "none").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_mode: Option<String>,

    /// Additional volume mounts in "host:container" format.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_volumes: Vec<String>,

    /// Remove the container after execution (equivalent to --rm).
    #[serde(default = "default_true")]
    pub remove_container: bool,
}

fn default_true() -> bool {
    true
}

/// Image pull policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullPolicy {
    Always,
    #[default]
    IfNotPresent,
    Never,
}

/// Resource limits for the container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Memory limit in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<i64>,

    /// CPU shares (relative weight).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_shares: Option<i64>,

    /// CPU quota in microseconds per cpu-period (100ms default period).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_quota: Option<i64>,
}

impl DockerConfig {
    pub fn into_spec(self) -> ExecutionSpec {
        self.into_spec_with_io(vec![], vec![])
    }

    pub fn into_spec_with_io(
        self,
        inputs: Vec<InputDeclaration>,
        outputs: Vec<OutputDeclaration>,
    ) -> ExecutionSpec {
        ExecutionSpec {
            backend: "docker".into(),
            inputs,
            outputs,
            config: serde_json::to_value(self).expect("DockerConfig serialization cannot fail"),
            config_ref: None,
        }
    }

    pub fn from_spec(spec: &ExecutionSpec) -> Result<Self, ExecutorError> {
        serde_json::from_value(spec.config.clone())
            .map_err(|e| ExecutorError::Config(format!("invalid docker backend config: {e}")))
    }
}
