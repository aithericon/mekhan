use aithericon_executor_domain::{MetricBatch, MetricPoint};
use chrono::Utc;
use tracing::debug;

use crate::traits::{MetricError, MetricSink};

/// NATS metric sink — publishes MetricBatch as JSON to NATS subjects.
///
/// Subject pattern: `executor.metrics.{execution_id}`
///
/// Downstream consumers (InfluxDB writer, Prometheus exporter, dashboard)
/// subscribe to these subjects for real-time metric ingestion.
pub struct NatsMetricSink {
    client: async_nats::Client,
}

impl NatsMetricSink {
    pub fn new(client: async_nats::Client) -> Self {
        Self { client }
    }

    fn subject(execution_id: &str) -> String {
        format!("executor.metrics.{}", execution_id.replace(['.', ' '], "_"))
    }
}

#[async_trait::async_trait]
impl MetricSink for NatsMetricSink {
    async fn record(&self, execution_id: &str, points: &[MetricPoint]) -> Result<(), MetricError> {
        if points.is_empty() {
            return Ok(());
        }

        let batch = MetricBatch {
            execution_id: execution_id.to_string(),
            points: points.to_vec(),
            logged_at: Utc::now(),
        };

        let payload =
            serde_json::to_vec(&batch).map_err(|e| MetricError::Serialization(e.to_string()))?;

        let subject = Self::subject(execution_id);
        debug!(
            subject,
            points = points.len(),
            "publishing metric batch to NATS"
        );

        self.client
            .publish(subject, payload.into())
            .await
            .map_err(|e| MetricError::Transport(e.to_string()))?;

        Ok(())
    }

    async fn flush(&self, execution_id: &str) -> Result<(), MetricError> {
        debug!(execution_id, "flushing NATS metric sink");
        self.client
            .flush()
            .await
            .map_err(|e| MetricError::Transport(e.to_string()))?;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "nats"
    }
}
