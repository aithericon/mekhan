use aithericon_executor_domain::LogEntry;

/// Errors from log sink operations.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("serialization failed: {0}")]
    Serialization(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("I/O error: {0}")]
    Io(String),
}

/// Abstraction over log storage/forwarding backends.
///
/// Implementations receive log entries in real-time as child processes
/// emit them via IPC. The sink decides how to store, forward, or aggregate.
#[async_trait::async_trait]
pub trait LogSink: Send + Sync + 'static {
    /// Record a batch of log entries for an execution.
    async fn record(&self, execution_id: &str, entries: &[LogEntry]) -> Result<(), LogError>;

    /// Flush any buffered data for an execution (e.g. on completion).
    async fn flush(&self, execution_id: &str) -> Result<(), LogError>;

    /// Backend name for diagnostics/logging.
    fn name(&self) -> &'static str;
}
