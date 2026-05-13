use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use aithericon_executor_backend::{ExecutionBackend, ProcessBackend, ProcessConfig};
use aithericon_executor_domain::{ExecutionSpec, RunContext, RunDirectory};

use super::kit::BackendTestKit;

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct ProcessTestKit;

#[async_trait]
impl BackendTestKit for ProcessTestKit {
    fn backend_name(&self) -> &'static str {
        "process"
    }

    async fn create_backend(&self) -> Result<Arc<dyn ExecutionBackend>, String> {
        Ok(Arc::new(ProcessBackend::new()))
    }

    fn echo_spec(&self) -> ExecutionSpec {
        ProcessConfig {
            command: "echo".into(),
            args: vec!["hello".into()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec()
    }

    fn failing_spec(&self) -> ExecutionSpec {
        ProcessConfig {
            command: "false".into(),
            args: vec![],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec()
    }

    fn sleep_spec(&self, secs: u64) -> ExecutionSpec {
        ProcessConfig {
            command: "sleep".into(),
            args: vec![secs.to_string()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec()
    }

    fn dual_output_spec(&self) -> ExecutionSpec {
        ProcessConfig {
            command: "bash".into(),
            args: vec![
                "-c".into(),
                "echo stdout_marker && echo stderr_marker >&2".into(),
            ],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec()
    }

    fn env_echo_spec(&self) -> ExecutionSpec {
        ProcessConfig {
            command: "bash".into(),
            args: vec!["-c".into(), "echo $CONFORMANCE_TEST_VAR".into()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec()
    }

    fn large_output_spec(&self, bytes: usize) -> ExecutionSpec {
        ProcessConfig {
            command: "bash".into(),
            args: vec![
                "-c".into(),
                format!("head -c {bytes} /dev/zero | tr '\\0' 'x'"),
            ],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec()
    }

    async fn make_run_context(
        &self,
        spec: ExecutionSpec,
        timeout: Duration,
        env: HashMap<String, String>,
    ) -> RunContext {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let eid = format!("proc-conform-{}-{}", std::process::id(), seq);
        let base = PathBuf::from(format!("/tmp/aithericon-conform-{eid}"));
        let run_dir = RunDirectory::new(&base, &eid);

        // Create directories
        for dir in run_dir.all_dirs() {
            tokio::fs::create_dir_all(dir).await.unwrap();
        }

        RunContext {
            execution_id: eid,
            spec,
            run_dir,
            timeout,
            env,
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
}
