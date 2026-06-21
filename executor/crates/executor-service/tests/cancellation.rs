use std::sync::atomic::Ordering;
use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_worker::CancelListenerTuning;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Poll until `token` is cancelled or `timeout` elapses; returns whether it was.
async fn await_cancel(token: &CancellationToken, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline && !token.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    token.is_cancelled()
}

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

/// Regression guard for the JetStream cancel transport: a cancel published while
/// no consumer is bound must NOT be replayed onto a freshly-bound consumer
/// (`DeliverPolicy::New`). Otherwise a runner restart could re-cancel a reused
/// execution id. A cancel published AFTER the bind must still be delivered.
#[tokio::test(flavor = "multi_thread")]
async fn test_cancel_deliver_new_ignores_stale() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("cancel-stale-{}", Uuid::new_v4().simple());

    // First listener: ensures the EXECUTOR_CANCEL stream + binds a consumer.
    // Shut it down so no consumer is pulling while we publish the stale cancel.
    let s1 = CancellationToken::new();
    let h1 = ctx.start_cancel_listener(s1.clone()).await;
    s1.cancel();
    let _ = h1.await;

    // A token as if a job were running.
    let token = ctx.cancel_registry.register(&eid);

    // Stale cancel: published while no consumer is bound (the stream persists).
    ctx.publish_cancel(&eid).await;

    // Fresh listener: its `DeliverPolicy::New` consumer must NOT replay the cancel
    // published before it bound.
    let s2 = CancellationToken::new();
    let h2 = ctx.start_cancel_listener(s2.clone()).await;

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !token.is_cancelled(),
        "DeliverPolicy::New consumer replayed a cancel published before it bound"
    );

    // Sanity: a cancel published AFTER the bind IS delivered.
    ctx.publish_cancel(&eid).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !token.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        token.is_cancelled(),
        "cancel published after the consumer bound was not delivered"
    );

    s2.cancel();
    let _ = h2.await;
    ctx.cleanup().await;
}

/// Regression guard: a duplicate delivery of an already-running execution_id must
/// NOT evict the live execution's cancellation token.
///
/// A duplicate delivery (apalis at-least-once redelivery, parallel pool
/// consumers, or Nomad dispatching multiple allocations for one execution_id)
/// loses the run-directory lock and is skipped. Before the fix, the skipped
/// duplicate had already `register`ed (replacing) then `deregister`ed the token,
/// emptying the registry — so a later cancel found nothing and the running job
/// ran to completion. The token is now registered only after the lock is won.
#[tokio::test(flavor = "multi_thread")]
async fn test_duplicate_delivery_preserves_live_cancel_token() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("cancel-dup-{}", Uuid::new_v4().simple());

    // The WINNER delivery: holds the run-dir lock and has its cancel token registered.
    let live = ctx.cancel_registry.register(&eid);
    ctx.precreate_run_lock(&eid).await;
    assert_eq!(ctx.cancel_registry.active_count(), 1);

    // A DUPLICATE delivery of the same execution_id: must lose the lock and skip
    // without touching the registry.
    let status = ctx.execute_once(&long_running_job(&eid, 300)).await;
    assert_eq!(
        status,
        ExecutionStatus::Failed,
        "duplicate delivery should skip (run-dir lock already held)"
    );

    // The live token must still be registered and cancellable.
    assert_eq!(
        ctx.cancel_registry.active_count(),
        1,
        "duplicate delivery deregistered the live execution's cancel token"
    );
    assert!(
        ctx.cancel_registry.cancel(&eid),
        "live cancel token missing after duplicate delivery"
    );
    assert!(
        live.is_cancelled(),
        "the registered live token was not the one cancelled"
    );

    ctx.cleanup().await;
}

/// Idle-survival: the heartbeated continuous pull keeps the listener's ephemeral
/// consumer alive across an idle period far longer than its `inactive_threshold`,
/// so a cancel that arrives after a long quiet stretch is still delivered — on
/// the SAME consumer, with no rebind churn.
///
/// Cancels can be hours apart in prod; if the consumer were reaped during idle
/// the signal would be silently dropped. Tuned to seconds: a 2s reap window is
/// idled past by 3× before the cancel is published, and `rebinds == 0` proves
/// the original consumer was never reaped/replaced.
#[tokio::test(flavor = "multi_thread")]
async fn test_cancel_consumer_survives_idle() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("cancel-idle-{}", Uuid::new_v4().simple());

    let shutdown = CancellationToken::new();
    let listener = ctx
        .start_cancel_listener_tuned(
            shutdown.clone(),
            CancelListenerTuning {
                inactive_threshold: Duration::from_secs(2),
                heartbeat: Duration::from_millis(500),
                rebind_backoff: Duration::from_millis(200),
            },
        )
        .await;

    // A token as if a job were running.
    let token = ctx.cancel_registry.register(&eid);

    // Idle well past the 2s reap window. If the continuous pull did not keep the
    // consumer active, the server would reap it here.
    tokio::time::sleep(Duration::from_secs(6)).await;

    // A cancel after the long idle must still land.
    ctx.publish_cancel(&eid).await;
    assert!(
        await_cancel(&token, Duration::from_secs(5)).await,
        "cancel after a long idle was not delivered (consumer reaped during idle?)"
    );

    // ...and it landed on the original consumer — no reap, no rebind churn.
    assert_eq!(
        listener.rebinds.load(Ordering::Relaxed),
        0,
        "consumer was rebound during idle; heartbeat pull is not keeping it alive"
    );

    shutdown.cancel();
    let _ = listener.handle.await;
    ctx.cleanup().await;
}

/// Rebind-on-dead-consumer: if the ephemeral consumer is reaped/deleted
/// out from under the listener, it rebinds a FRESH consumer rather than
/// tight-spinning on the dead one ("no responders"), and resumes delivering
/// cancels.
///
/// Simulates the reap by force-deleting the consumer server-side, then asserts
/// the listener recovers (`rebinds >= 1`) and a cancel published after the
/// rebind is delivered.
#[tokio::test(flavor = "multi_thread")]
async fn test_cancel_consumer_rebinds_after_reap() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("cancel-rebind-{}", Uuid::new_v4().simple());

    let shutdown = CancellationToken::new();
    let listener = ctx
        .start_cancel_listener_tuned(
            shutdown.clone(),
            CancelListenerTuning {
                // Don't auto-reap; we delete the consumer explicitly below.
                inactive_threshold: Duration::from_secs(300),
                heartbeat: Duration::from_millis(500),
                rebind_backoff: Duration::from_millis(200),
            },
        )
        .await;

    let token = ctx.cancel_registry.register(&eid);

    // Let the listener settle into its pull loop, then yank its consumer.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let deleted = ctx.delete_cancel_consumers().await;
    assert_eq!(deleted, 1, "expected exactly one cancel consumer to delete");

    // The listener must notice the dead consumer (heartbeat/pull error) and bind
    // a fresh one. Wait for that rebind rather than a fixed sleep.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline && listener.rebinds.load(Ordering::Relaxed) == 0 {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        listener.rebinds.load(Ordering::Relaxed) >= 1,
        "listener did not rebind after its consumer was deleted"
    );

    // A cancel published after the fresh consumer bound must be delivered.
    ctx.publish_cancel(&eid).await;
    assert!(
        await_cancel(&token, Duration::from_secs(10)).await,
        "cancel not delivered after the listener rebound its consumer"
    );

    shutdown.cancel();
    let _ = listener.handle.await;
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
