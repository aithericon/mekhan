use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use aithericon_executor_domain::{
    ExecutionOutcome, ExecutionSpec, ExecutionStatus, RunContext, RunDirectory,
};

use super::*;
use crate::traits::{ExecutionBackend, StatusCallback};

// ─── Unit tests (no process execution required) ─────────────────────────────

#[test]
fn process_config_serde_roundtrip() {
    let config = ProcessConfig {
        command: "python3".into(),
        args: vec!["train.py".into(), "--epochs".into(), "10".into()],
        env: HashMap::from([
            ("CUDA_VISIBLE_DEVICES".into(), "0".into()),
            ("OMP_NUM_THREADS".into(), "4".into()),
        ]),
        working_dir: Some("/workspace".into()),
        inherit_env: false,
    };

    let json = serde_json::to_string_pretty(&config).unwrap();
    let deserialized: ProcessConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.command, "python3");
    assert_eq!(
        deserialized.args,
        vec!["train.py", "--epochs", "10"]
    );
    assert_eq!(deserialized.env.len(), 2);
    assert_eq!(
        deserialized.env.get("CUDA_VISIBLE_DEVICES").unwrap(),
        "0"
    );
    assert_eq!(deserialized.working_dir.as_deref(), Some("/workspace"));
    assert!(!deserialized.inherit_env);
}

#[test]
fn process_config_defaults() {
    let json = r#"{ "command": "echo" }"#;
    let config: ProcessConfig = serde_json::from_str(json).unwrap();

    assert_eq!(config.command, "echo");
    assert!(config.args.is_empty());
    assert!(config.env.is_empty());
    assert!(config.working_dir.is_none());
    assert!(config.inherit_env);
}

#[test]
fn process_config_into_spec_roundtrip() {
    let config = ProcessConfig {
        command: "echo".into(),
        args: vec!["hello".into()],
        env: HashMap::from([("FOO".into(), "bar".into())]),
        working_dir: Some("/tmp".into()),
        inherit_env: false,
    };

    let spec = config.into_spec();
    assert_eq!(spec.backend, "process");
    assert!(spec.inputs.is_empty());
    assert!(spec.outputs.is_empty());

    let recovered = ProcessConfig::from_spec(&spec).unwrap();
    assert_eq!(recovered.command, "echo");
    assert_eq!(recovered.args, vec!["hello"]);
    assert_eq!(recovered.env.get("FOO").unwrap(), "bar");
    assert_eq!(recovered.working_dir.as_deref(), Some("/tmp"));
    assert!(!recovered.inherit_env);
}

#[test]
fn process_config_into_spec_with_io() {
    let config = ProcessConfig {
        command: "process".into(),
        args: vec![],
        env: HashMap::new(),
        working_dir: None,
        inherit_env: true,
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
    assert_eq!(spec.backend, "process");
    assert_eq!(spec.inputs.len(), 1);
    assert_eq!(spec.outputs.len(), 1);
}

#[test]
fn process_config_from_spec_invalid() {
    let spec = ExecutionSpec {
        backend: "process".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({ "command": 123 }),
        config_ref: None,
    };

    let result = ProcessConfig::from_spec(&spec);
    assert!(result.is_err());
}

#[test]
fn supports_process_spec() {
    let backend = ProcessBackend::new();

    let process_spec = ProcessConfig {
        command: "echo".into(),
        args: vec![],
        env: HashMap::new(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();

    let docker_spec = ExecutionSpec {
        backend: "docker".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({}),
        config_ref: None,
    };

    assert!(backend.supports(&process_spec));
    assert!(!backend.supports(&docker_spec));
}

#[test]
fn backend_name() {
    let backend = ProcessBackend::new();
    assert_eq!(backend.name(), "process");
}

// ─── Integration tests (require process execution) ──────────────────────────

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

static TEST_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn make_process_run_context(spec: ExecutionSpec, timeout: Duration) -> RunContext {
    let seq = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let execution_id = format!("process-test-{}-{}", std::process::id(), seq);
    RunContext {
        execution_id: execution_id.clone(),
        spec,
        run_dir: RunDirectory::new(&PathBuf::from("/tmp"), &execution_id),
        timeout,
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        metadata: HashMap::new(),
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: Vec::new(),
        backend_state: serde_json::Value::Null,
    }
}

fn make_process_run_context_with_env(
    spec: ExecutionSpec,
    timeout: Duration,
    env: HashMap<String, String>,
) -> RunContext {
    let mut ctx = make_process_run_context(spec, timeout);
    ctx.env = env;
    ctx
}

#[tokio::test]
async fn echo_succeeds() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "echo".into(),
        args: vec!["hello".into()],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::Success),
        "expected Success, got {:?}",
        result.outcome
    );
    assert_eq!(result.stdout_tail.as_deref(), Some("hello\n"));
}

#[tokio::test]
async fn false_exits_nonzero() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "false".into(),
        args: vec![],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

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
}

#[tokio::test]
async fn timeout_kills_process() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "sleep".into(),
        args: vec!["60".into()],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_millis(100));

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        matches!(result.outcome, ExecutionOutcome::TimedOut),
        "expected TimedOut, got {:?}",
        result.outcome
    );
}

#[tokio::test]
async fn cancellation_stops_process() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "sleep".into(),
        args: vec!["60".into()],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(60));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
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
}

#[tokio::test]
async fn reports_running_with_pid() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "echo".into(),
        args: vec!["hi".into()],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

    let (cb, log) = tracking_callback();
    backend
        .execute(&ctx, cb, None, CancellationToken::new())
        .await
        .unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, ExecutionStatus::Running);
    assert!(entries[0].1.get("pid").is_some());
}

#[tokio::test]
async fn captures_output_tail() {
    let backend = ProcessBackend::new().with_max_output_bytes(10);
    let spec = ProcessConfig {
        command: "sh".into(),
        args: vec![
            "-c".into(),
            "echo 'this is a long output that exceeds the buffer'".into(),
        ],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    let stdout = result.stdout_tail.unwrap();
    assert!(stdout.len() <= 10);
}

// ─── New integration tests ──────────────────────────────────────────────────

#[tokio::test]
async fn env_vars_from_spec_and_context() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "sh".into(),
        args: vec![
            "-c".into(),
            "echo SPEC=$SPEC_VAR; echo CTX=$CTX_VAR; echo OVERRIDE=$SHARED_VAR".into(),
        ],
        env: HashMap::from([
            ("SPEC_VAR".into(), "from_spec".into()),
            ("SHARED_VAR".into(), "spec_value".into()),
        ]),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();

    let ctx = make_process_run_context_with_env(
        spec,
        Duration::from_secs(10),
        HashMap::from([
            ("CTX_VAR".into(), "from_context".into()),
            ("SHARED_VAR".into(), "context_value".into()),
        ]),
    );

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    let stdout = result.stdout_tail.unwrap();
    assert!(
        stdout.contains("SPEC=from_spec"),
        "should contain spec env var, got: {stdout}"
    );
    assert!(
        stdout.contains("CTX=from_context"),
        "should contain context env var, got: {stdout}"
    );
    assert!(
        stdout.contains("OVERRIDE=context_value"),
        "RunContext should override spec env, got: {stdout}"
    );
}

#[tokio::test]
async fn stderr_captured_separately() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "sh".into(),
        args: vec!["-c".into(), "echo out_msg; echo err_msg >&2".into()],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    let stdout = result.stdout_tail.unwrap();
    let stderr = result.stderr_tail.unwrap();
    assert!(stdout.contains("out_msg"), "stdout: {stdout}");
    assert!(!stdout.contains("err_msg"), "stdout should not contain stderr: {stdout}");
    assert!(stderr.contains("err_msg"), "stderr: {stderr}");
    assert!(!stderr.contains("out_msg"), "stderr should not contain stdout: {stderr}");
}

#[tokio::test]
async fn working_dir_changes_cwd() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "pwd".into(),
        args: vec![],
        env: Default::default(),
        working_dir: Some("/tmp".into()),
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    let stdout = result.stdout_tail.unwrap();
    assert!(
        stdout.trim().ends_with("/tmp"),
        "expected cwd /tmp, got: {stdout}"
    );
}

#[tokio::test]
async fn inherit_env_false_clears_environment() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "sh".into(),
        args: vec!["-c".into(), "echo HOME=$HOME".into()],
        env: Default::default(),
        working_dir: None,
        inherit_env: false,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    let stdout = result.stdout_tail.unwrap_or_default();
    assert!(
        stdout.contains("HOME=\n") || stdout.trim() == "HOME=",
        "HOME should be empty with inherit_env=false, got: {stdout}"
    );
}

#[tokio::test]
async fn command_not_found_returns_spawn_error() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "/nonexistent/binary/that/does/not/exist".into(),
        args: vec![],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await;

    assert!(
        matches!(result, Err(aithericon_executor_domain::ExecutorError::SpawnFailed(_))),
        "expected SpawnFailed, got: {:?}",
        result
    );
}

#[tokio::test]
async fn multiple_args_passed_correctly() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "echo".into(),
        args: vec!["a".into(), "b".into(), "c".into()],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result.stdout_tail.as_deref(), Some("a b c\n"));
}

#[tokio::test]
async fn empty_output_returns_none() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "true".into(),
        args: vec![],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(result.stdout_tail.is_none());
    assert!(result.stderr_tail.is_none());
}

#[tokio::test]
async fn duration_is_nonzero() {
    let backend = ProcessBackend::new();
    let spec = ProcessConfig {
        command: "echo".into(),
        args: vec!["hi".into()],
        env: Default::default(),
        working_dir: None,
        inherit_env: true,
    }
    .into_spec();
    let ctx = make_process_run_context(spec, Duration::from_secs(10));

    let result = backend
        .execute(&ctx, noop_callback(), None, CancellationToken::new())
        .await
        .unwrap();

    assert!(result.duration > Duration::ZERO);
}
