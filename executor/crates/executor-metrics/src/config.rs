use serde::Deserialize;

/// Top-level metrics configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricsConfig {
    /// Enable metrics collection (default: true).
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Which sinks to activate.
    #[serde(default)]
    pub sinks: Vec<MetricSinkConfig>,

    /// Max metric points to buffer in-memory per execution (default: 100_000).
    #[serde(default = "default_max_buffer")]
    pub max_buffer_per_execution: usize,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            sinks: Vec::new(),
            max_buffer_per_execution: default_max_buffer(),
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_max_buffer() -> usize {
    100_000
}

/// Configuration for individual metric sink backends.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MetricSinkConfig {
    /// In-memory sink — stores points in a HashMap behind a RwLock.
    Memory,
    /// NATS sink — publishes MetricBatch as JSON to NATS subjects.
    Nats,
    /// Loki sink — pushes metric points to Grafana Loki HTTP API.
    Loki {
        /// Loki push URL (e.g. "http://loki:3100/loki/api/v1/push").
        url: String,
        /// Static labels added to every metric stream.
        #[serde(default)]
        static_labels: std::collections::HashMap<String, String>,
    },
}
