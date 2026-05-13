use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::debug;

use aithericon_executor_domain::LogEntry;

use crate::traits::{LogError, LogSink};

/// File-based log sink — writes structured JSONL to a per-execution file.
///
/// Path pattern: `{base_dir}/runs/{execution_id}/logs/{filename}`
///
/// Each execution gets its own file writer, opened lazily on first write.
/// Writers are flushed and closed when `flush()` is called.
pub struct FileLogSink {
    base_dir: PathBuf,
    filename: String,
    writers: Arc<Mutex<HashMap<String, tokio::io::BufWriter<tokio::fs::File>>>>,
}

impl FileLogSink {
    pub fn new(base_dir: PathBuf, filename: String) -> Self {
        Self {
            base_dir,
            filename,
            writers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn log_path(&self, execution_id: &str) -> PathBuf {
        self.base_dir
            .join("runs")
            .join(execution_id)
            .join("logs")
            .join(&self.filename)
    }
}

#[async_trait::async_trait]
impl LogSink for FileLogSink {
    async fn record(&self, execution_id: &str, entries: &[LogEntry]) -> Result<(), LogError> {
        if entries.is_empty() {
            return Ok(());
        }

        let mut writers = self.writers.lock().await;

        if !writers.contains_key(execution_id) {
            let path = self.log_path(execution_id);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| LogError::Io(format!("failed to create log dir: {e}")))?;
            }
            let file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
                .map_err(|e| LogError::Io(format!("failed to open log file: {e}")))?;
            writers.insert(execution_id.to_string(), tokio::io::BufWriter::new(file));
            debug!(execution_id, path = %path.display(), "opened log file");
        }

        let writer = writers.get_mut(execution_id).unwrap();

        for entry in entries {
            let line =
                serde_json::to_string(entry).map_err(|e| LogError::Serialization(e.to_string()))?;
            writer
                .write_all(line.as_bytes())
                .await
                .map_err(|e| LogError::Io(e.to_string()))?;
            writer
                .write_all(b"\n")
                .await
                .map_err(|e| LogError::Io(e.to_string()))?;
        }

        Ok(())
    }

    async fn flush(&self, execution_id: &str) -> Result<(), LogError> {
        let mut writers = self.writers.lock().await;
        if let Some(mut writer) = writers.remove(execution_id) {
            writer
                .flush()
                .await
                .map_err(|e| LogError::Io(e.to_string()))?;
            debug!(execution_id, "flushed and closed log file");
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "file"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_entry(level: aithericon_executor_domain::LogLevel, msg: &str) -> LogEntry {
        LogEntry {
            level,
            message: msg.into(),
            timestamp: Utc::now(),
            fields: Default::default(),
            repeat_count: 1,
        }
    }

    #[tokio::test]
    async fn write_and_flush() {
        use aithericon_executor_domain::LogLevel;

        let dir = tempfile::tempdir().unwrap();
        let sink = FileLogSink::new(dir.path().to_path_buf(), "test.jsonl".into());

        sink.record(
            "exec-1",
            &[
                make_entry(LogLevel::Info, "hello"),
                make_entry(LogLevel::Error, "oops"),
            ],
        )
        .await
        .unwrap();

        sink.flush("exec-1").await.unwrap();

        let path = dir.path().join("runs/exec-1/logs/test.jsonl");
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = contents.trim().lines().collect();
        assert_eq!(lines.len(), 2);

        let entry: LogEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(entry.level, LogLevel::Info);
        assert_eq!(entry.message, "hello");

        let entry: LogEntry = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(entry.level, LogLevel::Error);
        assert_eq!(entry.message, "oops");
    }

    #[tokio::test]
    async fn append_across_calls() {
        use aithericon_executor_domain::LogLevel;

        let dir = tempfile::tempdir().unwrap();
        let sink = FileLogSink::new(dir.path().to_path_buf(), "test.jsonl".into());

        sink.record("exec-1", &[make_entry(LogLevel::Info, "first")])
            .await
            .unwrap();
        sink.record("exec-1", &[make_entry(LogLevel::Warn, "second")])
            .await
            .unwrap();

        sink.flush("exec-1").await.unwrap();

        let path = dir.path().join("runs/exec-1/logs/test.jsonl");
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(contents.trim().lines().count(), 2);
    }

    #[tokio::test]
    async fn separate_executions() {
        use aithericon_executor_domain::LogLevel;

        let dir = tempfile::tempdir().unwrap();
        let sink = FileLogSink::new(dir.path().to_path_buf(), "test.jsonl".into());

        sink.record("exec-a", &[make_entry(LogLevel::Info, "a")])
            .await
            .unwrap();
        sink.record("exec-b", &[make_entry(LogLevel::Info, "b")])
            .await
            .unwrap();

        sink.flush("exec-a").await.unwrap();
        sink.flush("exec-b").await.unwrap();

        let a = tokio::fs::read_to_string(dir.path().join("runs/exec-a/logs/test.jsonl"))
            .await
            .unwrap();
        let b = tokio::fs::read_to_string(dir.path().join("runs/exec-b/logs/test.jsonl"))
            .await
            .unwrap();

        assert_eq!(a.trim().lines().count(), 1);
        assert_eq!(b.trim().lines().count(), 1);
    }

    #[tokio::test]
    async fn empty_record_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let sink = FileLogSink::new(dir.path().to_path_buf(), "test.jsonl".into());

        sink.record("exec-1", &[]).await.unwrap();
        sink.flush("exec-1").await.unwrap();

        // No file should be created for empty records
        let path = dir.path().join("runs/exec-1/logs/test.jsonl");
        assert!(!path.exists());
    }
}
