use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use aithericon_executor_backend::{ExecutionBackend, PythonBackend, PythonConfig};
use aithericon_executor_domain::{ExecutionSpec, InputSource, RunContext, RunDirectory};

use super::kit::BackendTestKit;

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct PythonTestKit;

#[async_trait]
impl BackendTestKit for PythonTestKit {
    fn backend_name(&self) -> &'static str {
        "python"
    }

    async fn create_backend(&self) -> Result<Arc<dyn ExecutionBackend>, String> {
        Ok(Arc::new(PythonBackend::new()))
    }

    async fn skip_reason(&self) -> Option<String> {
        match tokio::process::Command::new("python3")
            .arg("--version")
            .output()
            .await
        {
            Ok(output) if output.status.success() => None,
            Ok(output) => Some(format!(
                "python3 exited with status {}",
                output.status
            )),
            Err(e) => Some(format!("python3 not found: {e}")),
        }
    }

    fn echo_spec(&self) -> ExecutionSpec {
        PythonConfig::inline_spec("print('hello')")
    }

    fn failing_spec(&self) -> ExecutionSpec {
        PythonConfig::inline_spec("import sys; sys.exit(1)")
    }

    fn sleep_spec(&self, secs: u64) -> ExecutionSpec {
        PythonConfig::inline_spec(format!("import time; time.sleep({secs})"))
    }

    fn dual_output_spec(&self) -> ExecutionSpec {
        PythonConfig::inline_spec(
            "import sys; print('stdout_marker'); print('stderr_marker', file=sys.stderr)",
        )
    }

    fn env_echo_spec(&self) -> ExecutionSpec {
        PythonConfig::inline_spec("import os; print(os.environ['CONFORMANCE_TEST_VAR'])")
    }

    fn large_output_spec(&self, bytes: usize) -> ExecutionSpec {
        PythonConfig::inline_spec(format!("print('x' * {bytes})"))
    }

    async fn make_run_context(
        &self,
        spec: ExecutionSpec,
        timeout: Duration,
        env: HashMap<String, String>,
    ) -> RunContext {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let eid = format!("python-conform-{}-{}", std::process::id(), seq);
        let base = PathBuf::from(format!("/tmp/aithericon-python-conform-{eid}"));
        let run_dir = RunDirectory::new(&base, &eid);

        // Create directories
        for dir in run_dir.all_dirs() {
            tokio::fs::create_dir_all(dir).await.unwrap();
        }

        // Stage Raw inputs manually (conformance tests skip the staging pipeline)
        for input in &spec.inputs {
            let dest = run_dir.inputs_dir.join(&input.name);
            if let InputSource::Raw { content } = &input.source {
                tokio::fs::write(&dest, content.as_bytes()).await.unwrap();
            }
        }

        // Find user script and generate runner
        let config = PythonConfig::from_spec(&spec).unwrap();
        let user_code_path = run_dir.inputs_dir.join(&config.script);

        let runner_path = run_dir.root.join("__runner__.py");
        aithericon_executor_backend::python::runner::write_runner(&runner_path, &user_code_path)
            .await
            .unwrap();

        let backend_state = serde_json::json!({
            "python_bin": config.python,
            "runner_path": runner_path.to_string_lossy(),
        });

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
            backend_state,
        }
    }

    async fn cleanup_run_context(&self, ctx: &RunContext) {
        if let Some(base) = ctx.run_dir.root.parent().and_then(|p| p.parent()) {
            let _ = tokio::fs::remove_dir_all(base).await;
        }
    }
}
