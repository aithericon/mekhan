//! Kreuzberg backend conformance suite — drives the shared
//! `KreuzbergTestKit` tests against the real [`KreuzbergBackend`].
//!
//! The harness owns the test bodies (single-file extract, batch extract,
//! missing-input failure, status callback). This file only implements the
//! kit trait so future harness improvements (new test functions, new
//! contracts) automatically pick up coverage here.
//!
//! Run with: `cargo test -p aithericon-executor-kreuzberg --test conformance`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_domain::{ExecutionSpec, RunContext, RunDirectory};
use aithericon_executor_kreuzberg::KreuzbergBackend;
use aithericon_executor_test_harness::conformance::kreuzberg_kit::KreuzbergTestKit;
use aithericon_executor_test_harness::kreuzberg_conformance_tests;

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct KreuzbergConformanceKit;

impl KreuzbergConformanceKit {
    pub fn new() -> Self {
        Self
    }
}

fn temp_text_file(content: &str) -> tempfile::NamedTempFile {
    let f = tempfile::Builder::new()
        .suffix(".txt")
        .tempfile()
        .expect("tempfile");
    std::fs::write(f.path(), content).expect("write tempfile");
    f
}

fn make_empty_run_context(spec: ExecutionSpec, timeout: Duration) -> RunContext {
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!("kreuzberg-conform-{}-{}", std::process::id(), seq);
    RunContext {
        execution_id: id.clone(),
        spec,
        run_dir: RunDirectory::new(&std::env::temp_dir(), &id),
        timeout,
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    }
}

#[async_trait]
impl KreuzbergTestKit for KreuzbergConformanceKit {
    fn backend_name(&self) -> &'static str {
        "kreuzberg"
    }

    async fn create_backend(&self) -> Result<Arc<dyn ExecutionBackend>, String> {
        Ok(Arc::new(KreuzbergBackend::new()))
    }

    fn single_extract_spec(&self) -> ExecutionSpec {
        ExecutionSpec {
            backend: "kreuzberg".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({}),
            config_ref: None,
        }
    }

    fn batch_extract_spec(&self) -> ExecutionSpec {
        ExecutionSpec {
            backend: "kreuzberg".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({ "mode": "batch" }),
            config_ref: None,
        }
    }

    fn missing_input_spec(&self) -> ExecutionSpec {
        // References an explicit input that won't be staged — backend must
        // reject at prepare() (KreuzbergConfig validates staged_inputs has
        // the named file).
        ExecutionSpec {
            backend: "kreuzberg".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({ "file": "definitely_not_staged" }),
            config_ref: None,
        }
    }

    async fn stage_single_text_file(
        &self,
        spec: ExecutionSpec,
        timeout: Duration,
        content: &str,
    ) -> (RunContext, tempfile::NamedTempFile) {
        let tmp = temp_text_file(content);
        let mut ctx = make_empty_run_context(spec, timeout);
        ctx.staged_inputs
            .insert("file".into(), tmp.path().to_path_buf());
        (ctx, tmp)
    }

    async fn stage_batch_text_files(
        &self,
        spec: ExecutionSpec,
        timeout: Duration,
        contents: &[&str],
    ) -> (RunContext, Vec<tempfile::NamedTempFile>) {
        let mut tmps = Vec::with_capacity(contents.len());
        let mut staged: HashMap<String, PathBuf> = HashMap::new();
        for (i, body) in contents.iter().enumerate() {
            let t = temp_text_file(body);
            staged.insert(format!("file_{i}"), t.path().to_path_buf());
            tmps.push(t);
        }
        let mut ctx = make_empty_run_context(spec, timeout);
        ctx.staged_inputs = staged;
        (ctx, tmps)
    }

    async fn make_empty_run_context(
        &self,
        spec: ExecutionSpec,
        timeout: Duration,
    ) -> RunContext {
        make_empty_run_context(spec, timeout)
    }
}

kreuzberg_conformance_tests!(kreuzberg, KreuzbergConformanceKit::new());
