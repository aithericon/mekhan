use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Metric type — encodes aggregation semantics (like Prometheus).
///
/// Backends use this to decide how to store and display the metric:
/// - `Scalar`: generic value, no aggregation semantics
/// - `Counter`: monotonically increasing, suitable for rate computation
/// - `Gauge`: point-in-time value, last-value-wins
/// - `Histogram`: distribution of values (bucket counts)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MetricType {
    /// Generic scalar value (default). No aggregation semantics implied.
    #[default]
    Scalar,
    /// Monotonically increasing counter (suitable for rates).
    Counter,
    /// Point-in-time gauge (last-value wins).
    Gauge,
    /// Distribution of values (histogram buckets).
    Histogram,
}

impl MetricType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Counter => "counter",
            Self::Gauge => "gauge",
            Self::Histogram => "histogram",
        }
    }
}

impl std::fmt::Display for MetricType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single metric data point.
///
/// Inspired by W&B's `wandb.log()` — child processes can log arbitrary
/// named metrics with optional step tracking and dimensional labels.
///
/// Metric names support "/" for hierarchical grouping (e.g. "train/loss",
/// "gpu/utilization", "validation/accuracy").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MetricPoint {
    /// Metric name (e.g. "train/loss", "gpu/utilization").
    pub name: String,

    /// Numeric value.
    pub value: f64,

    /// Logical step (epoch, batch, iteration). Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u64>,

    /// Wall-clock timestamp.
    pub timestamp: DateTime<Utc>,

    /// Metric type — determines how backends aggregate/display.
    #[serde(default)]
    pub metric_type: MetricType,

    /// Dimensional labels for slicing (e.g. {"split": "train", "model": "v2"}).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
}

/// A batch of metric points from a single IPC call.
///
/// Published to NATS and forwarded to metric sinks as a unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MetricBatch {
    /// Execution that produced these metrics.
    pub execution_id: String,

    /// The metric points in this batch.
    pub points: Vec<MetricPoint>,

    /// When this batch was logged.
    pub logged_at: DateTime<Utc>,
}

/// Summary of metrics accumulated during an execution.
///
/// Included in `ExecutionResult` as a compact overview — the full
/// time-series data lives in the metric sink.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MetricSummary {
    /// Total metric points logged during this execution.
    pub total_points: u64,

    /// Distinct metric names seen.
    pub metric_names: Vec<String>,

    /// Last observed value for each metric (quick summary).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub latest_values: HashMap<String, f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_type_default_is_scalar() {
        assert_eq!(MetricType::default(), MetricType::Scalar);
    }

    #[test]
    fn metric_type_serde() {
        let json = serde_json::to_string(&MetricType::Counter).unwrap();
        assert_eq!(json, "\"counter\"");
        let deserialized: MetricType = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, MetricType::Counter);
    }

    #[test]
    fn metric_point_serde_roundtrip() {
        let point = MetricPoint {
            name: "train/loss".into(),
            value: 0.42,
            step: Some(100),
            timestamp: Utc::now(),
            metric_type: MetricType::Scalar,
            labels: HashMap::from([("split".into(), "train".into())]),
        };

        let json = serde_json::to_string_pretty(&point).unwrap();
        let deserialized: MetricPoint = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "train/loss");
        assert_eq!(deserialized.value, 0.42);
        assert_eq!(deserialized.step, Some(100));
        assert_eq!(deserialized.labels.get("split").unwrap(), "train");
    }

    #[test]
    fn metric_point_minimal_serde() {
        // Minimal point — no step, no labels
        let json = r#"{
            "name": "accuracy",
            "value": 0.95,
            "timestamp": "2024-01-01T00:00:00Z"
        }"#;
        let point: MetricPoint = serde_json::from_str(json).unwrap();
        assert_eq!(point.name, "accuracy");
        assert_eq!(point.step, None);
        assert_eq!(point.metric_type, MetricType::Scalar);
        assert!(point.labels.is_empty());
    }

    #[test]
    fn metric_batch_serde_roundtrip() {
        let batch = MetricBatch {
            execution_id: "exec-1".into(),
            points: vec![
                MetricPoint {
                    name: "loss".into(),
                    value: 0.5,
                    step: Some(1),
                    timestamp: Utc::now(),
                    metric_type: MetricType::Scalar,
                    labels: Default::default(),
                },
                MetricPoint {
                    name: "accuracy".into(),
                    value: 0.8,
                    step: Some(1),
                    timestamp: Utc::now(),
                    metric_type: MetricType::Gauge,
                    labels: Default::default(),
                },
            ],
            logged_at: Utc::now(),
        };

        let json = serde_json::to_string(&batch).unwrap();
        let deserialized: MetricBatch = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.execution_id, "exec-1");
        assert_eq!(deserialized.points.len(), 2);
    }

    #[test]
    fn metric_summary_default() {
        let summary = MetricSummary::default();
        assert_eq!(summary.total_points, 0);
        assert!(summary.metric_names.is_empty());
        assert!(summary.latest_values.is_empty());
    }
}
