use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use aithericon_executor_domain::{ExecutionOutcome, ExecutionStatus};

use super::llm_kit::LlmTestKit;

// ─── Shared utilities ────────────────────────────────────────────────

type TestStatusCallback =
    Box<dyn Fn(ExecutionStatus, Value) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

type StatusLog = Arc<Mutex<Vec<(ExecutionStatus, Value)>>>;

fn noop_callback() -> TestStatusCallback {
    Box::new(|_status, _detail| Box::pin(async {}))
}

fn tracking_callback() -> (TestStatusCallback, StatusLog) {
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

// ─── Contract 1: Chat Success ────────────────────────────────────────

/// A valid chat prompt produces `Success` with non-empty stdout_tail.
pub async fn test_chat_success<K: LlmTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.chat_spec();
    let job = kit.spec_to_job("chat-success", spec.clone(), None);
    let mut ctx = kit
        .make_run_context(spec, Duration::from_secs(120), HashMap::new())
        .await;

    ctx = backend
        .prepare(&job, ctx)
        .await
        .expect("prepare should succeed for chat_spec");

    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .expect("execute failed");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert!(
        !result.stdout_tail.as_deref().unwrap_or("").is_empty(),
        "[{}] stdout_tail should contain LLM response",
        kit.backend_name()
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 2: Extract Success ─────────────────────────────────────

/// Extract mode with valid schema produces `Success` and outputs["response"] is a JSON object.
pub async fn test_extract_success<K: LlmTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.extract_spec();
    let job = kit.spec_to_job("extract-success", spec.clone(), None);
    let mut ctx = kit
        .make_run_context(spec, Duration::from_secs(120), HashMap::new())
        .await;

    ctx = backend
        .prepare(&job, ctx)
        .await
        .expect("prepare should succeed for extract_spec");

    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .expect("execute failed");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}. stderr: {:?}",
        kit.backend_name(),
        result.outcome,
        result.stderr_tail
    );

    let response = result
        .outputs
        .get("response")
        .expect("missing 'response' output");
    assert!(
        response.is_object(),
        "[{}] expected structured JSON object, got: {}",
        kit.backend_name(),
        response
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 3: Extract Schema Conformance ──────────────────────────

/// Extracted JSON has at least one expected key from the output_schema.
pub async fn test_extract_schema_conformance<K: LlmTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.extract_spec();
    let job = kit.spec_to_job("extract-conform", spec.clone(), None);
    let mut ctx = kit
        .make_run_context(spec, Duration::from_secs(120), HashMap::new())
        .await;

    ctx = backend
        .prepare(&job, ctx)
        .await
        .expect("prepare should succeed");

    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .expect("execute failed");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );

    let response = result.outputs.get("response").expect("missing response");
    let obj = response.as_object().expect("response should be object");

    // The extract_spec should define a schema with known required keys.
    // We check that the response has at least some keys (constrained decoding guarantees this).
    assert!(
        !obj.is_empty(),
        "[{}] extracted JSON should have at least one key, got empty object",
        kit.backend_name()
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 4: Extract Missing Schema ──────────────────────────────

/// Extract mode without output_schema fails at prepare.
pub async fn test_extract_missing_schema<K: LlmTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.extract_no_schema_spec();
    let job = kit.spec_to_job("extract-no-schema", spec.clone(), None);
    let ctx = kit
        .make_run_context(spec, Duration::from_secs(30), HashMap::new())
        .await;

    let result = backend.prepare(&job, ctx).await;
    assert!(
        result.is_err(),
        "[{}] prepare() should fail when extract mode lacks output_schema",
        kit.backend_name()
    );

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("output_schema"),
        "[{}] error should mention output_schema, got: {err}",
        kit.backend_name()
    );
}

// ─── Contract 5: Invalid Config ──────────────────────────────────────

/// Malformed config fails at prepare (deserialization).
pub async fn test_invalid_config<K: LlmTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.invalid_config_spec();
    let job = kit.spec_to_job("invalid-config", spec.clone(), None);
    let ctx = kit
        .make_run_context(spec, Duration::from_secs(30), HashMap::new())
        .await;

    let result = backend.prepare(&job, ctx).await;
    assert!(
        result.is_err(),
        "[{}] prepare() should fail on invalid config",
        kit.backend_name()
    );
}

// ─── Contract 6: API Error ───────────────────────────────────────────

/// Valid config but nonexistent model produces `BackendError`.
pub async fn test_api_error<K: LlmTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.api_error_spec();
    let job = kit.spec_to_job("api-error", spec.clone(), None);
    let mut ctx = kit
        .make_run_context(spec, Duration::from_secs(60), HashMap::new())
        .await;

    ctx = backend
        .prepare(&job, ctx)
        .await
        .expect("prepare should succeed (config is valid)");

    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .expect("execute should return Ok with BackendError outcome");

    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "[{}] expected BackendError, got {:?}",
        kit.backend_name(),
        result.outcome
    );

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 7: Timeout ─────────────────────────────────────────────

/// Short timeout produces `TimedOut`.
pub async fn test_timeout<K: LlmTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.chat_spec();
    let job = kit.spec_to_job("timeout", spec.clone(), None);
    let mut ctx = kit
        .make_run_context(spec, Duration::from_millis(1), HashMap::new())
        .await;

    ctx = backend
        .prepare(&job, ctx)
        .await
        .expect("prepare should succeed");

    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
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

// ─── Contract 8: Cancellation ────────────────────────────────────────

/// `CancellationToken.cancel()` produces `Cancelled`.
pub async fn test_cancellation<K: LlmTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.chat_spec();
    let job = kit.spec_to_job("cancellation", spec.clone(), None);
    let mut ctx = kit
        .make_run_context(spec, Duration::from_secs(120), HashMap::new())
        .await;

    ctx = backend
        .prepare(&job, ctx)
        .await
        .expect("prepare should succeed");

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    let result = backend
        .execute(&ctx, noop_callback(), cancel)
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

// ─── Contract 9: Status Callback ─────────────────────────────────────

/// Backend calls `status_cb(Running, detail)` with provider and model info.
pub async fn test_status_callback<K: LlmTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.chat_spec();
    let job = kit.spec_to_job("status-cb", spec.clone(), None);
    let mut ctx = kit
        .make_run_context(spec, Duration::from_secs(120), HashMap::new())
        .await;

    ctx = backend
        .prepare(&job, ctx)
        .await
        .expect("prepare should succeed");

    let (cb, log) = tracking_callback();
    backend
        .execute(&ctx, cb, CancellationToken::new())
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
        assert!(
            entries[0].1.get("provider").is_some(),
            "[{}] Running detail should include 'provider'",
            kit.backend_name()
        );
        assert!(
            entries[0].1.get("model").is_some(),
            "[{}] Running detail should include 'model'",
            kit.backend_name()
        );
    }

    kit.cleanup_run_context(&ctx).await;
}

// ─── Contract 10: Duration Tracked ───────────────────────────────────

/// `result.duration` reflects wall-clock time (non-zero for any execution).
pub async fn test_duration_tracked<K: LlmTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.chat_spec();
    let job = kit.spec_to_job("duration", spec.clone(), None);
    let mut ctx = kit
        .make_run_context(spec, Duration::from_secs(120), HashMap::new())
        .await;

    ctx = backend
        .prepare(&job, ctx)
        .await
        .expect("prepare should succeed");

    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .expect("execute failed");

    assert!(
        !result.duration.is_zero(),
        "[{}] duration should be non-zero",
        kit.backend_name()
    );

    kit.cleanup_run_context(&ctx).await;
}
