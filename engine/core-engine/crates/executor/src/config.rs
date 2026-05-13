//! Executor configuration from environment variables.

use serde::Deserialize;

/// Executor integration configuration.
///
/// Intermediate struct for config-rs deserialization is not needed here
/// because the executor config is simple enough for direct env var reads.
///
/// ## Environment Variables
///
/// - `EXECUTOR_NATS_URL` - NATS URL for the executor (default: same as `NATS_URL`)
/// - `EXECUTOR_NAMESPACE` - apalis-nats job namespace (default: "executor_jobs")
/// - `EXECUTOR_STATUS_STREAM` - JetStream stream for status updates (default: "EXECUTOR_STATUS")
/// - `EXECUTOR_EVENTS_STREAM` - JetStream stream for mid-execution events (default: "EXECUTOR_EVENTS")
#[derive(Clone, Debug, Deserialize)]
pub struct ExecutorConfig {
    /// NATS URL for the executor. Falls back to `NATS_URL` if not set.
    pub nats_url: String,
    /// apalis-nats job namespace.
    pub namespace: String,
    /// JetStream stream name for status updates.
    pub status_stream: String,
    /// JetStream stream name for mid-execution events.
    pub events_stream: String,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            nats_url: "nats://localhost:4333".to_string(),
            namespace: "executor_jobs".to_string(),
            status_stream: "EXECUTOR_STATUS".to_string(),
            events_stream: "EXECUTOR_EVENTS".to_string(),
        }
    }
}

impl ExecutorConfig {
    /// Create configuration from environment variables.
    ///
    /// Returns `None` if neither `EXECUTOR_NATS_URL` nor `NATS_URL` is set,
    /// indicating the executor is not configured.
    pub fn from_env() -> Option<Self> {
        let nats_url = std::env::var("EXECUTOR_NATS_URL")
            .or_else(|_| std::env::var("NATS_URL"))
            .ok()?;

        Some(Self {
            nats_url,
            namespace: std::env::var("EXECUTOR_NAMESPACE")
                .unwrap_or_else(|_| "executor_jobs".to_string()),
            status_stream: std::env::var("EXECUTOR_STATUS_STREAM")
                .unwrap_or_else(|_| "EXECUTOR_STATUS".to_string()),
            events_stream: std::env::var("EXECUTOR_EVENTS_STREAM")
                .unwrap_or_else(|_| "EXECUTOR_EVENTS".to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ExecutorConfig::default();
        assert_eq!(config.nats_url, "nats://localhost:4333");
        assert_eq!(config.namespace, "executor_jobs");
        assert_eq!(config.status_stream, "EXECUTOR_STATUS");
        assert_eq!(config.events_stream, "EXECUTOR_EVENTS");
    }
}
