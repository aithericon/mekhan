use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_worker::BatchRunner;
use uuid::Uuid;

/// Execute a batch manifest with 2 echo jobs. Both should succeed.
#[tokio::test(flavor = "multi_thread")]
async fn test_batch_two_echo_jobs() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid1 = format!("batch-echo1-{}", Uuid::new_v4().simple());
    let eid2 = format!("batch-echo2-{}", Uuid::new_v4().simple());

    let manifest = batch_manifest(vec![echo_job(&eid1), echo_job(&eid2)]);

    let worker = ctx.spawn_worker();
    let runner = BatchRunner::new(ctx.storage.clone(), ctx.reporter.clone(), false);
    let result = runner.run(&manifest).await;

    assert_eq!(result.total, 2);
    assert_eq!(result.succeeded, 2);
    assert_eq!(result.failed, 0);
    assert_eq!(result.results.len(), 2);
    assert_eq!(result.results[0].execution_id, eid1);
    assert_eq!(result.results[0].status, ExecutionStatus::Completed);
    assert_eq!(result.results[1].execution_id, eid2);
    assert_eq!(result.results[1].status, ExecutionStatus::Completed);

    worker.abort();
    ctx.cleanup().await;
}

/// Execute a batch with one success and one failure.
#[tokio::test(flavor = "multi_thread")]
async fn test_batch_with_failure() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid1 = format!("batch-ok-{}", Uuid::new_v4().simple());
    let eid2 = format!("batch-fail-{}", Uuid::new_v4().simple());

    let manifest = batch_manifest(vec![echo_job(&eid1), failing_job(&eid2)]);

    let worker = ctx.spawn_worker();
    let runner = BatchRunner::new(ctx.storage.clone(), ctx.reporter.clone(), false);
    let result = runner.run(&manifest).await;

    assert_eq!(result.total, 2);
    assert_eq!(result.succeeded, 1);
    assert_eq!(result.failed, 1);
    assert_eq!(result.results[0].status, ExecutionStatus::Completed);
    assert_eq!(result.results[1].status, ExecutionStatus::Failed);

    worker.abort();
    ctx.cleanup().await;
}

/// With fail_fast=true, batch stops after first failure.
#[tokio::test(flavor = "multi_thread")]
async fn test_batch_fail_fast() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid1 = format!("batch-ff-fail-{}", Uuid::new_v4().simple());
    let eid2 = format!("batch-ff-skip-{}", Uuid::new_v4().simple());

    // First job fails, second should be skipped due to fail_fast
    let manifest = batch_manifest(vec![failing_job(&eid1), echo_job(&eid2)]);

    let worker = ctx.spawn_worker();
    let runner = BatchRunner::new(ctx.storage.clone(), ctx.reporter.clone(), true);
    let result = runner.run(&manifest).await;

    assert_eq!(result.total, 2, "total should reflect full manifest");
    assert_eq!(
        result.results.len(),
        1,
        "only 1 result — second job skipped due to fail_fast"
    );
    assert_eq!(result.results[0].execution_id, eid1);
    assert_eq!(result.results[0].status, ExecutionStatus::Failed);
    assert_eq!(result.succeeded, 0);
    assert_eq!(result.failed, 1);

    worker.abort();
    ctx.cleanup().await;
}

/// Batch mode still publishes status updates to NATS for monitoring.
#[tokio::test(flavor = "multi_thread")]
async fn test_batch_status_updates_published() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("batch-status-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("batch-status-test", &eid).await;

    let manifest = batch_manifest(vec![echo_job(&eid)]);

    let worker = ctx.spawn_worker();
    let runner = BatchRunner::new(ctx.storage.clone(), ctx.reporter.clone(), false);
    let result = runner.run(&manifest).await;

    assert_eq!(result.succeeded, 1);

    // Verify status updates were published to NATS
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

    worker.abort();
    ctx.cleanup().await;
}
