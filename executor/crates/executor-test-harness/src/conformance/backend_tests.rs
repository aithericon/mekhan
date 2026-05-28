use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use aithericon_executor_domain::{ExecutionOutcome, ExecutionStatus};

use super::kit::BackendTestKit;

// ─── Shared utilities ────────────────────────────────────────────────

/// Status callback type alias used in tests.
type TestStatusCallback =
    Box<dyn Fn(ExecutionStatus, Value) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Status log type alias for test tracking.
pub type StatusLog = Arc<Mutex<Vec<(ExecutionStatus, Value)>>>;

/// Status callback that does nothing.
pub fn noop_callback() -> TestStatusCallback {
    Box::new(|_status, _detail| Box::pin(async {}))
}

/// Status callback that records all calls.
pub fn tracking_callback() -> (TestStatusCallback, StatusLog) {
    let log: StatusLog = Arc::new(Mutex::new(Vec::new()));
    let log_clone = log.clone();
    let cb: TestStatusCallback = Box::new(move |status, detail| {
        let log = log_clone.clone();
        Box::pin(async move {
            log.lock().unwrap().push((status, detail));
        })
    });
    (cb, log)
}

// ─── Contract 1: Success ─────────────────────────────────────────────

/// Exit code 0 produces `ExecutionOutcome::Success` and captures stdout.
pub async fn test_success<K: BackendTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.echo_spec();
    let ctx = kit
        .make_run_context(spec, Duration::from_secs(30), HashMap::new())
        .await;

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute failed");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert!(
        result
            .stdout_tail
            .as_deref()
            .unwrap_or("")
            .contains("hello"),
        "[{}] stdout should contain 'hello', got: {:?}",
        kit.backend_name(),
        result.stdout_tail
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 2: Exit Failure ────────────────────────────────────────

/// Non-zero exit code produces `ExecutionOutcome::ExitFailure`.
pub async fn test_exit_failure<K: BackendTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.failing_spec();
    let ctx = kit
        .make_run_context(spec, Duration::from_secs(30), HashMap::new())
        .await;

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute failed");

    assert!(
        matches!(
            result.outcome,
            ExecutionOutcome::ExitFailure { exit_code: 1 }
        ),
        "[{}] expected ExitFailure(1), got {:?}",
        kit.backend_name(),
        result.outcome
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 3: Timeout ─────────────────────────────────────────────

/// Long process + short timeout produces `ExecutionOutcome::TimedOut`.
pub async fn test_timeout<K: BackendTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.sleep_spec(60);
    let ctx = kit
        .make_run_context(spec, Duration::from_millis(500), HashMap::new())
        .await;

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute failed");

    assert!(
        matches!(result.outcome, ExecutionOutcome::TimedOut),
        "[{}] expected TimedOut, got {:?}",
        kit.backend_name(),
        result.outcome
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 4: Cancellation ────────────────────────────────────────

/// `CancellationToken.cancel()` produces `ExecutionOutcome::Cancelled`.
pub async fn test_cancellation<K: BackendTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.sleep_spec(60);
    let ctx = kit
        .make_run_context(spec, Duration::from_secs(60), HashMap::new())
        .await;

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    let result = backend
        .execute(&ctx, noop_callback(), None, cancel)
        .await
        .expect("execute failed");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Cancelled),
        "[{}] expected Cancelled, got {:?}",
        kit.backend_name(),
        result.outcome
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 5: Status Callback ─────────────────────────────────────

/// Backend calls `status_cb(Running, detail)` with backend-specific info.
pub async fn test_status_callback<K: BackendTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.echo_spec();
    let ctx = kit
        .make_run_context(spec, Duration::from_secs(30), HashMap::new())
        .await;

    let (cb, log) = tracking_callback();
    backend
        .execute(&ctx, cb, None, CancellationToken::new())
        .await
        .expect("execute failed");

    {
        let entries = log.lock().unwrap();
        assert!(
            !entries.is_empty(),
            "[{}] expected at least one status callback",
            kit.backend_name()
        );
        assert_eq!(
            entries[0].0,
            ExecutionStatus::Running,
            "[{}] first callback should be Running",
            kit.backend_name()
        );
    }

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 6: Env Vars ────────────────────────────────────────────

/// `RunContext.env` is passed to the child process.
pub async fn test_env_vars<K: BackendTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.env_echo_spec();
    let env = HashMap::from([(
        "CONFORMANCE_TEST_VAR".to_string(),
        "conformance_42".to_string(),
    )]);
    let ctx = kit
        .make_run_context(spec, Duration::from_secs(30), env)
        .await;

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute failed");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert!(
        result
            .stdout_tail
            .as_deref()
            .unwrap_or("")
            .contains("conformance_42"),
        "[{}] stdout should contain env var value, got: {:?}",
        kit.backend_name(),
        result.stdout_tail
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 7: Independent Output Capture ──────────────────────────

/// stdout and stderr are captured independently with no cross-contamination.
pub async fn test_output_capture<K: BackendTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.dual_output_spec();
    let ctx = kit
        .make_run_context(spec, Duration::from_secs(30), HashMap::new())
        .await;

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute failed");

    let stdout = result.stdout_tail.as_deref().unwrap_or("");
    let stderr = result.stderr_tail.as_deref().unwrap_or("");

    assert!(
        stdout.contains("stdout_marker"),
        "[{}] stdout should contain 'stdout_marker', got: {:?}",
        kit.backend_name(),
        stdout
    );
    assert!(
        stderr.contains("stderr_marker"),
        "[{}] stderr should contain 'stderr_marker', got: {:?}",
        kit.backend_name(),
        stderr
    );
    assert!(
        !stdout.contains("stderr_marker"),
        "[{}] stdout should not contain stderr content",
        kit.backend_name()
    );
    assert!(
        !stderr.contains("stdout_marker"),
        "[{}] stderr should not contain stdout content",
        kit.backend_name()
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 8: Duration Tracked ────────────────────────────────────

/// `result.duration` reflects wall-clock time (non-zero for any execution).
pub async fn test_duration_tracked<K: BackendTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.echo_spec();
    let ctx = kit
        .make_run_context(spec, Duration::from_secs(30), HashMap::new())
        .await;

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute failed");

    assert!(
        !result.duration.is_zero(),
        "[{}] duration should be non-zero",
        kit.backend_name()
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 9: Large Output Bounded ────────────────────────────────

/// TailBuffer keeps output bounded (no OOM on large output).
pub async fn test_large_output_bounded<K: BackendTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.large_output_spec(200_000); // 200KB
    let ctx = kit
        .make_run_context(spec, Duration::from_secs(30), HashMap::new())
        .await;

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute failed");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );

    // Default TailBuffer is 64KB. Output should be captured but bounded.
    let stdout = result.stdout_tail.as_deref().unwrap_or("");
    assert!(
        stdout.len() <= 65536,
        "[{}] stdout_tail should be bounded to 64KB, got {} bytes",
        kit.backend_name(),
        stdout.len()
    );
    assert!(
        !stdout.is_empty(),
        "[{}] stdout_tail should not be empty",
        kit.backend_name()
    );

    kit.cleanup_run_context(&ctx).await;
}
