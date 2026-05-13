use std::sync::Arc;
use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_worker::{BackendRegistry, CleanupPolicy};
use uuid::Uuid;

/// Verify: nonexistent command → Accepted → Failed (no Running, since spawn fails
/// before the status callback in child.rs:60).
#[tokio::test]
async fn test_spawn_failure_nonexistent_command() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("spawn-fail-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("spawn-fail", &eid).await;
    let worker = ctx.spawn_worker();

    ctx.push_job(nonexistent_command_job(&eid)).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    // Spawn fails before Running is reported
    assert_status_sequence(
        &statuses,
        &[ExecutionStatus::Accepted, ExecutionStatus::Failed],
    );

    let failed = statuses.last().unwrap();
    let error = failed.detail["error"].as_str().unwrap();
    assert!(
        error.contains("spawn failed"),
        "expected 'spawn failed' in error, got: {error}"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: empty backend registry → Accepted → Failed with "unsupported spec type".
#[tokio::test]
async fn test_backend_not_found_empty_registry() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("no-backend-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("no-backend", &eid).await;

    // Spawn worker with an empty registry (no backends registered)
    let empty_registry = Arc::new(BackendRegistry::new(Duration::from_secs(30)));
    let worker = ctx.spawn_worker_custom(CleanupPolicy::Retain, None, empty_registry);

    ctx.push_job(echo_job(&eid)).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    assert_status_sequence(
        &statuses,
        &[ExecutionStatus::Accepted, ExecutionStatus::Failed],
    );

    let failed = statuses.last().unwrap();
    let error = failed.detail["error"].as_str().unwrap();
    assert!(
        error.contains("unsupported spec type"),
        "expected 'unsupported spec type' in error, got: {error}"
    );

    worker.abort();
    ctx.cleanup().await;
}
