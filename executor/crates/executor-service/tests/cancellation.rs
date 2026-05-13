use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Cancel a long-running job via NATS and verify the Cancelled terminal status.
///
/// Flow: push long-running job → wait for Running → publish cancel → expect Cancelled.
#[tokio::test(flavor = "multi_thread")]
async fn test_cancel_via_nats() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("cancel-nats-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("cancel-test", &eid).await;

    // Start cancel listener
    let shutdown = CancellationToken::new();
    let listener_handle = ctx.start_cancel_listener(shutdown.clone()).await;

    let worker = ctx.spawn_worker();

    // Push a long-running job (sleep 300s, no timeout)
    ctx.push_job(long_running_job(&eid, 300)).await;

    // Wait for Running status before sending cancel
    let mut saw_running = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if ctx.cancel_registry.active_count() > 0 {
            saw_running = true;
            break;
        }
    }
    assert!(saw_running, "job never registered in cancel registry");

    // Small delay to ensure the backend is in the execute select! loop
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Cancel via NATS
    ctx.publish_cancel(&eid).await;

    // Collect statuses — should see Accepted → Running → Cancelled
    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(20))
        .await;

    assert!(
        statuses.len() >= 2,
        "expected at least Accepted + Cancelled, got {}: {:?}",
        statuses.len(),
        statuses.iter().map(|s| s.status).collect::<Vec<_>>()
    );

    let terminal = statuses.last().unwrap();
    assert_eq!(
        terminal.status,
        ExecutionStatus::Cancelled,
        "expected Cancelled terminal status, got {:?}; full sequence: {:?}",
        terminal.status,
        statuses.iter().map(|s| s.status).collect::<Vec<_>>()
    );

    // Token should be deregistered after completion
    assert_eq!(
        ctx.cancel_registry.active_count(),
        0,
        "cancel token not deregistered after cancellation"
    );

    shutdown.cancel();
    let _ = listener_handle.await;
    worker.abort();
    ctx.cleanup().await;
}

/// Cancelling an unknown execution_id via the registry is a no-op (returns false).
#[tokio::test]
async fn test_cancel_unknown_is_noop() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let found = ctx.cancel_registry.cancel("nonexistent-exec-id");
    assert!(
        !found,
        "cancel should return false for unknown execution_id"
    );
    ctx.cleanup().await;
}

/// After a job completes, the cancel token is deregistered.
/// Cancelling a completed execution returns false.
#[tokio::test]
async fn test_cancel_after_completion_is_noop() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("cancel-post-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("cancel-post-test", &eid).await;
    let worker = ctx.spawn_worker();

    // Push a fast echo job
    ctx.push_job(echo_job(&eid)).await;

    // Wait for completion
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

    // Token should already be deregistered
    assert_eq!(ctx.cancel_registry.active_count(), 0);

    // Cancelling now should be a no-op
    let found = ctx.cancel_registry.cancel(&eid);
    assert!(!found, "cancel should return false after job completed");

    worker.abort();
    ctx.cleanup().await;
}
