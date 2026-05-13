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
