use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_domain::{ExecutionJob, ExecutionSpec, JobPriority, RunContext, RunDirectory};

/// Factory trait for file-ops conformance testing.
///
/// File-ops backends differ from process-style backends: no stdout/stderr,
/// no exit codes, no env vars. This trait provides file-ops-specific spec
/// factories, storage seeding, and verification helpers.
#[async_trait]
pub trait FileOpsTestKit: Send + Sync {
    /// Human-readable name for test output.
    fn backend_name(&self) -> &'static str;

    /// Create the backend instance for testing.
    async fn create_backend(&self) -> Result<Arc<dyn ExecutionBackend>, String>;

    /// Returns `Some("reason")` if tests should be skipped.
    async fn skip_reason(&self) -> Option<String> {
        None
    }

    /// Pre-populate storage with known test files.
    ///
    /// Expected seed data:
    /// - `"data/hello.csv"` — `"name,age\nAlice,30\nBob,25\n"`
    /// - `"data/sample.parquet"` — `"fake-parquet-bytes"`
    async fn seed_storage(&self);

    // ─── Spec factories ──────────────────────────────────────────────

    /// Stat an existing file (`"data/hello.csv"`).
    fn stat_existing_spec(&self) -> ExecutionSpec;

    /// Stat a non-existent file.
    fn stat_missing_spec(&self) -> ExecutionSpec;

    /// Delete an existing file (`"data/hello.csv"`).
    fn delete_existing_spec(&self) -> ExecutionSpec;

    /// Delete a non-existent file without `ignore_missing`.
    fn delete_missing_spec(&self) -> ExecutionSpec;

    /// Copy `"data/hello.csv"` → `"copy/hello.csv"`.
    fn copy_existing_spec(&self) -> ExecutionSpec;

    /// Move `"data/sample.parquet"` → `"moved/sample.parquet"`.
    fn move_existing_spec(&self) -> ExecutionSpec;

    /// List files under `"data/"`.
    fn list_spec(&self) -> ExecutionSpec;

    /// Annotate `"data/hello.csv"` with `{"source": "test"}`.
    fn annotate_spec(&self) -> ExecutionSpec;

    /// A spec with invalid/garbage config (for prepare rejection).
    fn invalid_config_spec(&self) -> ExecutionSpec;

    // ─── RunContext lifecycle ────────────────────────────────────────

    /// Build a RunContext for a backend-level test.
    async fn make_run_context(&self, spec: ExecutionSpec, timeout: Duration) -> RunContext;

    /// Cleanup after a test.
    async fn cleanup_run_context(&self, ctx: &RunContext);

    // ─── Verification helpers ────────────────────────────────────────

    /// Check whether a file exists in storage.
    async fn verify_file_exists(&self, path: &str) -> bool;

    /// Read file content from storage, or None if missing.
    async fn verify_file_content(&self, path: &str) -> Option<Vec<u8>>;

    // ─── Job helper (default impl) ──────────────────────────────────

    /// Convert a spec into a full `ExecutionJob`.
    fn spec_to_job(&self, eid: &str, spec: ExecutionSpec) -> ExecutionJob {
        ExecutionJob {
            execution_id: eid.to_string(),
            spec,
            metadata: HashMap::new(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        }
    }
}

// ---------------------------------------------------------------------------
// LocalFileOpsKit — test kit using local filesystem
// ---------------------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// File-ops test kit backed by a local filesystem temp directory.
///
/// Operations go through `dispatch()` → `build_op()` → `build_operator()`,
/// so we need a real filesystem for data visibility across operator instances.
pub struct LocalFileOpsKit {
    root: PathBuf,
    operator: opendal::Operator,
    storage_json: serde_json::Value,
}

impl Default for LocalFileOpsKit {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalFileOpsKit {
    pub fn new() -> Self {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "fileops-kit-{}-{}",
            std::process::id(),
            seq,
        ));
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

    fn storage_json(&self) -> serde_json::Value {
        self.storage_json.clone()
    }
}

impl Drop for LocalFileOpsKit {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn file_ops_spec(config: serde_json::Value) -> ExecutionSpec {
    ExecutionSpec {
        backend: "file_ops".into(),
        inputs: vec![],
        outputs: vec![],
        config,
        config_ref: None,
    }
}

#[async_trait]
impl FileOpsTestKit for LocalFileOpsKit {
    fn backend_name(&self) -> &'static str {
        "file_ops_local"
    }

    async fn create_backend(&self) -> Result<Arc<dyn ExecutionBackend>, String> {
        Ok(Arc::new(
            aithericon_executor_file_ops::FileOpsBackend::new(),
        ))
    }

    async fn seed_storage(&self) {
        self.operator
            .write("data/hello.csv", "name,age\nAlice,30\nBob,25\n")
            .await
            .unwrap();
        self.operator
            .write("data/sample.parquet", "fake-parquet-bytes")
            .await
            .unwrap();
    }

    fn stat_existing_spec(&self) -> ExecutionSpec {
        file_ops_spec(serde_json::json!({
            "operation": "stat",
            "path": "data/hello.csv",
            "storage": self.storage_json()
        }))
    }

    fn stat_missing_spec(&self) -> ExecutionSpec {
        file_ops_spec(serde_json::json!({
            "operation": "stat",
            "path": "nonexistent.csv",
            "storage": self.storage_json()
        }))
    }

    fn delete_existing_spec(&self) -> ExecutionSpec {
        file_ops_spec(serde_json::json!({
            "operation": "delete",
            "path": "data/hello.csv",
            "storage": self.storage_json()
        }))
    }

    fn delete_missing_spec(&self) -> ExecutionSpec {
        file_ops_spec(serde_json::json!({
            "operation": "delete",
            "path": "ghost.csv",
            "storage": self.storage_json()
        }))
    }

    fn copy_existing_spec(&self) -> ExecutionSpec {
        file_ops_spec(serde_json::json!({
            "operation": "copy",
            "source": "data/hello.csv",
            "destination": "copy/hello.csv",
            "source_storage": self.storage_json()
        }))
    }

    fn move_existing_spec(&self) -> ExecutionSpec {
        file_ops_spec(serde_json::json!({
            "operation": "move",
            "source": "data/sample.parquet",
            "destination": "moved/sample.parquet",
            "source_storage": self.storage_json()
        }))
    }

    fn list_spec(&self) -> ExecutionSpec {
        file_ops_spec(serde_json::json!({
            "operation": "list",
            "prefix": "data/",
            "storage": self.storage_json()
        }))
    }

    fn annotate_spec(&self) -> ExecutionSpec {
        file_ops_spec(serde_json::json!({
            "operation": "annotate",
            "path": "data/hello.csv",
            "annotations": {"source": "test"},
            "storage": self.storage_json()
        }))
    }

    fn invalid_config_spec(&self) -> ExecutionSpec {
        file_ops_spec(serde_json::json!({
            "bad": "config"
        }))
    }

    async fn make_run_context(&self, spec: ExecutionSpec, timeout: Duration) -> RunContext {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let eid = format!("fileops-conform-{}-{}", std::process::id(), seq);
        let base = PathBuf::from(format!("/tmp/aithericon-fileops-conform-{eid}"));
        let run_dir = RunDirectory::new(&base, &eid);

        // Create directories (needed for probe operations)
        for dir in run_dir.all_dirs() {
            tokio::fs::create_dir_all(dir).await.unwrap();
        }

        RunContext {
            execution_id: eid,
            spec,
            run_dir,
            timeout,
            env: HashMap::new(),
            metadata: HashMap::new(),
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: Vec::new(),
            backend_state: serde_json::Value::Null,
        }
    }

    async fn cleanup_run_context(&self, ctx: &RunContext) {
        if let Some(base) = ctx.run_dir.root.parent().and_then(|p| p.parent()) {
            let _ = tokio::fs::remove_dir_all(base).await;
        }
    }

    async fn verify_file_exists(&self, path: &str) -> bool {
        self.operator.exists(path).await.unwrap_or(false)
    }

    async fn verify_file_content(&self, path: &str) -> Option<Vec<u8>> {
        self.operator.read(path).await.ok().map(|b| b.to_vec())
    }
}
