use std::collections::HashMap;
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use aithericon_executor_ipc::proto;
use aithericon_executor_ipc::ExecutorSidecarClient;

use crate::cli::MetricTypeArg;
use crate::commands::artifact::parse_key_value_pairs;
use crate::error::CliError;
use crate::output::check_response;

fn to_proto_metric_type(t: &MetricTypeArg) -> proto::MetricType {
    match t {
        MetricTypeArg::Scalar => proto::MetricType::Scalar,
        MetricTypeArg::Counter => proto::MetricType::Counter,
        MetricTypeArg::Gauge => proto::MetricType::Gauge,
        MetricTypeArg::Histogram => proto::MetricType::Histogram,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub async fn log_metric(
    client: &mut ExecutorSidecarClient<tonic::transport::Channel>,
    name: String,
    value: f64,
    step: Option<u64>,
    metric_type: MetricTypeArg,
    label_pairs: Vec<String>,
) -> Result<(), CliError> {
    let labels: HashMap<String, String> = if label_pairs.is_empty() {
        HashMap::new()
    } else {
        parse_key_value_pairs(&label_pairs)?
    };

    let point = proto::MetricPoint {
        name,
        value,
        step,
        timestamp_ms: now_ms(),
        metric_type: to_proto_metric_type(&metric_type).into(),
        labels,
    };

    let resp = client
        .log_metrics(proto::LogMetricsRequest {
            points: vec![point],
        })
        .await?
        .into_inner();

    check_response(resp)
}

/// Batch-log metrics from a JSON array on stdin.
///
/// Expected format: `[{"name":"x","value":1.0,"step":0,"metric_type":"scalar","labels":{}}]`
pub async fn log_metrics_batch(
    client: &mut ExecutorSidecarClient<tonic::transport::Channel>,
) -> Result<(), CliError> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;

    let raw: Vec<MetricPointInput> = serde_json::from_str(&buf)?;
    let ts = now_ms();

    let points = raw
        .into_iter()
        .map(|p| {
            let metric_type = match p.metric_type.as_deref() {
                Some("counter") => proto::MetricType::Counter,
                Some("gauge") => proto::MetricType::Gauge,
                Some("histogram") => proto::MetricType::Histogram,
                _ => proto::MetricType::Scalar,
            };
            proto::MetricPoint {
                name: p.name,
                value: p.value,
                step: p.step,
                timestamp_ms: p.timestamp_ms.unwrap_or(ts),
                metric_type: metric_type.into(),
                labels: p.labels.unwrap_or_default(),
            }
        })
        .collect();

    let resp = client
        .log_metrics(proto::LogMetricsRequest { points })
        .await?
        .into_inner();

    check_response(resp)
}

#[derive(serde::Deserialize)]
struct MetricPointInput {
    name: String,
    value: f64,
    #[serde(default)]
    step: Option<u64>,
    #[serde(default)]
    timestamp_ms: Option<i64>,
    #[serde(default)]
    metric_type: Option<String>,
    #[serde(default)]
    labels: Option<HashMap<String, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- to_proto_metric_type tests --

    #[test]
    fn metric_type_scalar() {
        assert_eq!(
            to_proto_metric_type(&MetricTypeArg::Scalar) as i32,
            proto::MetricType::Scalar as i32
        );
    }

    #[test]
    fn metric_type_counter() {
        assert_eq!(
            to_proto_metric_type(&MetricTypeArg::Counter) as i32,
            proto::MetricType::Counter as i32
        );
    }

    #[test]
    fn metric_type_gauge() {
        assert_eq!(
            to_proto_metric_type(&MetricTypeArg::Gauge) as i32,
            proto::MetricType::Gauge as i32
        );
    }

    #[test]
    fn metric_type_histogram() {
        assert_eq!(
            to_proto_metric_type(&MetricTypeArg::Histogram) as i32,
            proto::MetricType::Histogram as i32
        );
    }

    // -- now_ms tests --

    #[test]
    fn now_ms_returns_positive() {
        assert!(now_ms() > 0);
    }

    // -- MetricPointInput deserialization tests --

    #[test]
    fn metric_point_input_minimal() {
        let json = r#"{"name":"loss","value":0.5}"#;
        let p: MetricPointInput = serde_json::from_str(json).unwrap();
        assert_eq!(p.name, "loss");
        assert!((p.value - 0.5).abs() < f64::EPSILON);
        assert!(p.step.is_none());
        assert!(p.timestamp_ms.is_none());
        assert!(p.metric_type.is_none());
        assert!(p.labels.is_none());
    }

    #[test]
    fn metric_point_input_full() {
        let json = r#"{
            "name": "accuracy",
            "value": 0.95,
            "step": 100,
            "timestamp_ms": 1700000000000,
            "metric_type": "gauge",
            "labels": {"env": "prod"}
        }"#;
        let p: MetricPointInput = serde_json::from_str(json).unwrap();
        assert_eq!(p.name, "accuracy");
        assert!((p.value - 0.95).abs() < f64::EPSILON);
        assert_eq!(p.step, Some(100));
        assert_eq!(p.timestamp_ms, Some(1700000000000));
        assert_eq!(p.metric_type.as_deref(), Some("gauge"));
        assert_eq!(p.labels.as_ref().unwrap()["env"], "prod");
    }
}
