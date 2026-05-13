use std::collections::HashMap;
use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use uuid::Uuid;

/// Verify end-to-end: push echo job → Accepted → Running → Completed with stdout.
#[tokio::test]
async fn test_e2e_echo_execution() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("echo-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("echo-test", &eid).await;
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

    // Verify execution_id and source on all updates
    for update in &statuses {
        assert_eq!(update.execution_id, eid);
        assert_eq!(update.source, "test-executor");
    }

    // Verify completed detail has stdout_tail
    let completed = statuses.last().unwrap();
    let stdout_tail = completed.detail["stdout_tail"].as_str().unwrap();
    assert_eq!(stdout_tail, "hello\n");

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: push `false` → Accepted → Running → Failed with exit_code 1.
#[tokio::test]
async fn test_failed_execution() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("fail-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("fail-test", &eid).await;
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

    // Verify exit_code in failed detail
    let failed = statuses.last().unwrap();
    let exit_code = failed.detail["outcome"]["exit_code"].as_i64().unwrap();
    assert_eq!(exit_code, 1);

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: push sleep 60 with 1s timeout → Accepted → Running → TimedOut.
#[tokio::test(flavor = "multi_thread")]
async fn test_timeout_execution() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("timeout-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("timeout-test", &eid).await;
    let worker = ctx.spawn_worker();

    ctx.push_job(sleep_job(&eid, 60, 1)).await;

    // Allow 15s: 1s exec timeout + 5s SIGTERM grace + margin
    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(15))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::TimedOut,
        ],
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify that metadata from the job is echoed on every StatusUpdate.
#[tokio::test]
async fn test_metadata_echoback() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("meta-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("meta-test", &eid).await;
    let worker = ctx.spawn_worker();

    let metadata = HashMap::from([
        ("petri_net_id".to_string(), "my-net".to_string()),
        ("petri_signal_key".to_string(), "abc-123".to_string()),
    ]);
    ctx.push_job(job_with_metadata(&eid, metadata.clone()))
        .await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    assert!(!statuses.is_empty(), "expected at least one status update");

    for update in &statuses {
        assert_eq!(
            update.metadata, metadata,
            "metadata mismatch on {} status",
            update.status
        );
    }

    worker.abort();
    ctx.cleanup().await;
}

/// Verify JetStream dedup: re-publishing with same Nats-Msg-Id doesn't increase message count.
#[tokio::test]
async fn test_status_dedup() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("dedup-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("dedup-test", &eid).await;
    let worker = ctx.spawn_worker();

    ctx.push_job(echo_job(&eid)).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    assert!(
        statuses.last().unwrap().status.is_terminal(),
        "expected terminal status"
    );

    // Record message count after normal execution
    let count_before = ctx.status_message_count().await;

    // Re-publish duplicate statuses with the same Nats-Msg-Id headers
    let js = ctx.jetstream().clone();
    for update in &statuses {
        let subject = format!(
            "{}.executor.status.{}.{}",
            ctx.prefix,
            update.execution_id,
            update.status.as_str()
        );
        let msg_id = update.msg_id();
        let payload = serde_json::to_vec(update).unwrap();

        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", msg_id.as_str());

        let ack = js
            .publish_with_headers(subject, headers, payload.into())
            .await
            .expect("publish failed");
        // Await ack — duplicates are accepted but silently deduplicated
        let _ = ack.await;
    }

    let count_after = ctx.status_message_count().await;
    assert_eq!(
        count_before, count_after,
        "message count should not change after re-publishing duplicates"
    );

    worker.abort();
    ctx.cleanup().await;
}
