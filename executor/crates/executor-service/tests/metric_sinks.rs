//! Real metric sink integration tests.
//!
//! These tests inject actual MetricSink implementations (NATS, Loki) through the
//! test harness and verify that metric data arrives at the sink after execution.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_metrics::{LokiMetricSink, NatsMetricSink};
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_test_harness::ipc_client::ipc_client_job;
use aithericon_executor_test_harness::loki::{flush_loki, query_loki, shared_loki_push_url};
use aithericon_executor_test_harness::nats::shared_nats_client;
use aithericon_executor_worker::{CleanupPolicy, SidecarLogConfig};
use futures::StreamExt;
use uuid::Uuid;

/// Inject a NatsMetricSink and verify metric batches arrive on NATS.
#[tokio::test]
async fn test_nats_metric_sink_publishes() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("nats-metric-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("nats-metric-status", &eid).await;

    let nats_client = shared_nats_client().await;
    let metric_sink = Arc::new(NatsMetricSink::new(nats_client.clone()));

    // Subscribe to metric subject before pushing the job
    let subject = format!("executor.metrics.{eid}");
    let mut metric_sub = nats_client.subscribe(subject).await.unwrap();

    let worker = ctx.spawn_worker_with_sinks(
        CleanupPolicy::Retain,
        None,
        Some(metric_sink),
        None,
        SidecarLogConfig::default(),
    );

    let plan = serde_json::json!({
        "actions": [
            {
                "type": "log_metrics",
                "points": [
                    { "name": "train/loss", "value": 0.42, "step": 1, "metric_type": "scalar" },
                    { "name": "gpu/util", "value": 85.0, "metric_type": "gauge" }
                ]
            }
        ],
        "exit_code": 0
    });

    ctx.push_job(ipc_client_job(&eid, &plan, vec![])).await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ],
    );

    // Read metric messages from NATS subscription
    let msg = tokio::time::timeout(Duration::from_secs(5), metric_sub.next())
        .await
        .expect("should receive metric batch within 5s")
        .expect("subscription should yield a message");

    let batch: serde_json::Value = serde_json::from_slice(&msg.payload).unwrap();
    assert_eq!(
        batch["execution_id"].as_str().unwrap(),
        eid,
        "metric batch should reference correct execution_id"
    );

    let points = batch["points"].as_array().expect("points should be array");
    assert_eq!(points.len(), 2, "should have 2 metric points");

    worker.abort();
    ctx.cleanup().await;
}

/// Inject a LokiMetricSink and verify metric data is queryable via LogQL.
#[tokio::test]
async fn test_loki_metric_sink_pushes() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("loki-metric-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("loki-metric-status", &eid).await;

    let push_url = shared_loki_push_url().await;
    let mut labels = HashMap::new();
    labels.insert("service".to_string(), "executor-test".to_string());
    let loki_sink = Arc::new(LokiMetricSink::new(push_url, labels));

    let worker = ctx.spawn_worker_with_sinks(
        CleanupPolicy::Retain,
        None,
        Some(loki_sink),
        None,
        SidecarLogConfig::default(),
    );

    let plan = serde_json::json!({
        "actions": [
            {
                "type": "log_metrics",
                "points": [
                    { "name": "train/loss", "value": 0.42, "step": 1, "metric_type": "scalar" },
                    { "name": "train/accuracy", "value": 0.95, "step": 1, "metric_type": "scalar" }
                ]
            }
        ],
        "exit_code": 0
    });

    ctx.push_job(ipc_client_job(&eid, &plan, vec![])).await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ],
    );

    // Flush Loki
    flush_loki().await;

    // Query for metric entries by execution_id and __type__=metric
    let logql = format!("{{execution_id=\"{eid}\", __type__=\"metric\"}}");
    let result = query_loki(&logql, 120).await;

    let status = result["status"].as_str().unwrap_or("");
    assert_eq!(status, "success", "Loki query should succeed");

    let streams = result["data"]["result"]
        .as_array()
        .expect("result should be array");
    assert!(
        !streams.is_empty(),
        "should have at least one metric stream in Loki"
    );

    // Count total metric entries
    let total_values: usize = streams
        .iter()
        .map(|s| s["values"].as_array().map_or(0, |v| v.len()))
        .sum();
    assert!(
        total_values >= 2,
        "should have at least 2 metric entries in Loki, got {total_values}"
    );

    worker.abort();
    ctx.cleanup().await;
}
