use aithericon_executor_domain::MetricPoint;

/// Errors from metric sink operations.
#[derive(Debug, thiserror::Error)]
pub enum MetricError {
    #[error("serialization failed: {0}")]
    Serialization(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("buffer full: execution {execution_id} has {count} points (max {max})")]
    BufferFull {
        execution_id: String,
        count: usize,
        max: usize,
    },
}

/// Abstraction over metric storage/forwarding backends.
///
/// Implementations receive batches of metric points in real-time as
/// child processes log them via IPC. The sink decides how to store,
/// forward, or aggregate them.
#[async_trait::async_trait]
pub trait MetricSink: Send + Sync + 'static {
    /// Record a batch of metric points for an execution.
    async fn record(&self, execution_id: &str, points: &[MetricPoint]) -> Result<(), MetricError>;

    /// Flush any buffered data for an execution (e.g. on completion).
    async fn flush(&self, execution_id: &str) -> Result<(), MetricError>;

    /// Backend name for diagnostics/logging.
    fn name(&self) -> &'static str;
}
