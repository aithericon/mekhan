use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aithericon_executor_domain::{ExecutionStatus, InputDeclaration, InputSource};
use aithericon_executor_storage::LocalArtifactStore;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use uuid::Uuid;

/// Verify InputSource::StoragePath: file is downloaded from store and staged in inputs_dir.
#[tokio::test]
async fn test_storage_path_input_staged() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    // Set up a LocalArtifactStore with a pre-seeded file
    let store_base = PathBuf::from(format!(
        "/tmp/store-test-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    let file_dir = store_base.join("test-data");
    std::fs::create_dir_all(&file_dir).expect("failed to create store dir");
    std::fs::write(file_dir.join("input.txt"), "storage_test_data")
        .expect("failed to write test file");

    let store = Arc::new(LocalArtifactStore::new(store_base.clone()));
    let ctx = ExecutorTestContext::new_with_store(store).await;
    let eid = format!("store-in-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("store-in", &eid).await;
    let worker = ctx.spawn_worker();

    let inputs = vec![InputDeclaration {
        name: "input.txt".into(),
        source: InputSource::StoragePath {
            path: "test-data/input.txt".into(),
            storage: None,
        },
        required: true,
    }];

    ctx.push_job(job_with_inline_inputs(
        &eid,
        "cat $AITHERICON_INPUTS_DIR/input.txt",
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
    let stdout_tail = completed.detail["stdout_tail"]
        .as_str()
        .expect("stdout_tail should be present");
    assert!(
        stdout_tail.contains("storage_test_data"),
        "stdout should contain file content from store, got: {stdout_tail:?}"
    );

    worker.abort();
    ctx.cleanup().await;
    let _ = std::fs::remove_dir_all(&store_base);
}

/// Verify: required StoragePath input without a configured store → staging fails → Failed.
#[tokio::test]
async fn test_storage_path_required_no_store_fails() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    // Default context has no artifact store
    let ctx = ExecutorTestContext::new().await;
    let eid = format!("store-no-{}", Uuid::new_v4().simple());
    let consumer = ctx.status_consumer("store-no", &eid).await;
    let worker = ctx.spawn_worker();

    let inputs = vec![InputDeclaration {
        name: "missing.txt".into(),
        source: InputSource::StoragePath {
            path: "nonexistent/path.txt".into(),
            storage: None,
        },
        required: true,
    }];

    ctx.push_job(job_with_inline_inputs(&eid, "echo should-not-run", inputs))
        .await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(10))
        .await;

    // Staging fails before execution starts → no Running status
    assert_status_sequence(
        &statuses,
        &[ExecutionStatus::Accepted, ExecutionStatus::Failed],
    );

    let failed = statuses.last().unwrap();
    let error = failed.detail["error"].as_str().unwrap();
    assert!(
        error.contains("requires ArtifactStore but none is configured"),
        "expected store-not-configured error, got: {error}"
    );

    worker.abort();
    ctx.cleanup().await;
}
