//! Real log sink integration tests.
//!
//! These tests inject actual LogSink implementations (File, Loki) through the
//! test harness and verify that log data arrives at the sink after execution.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aithericon_executor_domain::{ExecutionStatus, LogEntry};
use aithericon_executor_logs::{FileLogSink, LokiLogSink};
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_test_harness::ipc_client::ipc_client_job;
use aithericon_executor_test_harness::loki::{flush_loki, query_loki, shared_loki_push_url};
use aithericon_executor_worker::{CleanupPolicy, SidecarLogConfig};
use uuid::Uuid;

/// Inject a FileLogSink and verify JSONL entries appear on disk.
#[tokio::test]
async fn test_file_log_sink_writes_jsonl() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("file-sink-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("file-sink-status", &eid).await;

    let dir = tempfile::tempdir().unwrap();
    let file_sink = Arc::new(FileLogSink::new(
        dir.path().to_path_buf(),
        "test.jsonl".into(),
    ));

    let worker = ctx.spawn_worker_with_sinks(
        CleanupPolicy::Retain,
        None,
        None,
        Some(file_sink.clone()),
        SidecarLogConfig::default(),
    );

    let plan = serde_json::json!({
        "actions": [
            { "type": "log_message", "level": "info", "message": "hello from file sink" },
            { "type": "log_message", "level": "warn", "message": "a warning" },
            { "type": "log_message", "level": "error", "message": "an error" }
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

    // Give the fire-and-forget sink writes a moment to land
    tokio::time::sleep(Duration::from_millis(500)).await;

    let path = dir.path().join(format!("runs/{eid}/logs/test.jsonl"));
    assert!(path.exists(), "JSONL log file should exist at {path:?}");

    let contents = tokio::fs::read_to_string(&path).await.unwrap();
    let lines: Vec<&str> = contents.trim().lines().collect();
    assert!(
        lines.len() >= 3,
        "should have at least 3 lines, got {}",
        lines.len()
    );

    // Verify entries are valid LogEntry JSON
    let entry: LogEntry = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(entry.message, "hello from file sink");

    worker.abort();
    ctx.cleanup().await;
}

/// Inject a LokiLogSink and verify entries are queryable via LogQL.
#[tokio::test]
async fn test_loki_log_sink_pushes_and_queryable() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("loki-sink-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("loki-sink-status", &eid).await;

    let push_url = shared_loki_push_url().await;
    let mut labels = HashMap::new();
    labels.insert("service".to_string(), "executor-test".to_string());
    let loki_sink = Arc::new(LokiLogSink::new(push_url, labels));

    let worker = ctx.spawn_worker_with_sinks(
        CleanupPolicy::Retain,
        None,
        None,
        Some(loki_sink),
        SidecarLogConfig::default(),
    );

    let plan = serde_json::json!({
        "actions": [
            { "type": "log_message", "level": "info", "message": "loki test entry" },
            { "type": "log_message", "level": "error", "message": "loki error entry" }
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

    // Flush Loki so in-memory chunks become queryable
    flush_loki().await;

    // Query for our execution_id
    let logql = format!("{{execution_id=\"{eid}\"}}");
    let result = query_loki(&logql, 120).await;

    let status = result["status"].as_str().unwrap_or("");
    assert_eq!(status, "success", "Loki query should succeed");

    let streams = result["data"]["result"]
        .as_array()
        .expect("result should be array");
    assert!(
        !streams.is_empty(),
        "should have at least one stream from Loki"
    );

    // Count total log lines across all streams
    let total_values: usize = streams
        .iter()
        .map(|s| s["values"].as_array().map_or(0, |v| v.len()))
        .sum();
    assert!(
        total_values >= 2,
        "should have at least 2 log entries in Loki, got {total_values}"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify consecutive dedup: 10 identical messages collapse into repeat_count in JSONL.
#[tokio::test]
async fn test_log_dedup_consecutive() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("dedup-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("dedup-status", &eid).await;

    let dir = tempfile::tempdir().unwrap();
    let file_sink = Arc::new(FileLogSink::new(
        dir.path().to_path_buf(),
        "test.jsonl".into(),
    ));

    let config = SidecarLogConfig {
        batch_size: 100, // large batch so everything flushes at connection close
        ..SidecarLogConfig::default()
    };

    let worker = ctx.spawn_worker_with_sinks(
        CleanupPolicy::Retain,
        None,
        None,
        Some(file_sink.clone()),
        config,
    );

    // 10 identical info messages, then 1 different
    let mut actions = Vec::new();
    for _ in 0..10 {
        actions.push(serde_json::json!(
            { "type": "log_message", "level": "info", "message": "repeated line" }
        ));
    }
    actions.push(serde_json::json!(
        { "type": "log_message", "level": "warn", "message": "different line" }
    ));

    let plan = serde_json::json!({
        "actions": actions,
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

    // Allow sink writes to complete
    tokio::time::sleep(Duration::from_millis(500)).await;

    let path = dir.path().join(format!("runs/{eid}/logs/test.jsonl"));
    assert!(path.exists(), "JSONL log file should exist");

    let contents = tokio::fs::read_to_string(&path).await.unwrap();
    let lines: Vec<&str> = contents.trim().lines().collect();

    // Should be 2 entries: one collapsed "repeated line" (repeat_count=10) + one "different line"
    assert_eq!(
        lines.len(),
        2,
        "expected 2 JSONL lines (1 deduped + 1 different), got {}",
        lines.len()
    );

    let first: LogEntry = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first.message, "repeated line");
    assert_eq!(
        first.repeat_count, 10,
        "deduped entry should have repeat_count=10"
    );

    let second: LogEntry = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second.message, "different line");
    assert_eq!(second.repeat_count, 1);

    // Verify total_entries still counts all 11
    let completed = statuses.last().unwrap();
    let logs = &completed.detail["logs"];
    assert_eq!(logs["total_entries"].as_u64().unwrap(), 11);

    worker.abort();
    ctx.cleanup().await;
}

/// Verify rate limiting: dropped_count > 0 when exceeding the limit.
#[tokio::test]
async fn test_log_rate_limiting() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ratelimit-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ratelimit-status", &eid).await;

    let config = SidecarLogConfig {
        rate_limit: 5, // very low limit
        batch_size: 100,
        ..SidecarLogConfig::default()
    };

    let worker = ctx.spawn_worker_with_sinks(
        CleanupPolicy::Retain,
        None,
        None,
        None, // no sink, we just care about the summary
        config,
    );

    // Send 10 messages with different content (to avoid dedup)
    let mut actions = Vec::new();
    for i in 0..10 {
        actions.push(serde_json::json!(
            { "type": "log_message", "level": "info", "message": format!("msg {i}") }
        ));
    }

    let plan = serde_json::json!({
        "actions": actions,
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

    // total_entries counts everything (including dropped)
    assert_eq!(
        logs["total_entries"].as_u64().unwrap(),
        10,
        "total_entries should count all 10 messages"
    );

    // dropped_count = messages beyond the limit
    let dropped = logs["dropped_count"].as_u64().unwrap_or(0);
    assert_eq!(
        dropped, 5,
        "should have dropped 5 messages (10 total - 5 limit)"
    );

    worker.abort();
    ctx.cleanup().await;
}
