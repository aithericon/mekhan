use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_test_harness::ipc_client::ipc_client_job;
use uuid::Uuid;

/// Verify IPC log_metrics: metrics summary appears in terminal detail.
#[tokio::test]
async fn test_ipc_log_metrics_basic() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-metrics-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-metrics-status", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            {
                "type": "log_metrics",
                "points": [
                    { "name": "train/loss", "value": 0.42, "step": 1, "metric_type": "scalar" },
                    { "name": "train/loss", "value": 0.35, "step": 2, "metric_type": "scalar" },
                    { "name": "gpu/utilization", "value": 85.5, "metric_type": "gauge" }
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

    let completed = statuses.last().unwrap();
    let metrics = &completed.detail["metrics"];
    assert!(
        !metrics.is_null(),
        "metrics summary should be present in terminal detail"
    );

    assert_eq!(
        metrics["total_points"].as_u64().unwrap(),
        3,
        "should have logged 3 metric points"
    );

    let metric_names = metrics["metric_names"]
        .as_array()
        .expect("metric_names should be an array");
    assert_eq!(metric_names.len(), 2, "should have 2 distinct metric names");

    // Latest values should reflect the most recent value for each metric
    let latest = &metrics["latest_values"];
    assert!(
        (latest["train/loss"].as_f64().unwrap() - 0.35).abs() < f64::EPSILON,
        "latest train/loss should be 0.35"
    );
    assert!(
        (latest["gpu/utilization"].as_f64().unwrap() - 85.5).abs() < f64::EPSILON,
        "latest gpu/utilization should be 85.5"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify metrics event is emitted to the events stream.
#[tokio::test]
async fn test_ipc_log_metrics_event() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-metrics-evt-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-metrics-evt-status", &eid).await;
    let events_consumer = ctx.events_consumer("ipc-metrics-evt-events", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            {
                "type": "log_metrics",
                "points": [
                    { "name": "accuracy", "value": 0.95, "step": 100 }
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

    // Check for Metric event
    let events = ctx
        .collect_events(&events_consumer, 1, Duration::from_secs(5))
        .await;
    assert!(
        !events.is_empty(),
        "expected at least 1 Metric event, got 0"
    );

    let metric_event = &events[0];
    assert_eq!(metric_event.category.as_str(), "metric");

    worker.abort();
    ctx.cleanup().await;
}

/// Verify multiple log_metrics calls accumulate correctly.
#[tokio::test]
async fn test_ipc_log_metrics_multiple_batches() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-metrics-multi-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-metrics-multi-status", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            {
                "type": "log_metrics",
                "points": [
                    { "name": "train/loss", "value": 0.5, "step": 1 },
                    { "name": "train/acc", "value": 0.6, "step": 1 }
                ]
            },
            {
                "type": "log_metrics",
                "points": [
                    { "name": "train/loss", "value": 0.3, "step": 2 },
                    { "name": "train/acc", "value": 0.8, "step": 2 },
                    { "name": "val/loss", "value": 0.4, "step": 2 }
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

    let completed = statuses.last().unwrap();
    let metrics = &completed.detail["metrics"];

    assert_eq!(
        metrics["total_points"].as_u64().unwrap(),
        5,
        "should have 5 total metric points across 2 batches"
    );

    let metric_names = metrics["metric_names"]
        .as_array()
        .expect("metric_names should be an array");
    assert_eq!(
        metric_names.len(),
        3,
        "should have 3 distinct metric names: train/loss, train/acc, val/loss"
    );

    // Latest values should reflect the second batch
    let latest = &metrics["latest_values"];
    assert!(
        (latest["train/loss"].as_f64().unwrap() - 0.3).abs() < f64::EPSILON,
        "latest train/loss should be 0.3"
    );
    assert!(
        (latest["train/acc"].as_f64().unwrap() - 0.8).abs() < f64::EPSILON,
        "latest train/acc should be 0.8"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify metrics with labels and different metric types.
#[tokio::test]
async fn test_ipc_log_metrics_with_labels() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-metrics-labels-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-metrics-labels-status", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            {
                "type": "log_metrics",
                "points": [
                    {
                        "name": "train/loss",
                        "value": 0.42,
                        "step": 1,
                        "metric_type": "scalar",
                        "labels": { "split": "train", "model": "v2" }
                    },
                    {
                        "name": "gpu/mem",
                        "value": 8192.0,
                        "metric_type": "gauge",
                        "labels": { "device": "cuda:0" }
                    },
                    {
                        "name": "requests",
                        "value": 42.0,
                        "metric_type": "counter"
                    }
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

    let completed = statuses.last().unwrap();
    let metrics = &completed.detail["metrics"];
    assert_eq!(metrics["total_points"].as_u64().unwrap(), 3);

    worker.abort();
    ctx.cleanup().await;
}

/// Verify metrics combined with other IPC actions (outputs, progress) in a single workflow.
#[tokio::test]
async fn test_ipc_metrics_combined_workflow() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-metrics-combined-{}", Uuid::new_v4().simple());
    let status_consumer = ctx
        .status_consumer("ipc-metrics-combined-status", &eid)
        .await;
    let events_consumer = ctx
        .events_consumer("ipc-metrics-combined-events", &eid)
        .await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "update_progress", "fraction": 0.5, "message": "Training", "current_step": 50, "total_steps": 100 },
            {
                "type": "log_metrics",
                "points": [
                    { "name": "train/loss", "value": 0.42, "step": 50 }
                ]
            },
            { "type": "set_output", "name": "best_loss", "value_json": "0.42" },
            { "type": "update_progress", "fraction": 1.0, "message": "Done", "current_step": 100, "total_steps": 100 },
            {
                "type": "log_metrics",
                "points": [
                    { "name": "train/loss", "value": 0.15, "step": 100 }
                ]
            },
            { "type": "set_output", "name": "best_loss", "value_json": "0.15" }
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

    let completed = statuses.last().unwrap();
    let detail = &completed.detail;

    // Metrics summary
    let metrics = &detail["metrics"];
    assert_eq!(metrics["total_points"].as_u64().unwrap(), 2);
    assert!((metrics["latest_values"]["train/loss"].as_f64().unwrap() - 0.15).abs() < f64::EPSILON);

    // Outputs
    assert_eq!(detail["outputs"]["best_loss"], serde_json::json!(0.15));

    // Progress
    let progress = &detail["progress"];
    assert!(!progress.is_null());

    // Events: at least 1 Output + 1 Progress + 1 Metric = 3
    let events = ctx
        .collect_events(&events_consumer, 3, Duration::from_secs(5))
        .await;
    assert!(
        events.len() >= 3,
        "expected at least 3 events (output + progress + metric), got: {}",
        events.len()
    );

    worker.abort();
    ctx.cleanup().await;
}
