//! Integration tests for the kreuzberg backend.
//!
//! Tests exercise the full `ExecutionBackend` trait contract:
//! `prepare()` → `execute()` lifecycle, status callbacks, cancellation,
//! error propagation, metrics population, and batch workflows.
//!
//! All tests use real temp files — kreuzberg extracts text from actual
//! filesystem paths, so we need real files for data visibility.
//!
//! Run with:
//!   cargo test -p aithericon-executor-kreuzberg

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionSpec, ExecutionStatus, JobPriority, RunContext,
    RunDirectory,
};
use aithericon_executor_kreuzberg::KreuzbergBackend;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn noop_callback() -> StatusCallback {
    Box::new(|_status, _detail| Box::pin(async {}))
}

type StatusLog = Arc<Mutex<Vec<(ExecutionStatus, Value)>>>;

fn tracking_callback() -> (StatusCallback, StatusLog) {
    let log: StatusLog = Arc::new(Mutex::new(Vec::new()));
    let log_clone = log.clone();
    let cb: StatusCallback = Box::new(move |status, detail| {
        let log = log_clone.clone();
        Box::pin(async move {
            log.lock().unwrap().push((status, detail));
        })
    });
    (cb, log)
}

fn make_spec(config: Value) -> ExecutionSpec {
    ExecutionSpec {
        backend: "kreuzberg".into(),
        inputs: vec![],
        outputs: vec![],
        config,
    }
}

fn make_job(spec: &ExecutionSpec) -> ExecutionJob {
    ExecutionJob {
        execution_id: format!(
            "kreuzberg-integ-{}",
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

fn make_run_context(spec: ExecutionSpec, timeout: Duration) -> RunContext {
    let seq = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!("kreuzberg-integ-{}-{}", std::process::id(), seq);
    RunContext {
        execution_id: id.clone(),
        spec,
        run_dir: RunDirectory::new(&std::env::temp_dir(), &id),
        timeout,
        env: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: Value::Null,
    }
}

fn make_run_context_with_inputs(
    spec: ExecutionSpec,
    timeout: Duration,
    staged_inputs: HashMap<String, PathBuf>,
) -> RunContext {
    let mut ctx = make_run_context(spec, timeout);
    ctx.staged_inputs = staged_inputs;
    ctx
}

/// Create a temp file with a given extension and content.
fn temp_file(extension: &str, content: &str) -> tempfile::NamedTempFile {
    let f = tempfile::Builder::new()
        .suffix(extension)
        .tempfile()
        .unwrap();
    std::fs::write(f.path(), content).unwrap();
    f
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Lifecycle tests — full prepare → execute
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_txt_extraction() {
    let backend = KreuzbergBackend::new();
    let tmp = temp_file(".txt", "Hello, world! This is a test document.");

    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    let staged = HashMap::from([("file".into(), tmp.path().to_path_buf())]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let (cb, log) = tracking_callback();
    let result = backend
        .execute(&ctx, cb, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );

    // Content should contain our text.
    let content = result.outputs["content"].as_str().unwrap();
    assert!(
        content.contains("Hello"),
        "content should contain 'Hello', got: {content}"
    );

    // Standard output keys should be present.
    assert!(result.outputs.contains_key("word_count"));
    assert!(result.outputs.contains_key("char_count"));
    assert!(result.outputs.contains_key("tables"));
    assert!(result.outputs.contains_key("mime_type"));

    // Metrics should be populated.
    let metrics = result.metrics.as_ref().expect("missing metrics");
    assert!(
        metrics
            .latest_values
            .contains_key("kreuzberg/extraction_time_ms")
    );
    assert!(
        metrics
            .latest_values
            .contains_key("kreuzberg/content_length")
    );
    assert!(metrics.latest_values.contains_key("kreuzberg/word_count"));
    assert!(
        metrics
            .latest_values
            .contains_key("kreuzberg/table_count")
    );

    // Status callback should have been called with Running.
    let entries = log.lock().unwrap();
    assert!(!entries.is_empty());
    assert_eq!(entries[0].0, ExecutionStatus::Running);
    assert_eq!(entries[0].1["mode"], "single");
}

#[tokio::test]
async fn single_extraction_with_explicit_file_name() {
    let backend = KreuzbergBackend::new();
    let tmp = temp_file(".txt", "Named input test.");

    let spec = make_spec(serde_json::json!({ "file": "document" }));
    let job = make_job(&spec);
    let staged = HashMap::from([("document".into(), tmp.path().to_path_buf())]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    let content = result.outputs["content"].as_str().unwrap();
    assert!(content.contains("Named input test"));
}

#[tokio::test]
async fn single_extraction_sole_input_auto_resolved() {
    let backend = KreuzbergBackend::new();
    let tmp = temp_file(".txt", "Auto-resolved from sole input.");

    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    // Single input with non-standard name — should auto-resolve.
    let staged = HashMap::from([("invoice".into(), tmp.path().to_path_buf())]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    let content = result.outputs["content"].as_str().unwrap();
    assert!(content.contains("Auto-resolved"));
}

// ---------------------------------------------------------------------------
// Batch extraction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn batch_extraction_all_inputs() {
    let backend = KreuzbergBackend::new();
    let tmp_a = temp_file(".txt", "First document content.");
    let tmp_b = temp_file(".txt", "Second document content.");

    let spec = make_spec(serde_json::json!({ "mode": "batch" }));
    let job = make_job(&spec);
    let staged = HashMap::from([
        ("a".into(), tmp_a.path().to_path_buf()),
        ("b".into(), tmp_b.path().to_path_buf()),
    ]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let (cb, log) = tracking_callback();
    let result = backend
        .execute(&ctx, cb, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );

    // Batch outputs.
    assert_eq!(result.outputs["total_files"], serde_json::json!(2));
    assert_eq!(result.outputs["successful"], serde_json::json!(2));
    assert_eq!(result.outputs["failed"], serde_json::json!(0));

    let results_arr = result.outputs["results"].as_array().unwrap();
    assert_eq!(results_arr.len(), 2);

    // Each result should have content.
    for entry in results_arr {
        assert!(entry["content"].is_string());
        assert!(entry["word_count"].is_number());
    }

    // Progress should be complete.
    let progress = result.progress.as_ref().expect("missing progress");
    assert!((progress.fraction - 1.0).abs() < f64::EPSILON);
    assert_eq!(progress.total_steps, 2);

    // Metrics.
    let metrics = result.metrics.as_ref().expect("missing metrics");
    assert_eq!(metrics.latest_values["kreuzberg/total_files"] as usize, 2);
    assert_eq!(
        metrics.latest_values["kreuzberg/successful_files"] as usize,
        2
    );

    // Should have initial Running + 2 per-file progress callbacks.
    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].1["mode"], "batch");
    assert_eq!(entries[0].1["total_files"], 2);
}

#[tokio::test]
async fn batch_extraction_filtered_files() {
    let backend = KreuzbergBackend::new();
    let tmp_a = temp_file(".txt", "File A.");
    let tmp_b = temp_file(".txt", "File B.");
    let tmp_c = temp_file(".txt", "File C.");

    let spec = make_spec(serde_json::json!({
        "mode": "batch",
        "files": ["b"]
    }));
    let job = make_job(&spec);
    let staged = HashMap::from([
        ("a".into(), tmp_a.path().to_path_buf()),
        ("b".into(), tmp_b.path().to_path_buf()),
        ("c".into(), tmp_c.path().to_path_buf()),
    ]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["total_files"], serde_json::json!(1));
    assert_eq!(result.outputs["successful"], serde_json::json!(1));
}

#[tokio::test]
async fn batch_partial_failure() {
    let backend = KreuzbergBackend::new();
    let tmp_good = temp_file(".txt", "Good file.");

    let spec = make_spec(serde_json::json!({ "mode": "batch" }));
    let job = make_job(&spec);
    let staged = HashMap::from([
        ("good".into(), tmp_good.path().to_path_buf()),
        (
            "bad".into(),
            PathBuf::from("/tmp/nonexistent_kreuzberg_integ.txt"),
        ),
    ]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .unwrap();

    // Partial failure: 1 succeeded, 1 failed → still Success (not total failure).
    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "partial failure should be Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["successful"], serde_json::json!(1));
    assert_eq!(result.outputs["failed"], serde_json::json!(1));

    let errors = result.outputs["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
}

#[tokio::test]
async fn batch_total_failure_is_backend_error() {
    let backend = KreuzbergBackend::new();

    let spec = make_spec(serde_json::json!({ "mode": "batch" }));
    let job = make_job(&spec);
    let staged = HashMap::from([
        (
            "a".into(),
            PathBuf::from("/tmp/nonexistent_kreuzberg_a.txt"),
        ),
        (
            "b".into(),
            PathBuf::from("/tmp/nonexistent_kreuzberg_b.txt"),
        ),
    ]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .unwrap();

    // All files failed → BackendError.
    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "total failure should be BackendError, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["successful"], serde_json::json!(0));
    assert_eq!(result.outputs["failed"], serde_json::json!(2));
}

// ---------------------------------------------------------------------------
// Error propagation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_nonexistent_file_returns_backend_error() {
    let backend = KreuzbergBackend::new();

    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    let staged = HashMap::from([(
        "file".into(),
        PathBuf::from("/tmp/nonexistent_kreuzberg_test_file.xyz"),
    )]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .unwrap();

    // Failures are outcomes, not infra errors.
    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "expected BackendError, got {:?}",
        result.outcome
    );

    // Should have error logs.
    let logs = result.logs.as_ref().expect("missing logs");
    assert!(!logs.recent_errors.is_empty());
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancellation_returns_cancelled_outcome() {
    let backend = KreuzbergBackend::new();
    let tmp = temp_file(".txt", "content");

    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    let staged = HashMap::from([("file".into(), tmp.path().to_path_buf())]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();

    // Pre-cancel the token.
    let cancel = CancellationToken::new();
    cancel.cancel();

    let result = backend.execute(&ctx, noop_callback(), cancel).await.unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Cancelled));
}

// ---------------------------------------------------------------------------
// Config validation (prepare errors)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prepare_rejects_invalid_config() {
    let backend = KreuzbergBackend::new();
    let spec = make_spec(serde_json::json!({ "mode": 42 }));
    let job = make_job(&spec);
    let mut ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    ctx.staged_inputs
        .insert("file".into(), PathBuf::from("/tmp/test.txt"));

    let err = backend.prepare(&job, ctx).await.unwrap_err();
    assert!(
        err.to_string().contains("config"),
        "error should mention config: {}",
        err
    );
}

#[tokio::test]
async fn prepare_rejects_ambiguous_inputs() {
    let backend = KreuzbergBackend::new();
    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    let mut ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    ctx.staged_inputs
        .insert("a".into(), PathBuf::from("/tmp/a.txt"));
    ctx.staged_inputs
        .insert("b".into(), PathBuf::from("/tmp/b.txt"));

    let err = backend.prepare(&job, ctx).await.unwrap_err();
    assert!(
        err.to_string().contains("staged inputs"),
        "error should mention staged inputs: {}",
        err
    );
}

#[tokio::test]
async fn prepare_rejects_missing_named_input() {
    let backend = KreuzbergBackend::new();
    let spec = make_spec(serde_json::json!({ "file": "missing" }));
    let job = make_job(&spec);
    let mut ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    ctx.staged_inputs
        .insert("other".into(), PathBuf::from("/tmp/other.txt"));

    let err = backend.prepare(&job, ctx).await.unwrap_err();
    assert!(
        err.to_string().contains("missing"),
        "error should mention missing input: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// Output writing test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn writes_expected_output_files() {
    let backend = KreuzbergBackend::new();
    let tmp = temp_file(".txt", "Output writing test.");
    let out_dir = tempfile::tempdir().unwrap();

    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);

    let mut ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    ctx.staged_inputs
        .insert("file".into(), tmp.path().to_path_buf());
    let output_path = out_dir.path().join("content.json");
    ctx.expected_outputs
        .insert("content".into(), output_path.clone());

    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));

    // The expected output file should have been written.
    assert!(output_path.exists(), "expected output file should exist");
    let written = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        written.contains("Output writing test"),
        "written output should contain extraction content"
    );
}

// ---------------------------------------------------------------------------
// Logs structure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn success_populates_info_log() {
    let backend = KreuzbergBackend::new();
    let tmp = temp_file(".txt", "Log test.");

    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    let staged = HashMap::from([("file".into(), tmp.path().to_path_buf())]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    let logs = result.logs.as_ref().expect("missing logs");
    assert_eq!(logs.total_entries, 1);
    assert_eq!(logs.count_by_level["info"], 1);
    assert!(logs.recent_errors.is_empty());
}

#[tokio::test]
async fn failure_populates_error_log() {
    let backend = KreuzbergBackend::new();

    let spec = make_spec(serde_json::json!({}));
    let job = make_job(&spec);
    let staged = HashMap::from([(
        "file".into(),
        PathBuf::from("/tmp/nonexistent_kreuzberg_log_test.xyz"),
    )]);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, staged);

    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(
        result.outcome,
        ExecutionOutcome::BackendError { .. }
    ));
    let logs = result.logs.as_ref().expect("missing logs");
    assert_eq!(logs.total_entries, 1);
    assert_eq!(logs.count_by_level["error"], 1);
    assert_eq!(logs.recent_errors.len(), 1);
}
