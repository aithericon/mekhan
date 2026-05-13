use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_worker::CleanupPolicy;
use uuid::Uuid;

/// Verify: Retain policy keeps run directory after Completed.
#[tokio::test]
async fn test_cleanup_retain() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("retain-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("retain", &eid).await;
    let worker = ctx.spawn_worker_with(CleanupPolicy::Retain, None);

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

    // Allow async cleanup to settle
    tokio::time::sleep(Duration::from_millis(200)).await;

    let run_dir = ctx.run_dir_for(&eid);
    assert!(
        run_dir.root.exists(),
        "run directory should be retained: {}",
        run_dir.root.display()
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: Immediate policy removes run directory after Completed.
#[tokio::test]
async fn test_cleanup_immediate_success() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("imm-ok-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("imm-ok", &eid).await;
    let worker = ctx.spawn_worker_with(CleanupPolicy::Immediate, None);

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

    tokio::time::sleep(Duration::from_millis(200)).await;

    let run_dir = ctx.run_dir_for(&eid);
    assert!(
        !run_dir.root.exists(),
        "run directory should be removed: {}",
        run_dir.root.display()
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: Immediate policy removes run directory after Failed.
#[tokio::test]
async fn test_cleanup_immediate_failure() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("imm-fail-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("imm-fail", &eid).await;
    let worker = ctx.spawn_worker_with(CleanupPolicy::Immediate, None);

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

    tokio::time::sleep(Duration::from_millis(200)).await;

    let run_dir = ctx.run_dir_for(&eid);
    assert!(
        !run_dir.root.exists(),
        "run directory should be removed on failure: {}",
        run_dir.root.display()
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: OnSuccess policy removes run directory after Completed.
#[tokio::test]
async fn test_cleanup_on_success_completed() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("onsuc-ok-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("onsuc-ok", &eid).await;
    let worker = ctx.spawn_worker_with(CleanupPolicy::OnSuccess, None);

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

    tokio::time::sleep(Duration::from_millis(200)).await;

    let run_dir = ctx.run_dir_for(&eid);
    assert!(
        !run_dir.root.exists(),
        "run directory should be removed on success: {}",
        run_dir.root.display()
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: OnSuccess policy retains run directory after Failed.
#[tokio::test]
async fn test_cleanup_on_success_failed() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("onsuc-fail-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("onsuc-fail", &eid).await;
    let worker = ctx.spawn_worker_with(CleanupPolicy::OnSuccess, None);

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

    tokio::time::sleep(Duration::from_millis(200)).await;

    let run_dir = ctx.run_dir_for(&eid);
    assert!(
        run_dir.root.exists(),
        "run directory should be retained on failure: {}",
        run_dir.root.display()
    );

    worker.abort();
    ctx.cleanup().await;
}
