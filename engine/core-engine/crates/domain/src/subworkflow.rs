//! Subworkflow cancellation contract.
//!
//! Mirrors `timer::TimerClient`: domain defines the trait, application takes
//! `Arc<dyn SubWorkflowCancellor>`, api wires the concrete impl (a thin
//! wrapper over `NetRegistry::terminate`). This keeps the dependency direction
//! inward — `application` never sees `api`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubWorkflowCancelRequest {
    pub child_net_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Error)]
pub enum SubWorkflowCancelError {
    #[error("Subworkflow cancel failed: {0}")]
    CancellationFailed(String),
    #[error("Fatal subworkflow cancel error: {0}")]
    Fatal(String),
}

#[async_trait::async_trait]
pub trait SubWorkflowCancellor: Send + Sync {
    /// Terminate a running child net. Returns `true` when a net was found and
    /// cancelled, `false` when the net was already terminal / unknown.
    async fn cancel(
        &self,
        request: SubWorkflowCancelRequest,
    ) -> Result<bool, SubWorkflowCancelError>;

    fn name(&self) -> &str;
}
