use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use uuid::Uuid;

/// Verify stderr is captured in the terminal detail.
#[tokio::test]
async fn test_stderr_captured() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("stderr-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("stderr", &eid).await;
    let worker = ctx.spawn_worker();

    ctx.push_job(bash_job(&eid, "echo error_msg >&2")).await;

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
    let stderr_tail = completed.detail["stderr_tail"]
        .as_str()
        .expect("stderr_tail should be present");
    assert!(
        stderr_tail.contains("error_msg"),
        "stderr_tail should contain 'error_msg', got: {stderr_tail:?}"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify stdout and stderr are captured independently (no cross-contamination).
#[tokio::test]
async fn test_stdout_and_stderr_independent() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("stdio-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("stdio", &eid).await;
    let worker = ctx.spawn_worker();

    ctx.push_job(bash_job(&eid, "echo out_msg && echo err_msg >&2"))
        .await;

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
    let stdout_tail = completed.detail["stdout_tail"]
        .as_str()
        .expect("stdout_tail should be present");
    let stderr_tail = completed.detail["stderr_tail"]
        .as_str()
        .expect("stderr_tail should be present");

    assert_eq!(stdout_tail, "out_msg\n");
    assert_eq!(stderr_tail, "err_msg\n");

    // No cross-contamination
    assert!(
        !stdout_tail.contains("err_msg"),
        "stdout should not contain stderr content"
    );
    assert!(
        !stderr_tail.contains("out_msg"),
        "stderr should not contain stdout content"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify TailBuffer truncates output exceeding the 64KB limit.
#[tokio::test]
async fn test_tailbuffer_truncates_large_output() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("tailbuf-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("tailbuf", &eid).await;
    let worker = ctx.spawn_worker();

    // Produce 100KB of output; default TailBuffer limit is 64KB (65536 bytes)
    ctx.push_job(large_output_job(&eid, 100_000)).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(15))
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
    let stdout_tail = completed.detail["stdout_tail"]
        .as_str()
        .expect("stdout_tail should be present");

    // Output was truncated to 64KB
    assert!(
        stdout_tail.len() <= 65536,
        "stdout_tail should be at most 64KB, got {} bytes",
        stdout_tail.len()
    );

    // But it's not empty — proves truncation happened, not data loss
    assert!(
        stdout_tail.len() > 60000,
        "stdout_tail should contain substantial data (proof of truncation), got {} bytes",
        stdout_tail.len()
    );

    // Content should be all 'x' characters (from tr '\0' 'x')
    assert!(
        stdout_tail.chars().all(|c| c == 'x'),
        "stdout_tail should contain only 'x' characters"
    );

    worker.abort();
    ctx.cleanup().await;
}
