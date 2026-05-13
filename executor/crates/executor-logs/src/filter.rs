use std::sync::Arc;

use aithericon_executor_domain::{LogEntry, LogLevel};

use crate::traits::{LogError, LogSink};

/// Wrapper that filters log entries below a minimum level before
/// forwarding to the inner sink.
pub struct LevelFilterSink {
    inner: Arc<dyn LogSink>,
    min_level: LogLevel,
}

impl LevelFilterSink {
    pub fn new(inner: Arc<dyn LogSink>, min_level: LogLevel) -> Self {
        Self { inner, min_level }
    }
}

#[async_trait::async_trait]
impl LogSink for LevelFilterSink {
    async fn record(&self, execution_id: &str, entries: &[LogEntry]) -> Result<(), LogError> {
        let filtered: Vec<&LogEntry> = entries
            .iter()
            .filter(|e| e.level >= self.min_level)
            .collect();

        if filtered.is_empty() {
            return Ok(());
        }

        // We need owned entries for the inner sink
        let owned: Vec<LogEntry> = filtered.into_iter().cloned().collect();
        self.inner.record(execution_id, &owned).await
    }

    async fn flush(&self, execution_id: &str) -> Result<(), LogError> {
        self.inner.flush(execution_id).await
    }

    fn name(&self) -> &'static str {
        "level-filter"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::FileLogSink;
    use chrono::Utc;

    fn make_entry(level: LogLevel, msg: &str) -> LogEntry {
        LogEntry {
            level,
            message: msg.into(),
            timestamp: Utc::now(),
            fields: Default::default(),
            repeat_count: 1,
        }
    }

    #[tokio::test]
    async fn filters_below_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let file_sink = Arc::new(FileLogSink::new(
            dir.path().to_path_buf(),
            "test.jsonl".into(),
        ));

        let filter = LevelFilterSink::new(file_sink.clone(), LogLevel::Warn);

        filter
            .record(
                "exec-1",
                &[
                    make_entry(LogLevel::Debug, "debug msg"),
                    make_entry(LogLevel::Info, "info msg"),
                    make_entry(LogLevel::Warn, "warn msg"),
                    make_entry(LogLevel::Error, "error msg"),
                ],
            )
            .await
            .unwrap();

        filter.flush("exec-1").await.unwrap();

        let path = dir.path().join("runs/exec-1/logs/test.jsonl");
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = contents.trim().lines().collect();
        assert_eq!(lines.len(), 2, "only warn and error should pass through");

        let entry: LogEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(entry.level, LogLevel::Warn);

        let entry: LogEntry = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(entry.level, LogLevel::Error);
    }

    #[tokio::test]
    async fn all_filtered_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let file_sink = Arc::new(FileLogSink::new(
            dir.path().to_path_buf(),
            "test.jsonl".into(),
        ));

        let filter = LevelFilterSink::new(file_sink, LogLevel::Error);

        filter
            .record(
                "exec-1",
                &[
                    make_entry(LogLevel::Debug, "debug"),
                    make_entry(LogLevel::Info, "info"),
                    make_entry(LogLevel::Warn, "warn"),
                ],
            )
            .await
            .unwrap();

        filter.flush("exec-1").await.unwrap();

        let path = dir.path().join("runs/exec-1/logs/test.jsonl");
        assert!(
            !path.exists(),
            "no file should be created when all filtered"
        );
    }
}
