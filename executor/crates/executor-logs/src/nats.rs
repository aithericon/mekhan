use aithericon_executor_domain::{LogBatch, LogEntry};
use chrono::Utc;
use tracing::debug;

use crate::traits::{LogError, LogSink};

/// NATS log sink — publishes LogBatch as JSON to NATS subjects.
///
/// Subject pattern: `executor.logs.{execution_id}`
///
/// Downstream consumers (Loki writer, log aggregator, dashboard) subscribe
/// to these subjects for real-time log ingestion.
pub struct NatsLogSink {
    client: async_nats::Client,
}

impl NatsLogSink {
    pub fn new(client: async_nats::Client) -> Self {
        Self { client }
    }

    fn subject(execution_id: &str) -> String {
        format!("executor.logs.{}", execution_id.replace(['.', ' '], "_"))
    }
}

#[async_trait::async_trait]
impl LogSink for NatsLogSink {
    async fn record(&self, execution_id: &str, entries: &[LogEntry]) -> Result<(), LogError> {
        if entries.is_empty() {
            return Ok(());
        }

        let batch = LogBatch {
            execution_id: execution_id.to_string(),
            entries: entries.to_vec(),
            logged_at: Utc::now(),
        };

        let payload =
            serde_json::to_vec(&batch).map_err(|e| LogError::Serialization(e.to_string()))?;

        let subject = Self::subject(execution_id);
        debug!(
            subject,
            entries = entries.len(),
            "publishing log batch to NATS"
        );

        self.client
            .publish(subject, payload.into())
            .await
            .map_err(|e| LogError::Transport(e.to_string()))?;

        Ok(())
    }

    async fn flush(&self, execution_id: &str) -> Result<(), LogError> {
        debug!(execution_id, "flushing NATS log sink");
        self.client
            .flush()
            .await
            .map_err(|e| LogError::Transport(e.to_string()))?;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "nats"
    }
}
