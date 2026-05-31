use std::path::PathBuf;
use std::sync::OnceLock;

use aithericon_executor_domain::{
    ExecutionJob, InputDeclaration, InputSource, JobPriority, OutputDeclaration,
};
use aithericon_executor_process::ProcessConfig;

static CLIENT_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Build the IPC test client example binary (once) and return its path.
///
/// Panics if the build fails.
pub fn ipc_test_client_path() -> &'static PathBuf {
    CLIENT_PATH.get_or_init(|| {
        let output = std::process::Command::new("cargo")
            .args([
                "build",
                "--example",
                "ipc_test_client",
                "-p",
                "aithericon-executor-ipc",
                "--message-format=short",
            ])
            .output()
            .expect("failed to run cargo build");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("failed to build ipc_test_client:\n{stderr}");
        }

        // Find the binary in the target directory
        let metadata = std::process::Command::new("cargo")
            .args(["metadata", "--format-version=1", "--no-deps"])
            .output()
            .expect("cargo metadata failed");

        let meta: serde_json::Value =
            serde_json::from_slice(&metadata.stdout).expect("invalid cargo metadata");
        let target_dir = meta["target_directory"]
            .as_str()
            .expect("no target_directory in metadata");

        let binary = PathBuf::from(target_dir)
            .join("debug")
            .join("examples")
            .join("ipc_test_client");

        assert!(
            binary.exists(),
            "ipc_test_client binary not found at {}",
            binary.display()
        );

        binary
    })
}

/// Create an execution job that runs the IPC test client with the given plan JSON.
///
/// The plan is staged as an inline input at `ipc_plan.json`, and the job
/// runs `{client_path} $AITHERICON_INPUTS_DIR/ipc_plan.json`.
pub fn ipc_client_job(
    eid: &str,
    plan_json: &serde_json::Value,
    outputs: Vec<OutputDeclaration>,
) -> ExecutionJob {
    let client_path = ipc_test_client_path();

    let inputs = vec![InputDeclaration {
        name: "ipc_plan.json".into(),
        source: InputSource::Inline {
            value: plan_json.clone(),
        },
        required: true,
    }];

    let script = format!(
        "{} $AITHERICON_INPUTS_DIR/ipc_plan.json",
        client_path.display()
    );

    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "bash".into(),
            args: vec!["-c".into(), script],
            env: std::collections::HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec_with_io(inputs, outputs),
        metadata: std::collections::HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}
