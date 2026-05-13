use std::collections::HashMap;

use tracing::debug;

use aithericon_executor_domain::MetricPoint;

use crate::traits::{MetricError, MetricSink};

/// Loki metric sink — pushes metric points to Grafana Loki via HTTP push API.
///
/// Metrics are encoded as JSON log lines grouped by `metric_name` into
/// separate Loki streams, with a `__type__=metric` label to distinguish
/// them from log entries.
pub struct LokiMetricSink {
    client: reqwest::Client,
    push_url: String,
    static_labels: HashMap<String, String>,
}

impl LokiMetricSink {
    pub fn new(push_url: String, static_labels: HashMap<String, String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            push_url,
            static_labels,
        }
    }
}

/// Loki push API payload.
#[derive(serde::Serialize)]
struct LokiPushPayload {
    streams: Vec<LokiStream>,
}

#[derive(serde::Serialize)]
struct LokiStream {
    stream: HashMap<String, String>,
    values: Vec<[String; 2]>,
}

#[async_trait::async_trait]
impl MetricSink for LokiMetricSink {
    async fn record(&self, execution_id: &str, points: &[MetricPoint]) -> Result<(), MetricError> {
        if points.is_empty() {
            return Ok(());
        }

        // Group points by metric_name into separate Loki streams
        let mut streams_by_name: HashMap<String, Vec<[String; 2]>> = HashMap::new();

        for point in points {
            let ts_ns = point
                .timestamp
                .timestamp_nanos_opt()
                .unwrap_or_else(|| point.timestamp.timestamp() * 1_000_000_000)
                .to_string();

            let line = serde_json::to_string(point)
                .map_err(|e| MetricError::Serialization(e.to_string()))?;

            streams_by_name
                .entry(point.name.clone())
                .or_default()
                .push([ts_ns, line]);
        }

        let streams: Vec<LokiStream> = streams_by_name
            .into_iter()
            .map(|(metric_name, values)| {
                let mut labels = self.static_labels.clone();
                labels.insert("execution_id".to_string(), execution_id.to_string());
                labels.insert("metric_name".to_string(), metric_name);
                labels.insert("__type__".to_string(), "metric".to_string());
                LokiStream {
                    stream: labels,
                    values,
                }
            })
            .collect();

        let payload = LokiPushPayload { streams };

        debug!(
            execution_id,
            points = points.len(),
            "pushing metric points to Loki"
        );

        let response = self
            .client
            .post(&self.push_url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| MetricError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(MetricError::Transport(format!(
                "Loki push failed: {status} — {body}"
            )));
        }

        Ok(())
    }

    async fn flush(&self, execution_id: &str) -> Result<(), MetricError> {
        debug!(execution_id, "Loki metric sink flush (no-op, push-based)");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "loki"
    }
}
