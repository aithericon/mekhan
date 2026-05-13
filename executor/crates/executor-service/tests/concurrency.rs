use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use uuid::Uuid;

/// Verify two jobs execute concurrently and complete independently with correct results.
#[tokio::test]
async fn test_concurrent_jobs_complete_independently() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;

    let eid_a = format!("conc-a-{}", Uuid::new_v4().simple());
    let eid_b = format!("conc-b-{}", Uuid::new_v4().simple());

    let consumer_a = ctx.status_consumer("conc-a", &eid_a).await;
    let consumer_b = ctx.status_consumer("conc-b", &eid_b).await;

    // Worker has concurrency(2) — both jobs should run simultaneously
    let worker = ctx.spawn_worker();

    // Push two jobs with distinct stdout values
    ctx.push_job(bash_job(&eid_a, "echo output_alpha")).await;
    ctx.push_job(bash_job(&eid_b, "echo output_beta")).await;

    // Collect statuses for both jobs concurrently
    let (statuses_a, statuses_b) = tokio::join!(
        ctx.collect_statuses(&consumer_a, Duration::from_secs(10)),
        ctx.collect_statuses(&consumer_b, Duration::from_secs(10)),
    );

    // Both should complete successfully
    assert_status_sequence(
        &statuses_a,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ],
    );
    assert_status_sequence(
        &statuses_b,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ],
    );

    // Verify correct stdout for each (no cross-contamination)
    let stdout_a = statuses_a.last().unwrap().detail["stdout_tail"]
        .as_str()
        .unwrap();
    let stdout_b = statuses_b.last().unwrap().detail["stdout_tail"]
        .as_str()
        .unwrap();

    assert_eq!(stdout_a, "output_alpha\n");
    assert_eq!(stdout_b, "output_beta\n");

    // Verify execution_ids are correct on all updates
    for update in &statuses_a {
        assert_eq!(update.execution_id, eid_a);
    }
    for update in &statuses_b {
        assert_eq!(update.execution_id, eid_b);
    }

    worker.abort();
    ctx.cleanup().await;
}
