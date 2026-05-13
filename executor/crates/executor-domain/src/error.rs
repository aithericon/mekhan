use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("failed to spawn process: {0}")]
    SpawnFailed(#[source] std::io::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("execution timed out")]
    Timeout,

    #[error("execution cancelled")]
    Cancelled,

    #[error("unsupported execution spec: {0}")]
    UnsupportedSpec(String),

    #[error("failed to report status: {0}")]
    ReportFailed(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("staging failed: {0}")]
    StagingFailed(String),

    #[error("input not found: {0}")]
    InputNotFound(String),

    #[error("required output missing: {0}")]
    OutputMissing(String),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("run directory error: {0}")]
    RunDirectory(String),

    #[error("secret resolution failed: {0}")]
    SecretResolutionFailed(String),
}
