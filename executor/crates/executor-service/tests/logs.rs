use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_test_harness::ipc_client::ipc_client_job;
use uuid::Uuid;

/// Verify IPC log_message: log summary appears in terminal detail.
#[tokio::test]
async fn test_ipc_log_message_summary() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-logs-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-logs-status", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "log_message", "level": "info", "message": "starting training" },
            { "type": "log_message", "level": "warn", "message": "learning rate too high" },
            { "type": "log_message", "level": "error", "message": "gradient explosion detected" }
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
    let logs = &completed.detail["logs"];
    assert!(
        !logs.is_null(),
        "logs summary should be present in terminal detail"
    );

    assert_eq!(
        logs["total_entries"].as_u64().unwrap(),
        3,
        "should have logged 3 entries"
    );

    // Verify count_by_level
    let by_level = &logs["count_by_level"];
    assert_eq!(by_level["info"].as_u64().unwrap(), 1);
    assert_eq!(by_level["warn"].as_u64().unwrap(), 1);
    assert_eq!(by_level["error"].as_u64().unwrap(), 1);

    // Verify recent_errors contains warn and error entries
    let recent = logs["recent_errors"]
        .as_array()
        .expect("recent_errors should be an array");
    assert_eq!(
        recent.len(),
        2,
        "should have 2 recent errors (warn + error)"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify LogsForwarded event is emitted to the events stream.
#[tokio::test]
async fn test_ipc_log_message_event() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-logs-evt-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-logs-evt-status", &eid).await;
    let events_consumer = ctx.events_consumer("ipc-logs-evt-events", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "log_message", "level": "info", "message": "hello" },
            { "type": "log_message", "level": "error", "message": "something broke" }
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

    // Check for Log event
    let events = ctx
        .collect_events(&events_consumer, 1, Duration::from_secs(5))
        .await;
    assert!(!events.is_empty(), "expected at least 1 Log event, got 0");

    let log_event = events
        .iter()
        .find(|e| e.category.as_str() == "log")
        .expect("should have a log category event");
    assert_eq!(log_event.category.as_str(), "log");

    worker.abort();
    ctx.cleanup().await;
}

/// Verify all 5 log levels are tracked correctly.
#[tokio::test]
async fn test_ipc_log_all_levels() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-logs-levels-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-logs-levels-status", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "log_message", "level": "trace", "message": "trace msg" },
            { "type": "log_message", "level": "debug", "message": "debug msg" },
            { "type": "log_message", "level": "info", "message": "info msg" },
            { "type": "log_message", "level": "warn", "message": "warn msg" },
            { "type": "log_message", "level": "error", "message": "error msg" }
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
    let logs = &completed.detail["logs"];

    assert_eq!(
        logs["total_entries"].as_u64().unwrap(),
        5,
        "should have 5 total log entries"
    );

    let by_level = &logs["count_by_level"];
    assert_eq!(by_level["trace"].as_u64().unwrap(), 1);
    assert_eq!(by_level["debug"].as_u64().unwrap(), 1);
    assert_eq!(by_level["info"].as_u64().unwrap(), 1);
    assert_eq!(by_level["warn"].as_u64().unwrap(), 1);
    assert_eq!(by_level["error"].as_u64().unwrap(), 1);

    // recent_errors should contain warn + error = 2
    let recent = logs["recent_errors"]
        .as_array()
        .expect("recent_errors should be an array");
    assert_eq!(
        recent.len(),
        2,
        "should have 2 recent errors (warn + error)"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify logs combined with metrics and outputs in a single workflow.
#[tokio::test]
async fn test_ipc_logs_combined_workflow() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-logs-combined-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-logs-combined-status", &eid).await;
    let events_consumer = ctx.events_consumer("ipc-logs-combined-events", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "log_message", "level": "info", "message": "epoch 1 started" },
            {
                "type": "log_metrics",
                "points": [
                    { "name": "train/loss", "value": 0.5, "step": 1 }
                ]
            },
            { "type": "update_progress", "fraction": 0.5, "message": "Training", "current_step": 1, "total_steps": 2 },
            { "type": "log_message", "level": "warn", "message": "high loss detected" },
            { "type": "set_output", "name": "status", "value_json": "\"training\"" },
            { "type": "log_message", "level": "info", "message": "epoch 2 started" },
            {
                "type": "log_metrics",
                "points": [
                    { "name": "train/loss", "value": 0.2, "step": 2 }
                ]
            },
            { "type": "update_progress", "fraction": 1.0, "message": "Done", "current_step": 2, "total_steps": 2 }
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

    // Logs summary
    let logs = &detail["logs"];
    assert_eq!(logs["total_entries"].as_u64().unwrap(), 3);
    assert_eq!(logs["count_by_level"]["info"].as_u64().unwrap(), 2);
    assert_eq!(logs["count_by_level"]["warn"].as_u64().unwrap(), 1);

    // Metrics summary
    let metrics = &detail["metrics"];
    assert_eq!(metrics["total_points"].as_u64().unwrap(), 2);

    // Outputs
    assert_eq!(detail["outputs"]["status"], serde_json::json!("training"));

    // Progress
    assert!(!detail["progress"].is_null());

    // Events: at least Output + Progress + Metric + Log = 4
    let events = ctx
        .collect_events(&events_consumer, 4, Duration::from_secs(5))
        .await;
    assert!(
        events.len() >= 4,
        "expected at least 4 events (output + progress + metric + log), got: {}",
        events.len()
    );

    // Verify we have at least one of each category
    let categories: Vec<&str> = events.iter().map(|e| e.category.as_str()).collect();
    assert!(categories.contains(&"log"), "should have a log event");
    assert!(categories.contains(&"metric"), "should have a metric event");

    worker.abort();
    ctx.cleanup().await;
}
