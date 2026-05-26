use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use aithericon_executor_domain::{
    ExecutionOutcome, ExecutionSpec, ExecutionStatus, RunContext, RunDirectory,
};

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};

use super::*;

// ─── Unit tests (no Docker daemon required) ─────────────────────────────────

#[test]
fn docker_config_serde_roundtrip() {
    let config = DockerConfig {
        image: "python:3.12-slim".into(),
        command: vec!["python3".into(), "train.py".into()],
        entrypoint: None,
        env: HashMap::from([("CUDA".into(), "0".into())]),
        pull_policy: PullPolicy::Always,
        resource_limits: Some(ResourceLimits {
            memory_bytes: Some(4_294_967_296),
            cpu_shares: Some(1024),
            cpu_quota: None,
        }),
        network_mode: Some("bridge".into()),
        extra_volumes: vec!["/data:/data".into()],
        remove_container: true,
    };

    let json = serde_json::to_string_pretty(&config).unwrap();
    let deserialized: DockerConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.image, "python:3.12-slim");
    assert_eq!(deserialized.command, vec!["python3", "train.py"]);
    assert_eq!(deserialized.pull_policy, PullPolicy::Always);
    assert_eq!(
        deserialized.resource_limits.as_ref().unwrap().memory_bytes,
        Some(4_294_967_296)
    );
    assert_eq!(deserialized.network_mode.as_deref(), Some("bridge"));
    assert!(deserialized.remove_container);
}

#[test]
fn docker_config_defaults() {
    let json = r#"{ "image": "alpine" }"#;
    let config: DockerConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.image, "alpine");
    assert!(config.command.is_empty());
    assert!(config.entrypoint.is_none());
    assert!(config.env.is_empty());
    assert_eq!(config.pull_policy, PullPolicy::IfNotPresent);
    assert!(config.resource_limits.is_none());
    assert!(config.network_mode.is_none());
    assert!(config.extra_volumes.is_empty());
    assert!(config.remove_container);
}

#[test]
fn docker_config_into_spec_roundtrip() {
    let config = DockerConfig {
        image: "alpine:3.19".into(),
        command: vec!["echo".into(), "hello".into()],
        entrypoint: None,
        env: HashMap::new(),
        pull_policy: PullPolicy::IfNotPresent,
        resource_limits: None,
        network_mode: None,
        extra_volumes: vec![],
        remove_container: true,
    };

    let spec = config.into_spec();
    assert_eq!(spec.backend, "docker");
    assert!(spec.inputs.is_empty());
    assert!(spec.outputs.is_empty());

    // Round-trip through spec
    let recovered = DockerConfig::from_spec(&spec).unwrap();
    assert_eq!(recovered.image, "alpine:3.19");
    assert_eq!(recovered.command, vec!["echo", "hello"]);
}

#[test]
fn docker_config_into_spec_with_io() {
    let config = DockerConfig {
        image: "alpine".into(),
        command: vec![],
        entrypoint: None,
        env: HashMap::new(),
        pull_policy: PullPolicy::default(),
        resource_limits: None,
        network_mode: None,
        extra_volumes: vec![],
        remove_container: true,
    };

    let inputs = vec![aithericon_executor_domain::InputDeclaration {
        name: "data.csv".into(),
        source: aithericon_executor_domain::InputSource::Inline {
            value: serde_json::json!("test"),
        },
        required: true,
    }];

    let outputs = vec![aithericon_executor_domain::OutputDeclaration {
        name: "result.json".into(),
        path: Some("result.json".into()),
        required: true,
        kind: None,
        upload_to: None,
    }];

    let spec = config.into_spec_with_io(inputs, outputs);
    assert_eq!(spec.backend, "docker");
    assert_eq!(spec.inputs.len(), 1);
    assert_eq!(spec.outputs.len(), 1);
}

#[test]
fn docker_config_from_invalid_spec() {
    let spec = ExecutionSpec {
        backend: "docker".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({ "not_an_image": 42 }),
        config_ref: None,
    };

    let result = DockerConfig::from_spec(&spec);
    assert!(result.is_err());
}

#[test]
fn pull_policy_serde() {
    assert_eq!(
        serde_json::to_string(&PullPolicy::Always).unwrap(),
        "\"always\""
    );
    assert_eq!(
        serde_json::to_string(&PullPolicy::IfNotPresent).unwrap(),
        "\"if_not_present\""
    );
    assert_eq!(
        serde_json::to_string(&PullPolicy::Never).unwrap(),
        "\"never\""
    );

    assert_eq!(
        serde_json::from_str::<PullPolicy>("\"always\"").unwrap(),
        PullPolicy::Always
    );
    assert_eq!(
        serde_json::from_str::<PullPolicy>("\"if_not_present\"").unwrap(),
        PullPolicy::IfNotPresent
    );
}

#[test]
fn supports_docker_spec() {
    let backend = DockerBackend::with_client(
        bollard::Docker::connect_with_local_defaults().unwrap_or_else(|_| {
            // In CI without docker, this still creates a client object
            bollard::Docker::connect_with_http_defaults().unwrap()
        }),
    );

    let docker_spec = DockerConfig {
        image: "alpine".into(),
        command: vec![],
        entrypoint: None,
        env: HashMap::new(),
        pull_policy: PullPolicy::default(),
        resource_limits: None,
        network_mode: None,
        extra_volumes: vec![],
        remove_container: true,
    }
    .into_spec();

    let process_spec = ExecutionSpec {
        backend: "process".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({}),
        config_ref: None,
    };

    assert!(backend.supports(&docker_spec));
    assert!(!backend.supports(&process_spec));
}

// ─── Integration tests (require Docker daemon) ──────────────────────────────

fn noop_callback() -> StatusCallback {
    Box::new(|_status, _detail| Box::pin(async {}))
}

type StatusLog = std::sync::Arc<std::sync::Mutex<Vec<(ExecutionStatus, Value)>>>;

fn tracking_callback() -> (StatusCallback, StatusLog) {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log_clone = log.clone();
    let cb: StatusCallback = Box::new(move |status, detail| {
        let log = log_clone.clone();
        Box::pin(async move {
            log.lock().unwrap().push((status, detail));
        })
    });
    (cb, log)
}

/// Counter for generating unique test execution IDs.
static TEST_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn make_docker_run_context(config: DockerConfig, timeout: Duration) -> RunContext {
    let seq = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let execution_id = format!("docker-test-{}-{}", std::process::id(), seq);
    let base = PathBuf::from(format!("/tmp/aithericon-docker-test-{}", execution_id));
    RunContext {
        execution_id: execution_id.clone(),
        spec: config.into_spec(),
        run_dir: RunDirectory::new(&base, &execution_id),
        timeout,
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    }
}

#[tokio::test]
#[ignore = "requires Docker daemon"]
async fn echo_succeeds_in_container() {
    let backend = DockerBackend::new().expect("Docker connection failed");
    let config = DockerConfig {
        image: "alpine:3.19".into(),
        command: vec!["echo".into(), "hello from docker".into()],
        entrypoint: None,
        env: HashMap::new(),
        pull_policy: PullPolicy::IfNotPresent,
        resource_limits: None,
        network_mode: None,
        extra_volumes: vec![],
        remove_container: true,
    };

    // Ensure image is available
    container::ensure_image(&backend.client, &config.image, config.pull_policy)
        .await
        .unwrap();

    let ctx = make_docker_run_context(config, Duration::from_secs(30));

    // Create run directory
    tokio::fs::create_dir_all(&ctx.run_dir.root).await.unwrap();
    for dir in ctx.run_dir.all_dirs() {
        tokio::fs::create_dir_all(dir).await.unwrap();
    }

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert!(
        result
            .stdout_tail
            .as_deref()
            .unwrap_or("")
            .contains("hello from docker"),
        "stdout should contain 'hello from docker', got: {:?}",
        result.stdout_tail
    );

    // Cleanup
    let _ = tokio::fs::remove_dir_all(&ctx.run_dir.root).await;
}

#[tokio::test]
#[ignore = "requires Docker daemon"]
async fn exit_failure_in_container() {
    let backend = DockerBackend::new().expect("Docker connection failed");
    let config = DockerConfig {
        image: "alpine:3.19".into(),
        command: vec!["false".into()],
        entrypoint: None,
        env: HashMap::new(),
        pull_policy: PullPolicy::IfNotPresent,
        resource_limits: None,
        network_mode: None,
        extra_volumes: vec![],
        remove_container: true,
    };

    container::ensure_image(&backend.client, &config.image, config.pull_policy)
        .await
        .unwrap();

    let ctx = make_docker_run_context(config, Duration::from_secs(30));
    tokio::fs::create_dir_all(&ctx.run_dir.root).await.unwrap();
    for dir in ctx.run_dir.all_dirs() {
        tokio::fs::create_dir_all(dir).await.unwrap();
    }

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(
            result.outcome,
            ExecutionOutcome::ExitFailure { exit_code: 1 }
        ),
        "expected ExitFailure(1), got {:?}",
        result.outcome
    );

    let _ = tokio::fs::remove_dir_all(&ctx.run_dir.root).await;
}

#[tokio::test]
#[ignore = "requires Docker daemon"]
async fn timeout_kills_container() {
    let backend = DockerBackend::new().expect("Docker connection failed");
    let config = DockerConfig {
        image: "alpine:3.19".into(),
        command: vec!["sleep".into(), "60".into()],
        entrypoint: None,
        env: HashMap::new(),
        pull_policy: PullPolicy::IfNotPresent,
        resource_limits: None,
        network_mode: None,
        extra_volumes: vec![],
        remove_container: true,
    };

    container::ensure_image(&backend.client, &config.image, config.pull_policy)
        .await
        .unwrap();

    let ctx = make_docker_run_context(config, Duration::from_millis(500));
    tokio::fs::create_dir_all(&ctx.run_dir.root).await.unwrap();
    for dir in ctx.run_dir.all_dirs() {
        tokio::fs::create_dir_all(dir).await.unwrap();
    }

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::TimedOut),
        "expected TimedOut, got {:?}",
        result.outcome
    );

    let _ = tokio::fs::remove_dir_all(&ctx.run_dir.root).await;
}

#[tokio::test]
#[ignore = "requires Docker daemon"]
async fn cancellation_stops_container() {
    let backend = DockerBackend::new().expect("Docker connection failed");
    let config = DockerConfig {
        image: "alpine:3.19".into(),
        command: vec!["sleep".into(), "60".into()],
        entrypoint: None,
        env: HashMap::new(),
        pull_policy: PullPolicy::IfNotPresent,
        resource_limits: None,
        network_mode: None,
        extra_volumes: vec![],
        remove_container: true,
    };

    container::ensure_image(&backend.client, &config.image, config.pull_policy)
        .await
        .unwrap();

    let ctx = make_docker_run_context(config, Duration::from_secs(60));
    tokio::fs::create_dir_all(&ctx.run_dir.root).await.unwrap();
    for dir in ctx.run_dir.all_dirs() {
        tokio::fs::create_dir_all(dir).await.unwrap();
    }

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    let result = backend
        .execute(&ctx, noop_callback(), None, cancel)
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Cancelled),
        "expected Cancelled, got {:?}",
        result.outcome
    );

    let _ = tokio::fs::remove_dir_all(&ctx.run_dir.root).await;
}

#[tokio::test]
#[ignore = "requires Docker daemon"]
async fn reports_running_with_container_id() {
    let backend = DockerBackend::new().expect("Docker connection failed");
    let config = DockerConfig {
        image: "alpine:3.19".into(),
        command: vec!["echo".into(), "hi".into()],
        entrypoint: None,
        env: HashMap::new(),
        pull_policy: PullPolicy::IfNotPresent,
        resource_limits: None,
        network_mode: None,
        extra_volumes: vec![],
        remove_container: true,
    };

    container::ensure_image(&backend.client, &config.image, config.pull_policy)
        .await
        .unwrap();

    let ctx = make_docker_run_context(config, Duration::from_secs(30));
    tokio::fs::create_dir_all(&ctx.run_dir.root).await.unwrap();
    for dir in ctx.run_dir.all_dirs() {
        tokio::fs::create_dir_all(dir).await.unwrap();
    }

    let (cb, log) = tracking_callback();
    backend
        .execute(&ctx, cb, None, CancellationToken::new())
        .await
        .unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, ExecutionStatus::Running);
    assert!(entries[0].1.get("container_id").is_some());

    let _ = tokio::fs::remove_dir_all(&ctx.run_dir.root).await;
}

#[tokio::test]
#[ignore = "requires Docker daemon"]
async fn env_vars_injected_in_container() {
    let backend = DockerBackend::new().expect("Docker connection failed");
    let config = DockerConfig {
        image: "alpine:3.19".into(),
        command: vec![
            "sh".into(),
            "-c".into(),
            "echo CUSTOM=$CUSTOM_VAR && echo EID=$AITHERICON_EXECUTION_ID && echo RUN=$AITHERICON_RUN_DIR".into(),
        ],
        entrypoint: None,
        env: HashMap::from([("CUSTOM_VAR".into(), "custom_42".into())]),
        pull_policy: PullPolicy::IfNotPresent,
        resource_limits: None,
        network_mode: None,
        extra_volumes: vec![],
        remove_container: true,
    };

    container::ensure_image(&backend.client, &config.image, config.pull_policy)
        .await
        .unwrap();

    let ctx = make_docker_run_context(config, Duration::from_secs(30));
    let eid = ctx.execution_id.clone();
    tokio::fs::create_dir_all(&ctx.run_dir.root).await.unwrap();
    for dir in ctx.run_dir.all_dirs() {
        tokio::fs::create_dir_all(dir).await.unwrap();
    }

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    let stdout = result.stdout_tail.unwrap_or_default();
    assert!(
        stdout.contains("CUSTOM=custom_42"),
        "should contain custom env var, got: {stdout}"
    );
    assert!(
        stdout.contains(&format!("EID={eid}")),
        "should contain execution id, got: {stdout}"
    );
    assert!(
        stdout.contains(&format!("RUN={CONTAINER_RUN_DIR}")),
        "should contain container run dir, got: {stdout}"
    );

    let _ = tokio::fs::remove_dir_all(&ctx.run_dir.root).await;
}
