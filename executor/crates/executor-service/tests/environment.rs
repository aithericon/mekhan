use std::collections::HashMap;
use std::time::Duration;

use aithericon_executor_backend::ProcessConfig;
use aithericon_executor_domain::{ExecutionJob, ExecutionStatus, JobPriority};
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use uuid::Uuid;

/// Verify all 6 AITHERICON_* env vars are injected and accessible by the process.
#[tokio::test]
async fn test_aithericon_env_vars_injected() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("env-vars-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("env-vars", &eid).await;
    let worker = ctx.spawn_worker();

    let script = r#"
echo "RUN_DIR=$AITHERICON_RUN_DIR"
echo "IPC_SOCKET=$AITHERICON_IPC_SOCKET"
echo "INPUTS_DIR=$AITHERICON_INPUTS_DIR"
echo "OUTPUTS_DIR=$AITHERICON_OUTPUTS_DIR"
echo "ARTIFACTS_DIR=$AITHERICON_ARTIFACTS_DIR"
echo "EXECUTION_ID=$AITHERICON_EXECUTION_ID"
"#;

    ctx.push_job(bash_job(&eid, script)).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ],
    );

    let completed = statuses.last().unwrap();
    let stdout_tail = completed.detail["stdout_tail"].as_str().unwrap();

    // All env vars should be non-empty
    assert!(
        stdout_tail.contains("RUN_DIR=/"),
        "AITHERICON_RUN_DIR should be set, got: {stdout_tail}"
    );
    assert!(
        stdout_tail.contains("IPC_SOCKET="),
        "AITHERICON_IPC_SOCKET should be set, got: {stdout_tail}"
    );
    assert!(
        stdout_tail.contains("INPUTS_DIR="),
        "AITHERICON_INPUTS_DIR should be set, got: {stdout_tail}"
    );
    assert!(
        stdout_tail.contains("OUTPUTS_DIR="),
        "AITHERICON_OUTPUTS_DIR should be set, got: {stdout_tail}"
    );
    assert!(
        stdout_tail.contains("ARTIFACTS_DIR="),
        "AITHERICON_ARTIFACTS_DIR should be set, got: {stdout_tail}"
    );
    assert!(
        stdout_tail.contains(&format!("EXECUTION_ID={eid}")),
        "AITHERICON_EXECUTION_ID should equal {eid}, got: {stdout_tail}"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify spec-level env vars are passed through to the child process.
#[tokio::test]
async fn test_spec_env_vars() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("spec-env-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("spec-env", &eid).await;
    let worker = ctx.spawn_worker();

    let job = ExecutionJob {
        execution_id: eid.clone(),
        spec: ProcessConfig {
            command: "bash".into(),
            args: vec!["-c".into(), "echo $CUSTOM_VAR".into()],
            env: HashMap::from([("CUSTOM_VAR".into(), "custom_value_42".into())]),
            working_dir: None,
            inherit_env: true,
        }
        .into_spec(),
        metadata: HashMap::new(),
        timeout: None,
        priority: JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };

    ctx.push_job(job).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ],
    );

    let completed = statuses.last().unwrap();
    let stdout_tail = completed.detail["stdout_tail"].as_str().unwrap();
    assert!(
        stdout_tail.contains("custom_value_42"),
        "stdout should contain custom env var value, got: {stdout_tail}"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify context.json is written to the run directory.
#[tokio::test]
async fn test_context_json_written() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ctx-json-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("ctx-json", &eid).await;
    let worker = ctx.spawn_worker();

    ctx.push_job(bash_job(
        &eid,
        "test -f $AITHERICON_RUN_DIR/context.json && echo exists",
    ))
    .await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ],
    );

    let completed = statuses.last().unwrap();
    let stdout_tail = completed.detail["stdout_tail"].as_str().unwrap();
    assert_eq!(
        stdout_tail.trim(),
        "exists",
        "context.json should exist in the run directory"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify the run directory has the expected subdirectory structure.
#[tokio::test]
async fn test_run_directory_structure() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("dir-struct-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("dir-struct", &eid).await;
    let worker = ctx.spawn_worker();

    // Check all 4 subdirectories exist
    let script = r#"
ls -d "$AITHERICON_RUN_DIR/inputs" "$AITHERICON_RUN_DIR/outputs" "$AITHERICON_RUN_DIR/artifacts" "$AITHERICON_RUN_DIR/logs" && echo all_exist
"#;

    ctx.push_job(bash_job(&eid, script)).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ],
    );

    let completed = statuses.last().unwrap();
    let stdout_tail = completed.detail["stdout_tail"].as_str().unwrap();
    assert!(
        stdout_tail.contains("all_exist"),
        "all subdirectories should exist, got: {stdout_tail}"
    );

    worker.abort();
    ctx.cleanup().await;
}
