//! Integration tests for the `aithericon` CLI binary.
//!
//! Each test starts a real IPC sidecar, runs the CLI as a subprocess, and
//! inspects the `SidecarResult` returned by the sidecar. No Docker or NATS
//! required.
//!
//! **Key constraint**: the sidecar accepts exactly ONE connection, then shuts
//! down. Each test runs ONE CLI command per sidecar instance.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::OnceLock;

use aithericon_executor_worker::{start_ipc_sidecar, SidecarLogConfig, SidecarResult};
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build the CLI binary once and cache the path.
fn cli_binary_path() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let status = Command::new("cargo")
            .args(["build", "-p", "aithericon-executor-cli"])
            .status()
            .expect("failed to run cargo build");
        assert!(status.success(), "cargo build failed");

        let output = Command::new("cargo")
            .args(["metadata", "--format-version=1", "--no-deps"])
            .output()
            .expect("cargo metadata failed");
        let meta: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("bad cargo metadata JSON");
        let target_dir = meta["target_directory"]
            .as_str()
            .expect("no target_directory");
        PathBuf::from(target_dir).join("debug").join("aithericon")
    })
}

/// Create a short temp dir for the socket to keep under the ~104-char limit.
fn make_short_temp_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("at-")
        .tempdir_in("/tmp")
        .expect("failed to create temp dir")
}

/// Start a sidecar listening on the given socket path.
async fn start_test_sidecar(socket: PathBuf, artifacts_dir: PathBuf) -> JoinHandle<SidecarResult> {
    start_ipc_sidecar(
        socket,
        "test-exec-id".into(),
        "test".into(),
        HashMap::new(),
        None,
        artifacts_dir,
        None,
        None,
        SidecarLogConfig::default(),
        tokio_util::sync::CancellationToken::new(),
        None,
    )
    .await
    .expect("failed to start sidecar")
}

/// Run the CLI binary with the given socket path and arguments.
/// Uses spawn_blocking to avoid blocking the tokio runtime.
async fn run_cli(socket: &str, args: &[&str]) -> Output {
    let bin = cli_binary_path().clone();
    let socket = socket.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        Command::new(bin)
            .arg("--socket")
            .arg(&socket)
            .args(&args)
            .env_remove("AITHERICON_IPC_SOCKET")
            .output()
            .expect("failed to run CLI")
    })
    .await
    .expect("spawn_blocking panicked")
}

/// Run the CLI binary with stdin piped.
async fn run_cli_with_stdin(socket: &str, args: &[&str], stdin_data: &[u8]) -> Output {
    let bin = cli_binary_path().clone();
    let socket = socket.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let stdin_data = stdin_data.to_vec();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut child = Command::new(bin)
            .arg("--socket")
            .arg(&socket)
            .args(&args)
            .env_remove("AITHERICON_IPC_SOCKET")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("failed to spawn CLI");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&stdin_data)
            .expect("failed to write stdin");
        child.wait_with_output().expect("failed to wait for CLI")
    })
    .await
    .expect("spawn_blocking panicked")
}

/// Run the CLI binary with NO socket (tests fallback/error paths).
async fn run_cli_no_socket(args: &[&str]) -> Output {
    let bin = cli_binary_path().clone();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        Command::new(bin)
            .args(&args)
            .env_remove("AITHERICON_IPC_SOCKET")
            .env_remove("AITHERICON_OUTPUTS_DIR")
            .env_remove("AITHERICON_INPUTS_DIR")
            .output()
            .expect("failed to run CLI")
    })
    .await
    .expect("spawn_blocking panicked")
}

/// Run the CLI binary with no socket but with specific env vars.
async fn run_cli_no_socket_with_env(args: &[&str], env: &[(&str, &str)]) -> Output {
    let bin = cli_binary_path().clone();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new(bin);
        cmd.args(&args).env_remove("AITHERICON_IPC_SOCKET");
        for (k, v) in &env {
            cmd.env(k, v);
        }
        cmd.output().expect("failed to run CLI")
    })
    .await
    .expect("spawn_blocking panicked")
}

/// Await the sidecar result with a timeout.
async fn await_sidecar(handle: JoinHandle<SidecarResult>) -> SidecarResult {
    tokio::time::timeout(std::time::Duration::from_secs(10), handle)
        .await
        .expect("sidecar timed out")
        .expect("sidecar panicked")
}

// ---------------------------------------------------------------------------
// Output command tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_output_set_json_value() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["output", "set", "result", r#"{"score":42}"#],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    assert_eq!(result.outputs["result"], serde_json::json!({"score": 42}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_output_set_raw_string() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["output", "set", "greeting", "--raw", "hello"],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    assert_eq!(result.outputs["greeting"], serde_json::json!("hello"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_output_set_numeric() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["output", "set", "accuracy", "0.95"],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    assert_eq!(result.outputs["accuracy"], serde_json::json!(0.95));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_output_set_invalid_json() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    // CLI connects first, then validates JSON client-side. Need sidecar so
    // connection succeeds, then the CLI returns InvalidArgument before sending RPC.
    let _handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["output", "set", "key", "not valid json"],
    )
    .await;
    assert_eq!(out.status.code(), Some(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_output_set_json_mode() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["--json", "output", "set", "key", "42"],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#""status":"ok""#), "stdout: {stdout}");

    let _ = await_sidecar(handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_output_set_fallback() {
    let tmp = make_short_temp_dir();
    let outputs_dir = tmp.path().join("outputs");

    let out = run_cli_no_socket_with_env(
        &["output", "set", "result", r#"{"score":42}"#],
        &[("AITHERICON_OUTPUTS_DIR", outputs_dir.to_str().unwrap())],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let content = std::fs::read_to_string(outputs_dir.join("result.json")).unwrap();
    assert_eq!(content, r#"{"score":42}"#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_output_set_fallback_raw() {
    let tmp = make_short_temp_dir();
    let outputs_dir = tmp.path().join("outputs");

    let out = run_cli_no_socket_with_env(
        &["output", "set", "greeting", "--raw", "hello"],
        &[("AITHERICON_OUTPUTS_DIR", outputs_dir.to_str().unwrap())],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let content = std::fs::read_to_string(outputs_dir.join("greeting.json")).unwrap();
    assert_eq!(content, r#""hello""#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_output_set_fallback_no_env() {
    let out = run_cli_no_socket(&["output", "set", "key", "42"]).await;
    assert_eq!(out.status.code(), Some(2));
}

// ---------------------------------------------------------------------------
// Progress command tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_progress_update_basic() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(socket.to_str().unwrap(), &["progress", "update", "0.5"]).await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let progress = result.progress.expect("no progress");
    assert!((progress.fraction - 0.5).abs() < 0.01);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_progress_with_message_and_steps() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &[
            "progress",
            "update",
            "0.75",
            "--message",
            "Training",
            "--step",
            "75",
            "--total-steps",
            "100",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let progress = result.progress.expect("no progress");
    assert!((progress.fraction - 0.75).abs() < 0.01);
    assert_eq!(progress.message.as_deref(), Some("Training"));
    assert_eq!(progress.current_step, 75);
    assert_eq!(progress.total_steps, 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_progress_fraction_one() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(socket.to_str().unwrap(), &["progress", "update", "1.0"]).await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let progress = result.progress.expect("no progress");
    assert!((progress.fraction - 1.0).abs() < 0.01);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_progress_fraction_negative() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    // Fraction validation happens after connect. Need a sidecar.
    let _handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["progress", "update", "--", "-0.1"],
    )
    .await;
    assert_eq!(out.status.code(), Some(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_progress_fraction_above_one() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let _handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(socket.to_str().unwrap(), &["progress", "update", "1.5"]).await;
    assert_eq!(out.status.code(), Some(3));
}

// ---------------------------------------------------------------------------
// Phase command tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_phase_define() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["phase", "define", "prep", "train", "eval"],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let progress = result.progress.expect("no progress");
    assert_eq!(progress.phases.len(), 3);
    assert_eq!(progress.phases[0].name, "prep");
    assert_eq!(progress.phases[1].name, "train");
    assert_eq!(progress.phases[2].name, "eval");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_phase_update_not_found() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["phase", "update", "nonexistent", "running"],
    )
    .await;
    assert_eq!(out.status.code(), Some(1));

    let _ = await_sidecar(handle).await;
}

// ---------------------------------------------------------------------------
// Log command tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_info() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(socket.to_str().unwrap(), &["log", "info", "hello world"]).await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let summary = result.log_summary.expect("no log summary");
    assert_eq!(summary.total_entries, 1);
    assert_eq!(*summary.count_by_level.get("info").unwrap_or(&0), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_error() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["log", "error", "something broke"],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let summary = result.log_summary.expect("no log summary");
    assert!(!summary.recent_errors.is_empty());
    assert_eq!(summary.recent_errors[0].message, "something broke");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_with_fields() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["log", "error", "fail", "--field", "key=val"],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let summary = result.log_summary.expect("no log summary");
    assert_eq!(summary.recent_errors[0].fields.get("key").unwrap(), "val");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_log_debug() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(socket.to_str().unwrap(), &["log", "debug", "debug msg"]).await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let summary = result.log_summary.expect("no log summary");
    assert_eq!(*summary.count_by_level.get("debug").unwrap_or(&0), 1);
}

// ---------------------------------------------------------------------------
// Metric command tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_metric_log_scalar() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["metric", "log", "train/loss", "0.05"],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let summary = result.metric_summary.expect("no metric summary");
    assert_eq!(summary.total_points, 1);
    assert!(summary.latest_values.contains_key("train/loss"));
    assert!((summary.latest_values["train/loss"] - 0.05).abs() < f64::EPSILON);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_metric_with_step_and_type() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &[
            "metric",
            "log",
            "cpu_usage",
            "0.85",
            "--step",
            "100",
            "--type",
            "gauge",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let summary = result.metric_summary.expect("no metric summary");
    assert_eq!(summary.total_points, 1);
    assert!(summary.metric_names.contains(&"cpu_usage".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_metric_with_labels() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["metric", "log", "latency", "50.0", "--label", "env=prod"],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let summary = result.metric_summary.expect("no metric summary");
    assert_eq!(summary.total_points, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_metric_batch_stdin() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let batch = r#"[{"name":"loss","value":0.1},{"name":"acc","value":0.9}]"#;
    let out = run_cli_with_stdin(
        socket.to_str().unwrap(),
        &["metric", "batch"],
        batch.as_bytes(),
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    let summary = result.metric_summary.expect("no metric summary");
    assert_eq!(summary.total_points, 2);
    assert!(summary.metric_names.contains(&"loss".to_string()));
    assert!(summary.metric_names.contains(&"acc".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_metric_batch_invalid_json() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let _handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli_with_stdin(
        socket.to_str().unwrap(),
        &["metric", "batch"],
        b"not valid json",
    )
    .await;
    assert_eq!(out.status.code(), Some(4));
}

// ---------------------------------------------------------------------------
// Artifact command tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_artifact_log() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let artifact_file = tmp.path().join("model.pt");
    std::fs::write(&artifact_file, "fake model data").unwrap();

    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &[
            "artifact",
            "log",
            artifact_file.to_str().unwrap(),
            "--name",
            "my-model",
            "--category",
            "model",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    assert_eq!(result.artifacts.len(), 1);
    assert_eq!(result.artifacts[0].name, "my-model");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_artifact_defaults_name() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let artifact_file = tmp.path().join("weights.bin");
    std::fs::write(&artifact_file, "data").unwrap();

    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &["artifact", "log", artifact_file.to_str().unwrap()],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    assert_eq!(result.artifacts[0].name, "weights.bin");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_artifact_with_metadata() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let artifact_file = tmp.path().join("data.csv");
    std::fs::write(&artifact_file, "col1,col2").unwrap();

    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &[
            "artifact",
            "log",
            artifact_file.to_str().unwrap(),
            "--metadata",
            "k=v",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    assert_eq!(result.artifacts[0].metadata.get("k").unwrap(), "v");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_artifact_with_mime_type() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let artifact_file = tmp.path().join("chart.png");
    std::fs::write(&artifact_file, "fake png").unwrap();

    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(
        socket.to_str().unwrap(),
        &[
            "artifact",
            "log",
            artifact_file.to_str().unwrap(),
            "--mime-type",
            "image/png",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let result = await_sidecar(handle).await;
    assert_eq!(result.artifacts[0].mime_type.as_deref(), Some("image/png"));
}

// ---------------------------------------------------------------------------
// Health / Shutdown tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_health_check() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(socket.to_str().unwrap(), &["health"]).await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = await_sidecar(handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shutdown_default() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(socket.to_str().unwrap(), &["shutdown"]).await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = await_sidecar(handle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shutdown_with_exit_code() {
    let tmp = make_short_temp_dir();
    let socket = tmp.path().join("ipc.sock");
    let handle = start_test_sidecar(socket.clone(), tmp.path().to_path_buf()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let out = run_cli(socket.to_str().unwrap(), &["shutdown", "--exit-code", "42"]).await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let _ = await_sidecar(handle).await;
}

// ---------------------------------------------------------------------------
// Error path tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_no_socket_error() {
    let out = run_cli_no_socket(&["health"]).await;
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no socket path"), "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_connection_refused() {
    let out = run_cli("/tmp/nonexistent-socket-12345.sock", &["health"]).await;
    assert_eq!(out.status.code(), Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_json_mode_error() {
    let out = run_cli_no_socket(&["--json", "health"]).await;
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(r#""status":"error""#), "stderr: {stderr}");
}

// ---------------------------------------------------------------------------
// Inputs tests (no sidecar needed)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_inputs_list() {
    let tmp = make_short_temp_dir();
    let inputs_dir = tmp.path().join("inputs");
    std::fs::create_dir(&inputs_dir).unwrap();
    std::fs::write(inputs_dir.join("alpha.json"), "{}").unwrap();
    std::fs::write(inputs_dir.join("beta.txt"), "hello").unwrap();

    let out = run_cli_no_socket_with_env(
        &["inputs", "list"],
        &[("AITHERICON_INPUTS_DIR", inputs_dir.to_str().unwrap())],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("alpha.json"));
    assert!(stdout.contains("beta.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_inputs_list_json() {
    let tmp = make_short_temp_dir();
    let inputs_dir = tmp.path().join("inputs");
    std::fs::create_dir(&inputs_dir).unwrap();
    std::fs::write(inputs_dir.join("data.json"), r#"{"x":1}"#).unwrap();

    let out = run_cli_no_socket_with_env(
        &["--json", "inputs", "list"],
        &[("AITHERICON_INPUTS_DIR", inputs_dir.to_str().unwrap())],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    // Output may contain both the JSON array and {"status":"ok"} on separate lines.
    let first_line = stdout.lines().next().expect("no output");
    let names: Vec<String> = serde_json::from_str(first_line).expect("not JSON array");
    assert!(names.contains(&"data.json".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_inputs_get() {
    let tmp = make_short_temp_dir();
    let inputs_dir = tmp.path().join("inputs");
    std::fs::create_dir(&inputs_dir).unwrap();
    std::fs::write(inputs_dir.join("config.json"), r#"{"lr":0.01}"#).unwrap();

    let out = run_cli_no_socket_with_env(
        &["inputs", "get", "config.json"],
        &[("AITHERICON_INPUTS_DIR", inputs_dir.to_str().unwrap())],
    )
    .await;
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("lr"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_inputs_get_not_found() {
    let tmp = make_short_temp_dir();
    let inputs_dir = tmp.path().join("inputs");
    std::fs::create_dir(&inputs_dir).unwrap();

    let out = run_cli_no_socket_with_env(
        &["inputs", "get", "missing.json"],
        &[("AITHERICON_INPUTS_DIR", inputs_dir.to_str().unwrap())],
    )
    .await;
    assert_eq!(out.status.code(), Some(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_inputs_no_env() {
    let out = run_cli_no_socket(&["inputs", "list"]).await;
    assert_eq!(out.status.code(), Some(3));
}
