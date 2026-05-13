use std::time::Duration;

use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_worker::{drain_signal, DrainConfig};
use uuid::Uuid;

/// With max_jobs=3, the drain signal fires after processing 3 jobs.
#[tokio::test(flavor = "multi_thread")]
async fn test_drain_max_jobs() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let (worker, tracker) = ctx.spawn_worker_with_tracker();

    // Subscribe to completions BEFORE pushing jobs (like production).
    let rx = tracker.subscribe();
    let config = DrainConfig {
        min_jobs: None,
        max_jobs: Some(3),
        idle_timeout: Duration::from_secs(60),
    };

    // Push 3 jobs
    for i in 0..3 {
        let eid = format!("drain-max-{i}-{}", Uuid::new_v4().simple());
        ctx.push_job(echo_job(&eid)).await;
    }

    // drain_signal should return once all 3 complete
    tokio::time::timeout(Duration::from_secs(15), drain_signal(rx, &config))
        .await
        .expect("drain_signal should have returned after max_jobs=3");

    assert_eq!(tracker.completed(), 3);

    worker.abort();
    ctx.cleanup().await;
}

/// With min_jobs=2, the drain signal fires after 2 completions + idle timeout.
#[tokio::test(flavor = "multi_thread")]
async fn test_drain_min_jobs_then_idle() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let (worker, tracker) = ctx.spawn_worker_with_tracker();

    let rx = tracker.subscribe();
    let config = DrainConfig {
        min_jobs: Some(2),
        max_jobs: None,
        idle_timeout: Duration::from_millis(500),
    };

    // Push exactly 2 jobs
    for i in 0..2 {
        let eid = format!("drain-min-{i}-{}", Uuid::new_v4().simple());
        ctx.push_job(echo_job(&eid)).await;
    }

    // drain_signal: waits for min_jobs=2, then idle timeout
    tokio::time::timeout(Duration::from_secs(15), drain_signal(rx, &config))
        .await
        .expect("drain_signal should have returned after min_jobs met + idle timeout");

    assert_eq!(tracker.completed(), 2);

    worker.abort();
    ctx.cleanup().await;
}

/// With min_jobs=3 but only 1 job available, drain should NOT exit on idle alone.
#[tokio::test(flavor = "multi_thread")]
async fn test_drain_no_idle_before_min_jobs() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let (worker, tracker) = ctx.spawn_worker_with_tracker();

    let rx = tracker.subscribe();
    let config = DrainConfig {
        min_jobs: Some(3),
        max_jobs: None,
        idle_timeout: Duration::from_millis(200),
    };

    // Push only 1 job
    let eid = format!("drain-noidle-{}", Uuid::new_v4().simple());
    ctx.push_job(echo_job(&eid)).await;

    // Wait long enough for the job to complete and idle to expire (if it were eligible)
    let result = tokio::time::timeout(Duration::from_secs(3), drain_signal(rx, &config)).await;
    assert!(
        result.is_err(),
        "drain should NOT exit before min_jobs (only 1 of 3 completed)"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// With no limits set, drain exits on idle timeout alone.
#[tokio::test(flavor = "multi_thread")]
async fn test_drain_no_limits_idle_exit() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let (worker, tracker) = ctx.spawn_worker_with_tracker();

    let rx = tracker.subscribe();
    // Use a longer idle timeout so the job has time to complete before
    // the first idle period expires.
    let config = DrainConfig {
        min_jobs: None,
        max_jobs: None,
        idle_timeout: Duration::from_secs(5),
    };

    // Push 1 job
    let eid = format!("drain-nolimit-{}", Uuid::new_v4().simple());
    ctx.push_job(echo_job(&eid)).await;

    // Should exit after job completes + idle timeout
    tokio::time::timeout(Duration::from_secs(15), drain_signal(rx, &config))
        .await
        .expect("drain_signal should have returned after idle timeout");

    assert!(tracker.completed() >= 1);

    worker.abort();
    ctx.cleanup().await;
}

/// Both successful and failed jobs count toward drain limits.
#[tokio::test(flavor = "multi_thread")]
async fn test_drain_counts_failures() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let (worker, tracker) = ctx.spawn_worker_with_tracker();

    let rx = tracker.subscribe();
    let config = DrainConfig {
        min_jobs: None,
        max_jobs: Some(2),
        idle_timeout: Duration::from_secs(60),
    };

    // Push 1 success + 1 failure
    let eid_ok = format!("drain-ok-{}", Uuid::new_v4().simple());
    let eid_fail = format!("drain-fail-{}", Uuid::new_v4().simple());
    ctx.push_job(echo_job(&eid_ok)).await;
    ctx.push_job(failing_job(&eid_fail)).await;

    // max_jobs=2 should trigger after both complete (regardless of success/failure)
    tokio::time::timeout(Duration::from_secs(15), drain_signal(rx, &config))
        .await
        .expect("drain_signal should have returned with max_jobs=2 (1 success + 1 failure)");

    assert_eq!(tracker.completed(), 2);

    worker.abort();
    ctx.cleanup().await;
}
