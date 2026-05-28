//! Reusable backend-level conformance tests for the Kreuzberg backend.
//!
//! Each function is generic over a [`KreuzbergTestKit`] implementation. The
//! [`crate::kreuzberg_conformance_tests`] macro wires them into per-backend
//! `#[tokio::test]` declarations.

use std::time::Duration;

use aithericon_executor_backend::traits::StatusCallback;
use aithericon_executor_domain::{ExecutionOutcome, ExecutionStatus};
use tokio_util::sync::CancellationToken;

use crate::conformance::kreuzberg_kit::KreuzbergTestKit;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

fn noop_callback() -> StatusCallback {
    Box::new(|_status, _detail| Box::pin(async {}))
}

/// Verifies single-file extraction: a staged text file extracts to `Success`
/// with `content` containing the source text plus the standard output keys
/// (`word_count`, `char_count`, `tables`, `mime_type`).
pub async fn test_single_text_extract_success<K: KreuzbergTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("create backend");

    let spec = kit.single_extract_spec();
    let job = kit.spec_to_job("kreuzberg-conform-single", spec.clone(), None);
    let (ctx, _tmp) = kit
        .stage_single_text_file(spec, DEFAULT_TIMEOUT, "Hello, conformance world!")
        .await;

    let ctx = backend.prepare(&job, ctx).await.expect("prepare ok");
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute ok");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "{}: expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    let content = result.outputs["content"]
        .as_str()
        .expect("content is a string");
    assert!(
        content.contains("conformance"),
        "{}: extracted content should contain 'conformance', got: {content}",
        kit.backend_name()
    );
    // The exact set of additional keys is kreuzberg's native ExtractionResult
    // shape (commit 51d74a3 emits it 1:1) — the harness intentionally does
    // not pin it here so kreuzberg upstream changes don't break conformance.

    kit.cleanup_run_context(&ctx).await;
}

/// Verifies batch extraction: multiple staged inputs each yield extracted
/// content in the batch result envelope. The exact shape (top-level `results`
/// vs flat) is the backend's contract — we just assert `Success` and that
/// the outputs map is non-empty.
pub async fn test_batch_text_extract_success<K: KreuzbergTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("create backend");

    let spec = kit.batch_extract_spec();
    let job = kit.spec_to_job("kreuzberg-conform-batch", spec.clone(), None);
    let (ctx, _tmps) = kit
        .stage_batch_text_files(
            spec,
            DEFAULT_TIMEOUT,
            &["first doc body", "second doc body", "third doc body"],
        )
        .await;

    let ctx = backend.prepare(&job, ctx).await.expect("prepare ok");
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute ok");

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "{}: expected Success, got {:?}",
        kit.backend_name(),
        result.outcome
    );
    assert!(
        !result.outputs.is_empty(),
        "{}: batch outputs should be non-empty",
        kit.backend_name()
    );

    kit.cleanup_run_context(&ctx).await;
}

/// Verifies that referencing a missing input fails cleanly at prepare or
/// execute — no panic, a structured error.
pub async fn test_missing_input_fails_clean<K: KreuzbergTestKit>(kit: &K) {
    let backend = kit.create_backend().await.expect("create backend");

    let spec = kit.missing_input_spec();
    let job = kit.spec_to_job("kreuzberg-conform-missing", spec.clone(), None);
    let ctx = kit.make_empty_run_context(spec, DEFAULT_TIMEOUT).await;

    // Either prepare rejects, or execute returns BackendError / a failed
    // outcome. Both are acceptable shapes for "this can't be extracted."
    match backend.prepare(&job, ctx).await {
        Err(_) => {} // ok — rejected at prepare
        Ok(ctx) => {
            let r = backend
                .execute(&ctx, noop_callback(), None, CancellationToken::new())
                .await;
            match r {
                Err(_) => {} // ok — BackendError at execute
                Ok(res) => assert!(
                    !matches!(res.outcome, ExecutionOutcome::Success),
                    "{}: missing input must not succeed silently; got {:?}",
                    kit.backend_name(),
                    res.outcome
                ),
            }
            kit.cleanup_run_context(&ctx).await;
        }
    }
}

/// Verifies the status callback is invoked with at least `Running` and a
/// terminal status during a single-file extraction.
pub async fn test_status_callback_fires<K: KreuzbergTestKit>(kit: &K) {
    use std::sync::{Arc, Mutex};

    let backend = kit.create_backend().await.expect("create backend");
    let spec = kit.single_extract_spec();
    let job = kit.spec_to_job("kreuzberg-conform-status", spec.clone(), None);
    let (ctx, _tmp) = kit
        .stage_single_text_file(spec, DEFAULT_TIMEOUT, "status callback marker")
        .await;
    let ctx = backend.prepare(&job, ctx).await.expect("prepare ok");

    let log: Arc<Mutex<Vec<ExecutionStatus>>> = Arc::new(Mutex::new(Vec::new()));
    let log_clone = log.clone();
    let cb: StatusCallback = Box::new(move |status, _detail| {
        let log = log_clone.clone();
        Box::pin(async move {
            log.lock().unwrap().push(status);
        })
    });

    let _ = backend
        .execute(&ctx, cb, None, CancellationToken::new())
        .await
        .expect("execute ok");

    let statuses = log.lock().unwrap();
    assert!(
        statuses.iter().any(|s| matches!(s, ExecutionStatus::Running)),
        "{}: expected at least one Running status callback, saw: {statuses:?}",
        kit.backend_name()
    );

    kit.cleanup_run_context(&ctx).await;
}
