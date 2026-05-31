use std::time::Duration;

use aithericon_executor_domain::{
    ExecutionStatus, InputDeclaration, InputSource, OutputDeclaration,
};
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_test_harness::ipc_client::ipc_test_client_path;
use uuid::Uuid;

/// Verify: required output produced -> Completed.
#[tokio::test]
async fn test_required_output_present() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("out-present-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("out-present", &eid).await;
    let worker = ctx.spawn_worker();

    let outputs = vec![OutputDeclaration {
        name: "result.txt".into(),
        path: Some("result.txt".into()),
        required: true,
        kind: None,
        upload_to: None,
    }];

    ctx.push_job(job_with_outputs(
        &eid,
        "echo result > $AITHERICON_OUTPUTS_DIR/result.txt",
        outputs,
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

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: required output missing -> Failed with descriptive error.
#[tokio::test]
async fn test_required_output_missing() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("out-missing-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("out-missing", &eid).await;
    let worker = ctx.spawn_worker();

    let outputs = vec![OutputDeclaration {
        name: "result.txt".into(),
        path: Some("result.txt".into()),
        required: true,
        kind: None,
        upload_to: None,
    }];

    ctx.push_job(job_with_outputs(&eid, "echo done", outputs))
        .await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Failed,
        ],
    );

    let failed = statuses.last().unwrap();
    let detail_str = serde_json::to_string(&failed.detail).unwrap();
    assert!(
        detail_str.contains("required output") && detail_str.contains("result.txt"),
        "detail should mention missing required output, got: {detail_str}"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: optional output missing -> still Completed.
#[tokio::test]
async fn test_optional_output_missing() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("out-optional-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("out-optional", &eid).await;
    let worker = ctx.spawn_worker();

    let outputs = vec![OutputDeclaration {
        name: "optional.txt".into(),
        path: Some("optional.txt".into()),
        required: false,
        kind: None,
        upload_to: None,
    }];

    ctx.push_job(job_with_outputs(&eid, "echo done", outputs))
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

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: output check is skipped when the process itself fails (exit 1).
#[tokio::test]
async fn test_output_check_skipped_on_failure() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("out-skip-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("out-skip", &eid).await;
    let worker = ctx.spawn_worker();

    let outputs = vec![OutputDeclaration {
        name: "result.txt".into(),
        path: Some("result.txt".into()),
        required: true,
        kind: None,
        upload_to: None,
    }];

    ctx.push_job(job_with_outputs(&eid, "exit 1", outputs))
        .await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Failed,
        ],
    );

    // Should fail due to exit code, not missing output
    let failed = statuses.last().unwrap();
    let exit_code = failed.detail["outcome"]["exit_code"].as_i64().unwrap();
    assert_eq!(exit_code, 1);

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: file-based output with valid JSON content is parsed and appears in detail["outputs"].
#[tokio::test]
async fn test_file_output_json_content() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("out-json-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("out-json", &eid).await;
    let worker = ctx.spawn_worker();

    let outputs = vec![OutputDeclaration {
        name: "result".into(),
        path: Some("result.json".into()),
        required: true,
        kind: None,
        upload_to: None,
    }];

    ctx.push_job(job_with_outputs(
        &eid,
        r#"printf '{"score": 42}' > $AITHERICON_OUTPUTS_DIR/result.json"#,
        outputs,
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
    let output_value = &completed.detail["outputs"]["result"];
    assert_eq!(
        *output_value,
        serde_json::json!({"score": 42}),
        "file-based output should be parsed as JSON, got: {output_value}"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: file-based output with non-JSON content becomes a JSON string in detail["outputs"].
#[tokio::test]
async fn test_file_output_plain_text_content() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("out-text-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("out-text", &eid).await;
    let worker = ctx.spawn_worker();

    let outputs = vec![OutputDeclaration {
        name: "greeting".into(),
        path: Some("greeting.txt".into()),
        required: true,
        kind: None,
        upload_to: None,
    }];

    ctx.push_job(job_with_outputs(
        &eid,
        "printf 'hello world' > $AITHERICON_OUTPUTS_DIR/greeting.txt",
        outputs,
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
    let output_value = &completed.detail["outputs"]["greeting"];
    assert_eq!(
        *output_value,
        serde_json::json!("hello world"),
        "non-JSON output should be stored as a string, got: {output_value}"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify: IPC set_output takes precedence over file-based output with the same name.
#[tokio::test]
async fn test_file_output_ipc_takes_precedence() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("out-precedence-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("out-precedence", &eid).await;
    let worker = ctx.spawn_worker();

    // IPC plan: set "result" to 999 via IPC
    let plan = serde_json::json!({
        "actions": [
            { "type": "set_output", "name": "result", "value_json": "999" }
        ],
        "exit_code": 0
    });

    // Composite script: write a different value to the file, then run IPC client
    let script = format!(
        r#"printf '{{"from": "file"}}' > $AITHERICON_OUTPUTS_DIR/result.json && {} $AITHERICON_INPUTS_DIR/ipc_plan.json"#,
        ipc_test_client_path().display()
    );

    let inputs = vec![InputDeclaration {
        name: "ipc_plan.json".into(),
        source: InputSource::Inline { value: plan },
        required: true,
    }];

    let outputs = vec![OutputDeclaration {
        name: "result".into(),
        path: Some("result.json".into()),
        required: true,
        kind: None,
        upload_to: None,
    }];

    ctx.push_job(job_with_io(&eid, &script, inputs, outputs))
        .await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(15))
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
    let output_value = &completed.detail["outputs"]["result"];
    assert_eq!(
        *output_value,
        serde_json::json!(999),
        "IPC output should take precedence over file output, got: {output_value}"
    );

    worker.abort();
    ctx.cleanup().await;
}
