pub mod docker;
pub mod file_ops;
pub mod http;
pub mod kreuzberg;
pub mod llm;
pub mod loki;
pub mod postgres;
pub mod process;
pub mod prometheus;
pub mod python;
pub mod ros;
pub mod smtp;

use serde::de::DeserializeOwned;

use aithericon_executor_domain::{ExecutionSpec, ExecutorError};

/// Deserialize a backend config DTO from a spec's `config` payload.
///
/// Every backend's `from_spec` is the same `from_value(spec.config.clone())`
/// with a backend-named [`ExecutorError::Config`] message; this is the single
/// implementation they all delegate to. `name` is the backend label used in
/// the error (e.g. `"docker"`, `"http"`).
pub fn from_spec<T: DeserializeOwned>(
    spec: &ExecutionSpec,
    name: &str,
) -> Result<T, ExecutorError> {
    serde_json::from_value(spec.config.clone())
        .map_err(|e| ExecutorError::Config(format!("invalid {name} backend config: {e}")))
}
