use std::collections::HashMap;
use std::time::Duration;

use aithericon_executor_domain::{
    ExecutionJob, ExecutionStatus, InputDeclaration, JobPriority, OutputDeclaration, StatusUpdate,
};
use aithericon_executor_process::ProcessConfig;

/// Create an echo job that prints "hello" to stdout.
pub fn echo_job(eid: &str) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "echo".into(),
            args: vec!["hello".into()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec(),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create a job that always fails (exit code 1).
pub fn failing_job(eid: &str) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "false".into(),
            args: vec![],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec(),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create a sleep job with a timeout shorter than the sleep duration.
pub fn sleep_job(eid: &str, secs: u64, timeout_secs: u64) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "sleep".into(),
            args: vec![secs.to_string()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec(),
        metadata: HashMap::new(),
        timeout: Some(Duration::from_secs(timeout_secs)),
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create an echo job with custom metadata.
pub fn job_with_metadata(eid: &str, metadata: HashMap<String, String>) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "echo".into(),
            args: vec!["hello".into()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec(),
        metadata,
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create a bash script job with the given script.
pub fn bash_job(eid: &str, script: &str) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "bash".into(),
            args: vec!["-c".into(), script.into()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec(),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create a bash script job with inline input declarations.
pub fn job_with_inline_inputs(
    eid: &str,
    script: &str,
    inputs: Vec<InputDeclaration>,
) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "bash".into(),
            args: vec!["-c".into(), script.into()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec_with_io(inputs, vec![]),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create a bash script job with output declarations.
pub fn job_with_outputs(eid: &str, script: &str, outputs: Vec<OutputDeclaration>) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "bash".into(),
            args: vec!["-c".into(), script.into()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec_with_io(vec![], outputs),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create a bash script job with both input and output declarations.
pub fn job_with_io(
    eid: &str,
    script: &str,
    inputs: Vec<InputDeclaration>,
    outputs: Vec<OutputDeclaration>,
) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "bash".into(),
            args: vec!["-c".into(), script.into()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec_with_io(inputs, outputs),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create a job with a nonexistent command (triggers SpawnFailed).
pub fn nonexistent_command_job(eid: &str) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "nonexistent_cmd_xyz_12345".into(),
            args: vec![],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec(),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create a job that produces `byte_count` bytes of stdout (for TailBuffer testing).
pub fn large_output_job(eid: &str, byte_count: usize) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "bash".into(),
            args: vec![
                "-c".into(),
                format!("head -c {byte_count} /dev/zero | tr '\\0' 'x'"),
            ],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec(),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create a long-running sleep job with no timeout (for cancellation testing).
pub fn long_running_job(eid: &str, sleep_secs: u64) -> ExecutionJob {
    ExecutionJob {
        execution_id: eid.to_string(),
        spec: ProcessConfig {
            command: "sleep".into(),
            args: vec![sleep_secs.to_string()],
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec(),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    }
}

/// Create a batch manifest from a list of jobs.
pub fn batch_manifest(jobs: Vec<ExecutionJob>) -> aithericon_executor_domain::BatchManifest {
    aithericon_executor_domain::BatchManifest { jobs }
}

/// Assert that the status sequence of updates matches the expected statuses in order.
pub fn assert_status_sequence(updates: &[StatusUpdate], expected: &[ExecutionStatus]) {
    let actual: Vec<ExecutionStatus> = updates.iter().map(|u| u.status).collect();
    assert_eq!(
        actual, expected,
        "Status sequence mismatch.\n  actual:   {actual:?}\n  expected: {expected:?}"
    );
}
