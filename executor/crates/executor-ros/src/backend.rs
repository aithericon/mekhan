//! `RosBackend` — `ExecutionBackend` impl for ROS interactions over a
//! rosbridge WebSocket.
//!
//! ## Connection model
//!
//! The rosbridge endpoint is **runner-local**: the URL is configured on the
//! executor daemon (`EXECUTOR_ROS__WS_URL`, default `ws://localhost:9090`),
//! not bound per-step as a workspace resource. `RosBackend` holds the URL it
//! was constructed with.
//!
//! ## P1 STUB
//!
//! `execute` reports `ros backend not yet implemented` via a `BackendError`
//! outcome. The rosbridge client + the publish/call/await operations land in
//! P2; the typedef→Port mapping lands alongside.

use std::collections::HashMap;
use std::time::Instant;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::info;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionResult, ExecutionSpec, ExecutorError, RunContext,
};

/// Backend name surfaced to `ExecutionSpec.backend` matching.
pub const BACKEND_NAME: &str = "ros";

/// `ExecutionBackend` implementation for ROS interactions.
///
/// Holds the runner-local rosbridge WebSocket URL. P1 stub — no transport is
/// opened yet.
pub struct RosBackend {
    /// The rosbridge WebSocket URL (e.g. `ws://localhost:9090`).
    pub ws_url: String,
}

impl RosBackend {
    /// Construct a backend bound to a rosbridge WebSocket URL.
    pub fn new(ws_url: impl Into<String>) -> Self {
        Self {
            ws_url: ws_url.into(),
        }
    }
}

#[async_trait]
impl ExecutionBackend for RosBackend {
    fn name(&self) -> &'static str {
        BACKEND_NAME
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == BACKEND_NAME
    }

    async fn prepare(
        &self,
        _job: &ExecutionJob,
        run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // P1 stub: nothing to prepare. The rosbridge connection + template
        // resolution land in P2.
        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        _status_cb: StatusCallback,
        _event_stream: Option<std::sync::Arc<dyn aithericon_executor_backend::traits::EventStream>>,
        _cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let start = Instant::now();
        let message = format!(
            "ros backend not yet implemented (P1 stub; ws_url={})",
            self.ws_url
        );
        info!(ws_url = %self.ws_url, "ros backend execute called (P1 stub)");
        Ok(ExecutionResult {
            outcome: ExecutionOutcome::BackendError {
                message: message.clone(),
            },
            duration: start.elapsed(),
            stdout_tail: None,
            stderr_tail: Some(message),
            artifact_manifest: None,
            outputs: HashMap::new(),
            progress: None,
            run_dir: Some(run_context.run_dir.clone()),
            metrics: None,
            logs: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn backend_supports_and_name() {
        let backend = RosBackend::new("ws://localhost:9090");
        assert_eq!(backend.name(), "ros");
        let spec = ExecutionSpec {
            backend: "ros".into(),
            inputs: vec![],
            outputs: vec![],
            config: Value::Null,
            config_ref: None,
        };
        assert!(backend.supports(&spec));
        let other = ExecutionSpec {
            backend: "http".into(),
            ..spec
        };
        assert!(!backend.supports(&other));
    }
}
