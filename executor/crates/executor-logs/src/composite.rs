use std::sync::Arc;

use aithericon_executor_domain::LogEntry;
use tracing::warn;

use crate::traits::{LogError, LogSink};

/// Composite sink that fans out to multiple underlying sinks.
///
/// Records and flushes are forwarded to all child sinks. If any sink
/// fails, the error is logged but does not prevent other sinks from
/// receiving the data. The first error encountered is returned.
pub struct CompositeLogSink {
    sinks: Vec<Arc<dyn LogSink>>,
}

impl CompositeLogSink {
    pub fn new(sinks: Vec<Arc<dyn LogSink>>) -> Self {
        Self { sinks }
    }
}

#[async_trait::async_trait]
impl LogSink for CompositeLogSink {
    async fn record(&self, execution_id: &str, entries: &[LogEntry]) -> Result<(), LogError> {
        let mut first_err: Option<LogError> = None;

        for sink in &self.sinks {
            if let Err(e) = sink.record(execution_id, entries).await {
                warn!(sink = sink.name(), error = %e, "log sink record failed");
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn flush(&self, execution_id: &str) -> Result<(), LogError> {
        let mut first_err: Option<LogError> = None;

        for sink in &self.sinks {
            if let Err(e) = sink.flush(execution_id).await {
                warn!(sink = sink.name(), error = %e, "log sink flush failed");
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn name(&self) -> &'static str {
        "composite"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::FileLogSink;
    use aithericon_executor_domain::LogLevel;
    use chrono::Utc;

    fn make_entry(msg: &str) -> LogEntry {
        LogEntry {
            level: LogLevel::Info,
            message: msg.into(),
            timestamp: Utc::now(),
            fields: Default::default(),
            repeat_count: 1,
        }
    }

    #[tokio::test]
    async fn forwards_to_all_sinks() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();

        let sink_a = Arc::new(FileLogSink::new(
            dir_a.path().to_path_buf(),
            "test.jsonl".into(),
        ));
        let sink_b = Arc::new(FileLogSink::new(
            dir_b.path().to_path_buf(),
            "test.jsonl".into(),
        ));

        let composite = CompositeLogSink::new(vec![
            sink_a.clone() as Arc<dyn LogSink>,
            sink_b.clone() as Arc<dyn LogSink>,
        ]);

        composite
            .record("exec-1", &[make_entry("hello")])
            .await
            .unwrap();

        composite.flush("exec-1").await.unwrap();

        let a = tokio::fs::read_to_string(dir_a.path().join("runs/exec-1/logs/test.jsonl"))
            .await
            .unwrap();
        let b = tokio::fs::read_to_string(dir_b.path().join("runs/exec-1/logs/test.jsonl"))
            .await
            .unwrap();

        assert_eq!(a.trim().lines().count(), 1);
        assert_eq!(b.trim().lines().count(), 1);
    }
}
