use std::collections::HashMap;

use tracing::debug;

use aithericon_executor_domain::LogEntry;

use crate::traits::{LogError, LogSink};

/// Loki log sink — pushes log entries to Grafana Loki via HTTP push API.
///
/// Uses Loki's `POST /loki/api/v1/push` endpoint with JSON encoding.
/// Entries are grouped by label set (execution_id + level) into separate
/// Loki streams for efficient querying.
pub struct LokiLogSink {
    client: reqwest::Client,
    push_url: String,
    static_labels: HashMap<String, String>,
}

impl LokiLogSink {
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
impl LogSink for LokiLogSink {
    async fn record(&self, execution_id: &str, entries: &[LogEntry]) -> Result<(), LogError> {
        if entries.is_empty() {
            return Ok(());
        }

        // Group entries by level into separate Loki streams
        let mut streams_by_level: HashMap<String, Vec<[String; 2]>> = HashMap::new();

        for entry in entries {
            let level_str = entry.level.as_str().to_string();
            let ts_ns = entry
                .timestamp
                .timestamp_nanos_opt()
                .unwrap_or_else(|| entry.timestamp.timestamp() * 1_000_000_000)
                .to_string();

            let line =
                serde_json::to_string(entry).map_err(|e| LogError::Serialization(e.to_string()))?;

            streams_by_level
                .entry(level_str)
                .or_default()
                .push([ts_ns, line]);
        }

        let streams: Vec<LokiStream> = streams_by_level
            .into_iter()
            .map(|(level, values)| {
                let mut labels = self.static_labels.clone();
                labels.insert("execution_id".to_string(), execution_id.to_string());
                labels.insert("level".to_string(), level);
                LokiStream {
                    stream: labels,
                    values,
                }
            })
            .collect();

        let payload = LokiPushPayload { streams };

        debug!(
            execution_id,
            entries = entries.len(),
            "pushing log entries to Loki"
        );

        let response = self
            .client
            .post(&self.push_url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| LogError::Transport(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(LogError::Transport(format!(
                "Loki push failed: {status} — {body}"
            )));
        }

        Ok(())
    }

    async fn flush(&self, execution_id: &str) -> Result<(), LogError> {
        debug!(execution_id, "Loki log sink flush (no-op, push-based)");
        Ok(())
    }

    fn name(&self) -> &'static str {
        "loki"
    }
}
