use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use uuid::Uuid;

/// Verify the events stream exists for this test context.
#[tokio::test]
async fn test_events_stream_exists() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;

    let stream = ctx.jetstream().get_stream(ctx.events_stream_name()).await;

    assert!(
        stream.is_ok(),
        "events stream should exist: {}",
        ctx.events_stream_name()
    );

    ctx.cleanup().await;
}

/// Verify no events are published for a simple echo job (no IPC interaction).
#[tokio::test]
async fn test_no_events_for_simple_job() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("no-events-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("no-events-status", &eid).await;
    let events_consumer = ctx.events_consumer("no-events", &eid).await;
    let worker = ctx.spawn_worker();

    ctx.push_job(echo_job(&eid)).await;

    // Wait for completion
    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(10))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ],
    );

    // Try to collect events — should get 0
    let events = ctx
        .collect_events(&events_consumer, 1, Duration::from_secs(2))
        .await;

    assert!(
        events.is_empty(),
        "no events should be published for a simple echo job, got: {}",
        events.len()
    );

    worker.abort();
    ctx.cleanup().await;
}
