//! Integration tests for the file-ops backend.
//!
//! Tests exercise the full `ExecutionBackend` trait contract:
//! `prepare()` → `execute()` lifecycle, status callbacks, cancellation,
//! error propagation, config validation, and multi-operation workflows.
//!
//! All tests use a local filesystem temp directory — operators are built
//! on-the-fly from `StorageConfig` by `dispatch()`, so we need real
//! filesystem paths for data visibility.
//!
//! Run with:
//!   cargo test -p aithericon-executor-file-ops

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aithericon_executor_backend::traits::{EventStream, ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionJob, ExecutionOutcome, ExecutionSpec, ExecutionStatus, JobPriority, LogLevel,
    RunContext, RunDirectory,
};
use aithericon_executor_file_ops::FileOpsBackend;
use async_trait::async_trait;
use opendal::Operator;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

static ENV_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A local test environment: tempdir + operator for seeding + storage JSON for configs.
struct TestEnv {
    root: std::path::PathBuf,
    operator: Operator,
    storage_json: Value,
}

impl TestEnv {
    fn new() -> Self {
        let seq = ENV_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("fileops-integ-{}-{}", std::process::id(), seq,));
        std::fs::create_dir_all(&root).unwrap();

        let storage_json = serde_json::json!({
            "backend": "local",
            "endpoint": root.to_str().unwrap()
        });

        let config: aithericon_executor_storage::StorageConfig =
            serde_json::from_value(storage_json.clone()).unwrap();
        let operator = aithericon_executor_storage::build_operator(&config).unwrap();

        Self {
            root,
            operator,
            storage_json,
        }
    }

    fn with_prefix(prefix: &str) -> Self {
        let seq = ENV_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("fileops-integ-pfx-{}-{}", std::process::id(), seq,));
        std::fs::create_dir_all(&root).unwrap();

        let storage_json = serde_json::json!({
            "backend": "local",
            "endpoint": root.to_str().unwrap(),
            "prefix": prefix
        });

        let config: aithericon_executor_storage::StorageConfig =
            serde_json::from_value(storage_json.clone()).unwrap();
        let operator = aithericon_executor_storage::build_operator(&config).unwrap();

        Self {
            root,
            operator,
            storage_json,
        }
    }

    fn storage(&self) -> Value {
        self.storage_json.clone()
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn make_spec(config: Value) -> ExecutionSpec {
    ExecutionSpec {
        backend: "file_ops".into(),
        inputs: vec![],
        outputs: vec![],
        config,
        config_ref: None,
    }
}

fn make_job(spec: &ExecutionSpec) -> ExecutionJob {
    ExecutionJob {
        execution_id: "test-file-ops".into(),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        feed_chunks: false,
        channels: Vec::new(),
        wrapped_secrets: None,
    }
}

fn make_run_context(spec: ExecutionSpec, timeout: Duration) -> RunContext {
    RunContext {
        execution_id: "test-file-ops".into(),
        spec,
        run_dir: RunDirectory::new(&std::env::temp_dir(), "test-file-ops"),
        timeout,
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: Value::Null,
    }
}

/// Create a `RunContext` with a unique execution ID (needed for tests that
/// touch the filesystem, e.g. probe, to avoid collisions under parallel runs).
fn make_run_context_with_id(spec: ExecutionSpec, timeout: Duration, id: &str) -> RunContext {
    RunContext {
        execution_id: id.into(),
        spec,
        run_dir: RunDirectory::new(&std::env::temp_dir(), id),
        timeout,
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: Value::Null,
    }
}

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

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Lifecycle tests — full prepare → execute for each operation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backend_stat_success() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator.write("data/test.csv", "hello").await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "stat",
        "path": "data/test.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["exists"], serde_json::json!(true));
    assert_eq!(result.outputs["content_length"], serde_json::json!(5));
    assert_eq!(result.outputs["path"], serde_json::json!("data/test.csv"));
    assert!(result.duration.as_nanos() > 0);
}

#[tokio::test]
async fn backend_stat_missing_returns_success() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    let spec = make_spec(serde_json::json!({
        "operation": "stat",
        "path": "nonexistent.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    // Stat of a missing file is not an error — it returns exists: false
    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["exists"], serde_json::json!(false));
}

#[tokio::test]
async fn backend_delete_success() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator
        .write("temp/scratch.csv", "data")
        .await
        .unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "delete",
        "path": "temp/scratch.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["deleted"], serde_json::json!(true));
    assert!(!env.operator.exists("temp/scratch.csv").await.unwrap());
}

#[tokio::test]
async fn backend_delete_ignore_missing() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    let spec = make_spec(serde_json::json!({
        "operation": "delete",
        "path": "nonexistent.csv",
        "ignore_missing": true,
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    // With ignore_missing: true, deleting a non-existent file succeeds
    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["deleted"], serde_json::json!(true));
}

#[tokio::test]
async fn backend_copy_success() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator.write("src/file.csv", "content").await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "copy",
        "source": "src/file.csv",
        "destination": "dst/file.csv",
        "source_storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["copied"], serde_json::json!(true));
    assert_eq!(result.outputs["cross_backend"], serde_json::json!(false));
    assert!(env.operator.exists("src/file.csv").await.unwrap());
    assert!(env.operator.exists("dst/file.csv").await.unwrap());
    let content = env.operator.read("dst/file.csv").await.unwrap();
    assert_eq!(&content.to_vec(), b"content");
}

#[tokio::test]
async fn backend_copy_cross_backend() {
    let src_env = TestEnv::new();
    let dst_env = TestEnv::new();
    let backend = FileOpsBackend::new();
    src_env
        .operator
        .write("data/file.csv", "cross-data")
        .await
        .unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "copy",
        "source": "data/file.csv",
        "destination": "imported/file.csv",
        "source_storage": src_env.storage(),
        "destination_storage": dst_env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["copied"], serde_json::json!(true));
    assert_eq!(result.outputs["cross_backend"], serde_json::json!(true));
    assert!(src_env.operator.exists("data/file.csv").await.unwrap());
    assert!(dst_env.operator.exists("imported/file.csv").await.unwrap());
    let content = dst_env.operator.read("imported/file.csv").await.unwrap();
    assert_eq!(&content.to_vec(), b"cross-data");
}

#[tokio::test]
async fn backend_move_success() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator.write("old/file.csv", "data").await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "move",
        "source": "old/file.csv",
        "destination": "new/file.csv",
        "source_storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["moved"], serde_json::json!(true));
    assert!(!env.operator.exists("old/file.csv").await.unwrap());
    assert!(env.operator.exists("new/file.csv").await.unwrap());
}

#[tokio::test]
async fn backend_move_cross_backend() {
    let src_env = TestEnv::new();
    let dst_env = TestEnv::new();
    let backend = FileOpsBackend::new();
    src_env
        .operator
        .write("data/file.csv", "cross-move")
        .await
        .unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "move",
        "source": "data/file.csv",
        "destination": "archive/file.csv",
        "source_storage": src_env.storage(),
        "destination_storage": dst_env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["moved"], serde_json::json!(true));
    assert_eq!(result.outputs["cross_backend"], serde_json::json!(true));
    assert!(!src_env.operator.exists("data/file.csv").await.unwrap());
    assert!(dst_env.operator.exists("archive/file.csv").await.unwrap());
    let content = dst_env.operator.read("archive/file.csv").await.unwrap();
    assert_eq!(&content.to_vec(), b"cross-move");
}

#[tokio::test]
async fn backend_list_success() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator.write("datasets/a.csv", "aa").await.unwrap();
    env.operator.write("datasets/b.csv", "bb").await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "list",
        "prefix": "datasets/",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["count"], serde_json::json!(2));
    let files = result.outputs["files"].as_array().unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
    assert!(paths.contains(&"datasets/a.csv"));
    assert!(paths.contains(&"datasets/b.csv"));
}

#[tokio::test]
async fn backend_annotate_success() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator
        .write("data/file.parquet", "parquet-data")
        .await
        .unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "annotate",
        "path": "data/file.parquet",
        "annotations": {"owner": "ml-team", "version": 2},
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(
        result.outputs["sidecar_path"],
        serde_json::json!("data/file.parquet.meta.json")
    );

    // Verify sidecar was written
    let sidecar = env
        .operator
        .read("data/file.parquet.meta.json")
        .await
        .unwrap();
    let parsed: Value = serde_json::from_slice(&sidecar.to_vec()).unwrap();
    assert_eq!(parsed["owner"], serde_json::json!("ml-team"));
    assert_eq!(parsed["version"], serde_json::json!(2));
}

#[tokio::test]
async fn backend_probe_csv() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    let csv = "name,age\nAlice,30\nBob,25\n";
    env.operator.write("data/people.csv", csv).await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "probe",
        "path": "data/people.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context_with_id(spec, DEFAULT_TIMEOUT, "test-probe-integ");

    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["path"], serde_json::json!("data/people.csv"));
    assert!(result.outputs.contains_key("metadata"));
    assert!(result.outputs.contains_key("format"));

    // Cleanup temp run directory
    let _ = tokio::fs::remove_dir_all(&ctx.run_dir.root).await;
}

// ---------------------------------------------------------------------------
// Error propagation tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backend_error_delete_not_found() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    let spec = make_spec(serde_json::json!({
        "operation": "delete",
        "path": "nonexistent.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "expected BackendError, got {:?}",
        result.outcome
    );
    if let ExecutionOutcome::BackendError { message } = &result.outcome {
        assert!(
            message.contains("not found"),
            "error should mention not found: {message}"
        );
    }
}

#[tokio::test]
async fn backend_error_copy_not_found() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    let spec = make_spec(serde_json::json!({
        "operation": "copy",
        "source": "nonexistent.csv",
        "destination": "dst.csv",
        "source_storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "expected BackendError, got {:?}",
        result.outcome
    );
}

#[tokio::test]
async fn backend_error_probe_not_found() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    let spec = make_spec(serde_json::json!({
        "operation": "probe",
        "path": "nonexistent.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context_with_id(spec, DEFAULT_TIMEOUT, "test-probe-not-found");

    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "expected BackendError, got {:?}",
        result.outcome
    );

    let _ = tokio::fs::remove_dir_all(&ctx.run_dir.root).await;
}

// ---------------------------------------------------------------------------
// Config validation tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prepare_rejects_invalid_config() {
    let backend = FileOpsBackend::new();

    let spec = make_spec(serde_json::json!({"bad": "config"}));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);

    let result = backend.prepare(&job, ctx).await;
    assert!(result.is_err(), "prepare should reject invalid config");

    let err = result.unwrap_err().to_string();
    assert!(err.contains("config"), "error should mention config: {err}");
}

#[tokio::test]
async fn prepare_rejects_unknown_operation() {
    let backend = FileOpsBackend::new();

    let spec = make_spec(serde_json::json!({
        "operation": "unknown_op",
        "path": "test.csv"
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);

    let result = backend.prepare(&job, ctx).await;
    assert!(result.is_err(), "prepare should reject unknown operation");
}

#[tokio::test]
async fn prepare_rejects_empty_path() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    let spec = make_spec(serde_json::json!({
        "operation": "stat",
        "path": "",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);

    let result = backend.prepare(&job, ctx).await;
    assert!(result.is_err(), "prepare should reject empty path");

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("empty") || err.contains("validation"),
        "error should mention validation: {err}"
    );
}

#[tokio::test]
async fn prepare_populates_backend_state() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    let spec = make_spec(serde_json::json!({
        "operation": "stat",
        "path": "test.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);

    let ctx = backend.prepare(&job, ctx).await.unwrap();
    assert!(
        !ctx.backend_state.is_null(),
        "backend_state should be populated after prepare"
    );
    assert_eq!(ctx.backend_state["operation"], "stat");
    assert_eq!(ctx.backend_state["path"], "test.csv");
}

// ---------------------------------------------------------------------------
// Cancellation test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backend_cancellation() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator.write("data/file.csv", "hello").await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "stat",
        "path": "data/file.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    // Pre-cancel the token before execute — biased select checks cancel first
    let cancel = CancellationToken::new();
    cancel.cancel();

    let result = backend
        .execute(&ctx, noop_callback(), None, cancel)
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Cancelled),
        "expected Cancelled, got {:?}",
        result.outcome
    );
}

// ---------------------------------------------------------------------------
// Status callback test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backend_reports_running_status() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator.write("data/file.csv", "hello").await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "stat",
        "path": "data/file.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let (cb, status_log) = tracking_callback();
    let result = backend
        .execute(&ctx, cb, None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));

    // Verify the callback was invoked with Running status and operation info
    let entries = status_log.lock().unwrap();
    assert_eq!(entries.len(), 1, "expected exactly one status callback");
    assert_eq!(entries[0].0, ExecutionStatus::Running);
    assert_eq!(entries[0].1["operation"], "stat");
}

// ---------------------------------------------------------------------------
// Multi-operation workflow test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn workflow_write_annotate_probe_copy_list() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    // Step 1: Seed a CSV file directly in storage
    let csv = "city,population\nParis,2161000\nLondon,8982000\n";
    env.operator
        .write("datasets/cities.csv", csv)
        .await
        .unwrap();

    // Step 2: Annotate the file
    let annotate_spec = make_spec(serde_json::json!({
        "operation": "annotate",
        "path": "datasets/cities.csv",
        "annotations": {"source": "census", "year": 2024},
        "storage": env.storage()
    }));
    let job = make_job(&annotate_spec);
    let ctx = make_run_context(annotate_spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(result.outcome, ExecutionOutcome::Success));

    // Step 3: Probe the file
    let probe_spec = make_spec(serde_json::json!({
        "operation": "probe",
        "path": "datasets/cities.csv",
        "storage": env.storage()
    }));
    let job = make_job(&probe_spec);
    let ctx = make_run_context_with_id(probe_spec, DEFAULT_TIMEOUT, "test-workflow-probe");
    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "probe failed: {:?}",
        result.outcome
    );
    assert!(result.outputs.contains_key("format"));
    assert!(result.outputs.contains_key("metadata"));
    let _ = tokio::fs::remove_dir_all(&ctx.run_dir.root).await;

    // Step 4: Copy to a new location
    let copy_spec = make_spec(serde_json::json!({
        "operation": "copy",
        "source": "datasets/cities.csv",
        "destination": "archive/cities_v1.csv",
        "source_storage": env.storage()
    }));
    let job = make_job(&copy_spec);
    let ctx = make_run_context(copy_spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(result.outcome, ExecutionOutcome::Success));

    // Step 5: List to verify all files
    let list_spec = make_spec(serde_json::json!({
        "operation": "list",
        "prefix": "datasets/",
        "storage": env.storage()
    }));
    let job = make_job(&list_spec);
    let ctx = make_run_context(list_spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(result.outcome, ExecutionOutcome::Success));

    // Should see: cities.csv + cities.csv.meta.json
    let count = result.outputs["count"].as_u64().unwrap();
    assert_eq!(count, 2, "expected 2 files (data + sidecar), got {count}");

    let files = result.outputs["files"].as_array().unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.as_str().unwrap()).collect();
    assert!(paths.contains(&"datasets/cities.csv"));
    assert!(paths.contains(&"datasets/cities.csv.meta.json"));

    // Also verify the archive copy exists
    assert!(env.operator.exists("archive/cities_v1.csv").await.unwrap());
}

// ---------------------------------------------------------------------------
// Prefix handling test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backend_with_prefix() {
    let env = TestEnv::with_prefix("tenant-a/");
    let backend = FileOpsBackend::new();

    // Write with the prefixed path (as the storage sees it)
    env.operator
        .write("tenant-a/data/test.csv", "hello")
        .await
        .unwrap();

    // Stat via backend with unprefixed path (dispatch prepends prefix)
    let spec = make_spec(serde_json::json!({
        "operation": "stat",
        "path": "data/test.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["exists"], serde_json::json!(true));
    assert_eq!(result.outputs["content_length"], serde_json::json!(5));
}

// ---------------------------------------------------------------------------
// Trait tests
// ---------------------------------------------------------------------------

#[test]
fn supports_file_ops_only() {
    let backend = FileOpsBackend::new();

    let file_ops_spec = ExecutionSpec {
        backend: "file_ops".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({}),
        config_ref: None,
    };
    assert!(backend.supports(&file_ops_spec));

    let process_spec = ExecutionSpec {
        backend: "process".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({}),
        config_ref: None,
    };
    assert!(!backend.supports(&process_spec));

    let docker_spec = ExecutionSpec {
        backend: "docker".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({}),
        config_ref: None,
    };
    assert!(!backend.supports(&docker_spec));

    let llm_spec = ExecutionSpec {
        backend: "llm".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({}),
        config_ref: None,
    };
    assert!(!backend.supports(&llm_spec));
}

// ---------------------------------------------------------------------------
// Streaming / compression tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn backend_copy_with_gzip_compression() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    let plain = "name,age\nAlice,30\nBob,25\nCharlie,35\nDiana,28\n";
    env.operator.write("data/people.csv", plain).await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "copy",
        "source": "data/people.csv",
        "destination": "archive/people.csv.gz",
        "source_storage": env.storage(),
        "compress": "gzip"
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["copied"], serde_json::json!(true));
    assert!(result.outputs.contains_key("bytes_transferred"));

    // Source still exists, destination is compressed
    assert!(env.operator.exists("data/people.csv").await.unwrap());
    assert!(env.operator.exists("archive/people.csv.gz").await.unwrap());

    // Compressed output should be different from plain
    let compressed = env.operator.read("archive/people.csv.gz").await.unwrap();
    assert_ne!(&compressed.to_vec(), plain.as_bytes());
    // Gzip files start with magic bytes 0x1f 0x8b
    assert_eq!(compressed.to_vec()[0], 0x1f);
    assert_eq!(compressed.to_vec()[1], 0x8b);
}

#[tokio::test]
async fn backend_copy_decompress_gzip() {
    use std::io::Write;

    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    // Create a gzip-compressed file
    let plain = b"name,age\nAlice,30\nBob,25\n";
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(plain).unwrap();
    let compressed = encoder.finish().unwrap();
    env.operator
        .write("archive/people.csv.gz", compressed)
        .await
        .unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "copy",
        "source": "archive/people.csv.gz",
        "destination": "data/people.csv",
        "source_storage": env.storage(),
        "decompress": "gzip"
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );

    // Decompressed output should match original plain text
    let output = env.operator.read("data/people.csv").await.unwrap();
    assert_eq!(&output.to_vec(), plain);
}

#[tokio::test]
async fn backend_copy_transcode_gzip_to_zstd() {
    use std::io::Write;

    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    // Create gzip source
    let plain = b"city,population\nParis,2161000\nLondon,8982000\n";
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(plain).unwrap();
    let gzipped = encoder.finish().unwrap();
    env.operator
        .write("archive/cities.csv.gz", gzipped)
        .await
        .unwrap();

    // Transcode: decompress gzip → compress zstd
    let spec = make_spec(serde_json::json!({
        "operation": "copy",
        "source": "archive/cities.csv.gz",
        "destination": "warehouse/cities.csv.zst",
        "source_storage": env.storage(),
        "decompress": "gzip",
        "compress": "zstd"
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );

    // Verify output is zstd-compressed (magic bytes: 0x28 0xb5 0x2f 0xfd)
    let zstd_data = env.operator.read("warehouse/cities.csv.zst").await.unwrap();
    let zstd_bytes = zstd_data.to_vec();
    assert_eq!(zstd_bytes[0], 0x28);
    assert_eq!(zstd_bytes[1], 0xb5);
    assert_eq!(zstd_bytes[2], 0x2f);
    assert_eq!(zstd_bytes[3], 0xfd);

    // Verify roundtrip: decompress zstd and compare with original
    let decompressed = zstd::decode_all(zstd_bytes.as_slice()).unwrap();
    assert_eq!(&decompressed, plain);
}

#[tokio::test]
async fn backend_move_with_compression() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    let plain = "id,value\n1,foo\n2,bar\n";
    env.operator.write("staging/data.csv", plain).await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "move",
        "source": "staging/data.csv",
        "destination": "archive/data.csv.gz",
        "source_storage": env.storage(),
        "compress": "gzip"
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["moved"], serde_json::json!(true));

    // Source deleted, destination exists and is gzip
    assert!(!env.operator.exists("staging/data.csv").await.unwrap());
    assert!(env.operator.exists("archive/data.csv.gz").await.unwrap());

    let compressed = env.operator.read("archive/data.csv.gz").await.unwrap();
    assert_eq!(compressed.to_vec()[0], 0x1f);
    assert_eq!(compressed.to_vec()[1], 0x8b);
}

#[tokio::test]
async fn backend_cross_backend_streaming_reports_bytes() {
    let src_env = TestEnv::new();
    let dst_env = TestEnv::new();
    let backend = FileOpsBackend::new();
    let data = "hello streaming world";
    src_env
        .operator
        .write("data/stream.txt", data)
        .await
        .unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "copy",
        "source": "data/stream.txt",
        "destination": "imported/stream.txt",
        "source_storage": src_env.storage(),
        "destination_storage": dst_env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["cross_backend"], serde_json::json!(true));

    let bytes = result.outputs["bytes_transferred"].as_u64().unwrap();
    assert_eq!(bytes, data.len() as u64);

    let content = dst_env.operator.read("imported/stream.txt").await.unwrap();
    assert_eq!(&content.to_vec(), data.as_bytes());
}

#[tokio::test]
async fn backend_copy_with_zstd_compression() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    let plain = "col_a,col_b\nfoo,1\nbar,2\nbaz,3\n";
    env.operator.write("data/table.csv", plain).await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "copy",
        "source": "data/table.csv",
        "destination": "archive/table.csv.zst",
        "source_storage": env.storage(),
        "compress": "zstd"
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );

    // Verify zstd magic bytes
    let compressed = env.operator.read("archive/table.csv.zst").await.unwrap();
    let bytes = compressed.to_vec();
    assert_eq!(bytes[0], 0x28);
    assert_eq!(bytes[1], 0xb5);

    // Verify roundtrip
    let decompressed = zstd::decode_all(bytes.as_slice()).unwrap();
    assert_eq!(&decompressed, plain.as_bytes());
}

// ---------------------------------------------------------------------------
// Input resolution tests — {{input:NAME}} pattern in configs
// ---------------------------------------------------------------------------

/// Helper: write a JSON value to a file and return (name, path) for staged_inputs.
fn stage_inline_input(
    dir: &std::path::Path,
    name: &str,
    value: &Value,
) -> (String, std::path::PathBuf) {
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    (name.to_string(), path)
}

/// Create a RunContext pre-populated with staged inputs.
fn make_run_context_with_inputs(
    spec: ExecutionSpec,
    timeout: Duration,
    id: &str,
    staged_inputs: HashMap<String, std::path::PathBuf>,
) -> RunContext {
    RunContext {
        execution_id: id.into(),
        spec,
        run_dir: RunDirectory::new(&std::env::temp_dir(), id),
        timeout,
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs,
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: Value::Null,
    }
}

#[tokio::test]
async fn backend_annotate_with_input_annotations() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator
        .write("data/file.parquet", "parquet-data")
        .await
        .unwrap();

    // Simulate an Inline input containing probe metadata
    let inputs_dir =
        std::env::temp_dir().join(format!("fileops-inputs-annotate-{}", std::process::id()));
    std::fs::create_dir_all(&inputs_dir).unwrap();

    let probe_metadata = serde_json::json!({
        "format": "Parquet",
        "num_rows": 50000,
        "checksum": "sha256:abc123",
        "column_names": ["id", "value", "timestamp"]
    });
    let (name, path) = stage_inline_input(&inputs_dir, "probe_result", &probe_metadata);
    let staged = HashMap::from([(name, path)]);

    // Config uses {{input:probe_result}} for the annotations field
    let spec = make_spec(serde_json::json!({
        "operation": "annotate",
        "path": "data/file.parquet",
        "annotations": "{{input:probe_result}}",
        "merge": false,
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, "test-annotate-input", staged);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );

    // Verify the sidecar contains the probe metadata as annotations
    let sidecar = env
        .operator
        .read("data/file.parquet.meta.json")
        .await
        .unwrap();
    let parsed: Value = serde_json::from_slice(&sidecar.to_vec()).unwrap();
    assert_eq!(parsed["format"], serde_json::json!("Parquet"));
    assert_eq!(parsed["num_rows"], serde_json::json!(50000));
    assert_eq!(parsed["checksum"], serde_json::json!("sha256:abc123"));
    assert_eq!(
        parsed["column_names"],
        serde_json::json!(["id", "value", "timestamp"])
    );

    // Verify the output also reflects the resolved annotations
    let output_annotations = &result.outputs["annotations"];
    assert_eq!(output_annotations["format"], serde_json::json!("Parquet"));
    assert_eq!(output_annotations["num_rows"], serde_json::json!(50000));

    // Cleanup
    let _ = std::fs::remove_dir_all(&inputs_dir);
}

#[tokio::test]
async fn backend_copy_with_input_path() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator
        .write("uploads/report.csv", "col1,col2\na,b\n")
        .await
        .unwrap();

    // Simulate an Inline input containing the source path string
    let inputs_dir =
        std::env::temp_dir().join(format!("fileops-inputs-copy-{}", std::process::id()));
    std::fs::create_dir_all(&inputs_dir).unwrap();

    let (name, path) = stage_inline_input(
        &inputs_dir,
        "source_path",
        &serde_json::json!("uploads/report.csv"),
    );
    let staged = HashMap::from([(name, path)]);

    // Config uses {{input:source_path}} for the source field
    let spec = make_spec(serde_json::json!({
        "operation": "copy",
        "source": "{{input:source_path}}",
        "destination": "archive/report.csv",
        "source_storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, "test-copy-input", staged);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["copied"], serde_json::json!(true));

    // Verify the file was copied from the resolved source path
    assert!(env.operator.exists("uploads/report.csv").await.unwrap());
    assert!(env.operator.exists("archive/report.csv").await.unwrap());
    let content = env.operator.read("archive/report.csv").await.unwrap();
    assert_eq!(&content.to_vec(), b"col1,col2\na,b\n");

    // Cleanup
    let _ = std::fs::remove_dir_all(&inputs_dir);
}

#[tokio::test]
async fn backend_input_resolution_with_string_interpolation() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator
        .write("data/2026/feb/report.csv", "data")
        .await
        .unwrap();

    // Simulate inputs for path components
    let inputs_dir =
        std::env::temp_dir().join(format!("fileops-inputs-interp-{}", std::process::id()));
    std::fs::create_dir_all(&inputs_dir).unwrap();

    let (name, path) = stage_inline_input(&inputs_dir, "subdir", &serde_json::json!("2026/feb"));
    let staged = HashMap::from([(name, path)]);

    // Config uses string interpolation: data/{{input:subdir}}/report.csv
    let spec = make_spec(serde_json::json!({
        "operation": "stat",
        "path": "data/{{input:subdir}}/report.csv",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, "test-stat-interp", staged);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["exists"], serde_json::json!(true));

    // Cleanup
    let _ = std::fs::remove_dir_all(&inputs_dir);
}

// ---------------------------------------------------------------------------
// Crawl op tests — recursive streaming walk with a capturing EventStream
// ---------------------------------------------------------------------------

/// A recorded `item` emission from the crawl op.
#[derive(Clone)]
struct ItemCall {
    channel: String,
    episode_uid: String,
    idx: u64,
    payload: Value,
}

/// A recorded `close` emission.
#[derive(Clone)]
struct CloseCall {
    channel: String,
    episode_uid: String,
    count: u64,
}

/// Capturing `EventStream` — records every `item`/`close` the crawl op emits so
/// the test can assert batch count, file count, and per-entry sizes.
#[derive(Default)]
struct CapturingEventStream {
    items: Mutex<Vec<ItemCall>>,
    closes: Mutex<Vec<CloseCall>>,
}

#[async_trait]
impl EventStream for CapturingEventStream {
    async fn log(&self, _level: LogLevel, _message: String, _fields: HashMap<String, String>) {}

    async fn item(&self, channel: String, episode_uid: String, idx: u64, payload: Value) {
        self.items.lock().unwrap().push(ItemCall {
            channel,
            episode_uid,
            idx,
            payload,
        });
    }

    async fn close(&self, channel: String, episode_uid: String, count: u64) {
        self.closes.lock().unwrap().push(CloseCall {
            channel,
            episode_uid,
            count,
        });
    }
}

#[tokio::test]
async fn backend_crawl_streams_batches() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    // Seed a nested tree: 5 files across nested subdirs, each non-empty so we
    // can prove per-entry stat populated `size`.
    env.operator.write("nas/a.txt", "aaaa").await.unwrap();
    env.operator.write("nas/b.txt", "bbbbbb").await.unwrap();
    env.operator
        .write("nas/sub/c.txt", "cc")
        .await
        .unwrap();
    env.operator
        .write("nas/sub/deep/d.txt", "dddddddd")
        .await
        .unwrap();
    env.operator
        .write("nas/sub/deep/e.txt", "ee")
        .await
        .unwrap();
    let expected_files = 5u64;

    // batch_size = 2 → expect ceil(5/2) = 3 item batches + 1 close.
    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "batch_size": 2,
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let stream = Arc::new(CapturingEventStream::default());
    let result = backend
        .execute(
            &ctx,
            noop_callback(),
            Some(stream.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["count"], serde_json::json!(expected_files));
    assert_eq!(result.outputs["batches"], serde_json::json!(3));
    assert_eq!(result.outputs["cancelled"], serde_json::json!(false));

    // -- close: exactly one, count == batch count (a `join: gather` consumer
    // sizes its barrier on items/batches emitted, NOT the file total; see
    // crawl::execute) --
    let closes = stream.closes.lock().unwrap().clone();
    assert_eq!(closes.len(), 1, "expected exactly one close call");
    assert_eq!(closes[0].channel, "crawl");
    assert_eq!(closes[0].count, 3);

    // -- items: 3 batches, all sharing the close's episode_uid --
    let items = stream.items.lock().unwrap().clone();
    assert_eq!(items.len(), 3, "expected 3 item batches for 5 files @ batch=2");
    let episode = &closes[0].episode_uid;
    for (i, item) in items.iter().enumerate() {
        assert_eq!(item.channel, "crawl");
        assert_eq!(&item.episode_uid, episode, "all items share the episode uid");
        assert_eq!(item.idx, i as u64, "item idx is 0-based and monotonic");
    }

    // -- emitted entries: count matches, sizes non-zero (proves per-entry stat),
    //    paths are user-facing (prefix stripped), every seeded file present --
    let mut total_entries = 0u64;
    let mut seen_paths: Vec<String> = Vec::new();
    for item in &items {
        let arr = item.payload["items"].as_array().expect("items array");
        for e in arr {
            total_entries += 1;
            let size = e["size"].as_u64().expect("size");
            assert!(size > 0, "per-entry stat must yield non-zero size: {e}");
            let path = e["path"].as_str().expect("path").to_string();
            // mtime present (fs backend reports last_modified)
            assert!(e.get("mtime").is_some(), "mtime key present");
            seen_paths.push(path);
        }
    }
    assert_eq!(total_entries, expected_files);
    // Storage prefix is empty here, so paths carry the walked `config.prefix`
    // ("nas/"); only the *storage* prefix is stripped (covered by with_prefix).
    for f in [
        "nas/a.txt",
        "nas/b.txt",
        "nas/sub/c.txt",
        "nas/sub/deep/d.txt",
        "nas/sub/deep/e.txt",
    ] {
        assert!(
            seen_paths.iter().any(|p| p == f),
            "expected crawled path {f}, got {seen_paths:?}"
        );
    }

    // last_path is one of the crawled files (resume cursor).
    let last = result.outputs["last_path"].as_str().unwrap();
    assert!(seen_paths.iter().any(|p| p == last));
}

#[tokio::test]
async fn backend_crawl_strips_storage_prefix() {
    // With a storage prefix, crawl returns user-facing paths (prefix stripped),
    // mirroring `list`'s behavior.
    let env = TestEnv::with_prefix("tenant-a/");
    let backend = FileOpsBackend::new();
    env.operator
        .write("tenant-a/nas/one.txt", "11")
        .await
        .unwrap();
    env.operator
        .write("tenant-a/nas/two.txt", "222")
        .await
        .unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "batch_size": 10,
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let stream = Arc::new(CapturingEventStream::default());
    let result = backend
        .execute(
            &ctx,
            noop_callback(),
            Some(stream.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["count"], serde_json::json!(2));

    let items = stream.items.lock().unwrap().clone();
    let mut paths: Vec<String> = Vec::new();
    for item in &items {
        for e in item.payload["items"].as_array().unwrap() {
            paths.push(e["path"].as_str().unwrap().to_string());
        }
    }
    assert!(paths.contains(&"nas/one.txt".to_string()), "got {paths:?}");
    assert!(paths.contains(&"nas/two.txt".to_string()), "got {paths:?}");
    // The storage prefix must NOT leak into emitted paths.
    assert!(
        paths.iter().all(|p| !p.starts_with("tenant-a/")),
        "storage prefix leaked: {paths:?}"
    );
}

#[tokio::test]
async fn backend_crawl_no_event_stream_still_counts() {
    // Without an EventStream the op still walks + reports count/last_path
    // (the direct-call / small-crawl path).
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    env.operator.write("nas/x.txt", "xx").await.unwrap();
    env.operator.write("nas/y/z.txt", "zzz").await.unwrap();

    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["count"], serde_json::json!(2));
    // default batch_size (5000) ⇒ a single trailing batch ⇒ batches == 1
    assert_eq!(result.outputs["batches"], serde_json::json!(1));
    // Natural EOF ⇒ exhausted (the cursor-loop exit condition).
    assert_eq!(result.outputs["exhausted"], serde_json::json!(true));
}

// ---------------------------------------------------------------------------
// Crawl chunking + sink mode (docs/32 batch-fold)
// ---------------------------------------------------------------------------

/// Capturing `BatchSink` — records every published `FoldBatch`; optionally
/// fails every publish to exercise the hard-error path.
#[derive(Default)]
struct CapturingBatchSink {
    batches: Mutex<Vec<aithericon_executor_domain::FoldBatch>>,
    fail: bool,
}

#[async_trait]
impl aithericon_executor_backend::BatchSink for CapturingBatchSink {
    async fn publish(&self, batch: &aithericon_executor_domain::FoldBatch) -> Result<(), String> {
        if self.fail {
            return Err("synthetic publish failure".into());
        }
        self.batches.lock().unwrap().push(batch.clone());
        Ok(())
    }
}

/// Seed 5 files under `nas/` and return the expected count.
async fn seed_five(env: &TestEnv) -> u64 {
    env.operator.write("nas/a.txt", "aaaa").await.unwrap();
    env.operator.write("nas/b.txt", "bbbbbb").await.unwrap();
    env.operator.write("nas/sub/c.txt", "cc").await.unwrap();
    env.operator
        .write("nas/sub/deep/d.txt", "dddddddd")
        .await
        .unwrap();
    env.operator.write("nas/sub/deep/e.txt", "ee").await.unwrap();
    5
}

#[tokio::test]
async fn backend_crawl_max_batches_chunks_with_cursor() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    seed_five(&env).await;

    // batch_size 2, max_batches 1 ⇒ stop after 2 files; NOT exhausted.
    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "batch_size": 2,
        "max_batches": 1,
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["count"], serde_json::json!(2));
    assert_eq!(result.outputs["batches"], serde_json::json!(1));
    assert_eq!(result.outputs["exhausted"], serde_json::json!(false));
    assert_eq!(result.outputs["cancelled"], serde_json::json!(false));
    let cursor = result.outputs["last_path"].as_str().expect("cursor");

    // Resume from the cursor with no cap ⇒ the remaining 3 files, exhausted.
    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "batch_size": 2,
        "resume_from": cursor,
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["count"], serde_json::json!(3));
    assert_eq!(result.outputs["exhausted"], serde_json::json!(true));
}

#[tokio::test]
async fn backend_crawl_vanished_cursor_is_terminal() {
    // A client-side resume whose cursor no longer exists must error (silent
    // restart could re-emit the same chunk forever in a campaign loop).
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    seed_five(&env).await;

    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "resume_from": "nas/deleted-since-last-chunk.txt",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "vanished cursor must error, got {:?}",
        result.outcome
    );
}

#[tokio::test]
async fn backend_crawl_empty_resume_from_means_from_start() {
    // Interpolated campaign configs deliver `""` on iteration 0 — must walk
    // everything, not `start_after("")`.
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();
    let expected = seed_five(&env).await;

    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "resume_from": "",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(result.outcome, ExecutionOutcome::Success));
    assert_eq!(result.outputs["count"], serde_json::json!(expected));
    assert_eq!(result.outputs["exhausted"], serde_json::json!(true));
}

#[tokio::test]
async fn backend_crawl_sink_mode_publishes_no_channel_items() {
    let env = TestEnv::new();
    let sink = Arc::new(CapturingBatchSink::default());
    let backend = FileOpsBackend::new().with_batch_sink(Some(sink.clone()));
    let expected = seed_five(&env).await;

    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "batch_size": 2,
        "sink": { "mode": "index", "file_server_id": "demo-nas" },
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();

    // Hand an EventStream too — sink mode must NOT emit on it.
    let stream = Arc::new(CapturingEventStream::default());
    let result = backend
        .execute(
            &ctx,
            noop_callback(),
            Some(stream.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.outputs["count"], serde_json::json!(expected));
    assert_eq!(result.outputs["batches"], serde_json::json!(3));
    assert_eq!(result.outputs["exhausted"], serde_json::json!(true));

    // No channel traffic at all in sink mode.
    assert!(stream.items.lock().unwrap().is_empty(), "no item() calls");
    assert!(stream.closes.lock().unwrap().is_empty(), "no close() calls");

    // Published batches: 3 (2+2+1 trailing partial), monotonic idx, shared
    // episode, envelope carries mode/server/root, items carry path+size.
    let batches = sink.batches.lock().unwrap().clone();
    assert_eq!(batches.len(), 3);
    let episode = &batches[0].episode_uid;
    let mut total = 0;
    for (i, b) in batches.iter().enumerate() {
        assert_eq!(b.batch_idx, i as u64);
        assert_eq!(&b.episode_uid, episode);
        assert_eq!(b.mode, aithericon_executor_domain::FoldMode::Index);
        assert_eq!(b.file_server_id, "demo-nas");
        assert!(!b.endpoint_root.is_empty(), "endpoint_root on envelope");
        for item in &b.items {
            assert!(item.size > 0, "per-entry stat populated size");
            assert!(item.path.starts_with("nas/"), "user-facing path");
        }
        total += b.items.len() as u64;
    }
    assert_eq!(total, expected);
    assert_eq!(batches[2].items.len(), 1, "trailing partial batch");
}

#[tokio::test]
async fn backend_crawl_sink_publish_failure_is_terminal() {
    let env = TestEnv::new();
    let sink = Arc::new(CapturingBatchSink {
        fail: true,
        ..Default::default()
    });
    let backend = FileOpsBackend::new().with_batch_sink(Some(sink));
    seed_five(&env).await;

    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "batch_size": 2,
        "sink": { "mode": "reconcile", "file_server_id": "demo-nas" },
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "publish failure must fail the job, got {:?}",
        result.outcome
    );
}

#[tokio::test]
async fn backend_crawl_sink_mode_without_injected_sink_errors() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new(); // no sink injected
    seed_five(&env).await;

    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "sink": { "mode": "index", "file_server_id": "demo-nas" },
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let ctx = backend.prepare(&job, ctx).await.unwrap();
    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(result.outcome, ExecutionOutcome::BackendError { .. }),
        "sink mode without a host sink must error, got {:?}",
        result.outcome
    );
}

#[tokio::test]
async fn backend_crawl_sink_mode_rejects_bad_config() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    // Bad mode fails prepare-time validation.
    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "sink": { "mode": "yolo", "file_server_id": "demo-nas" },
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let err = backend.prepare(&job, ctx).await.unwrap_err().to_string();
    assert!(err.contains("sink.mode"), "got: {err}");

    // Empty file_server_id fails too.
    let spec = make_spec(serde_json::json!({
        "operation": "crawl",
        "prefix": "nas/",
        "sink": { "mode": "index", "file_server_id": "  " },
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context(spec, DEFAULT_TIMEOUT);
    let err = backend.prepare(&job, ctx).await.unwrap_err().to_string();
    assert!(err.contains("file_server_id"), "got: {err}");
}

#[tokio::test]
async fn backend_prepare_fails_on_missing_input() {
    let env = TestEnv::new();
    let backend = FileOpsBackend::new();

    // Need at least one staged input so the empty-check fast path is
    // skipped, but reference a different name that doesn't exist.
    let inputs_dir =
        std::env::temp_dir().join(format!("fileops-inputs-miss-{}", std::process::id()));
    std::fs::create_dir_all(&inputs_dir).unwrap();
    let (name, path) =
        stage_inline_input(&inputs_dir, "other_input", &serde_json::json!("irrelevant"));
    let staged = HashMap::from([(name, path)]);

    let spec = make_spec(serde_json::json!({
        "operation": "stat",
        "path": "{{input:missing_input}}",
        "storage": env.storage()
    }));
    let job = make_job(&spec);
    let ctx = make_run_context_with_inputs(spec, DEFAULT_TIMEOUT, "test-missing-input", staged);

    let result = backend.prepare(&job, ctx).await;
    assert!(result.is_err(), "prepare should fail on missing input");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("missing_input"),
        "error should mention the missing input name: {err}"
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&inputs_dir);
}
