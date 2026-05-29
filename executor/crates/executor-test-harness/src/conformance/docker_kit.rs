use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_docker::{DockerBackend, DockerConfig, PullPolicy};
use aithericon_executor_domain::{ExecutionSpec, RunContext, RunDirectory};

use super::kit::BackendTestKit;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Default image for Docker conformance tests.
const DEFAULT_IMAGE: &str = "alpine:3.19";

pub struct DockerTestKit;

impl DockerTestKit {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BackendTestKit for DockerTestKit {
    fn backend_name(&self) -> &'static str {
        "docker"
    }

    async fn create_backend(&self) -> Result<Arc<dyn ExecutionBackend>, String> {
        DockerBackend::new()
            .map(|b| Arc::new(b) as Arc<dyn ExecutionBackend>)
            .map_err(|e| format!("Docker daemon unavailable: {e}"))
    }

    async fn skip_reason(&self) -> Option<String> {
        match bollard::Docker::connect_with_local_defaults() {
            Ok(client) => match client.ping().await {
                Ok(_) => None,
                Err(e) => Some(format!("Docker daemon not responding: {e}")),
            },
            Err(e) => Some(format!("Cannot connect to Docker: {e}")),
        }
    }

    fn echo_spec(&self) -> ExecutionSpec {
        docker_spec(vec!["echo".into(), "hello".into()])
    }

    fn failing_spec(&self) -> ExecutionSpec {
        docker_spec(vec!["false".into()])
    }

    fn sleep_spec(&self, secs: u64) -> ExecutionSpec {
        docker_spec(vec!["sleep".into(), secs.to_string()])
    }

    fn dual_output_spec(&self) -> ExecutionSpec {
        docker_spec(vec![
            "sh".into(),
            "-c".into(),
            "echo stdout_marker && echo stderr_marker >&2".into(),
        ])
    }

    fn env_echo_spec(&self) -> ExecutionSpec {
        docker_spec(vec![
            "sh".into(),
            "-c".into(),
            "echo $CONFORMANCE_TEST_VAR".into(),
        ])
    }

    fn large_output_spec(&self, bytes: usize) -> ExecutionSpec {
        docker_spec(vec![
            "sh".into(),
            "-c".into(),
            format!("head -c {bytes} /dev/zero | tr '\\0' 'x'"),
        ])
    }

    async fn make_run_context(
        &self,
        spec: ExecutionSpec,
        timeout: Duration,
        env: HashMap<String, String>,
    ) -> RunContext {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let eid = format!("docker-conform-{}-{}", std::process::id(), seq);
        let base = PathBuf::from(format!("/tmp/aithericon-docker-conform-{eid}"));
        let run_dir = RunDirectory::new(&base, &eid);

        // Docker requires host directories to exist before bind mounting
        for dir in run_dir.all_dirs() {
            tokio::fs::create_dir_all(dir).await.unwrap();
        }

        RunContext {
            env,
            ..RunContext::for_test(eid, spec, run_dir, timeout)
        }
    }

    async fn cleanup_run_context(&self, ctx: &RunContext) {
        if let Some(base) = ctx.run_dir.root.parent().and_then(|p| p.parent()) {
            let _ = tokio::fs::remove_dir_all(base).await;
        }
    }

    async fn pipeline_setup(&self) -> Result<(), String> {
        // Pre-pull the image so pipeline tests don't timeout during pull
        let client = bollard::Docker::connect_with_local_defaults()
            .map_err(|e| format!("Docker unavailable: {e}"))?;
        aithericon_executor_docker::container::ensure_image(
            &client,
            DEFAULT_IMAGE,
            PullPolicy::IfNotPresent,
        )
        .await
        .map_err(|e| format!("Image pull failed: {e}"))
    }
}

/// Build a DockerConfig spec with the default Alpine image.
fn docker_spec(command: Vec<String>) -> ExecutionSpec {
    DockerConfig {
        image: DEFAULT_IMAGE.into(),
        command,
        entrypoint: None,
        env: HashMap::new(),
        pull_policy: PullPolicy::IfNotPresent,
        resource_limits: None,
        network_mode: None,
        extra_volumes: vec![],
        remove_container: true,
    }
    .into_spec()
}
