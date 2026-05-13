use std::time::Duration;

use aithericon_executor_domain::{ExecutionStatus, InputDeclaration, InputSource};
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use uuid::Uuid;

/// Verify inline input is staged to $AITHERICON_INPUTS_DIR and readable by the process.
#[tokio::test]
async fn test_inline_input_staged() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("input-staged-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("input-staged", &eid).await;
    let worker = ctx.spawn_worker();

    let inputs = vec![InputDeclaration {
        name: "config.json".into(),
        source: InputSource::Inline {
            value: serde_json::json!({"lr": 0.001}),
        },
        required: true,
    }];

    ctx.push_job(job_with_inline_inputs(
        &eid,
        "cat $AITHERICON_INPUTS_DIR/config.json",
        inputs,
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
    assert!(
        stdout_tail.contains("\"lr\": 0.001") || stdout_tail.contains("\"lr\":0.001"),
        "stdout should contain the inline JSON value, got: {stdout_tail}"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify multiple inline inputs are staged correctly.
#[tokio::test]
async fn test_inline_input_multiple() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("input-multi-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("input-multi", &eid).await;
    let worker = ctx.spawn_worker();

    let inputs = vec![
        InputDeclaration {
            name: "data.json".into(),
            source: InputSource::Inline {
                value: serde_json::json!({"rows": 100}),
            },
            required: true,
        },
        InputDeclaration {
            name: "params.json".into(),
            source: InputSource::Inline {
                value: serde_json::json!({"epochs": 10}),
            },
            required: true,
        },
    ];

    ctx.push_job(job_with_inline_inputs(
        &eid,
        "ls $AITHERICON_INPUTS_DIR | sort",
        inputs,
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
    assert!(
        stdout_tail.contains("data.json"),
        "stdout should list data.json, got: {stdout_tail}"
    );
    assert!(
        stdout_tail.contains("params.json"),
        "stdout should list params.json, got: {stdout_tail}"
    );

    worker.abort();
    ctx.cleanup().await;
}
