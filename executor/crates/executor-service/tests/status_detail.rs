use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use uuid::Uuid;

/// Verify the terminal detail for a Completed execution has the expected structure.
#[tokio::test]
async fn test_completed_detail_structure() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("detail-ok-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("detail-ok", &eid).await;
    let worker = ctx.spawn_worker();

    ctx.push_job(echo_job(&eid)).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
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

    // outcome.type == "success"
    assert_eq!(
        detail["outcome"]["type"].as_str().unwrap(),
        "success",
        "outcome type should be success"
    );

    // duration_ms > 0
    let duration_ms = detail["duration_ms"].as_u64().unwrap();
    assert!(
        duration_ms > 0,
        "duration_ms should be positive, got: {duration_ms}"
    );

    // stdout_tail == "hello\n"
    let stdout_tail = detail["stdout_tail"].as_str().unwrap();
    assert_eq!(stdout_tail, "hello\n");

    worker.abort();
    ctx.cleanup().await;
}

/// Verify the terminal detail for a Failed execution has exit_code.
#[tokio::test]
async fn test_failed_detail_structure() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("detail-fail-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("detail-fail", &eid).await;
    let worker = ctx.spawn_worker();

    ctx.push_job(failing_job(&eid)).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Failed,
        ],
    );

    let failed = statuses.last().unwrap();
    let detail = &failed.detail;

    // outcome.type == "exit_failure"
    assert_eq!(
        detail["outcome"]["type"].as_str().unwrap(),
        "exit_failure",
        "outcome type should be exit_failure"
    );

    // outcome.exit_code == 1
    assert_eq!(detail["outcome"]["exit_code"].as_i64().unwrap(), 1);

    // duration_ms present
    let duration_ms = detail["duration_ms"].as_u64().unwrap();
    assert!(
        duration_ms > 0,
        "duration_ms should be positive, got: {duration_ms}"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify the terminal detail for a Completed execution has all expected top-level keys.
#[tokio::test]
async fn test_terminal_detail_has_all_fields() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("detail-fields-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("detail-fields", &eid).await;
    let worker = ctx.spawn_worker();

    ctx.push_job(echo_job(&eid)).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
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
    let detail = completed
        .detail
        .as_object()
        .expect("detail should be an object");

    // All expected keys should be present
    let expected_keys = [
        "outcome",
        "duration_ms",
        "stdout_tail",
        "stderr_tail",
        "artifact_manifest",
        "outputs",
        "progress",
    ];
    for key in &expected_keys {
        assert!(
            detail.contains_key(*key),
            "terminal detail should contain key '{}', got keys: {:?}",
            key,
            detail.keys().collect::<Vec<_>>()
        );
    }

    worker.abort();
    ctx.cleanup().await;
}
