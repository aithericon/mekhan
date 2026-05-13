use std::collections::HashMap;

use aithericon_executor_domain::LogLevel;
use serde::Deserialize;

/// Top-level log forwarding configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct LogsConfig {
    /// Enable log forwarding (default: true).
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Which sinks to activate.
    #[serde(default)]
    pub sinks: Vec<LogSinkConfig>,

    /// Max error entries to keep in summary ring buffer (default: 50).
    #[serde(default = "default_max_recent_errors")]
    pub max_recent_errors: usize,

    /// Filename for JSONL file sink (default: "structured.jsonl").
    #[serde(default = "default_filename")]
    pub filename: String,

    /// Max log entries per execution before rate-limiting kicks in. 0 = unlimited. Default: 100_000.
    #[serde(default = "default_rate_limit_max_entries")]
    pub rate_limit_max_entries: u64,

    /// Entries buffered before flushing to sinks as a batch. Default: 50.
    #[serde(default = "default_sidecar_batch_size")]
    pub batch_size: usize,

    /// Max milliseconds to hold a partial batch before flushing. Default: 500.
    #[serde(default = "default_batch_flush_interval_ms")]
    pub batch_flush_interval_ms: u64,
}

impl Default for LogsConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            sinks: Vec::new(),
            max_recent_errors: default_max_recent_errors(),
            filename: default_filename(),
            rate_limit_max_entries: default_rate_limit_max_entries(),
            batch_size: default_sidecar_batch_size(),
            batch_flush_interval_ms: default_batch_flush_interval_ms(),
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_max_recent_errors() -> usize {
    50
}

fn default_filename() -> String {
    "structured.jsonl".into()
}

fn default_nats_min_level() -> LogLevel {
    LogLevel::Warn
}

fn default_loki_min_level() -> LogLevel {
    LogLevel::Info
}

fn default_batch_size() -> usize {
    100
}

fn default_rate_limit_max_entries() -> u64 {
    100_000
}

fn default_sidecar_batch_size() -> usize {
    50
}

fn default_batch_flush_interval_ms() -> u64 {
    500
}

/// Configuration for individual log sink backends.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LogSinkConfig {
    /// File sink — writes JSONL to run_dir/logs/.
    File {
        /// Minimum level to write. Default: everything (None).
        #[serde(default)]
        min_level: Option<LogLevel>,
    },
    /// NATS sink — publishes LogBatch to NATS subjects.
    Nats {
        /// Minimum level to forward. Default: Warn.
        #[serde(default = "default_nats_min_level")]
        min_level: LogLevel,
        /// Max entries per batch before flush. Default: 100.
        #[serde(default = "default_batch_size")]
        batch_size: usize,
    },
    /// Loki sink — pushes to Grafana Loki HTTP API.
    Loki {
        /// Loki push URL (e.g. "http://loki:3100/loki/api/v1/push").
        url: String,
        /// Minimum level to push. Default: Info.
        #[serde(default = "default_loki_min_level")]
        min_level: LogLevel,
        /// Static labels added to every log stream.
        #[serde(default)]
        static_labels: HashMap<String, String>,
        /// Max entries per batch before flush. Default: 100.
        #[serde(default = "default_batch_size")]
        batch_size: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = LogsConfig::default();
        assert!(config.enabled);
        assert!(config.sinks.is_empty());
        assert_eq!(config.max_recent_errors, 50);
        assert_eq!(config.filename, "structured.jsonl");
        assert_eq!(config.rate_limit_max_entries, 100_000);
        assert_eq!(config.batch_size, 50);
        assert_eq!(config.batch_flush_interval_ms, 500);
    }

    #[test]
    fn deserialize_file_sink() {
        let json = r#"{ "type": "file" }"#;
        let config: LogSinkConfig = serde_json::from_str(json).unwrap();
        match config {
            LogSinkConfig::File { min_level } => assert!(min_level.is_none()),
            _ => panic!("expected File variant"),
        }
    }

    #[test]
    fn deserialize_nats_sink() {
        let json = r#"{ "type": "nats" }"#;
        let config: LogSinkConfig = serde_json::from_str(json).unwrap();
        match config {
            LogSinkConfig::Nats {
                min_level,
                batch_size,
            } => {
                assert_eq!(min_level, LogLevel::Warn);
                assert_eq!(batch_size, 100);
            }
            _ => panic!("expected Nats variant"),
        }
    }

    #[test]
    fn deserialize_loki_sink() {
        let json = r#"{
            "type": "loki",
            "url": "http://loki:3100/loki/api/v1/push",
            "static_labels": { "service": "executor" }
        }"#;
        let config: LogSinkConfig = serde_json::from_str(json).unwrap();
        match config {
            LogSinkConfig::Loki {
                url,
                min_level,
                static_labels,
                batch_size,
            } => {
                assert_eq!(url, "http://loki:3100/loki/api/v1/push");
                assert_eq!(min_level, LogLevel::Info);
                assert_eq!(static_labels.get("service").unwrap(), "executor");
                assert_eq!(batch_size, 100);
            }
            _ => panic!("expected Loki variant"),
        }
    }
}
