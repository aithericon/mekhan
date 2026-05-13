use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Log severity level — mirrors tracing levels and the FlatBuffer LogLevel enum.
///
/// Derives `Ord` with Trace < Debug < Info < Warn < Error so that
/// level filtering (`>= Warn`) is a simple comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single structured log entry from a child process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LogEntry {
    /// Severity level.
    pub level: LogLevel,

    /// Human-readable message.
    pub message: String,

    /// Wall-clock timestamp.
    pub timestamp: DateTime<Utc>,

    /// Structured fields (key-value pairs from the child).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub fields: HashMap<String, String>,

    /// Number of consecutive identical messages this entry represents.
    /// Default 1 (single occurrence). Values > 1 indicate deduplication.
    #[serde(default = "default_repeat_count", skip_serializing_if = "is_one")]
    pub repeat_count: u64,
}

fn default_repeat_count() -> u64 {
    1
}

fn is_one(v: &u64) -> bool {
    *v == 1
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// A batch of log entries from a single IPC call or buffered interval.
///
/// Published to NATS and forwarded to log sinks as a unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LogBatch {
    /// Execution that produced these logs.
    pub execution_id: String,

    /// The log entries in this batch.
    pub entries: Vec<LogEntry>,

    /// When this batch was assembled.
    pub logged_at: DateTime<Utc>,
}

/// Summary of logs accumulated during an execution.
///
/// Included in `ExecutionResult` as a compact overview —
/// the full log stream lives in the log sinks (file/NATS/Loki).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LogSummary {
    /// Total log entries received during this execution.
    pub total_entries: u64,

    /// Count per severity level.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub count_by_level: HashMap<String, u64>,

    /// Last N error/warn messages (ring buffer in sidecar).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_errors: Vec<LogEntry>,

    /// Number of log entries dropped due to rate limiting.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_ordering() {
        assert!(LogLevel::Trace < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }

    #[test]
    fn log_level_serde() {
        let json = serde_json::to_string(&LogLevel::Warn).unwrap();
        assert_eq!(json, "\"warn\"");
        let deserialized: LogLevel = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, LogLevel::Warn);
    }

    #[test]
    fn log_entry_serde_roundtrip() {
        let entry = LogEntry {
            level: LogLevel::Error,
            message: "something broke".into(),
            timestamp: Utc::now(),
            fields: HashMap::from([("module".into(), "training".into())]),
            repeat_count: 1,
        };

        let json = serde_json::to_string_pretty(&entry).unwrap();
        let deserialized: LogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.level, LogLevel::Error);
        assert_eq!(deserialized.message, "something broke");
        assert_eq!(deserialized.fields.get("module").unwrap(), "training");
    }

    #[test]
    fn log_entry_minimal_serde() {
        let json = r#"{
            "level": "info",
            "message": "hello",
            "timestamp": "2024-01-01T00:00:00Z"
        }"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.level, LogLevel::Info);
        assert!(entry.fields.is_empty());
        assert_eq!(entry.repeat_count, 1, "default repeat_count should be 1");
    }

    #[test]
    fn log_batch_serde_roundtrip() {
        let batch = LogBatch {
            execution_id: "exec-1".into(),
            entries: vec![LogEntry {
                level: LogLevel::Info,
                message: "started".into(),
                timestamp: Utc::now(),
                fields: Default::default(),
                repeat_count: 1,
            }],
            logged_at: Utc::now(),
        };

        let json = serde_json::to_string(&batch).unwrap();
        let deserialized: LogBatch = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.execution_id, "exec-1");
        assert_eq!(deserialized.entries.len(), 1);
    }

    #[test]
    fn log_summary_default() {
        let summary = LogSummary::default();
        assert_eq!(summary.total_entries, 0);
        assert!(summary.count_by_level.is_empty());
        assert!(summary.recent_errors.is_empty());
        assert_eq!(summary.dropped_count, 0);
    }
}
