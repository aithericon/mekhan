use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerScheduleRequest {
    /// Net ID to signal back to
    pub net_id: String,
    /// Place ID to signal back to
    pub place_id: String,
    /// Unique ID for this timer
    pub correlation_id: Uuid,
    /// Delay in milliseconds
    pub delay_ms: u64,
    /// Optional payload to include in the signal token
    pub payload: serde_json::Value,
    /// Multi-tenancy: the workspace of the net scheduling this timer. Persisted
    /// into the durable timer record so the Clockmaster fires the wake signal
    /// under the right tenant (`petri.{workspace_id}.{net}.signal.{place}`),
    /// even while a single shared Clockmaster watches the process bucket.
    /// Defaults to `"default"` for legacy/SDK callers that omit it.
    #[serde(default = "default_workspace")]
    pub workspace_id: String,
}

fn default_workspace() -> String {
    "default".to_string()
}

#[derive(Debug, Error)]
pub enum TimerError {
    #[error("Timer scheduling failed: {0}")]
    SchedulingFailed(String),
    #[error("Fatal timer error: {0}")]
    Fatal(String),
}

/// Request to cancel a timer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerCancelRequest {
    /// Net ID the timer was scheduled for
    pub net_id: String,
    /// Place ID the timer was scheduled for
    pub place_id: String,
    /// Correlation ID of the timer to cancel
    pub correlation_id: Uuid,
}

#[async_trait::async_trait]
pub trait TimerClient: Send + Sync {
    /// Schedule a durable timer
    async fn schedule(&self, request: TimerScheduleRequest) -> Result<(), TimerError>;

    /// Cancel a scheduled timer (if it hasn't fired yet)
    async fn cancel(&self, request: TimerCancelRequest) -> Result<bool, TimerError>;

    /// Human-readable name of the timer client
    fn name(&self) -> &str;
}
