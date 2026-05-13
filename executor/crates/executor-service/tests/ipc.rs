use std::sync::Arc;
use std::time::Duration;

use aithericon_executor_domain::{ExecutionStatus, OutputDeclaration};
use aithericon_executor_storage::LocalArtifactStore;
use aithericon_executor_test_harness::context::ExecutorTestContext;
use aithericon_executor_test_harness::helpers::*;
use aithericon_executor_test_harness::ipc_client::ipc_client_job;
use uuid::Uuid;

/// Verify IPC set_output: output value appears in terminal detail and an Output event is emitted.
#[tokio::test]
async fn test_ipc_set_output() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-output-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-output-status", &eid).await;
    let events_consumer = ctx.events_consumer("ipc-output-events", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "set_output", "name": "accuracy", "value_json": "0.95" }
        ],
        "exit_code": 0
    });

    ctx.push_job(ipc_client_job(&eid, &plan, vec![])).await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
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
    let outputs = &completed.detail["outputs"];
    assert_eq!(
        outputs["accuracy"],
        serde_json::json!(0.95),
        "accuracy output should be 0.95"
    );

    // Check for Output event
    let events = ctx
        .collect_events(&events_consumer, 1, Duration::from_secs(5))
        .await;
    assert!(
        !events.is_empty(),
        "expected at least 1 Output event, got: {}",
        events.len()
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify IPC update_progress: progress appears in terminal detail and a Progress event is emitted.
#[tokio::test]
async fn test_ipc_progress() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-progress-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-progress-status", &eid).await;
    let events_consumer = ctx.events_consumer("ipc-progress-events", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "update_progress", "fraction": 0.75, "message": "Training", "current_step": 75, "total_steps": 100 }
        ],
        "exit_code": 0
    });

    ctx.push_job(ipc_client_job(&eid, &plan, vec![])).await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
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
    let progress = &completed.detail["progress"];
    assert!(
        !progress.is_null(),
        "progress should be present in terminal detail"
    );

    let fraction = progress["fraction"].as_f64().unwrap();
    assert!(
        (fraction - 0.75).abs() < 0.01,
        "fraction should be ~0.75, got: {fraction}"
    );

    // Check for Progress event
    let events = ctx
        .collect_events(&events_consumer, 1, Duration::from_secs(5))
        .await;
    assert!(
        !events.is_empty(),
        "expected at least 1 Progress event, got: {}",
        events.len()
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify IPC define_phases + update_phase: phases appear in terminal detail.
#[tokio::test]
async fn test_ipc_phases() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-phases-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-phases-status", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "define_phases", "phase_names": ["preprocess", "train"] },
            { "type": "update_phase", "phase_name": "preprocess", "status": "completed" },
            { "type": "update_phase", "phase_name": "train", "status": "running" }
        ],
        "exit_code": 0
    });

    ctx.push_job(ipc_client_job(&eid, &plan, vec![])).await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
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
    let progress = &completed.detail["progress"];
    assert!(
        !progress.is_null(),
        "progress with phases should be present"
    );

    let phases = progress["phases"]
        .as_array()
        .expect("phases should be an array");
    assert_eq!(phases.len(), 2, "should have 2 phases");
    assert_eq!(phases[0]["name"].as_str().unwrap(), "preprocess");
    assert_eq!(phases[0]["status"].as_str().unwrap(), "completed");
    assert_eq!(phases[1]["name"].as_str().unwrap(), "train");
    assert_eq!(phases[1]["status"].as_str().unwrap(), "running");

    worker.abort();
    ctx.cleanup().await;
}

/// Verify IPC log_artifact without an artifact store: artifact appears in manifest.
#[tokio::test]
async fn test_ipc_artifact_no_store() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-art-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-art-status", &eid).await;
    let events_consumer = ctx.events_consumer("ipc-art-events", &eid).await;
    let worker = ctx.spawn_worker();

    // The plan creates a file then logs it as an artifact
    let plan = serde_json::json!({
        "actions": [
            {
                "type": "log_artifact",
                "artifact_id": "art-001",
                "path": "${AITHERICON_ARTIFACTS_DIR}/model.pt",
                "name": "model",
                "category": "model"
            }
        ],
        "exit_code": 0
    });

    // Pre-create the artifact file: the bash wrapper creates it before running the client
    let script = format!(
        "echo model_data > $AITHERICON_ARTIFACTS_DIR/model.pt && {} $AITHERICON_INPUTS_DIR/ipc_plan.json",
        aithericon_executor_test_harness::ipc_client::ipc_test_client_path().display()
    );

    let inputs = vec![aithericon_executor_domain::InputDeclaration {
        name: "ipc_plan.json".into(),
        source: aithericon_executor_domain::InputSource::Inline { value: plan },
        required: true,
    }];

    ctx.push_job(
        aithericon_executor_test_harness::helpers::job_with_inline_inputs(&eid, &script, inputs),
    )
    .await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
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
    let manifest = &completed.detail["artifact_manifest"];
    assert!(!manifest.is_null(), "artifact_manifest should be present");

    let artifacts = manifest["artifacts"]
        .as_array()
        .expect("artifacts should be an array");
    assert!(!artifacts.is_empty(), "should have at least 1 artifact");
    assert_eq!(artifacts[0]["name"].as_str().unwrap(), "model");

    // Check for Artifact event
    let events = ctx
        .collect_events(&events_consumer, 1, Duration::from_secs(5))
        .await;
    assert!(
        !events.is_empty(),
        "expected at least 1 Artifact event, got: {}",
        events.len()
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify IPC log_artifact with LocalArtifactStore: artifact gets a storage_path.
#[tokio::test]
async fn test_ipc_artifact_with_store() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-art-store-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-art-store-status", &eid).await;

    let store = Arc::new(LocalArtifactStore::new(ctx.base_dir.clone()));
    let worker = ctx.spawn_worker_with(
        aithericon_executor_worker::CleanupPolicy::Retain,
        Some(store),
    );

    let plan = serde_json::json!({
        "actions": [
            {
                "type": "log_artifact",
                "artifact_id": "art-store-001",
                "path": "${AITHERICON_ARTIFACTS_DIR}/model.pt",
                "name": "stored-model",
                "category": "model"
            }
        ],
        "exit_code": 0
    });

    let script = format!(
        "echo model_data > $AITHERICON_ARTIFACTS_DIR/model.pt && {} $AITHERICON_INPUTS_DIR/ipc_plan.json",
        aithericon_executor_test_harness::ipc_client::ipc_test_client_path().display()
    );

    let inputs = vec![aithericon_executor_domain::InputDeclaration {
        name: "ipc_plan.json".into(),
        source: aithericon_executor_domain::InputSource::Inline { value: plan },
        required: true,
    }];

    ctx.push_job(
        aithericon_executor_test_harness::helpers::job_with_inline_inputs(&eid, &script, inputs),
    )
    .await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
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
    let artifacts = completed.detail["artifact_manifest"]["artifacts"]
        .as_array()
        .expect("artifacts should be an array");
    assert!(!artifacts.is_empty());

    let storage_path = &artifacts[0]["storage_path"];
    assert!(
        !storage_path.is_null(),
        "artifact should have a storage_path when store is configured"
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify multiple IPC set_output calls: all outputs in terminal detail and correct event count.
#[tokio::test]
async fn test_ipc_multiple_outputs() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-multi-out-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-multi-out-status", &eid).await;
    let events_consumer = ctx.events_consumer("ipc-multi-out-events", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "set_output", "name": "accuracy", "value_json": "0.95" },
            { "type": "set_output", "name": "loss", "value_json": "0.05" },
            { "type": "set_output", "name": "epochs", "value_json": "100" }
        ],
        "exit_code": 0
    });

    ctx.push_job(ipc_client_job(&eid, &plan, vec![])).await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
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
    let outputs = &completed.detail["outputs"];
    assert_eq!(outputs["accuracy"], serde_json::json!(0.95));
    assert_eq!(outputs["loss"], serde_json::json!(0.05));
    assert_eq!(outputs["epochs"], serde_json::json!(100));

    // Should have 3 Output events
    let events = ctx
        .collect_events(&events_consumer, 3, Duration::from_secs(5))
        .await;
    assert_eq!(events.len(), 3, "expected 3 Output events");

    worker.abort();
    ctx.cleanup().await;
}

/// Verify combined IPC workflow: outputs, progress, phases, artifact all in terminal detail.
#[tokio::test]
async fn test_ipc_combined_workflow() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-combined-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-combined-status", &eid).await;
    let events_consumer = ctx.events_consumer("ipc-combined-events", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "set_output", "name": "accuracy", "value_json": "0.95" },
            { "type": "update_progress", "fraction": 1.0, "message": "Done", "current_step": 100, "total_steps": 100 },
            { "type": "define_phases", "phase_names": ["preprocess", "train"] },
            { "type": "update_phase", "phase_name": "preprocess", "status": "completed" },
            { "type": "update_phase", "phase_name": "train", "status": "completed" },
            {
                "type": "log_artifact",
                "artifact_id": "art-combined-001",
                "path": "${AITHERICON_ARTIFACTS_DIR}/result.bin",
                "name": "result"
            },
            { "type": "log_message", "level": "info", "message": "all done" },
            { "type": "health_check", "sequence": 1 }
        ],
        "exit_code": 0
    });

    let script = format!(
        "echo data > $AITHERICON_ARTIFACTS_DIR/result.bin && {} $AITHERICON_INPUTS_DIR/ipc_plan.json",
        aithericon_executor_test_harness::ipc_client::ipc_test_client_path().display()
    );

    let inputs = vec![aithericon_executor_domain::InputDeclaration {
        name: "ipc_plan.json".into(),
        source: aithericon_executor_domain::InputSource::Inline { value: plan },
        required: true,
    }];

    ctx.push_job(
        aithericon_executor_test_harness::helpers::job_with_inline_inputs(&eid, &script, inputs),
    )
    .await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
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
    let detail = &completed.detail;

    // Outputs
    assert_eq!(detail["outputs"]["accuracy"], serde_json::json!(0.95));

    // Progress
    let progress = &detail["progress"];
    assert!(!progress.is_null());
    let fraction = progress["fraction"].as_f64().unwrap();
    assert!((fraction - 1.0).abs() < 0.01);

    // Phases
    let phases = progress["phases"].as_array().expect("phases");
    assert_eq!(phases.len(), 2);

    // Artifact manifest
    let manifest = &detail["artifact_manifest"];
    assert!(!manifest.is_null());
    let artifacts = manifest["artifacts"].as_array().expect("artifacts");
    assert!(!artifacts.is_empty());

    // Events: 1 Output + 1 Progress + 1 Artifact = at least 3
    let events = ctx
        .collect_events(&events_consumer, 3, Duration::from_secs(5))
        .await;
    assert!(
        events.len() >= 3,
        "expected at least 3 events, got: {}",
        events.len()
    );

    worker.abort();
    ctx.cleanup().await;
}

/// Verify IPC set_output satisfies a required output declaration (path: None).
#[tokio::test]
async fn test_ipc_output_satisfies_required() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-out-req-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-out-req-status", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "set_output", "name": "result", "value_json": "42" }
        ],
        "exit_code": 0
    });

    let outputs = vec![OutputDeclaration {
        name: "result".into(),
        path: None, // No file path — satisfied by IPC set_output
        required: true,
        upload_to: None,
    }];

    ctx.push_job(ipc_client_job(&eid, &plan, outputs)).await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
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

/// Verify IPC health_check alone completes successfully.
#[tokio::test]
async fn test_ipc_health_check() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let ctx = ExecutorTestContext::new().await;
    let eid = format!("ipc-health-{}", Uuid::new_v4().simple());
    let status_consumer = ctx.status_consumer("ipc-health-status", &eid).await;
    let worker = ctx.spawn_worker();

    let plan = serde_json::json!({
        "actions": [
            { "type": "health_check", "sequence": 1 }
        ],
        "exit_code": 0
    });

    ctx.push_job(ipc_client_job(&eid, &plan, vec![])).await;

    let statuses = ctx
        .collect_statuses(&status_consumer, Duration::from_secs(15))
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
