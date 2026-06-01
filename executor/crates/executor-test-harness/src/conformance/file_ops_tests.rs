use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_domain::{ExecutionOutcome, ExecutionStatus};

use super::file_ops_kit::FileOpsTestKit;

// ─── Shared utilities ────────────────────────────────────────────────

type TestStatusCallback =
    Box<dyn Fn(ExecutionStatus, Value) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

pub type StatusLog = Arc<Mutex<Vec<(ExecutionStatus, Value)>>>;

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

/// Run prepare → execute for a spec, returning the result.
async fn prepare_and_execute(
    backend: &Arc<dyn ExecutionBackend>,
    kit: &impl FileOpsTestKit,
    spec: aithericon_executor_domain::ExecutionSpec,
    timeout: Duration,
    cb: TestStatusCallback,
    cancel: CancellationToken,
) -> aithericon_executor_domain::ExecutionResult {
    let ctx = kit.make_run_context(spec.clone(), timeout).await;
    let job = kit.spec_to_job(&ctx.execution_id, spec);
    let ctx = backend
        .prepare(&job, ctx)
        .await
        .expect("prepare should succeed");
    let result = backend
        .execute(&ctx, cb, None, cancel)
        .await
        .expect("execute should not return Err");
    kit.cleanup_run_context(&ctx).await;
    result
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

// ─── Contract 1: Stat existing file ─────────────────────────────────

/// Stat an existing file produces `Success` with `exists: true` and valid metadata.
pub async fn test_stat_existing<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.stat_existing_spec();

    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        noop_callback(),
        CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert_eq!(
        result.outputs["exists"],
        serde_json::json!(true),
        "[{}] stat existing should report exists: true",
        kit.backend_name()
    );
    assert!(
        result.outputs["content_length"].as_u64().unwrap_or(0) > 0,
        "[{}] stat existing should report non-zero content_length",
        kit.backend_name()
    );
}

// ─── Contract 2: Stat missing file ──────────────────────────────────

/// Stat a missing file produces `Success` with `exists: false` (not an error).
pub async fn test_stat_missing<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.stat_missing_spec();

    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        noop_callback(),
        CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] stat missing should still be Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert_eq!(
        result.outputs["exists"],
        serde_json::json!(false),
        "[{}] stat missing should report exists: false",
        kit.backend_name()
    );
}

// ─── Contract 3: Delete existing file ───────────────────────────────

/// Delete an existing file produces `Success` and the file is actually gone.
pub async fn test_delete_existing<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.delete_existing_spec();

    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        noop_callback(),
        CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert!(
        !kit.verify_file_exists("data/hello.csv").await,
        "[{}] file should be deleted after delete operation",
        kit.backend_name()
    );
}

// ─── Contract 4: Copy existing file ─────────────────────────────────

/// Copy produces `Success` and both source and destination exist.
pub async fn test_copy_existing<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.copy_existing_spec();

    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        noop_callback(),
        CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert!(
        kit.verify_file_exists("data/hello.csv").await,
        "[{}] source should still exist after copy",
        kit.backend_name()
    );
    assert!(
        kit.verify_file_exists("copy/hello.csv").await,
        "[{}] destination should exist after copy",
        kit.backend_name()
    );

    // Content should match
    let src = kit.verify_file_content("data/hello.csv").await;
    let dst = kit.verify_file_content("copy/hello.csv").await;
    assert_eq!(
        src,
        dst,
        "[{}] copied file content should match source",
        kit.backend_name()
    );
}

// ─── Contract 5: Move existing file ─────────────────────────────────

/// Move produces `Success`, source is gone, destination exists.
pub async fn test_move_existing<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.move_existing_spec();

    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        noop_callback(),
        CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert!(
        !kit.verify_file_exists("data/sample.parquet").await,
        "[{}] source should be gone after move",
        kit.backend_name()
    );
    assert!(
        kit.verify_file_exists("moved/sample.parquet").await,
        "[{}] destination should exist after move",
        kit.backend_name()
    );
}

// ─── Contract 6: List files ─────────────────────────────────────────

/// List produces `Success` with `count > 0` and a non-empty `files` array.
pub async fn test_list<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.list_spec();

    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        noop_callback(),
        CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert!(
        result.outputs["count"].as_u64().unwrap_or(0) > 0,
        "[{}] list count should be > 0",
        kit.backend_name()
    );
    let files = result.outputs["files"]
        .as_array()
        .expect("files should be an array");
    assert!(
        !files.is_empty(),
        "[{}] files array should not be empty",
        kit.backend_name()
    );
}

// ─── Contract 7: Annotate ───────────────────────────────────────────

/// Annotate produces `Success` and creates a sidecar `.meta.json` file.
pub async fn test_annotate<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.annotate_spec();

    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        noop_callback(),
        CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert!(
        kit.verify_file_exists("data/hello.csv.meta.json").await,
        "[{}] sidecar .meta.json should exist after annotate",
        kit.backend_name()
    );

    // Verify sidecar content
    if let Some(content) = kit.verify_file_content("data/hello.csv.meta.json").await {
        let parsed: serde_json::Value =
            serde_json::from_slice(&content).expect("sidecar should be valid JSON");
        assert_eq!(
            parsed["source"],
            serde_json::json!("test"),
            "[{}] sidecar should contain annotations",
            kit.backend_name()
        );
    }
}

// ─── Contract 8: Error propagation ──────────────────────────────────

/// An operation that should fail produces `BackendError` (not `Err`).
pub async fn test_error_propagation<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.delete_missing_spec();

    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        noop_callback(),
        CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "[{}] expected BackendError, got {:?}",
        kit.backend_name(),
        result.outcome
    );
}

// ─── Contract 9: Config validation ──────────────────────────────────

/// Invalid config is rejected by `prepare()` with `Err(ExecutorError::Config)`.
pub async fn test_config_validation<K: FileOpsTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.invalid_config_spec();
    let ctx = kit.make_run_context(spec.clone(), DEFAULT_TIMEOUT).await;
    let job = kit.spec_to_job(&ctx.execution_id, spec);

    let result = backend.prepare(&job, ctx).await;
    assert!(
        result.is_err(),
        "[{}] prepare should reject invalid config",
        kit.backend_name()
    );
}

// ─── Contract 10: Cancellation ──────────────────────────────────────

/// Pre-cancelled token produces `ExecutionOutcome::Cancelled`.
pub async fn test_cancellation<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.stat_existing_spec();

    let cancel = CancellationToken::new();
    cancel.cancel();

    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        noop_callback(),
        cancel,
    )
    .await;

    assert!(
        matches!(result.outcome, ExecutionOutcome::Cancelled),
        "[{}] expected Cancelled, got {:?}",
        kit.backend_name(),
        result.outcome
    );
}

// ─── Contract 11: Status callback ───────────────────────────────────

/// Backend calls `status_cb(Running, detail)` with an `operation` field.
pub async fn test_status_callback<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.stat_existing_spec();

    let (cb, log) = tracking_callback();
    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        cb,
        CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "[{}] expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );

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
        entries[0].1.get("operation").is_some(),
        "[{}] Running detail should contain 'operation' field",
        kit.backend_name()
    );
}

// ─── Contract 12: Duration tracked ──────────────────────────────────

/// `result.duration` reflects wall-clock time (non-zero for any execution).
pub async fn test_duration_tracked<K: FileOpsTestKit>(kit: &K) {
    kit.seed_storage().await;
    let backend = kit.create_backend().await.expect("backend creation failed");
    let spec = kit.stat_existing_spec();

    let result = prepare_and_execute(
        &backend,
        kit,
        spec,
        DEFAULT_TIMEOUT,
        noop_callback(),
        CancellationToken::new(),
    )
    .await;

    assert!(
        !result.duration.is_zero(),
        "[{}] duration should be non-zero",
        kit.backend_name()
    );
}
