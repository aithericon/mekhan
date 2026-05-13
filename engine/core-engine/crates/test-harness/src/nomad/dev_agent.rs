//! Starts and manages a `nomad agent -dev` process for integration tests.

use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::sync::OnceCell;

/// Default address for the dev agent.
pub const NOMAD_DEV_ADDR: &str = "http://127.0.0.1:4646";

/// Handle to a running `nomad agent -dev` process.
///
/// The agent is killed when this struct is dropped.
pub struct NomadDevAgent {
    child: Option<Child>,
    pub addr: String,
}

impl NomadDevAgent {
    /// Start a new `nomad agent -dev` process.
    ///
    /// Waits for the agent to become healthy before returning.
    fn start() -> Result<Self, String> {
        // First check if one is already running
        if is_nomad_healthy_sync(NOMAD_DEV_ADDR) {
            tracing::info!(addr = NOMAD_DEV_ADDR, "Nomad dev agent already running");
            return Ok(Self {
                child: None,
                addr: NOMAD_DEV_ADDR.to_string(),
            });
        }

        // Ensure nomad binary exists
        let nomad_path = which_nomad().ok_or_else(|| {
            "nomad binary not found on PATH. Install from https://developer.hashicorp.com/nomad/install".to_string()
        })?;

        tracing::info!(path = %nomad_path, "Starting nomad agent -dev");

        // Write a temp config to override CPU detection (broken on macOS)
        // and enable raw_exec driver.
        let config_dir = std::env::temp_dir().join("petri-nomad-test");
        std::fs::create_dir_all(&config_dir)
            .map_err(|e| format!("Failed to create config dir: {}", e))?;
        let config_path = config_dir.join("dev.hcl");
        std::fs::write(
            &config_path,
            r#"
client {
  cpu_total_compute = 10000
}
"#,
        )
        .map_err(|e| format!("Failed to write nomad config: {}", e))?;

        let child = Command::new(&nomad_path)
            .args([
                "agent",
                "-dev",
                "-log-level",
                "WARN",
                "-config",
                config_path.to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn nomad agent -dev: {}", e))?;

        let agent = Self {
            child: Some(child),
            addr: NOMAD_DEV_ADDR.to_string(),
        };

        // Wait for agent to become healthy
        wait_for_healthy_sync(NOMAD_DEV_ADDR, Duration::from_secs(30))?;

        tracing::info!(addr = NOMAD_DEV_ADDR, "Nomad dev agent is ready");
        Ok(agent)
    }
}

impl Drop for NomadDevAgent {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child {
            tracing::info!(pid = child.id(), "Stopping nomad dev agent");
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Global shared dev agent (one per test binary).
static SHARED_NOMAD: OnceCell<NomadDevAgent> = OnceCell::const_new();

/// Ensure a Nomad dev agent is running, starting one if needed.
///
/// Returns the HTTP address (e.g., `"http://127.0.0.1:4646"`).
/// The agent is shared across all tests and killed when the process exits.
///
/// # Panics
/// Panics if the nomad binary is not found or the agent fails to start.
pub async fn ensure_nomad_dev() -> &'static str {
    &SHARED_NOMAD
        .get_or_init(|| async { NomadDevAgent::start().expect("Failed to start nomad dev agent") })
        .await
        .addr
}

/// Check if nomad binary is on PATH.
fn which_nomad() -> Option<String> {
    let output = Command::new("which").arg("nomad").output().ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Synchronous health check against the Nomad HTTP API.
fn is_nomad_healthy_sync(addr: &str) -> bool {
    let url = format!("{}/v1/status/leader", addr);
    match std::net::TcpStream::connect_timeout(
        &"127.0.0.1:4646".parse().unwrap(),
        Duration::from_secs(1),
    ) {
        Ok(_) => {
            // TCP connected, now check HTTP
            let output = Command::new("curl")
                .args(["-sf", "--max-time", "2", &url])
                .output();
            matches!(output, Ok(o) if o.status.success())
        }
        Err(_) => false,
    }
}

/// Wait for nomad to become healthy, polling every 500ms.
fn wait_for_healthy_sync(addr: &str, timeout: Duration) -> Result<(), String> {
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(500);

    loop {
        if is_nomad_healthy_sync(addr) {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(format!(
                "Nomad agent at {} did not become healthy within {:?}",
                addr, timeout
            ));
        }
        std::thread::sleep(poll_interval);
    }
}

/// Purge a job and all its dispatched children to free resources.
///
/// Queries all jobs, finds any whose ID starts with `job_id` (parent + dispatches),
/// and purges them.
fn purge_job_tree(addr: &str, job_id: &str) {
    // List all jobs matching this prefix
    let output = Command::new("curl")
        .args([
            "-sf",
            "--max-time",
            "3",
            &format!("{}/v1/jobs?prefix={}", addr, job_id),
        ])
        .output();

    let jobs: Vec<String> = match output {
        Ok(o) if o.status.success() => {
            let body = String::from_utf8_lossy(&o.stdout);
            // Parse JSON array, extract IDs
            serde_json::from_str::<Vec<serde_json::Value>>(&body)
                .unwrap_or_default()
                .iter()
                .filter_map(|j| j.get("ID").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .collect()
        }
        _ => Vec::new(),
    };

    for id in &jobs {
        let _ = Command::new("curl")
            .args([
                "-sf",
                "-X",
                "DELETE",
                "--max-time",
                "3",
                &format!("{}/v1/job/{}?purge=true", addr, id),
            ])
            .output();
    }

    if !jobs.is_empty() {
        tracing::debug!(count = jobs.len(), job_id, "Purged stale jobs");
    }
}

/// Register a parameterized batch job template for testing.
///
/// Creates a simple job that runs a command and exits.
/// Any existing job with this ID is purged first to free resources.
pub async fn register_test_job_template(
    addr: &str,
    job_id: &str,
    command: &str,
    args: &[&str],
) -> Result<(), String> {
    // Purge any existing job (and its dispatches) to free resources.
    purge_job_tree(addr, job_id);

    let args_json: Vec<String> = args.iter().map(|a| format!("\"{}\"", a)).collect();
    let args_str = args_json.join(", ");

    let job_hcl = format!(
        r#"{{
  "Job": {{
    "ID": "{job_id}",
    "Name": "{job_id}",
    "Type": "batch",
    "Datacenters": ["dc1"],
    "ParameterizedJob": {{
      "Payload": "optional",
      "MetaRequired": [],
      "MetaOptional": ["petri_net_id", "petri_place", "petri_signal_key", "petri_signal_running", "petri_signal_completed", "petri_signal_failed"]
    }},
    "TaskGroups": [{{
      "Name": "main",
      "Count": 1,
      "RestartPolicy": {{
        "Attempts": 0,
        "Mode": "fail"
      }},
      "ReschedulePolicy": {{
        "Attempts": 0
      }},
      "Tasks": [{{
        "Name": "petri-worker",
        "Driver": "raw_exec",
        "Config": {{
          "command": "{command}",
          "args": [{args_str}]
        }},
        "Resources": {{
          "CPU": 1,
          "MemoryMB": 32
        }}
      }}]
    }}]
  }}
}}"#
    );

    let url = format!("{}/v1/jobs", addr);
    let output = Command::new("curl")
        .args(["-sf", "-X", "POST", &url, "-d", &job_hcl, "--max-time", "5"])
        .output()
        .map_err(|e| format!("Failed to register job template: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "Failed to register job template {}: {} {}",
            job_id, stdout, stderr
        ));
    }

    tracing::info!(job_id = %job_id, "Registered test job template");
    Ok(())
}
