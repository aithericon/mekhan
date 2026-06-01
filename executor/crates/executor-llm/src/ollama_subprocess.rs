//! Managed Ollama subprocess lifecycle.
//!
//! Ports the subprocess-management logic from
//! `cloud-layer/cloud-layer-pool-ollama/src/ollama.rs` (sub-phase 2.2 wave,
//! slice B3) into the executor-llm crate. The cloud-layer pool crate is
//! scheduled for deletion at the close of sub-phase 2.2; this module is the
//! executor's replacement-of-record for that responsibility.
//!
//! ## What it does
//!
//! - Spawns `ollama serve` on a configurable port (default 11436) with
//!   captured stdout/stderr.
//! - Waits for readiness via polling `/api/tags`.
//! - Provides a runtime health check (`/api/tags`) and a clean shutdown
//!   (graceful SIGTERM, then forced kill on timeout).
//! - Provides model warm-up (`POST /api/pull`) and removal
//!   (`DELETE /api/delete`) via Ollama's HTTP API.
//!
//! ## What it does NOT do
//!
//! - This module is the **lifecycle** surface. The per-request completion
//!   path lives in `adapters/ollama.rs` and uses `reqwest` directly against
//!   the base URL — same as the legacy pool crate. There is no shared
//!   `OllamaManager`-style routing layer here; the executor's LlmBackend
//!   already routes per-request via the adapter, and Q7=A keeps the
//!   executor's LlmBackend as a distinct cluster-status row from the
//!   compute-agent — no collapsing.
//! - Crash-restart supervision is intentionally **NOT** ported in this slice.
//!   The legacy pool's `supervise()` background task is a safety net for
//!   long-running daemons; the executor's lifecycle model is per-job
//!   start/stop, where caller code decides whether to restart. Adding the
//!   supervisor here would silently swallow process-exit signals that the
//!   executor needs to surface to its job runtime. If a future slice
//!   requires daemon-mode supervision, that's a separate ticket.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time;
use tracing::{info, warn};

use crate::port::LlmError;

const READINESS_POLL_INTERVAL_MS: u64 = 500;
const READINESS_TIMEOUT_SECS: u64 = 30;
const SHUTDOWN_GRACE_SECS: u64 = 10;
const DEFAULT_OLLAMA_PORT: u16 = 11436;

/// Configuration for spawning a managed Ollama subprocess.
#[derive(Debug, Clone)]
pub struct OllamaSubprocessConfig {
    /// TCP port for `ollama serve` to bind. Default 11436 (one above the
    /// system-Ollama default 11434, and one above the clinic dev value
    /// 11435).
    pub port: u16,

    /// Optional path to the `ollama` binary. When `None`, uses `ollama`
    /// from PATH. Useful for tests that bring their own binary and for
    /// production deployments that pin a specific binary version.
    pub binary_path: Option<String>,

    /// Readiness timeout (seconds). Defaults to `READINESS_TIMEOUT_SECS`.
    pub readiness_timeout_secs: u64,
}

impl Default for OllamaSubprocessConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_OLLAMA_PORT,
            binary_path: None,
            readiness_timeout_secs: READINESS_TIMEOUT_SECS,
        }
    }
}

/// A managed Ollama subprocess. Holds the `Child` handle and metadata
/// (binding port, base URL). Drop semantics: dropping without calling
/// `stop()` leaves the child process running in the background — call
/// `stop()` to guarantee shutdown.
pub struct OllamaSubprocess {
    port: u16,
    child: Arc<Mutex<Option<Child>>>,
}

impl OllamaSubprocess {
    /// Spawn `ollama serve` and wait for readiness.
    pub async fn start(config: &OllamaSubprocessConfig) -> Result<Self, LlmError> {
        let binary = config.binary_path.as_deref().unwrap_or("ollama");
        let host = format!("0.0.0.0:{}", config.port);

        info!(
            binary = binary,
            port = config.port,
            "Spawning managed Ollama subprocess"
        );

        let mut child = Command::new(binary)
            .arg("serve")
            .env("OLLAMA_HOST", &host)
            // Capture stdout / stderr so they can be drained for logs (spec).
            // Spawned drain tasks below forward each line to tracing — without
            // them, Ollama's writes fill the pipe buffer (~16KB initial, grows
            // to ~64KB max on macOS / 64KB hard cap on Linux) within minutes
            // of routine heartbeat probes, after which Ollama's next stdout
            // write() blocks indefinitely and the entire Ollama subprocess
            // appears unresponsive. Root cause of the Session N+3 Wave 4
            // Degraded-health observation; empirically confirmed via
            // `lsof -p <ollama_pid>` showing the stdout pipe at 65536 bytes
            // (the macOS pipe max) after ~4 minutes of routine heartbeats.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Do NOT kill on drop — let the operator decide shutdown
            // semantics. The legacy pool used `kill_on_drop(false)` for the
            // same reason; matching that here for parity.
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| {
                LlmError::Config(format!(
                    "failed to spawn ollama subprocess (binary={binary}): {e}"
                ))
            })?;

        // Drain stdout + stderr. Each task reads line-by-line and forwards
        // to tracing at info level so operators see Ollama's logs through
        // the executor's tracing pipeline (RUST_LOG=info). When the child
        // exits, the pipes close → drain tasks see EOF → exit cleanly.
        // Handles are deliberately not retained: tokio tasks aren't
        // cancelled on JoinHandle drop, and EOF-on-pipe-close is the natural
        // termination signal.
        if let Some(stdout) = child.stdout.take() {
            let port = config.port;
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            tracing::info!(
                                target: "ollama_subprocess",
                                port,
                                "ollama[stdout]: {}",
                                line
                            );
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::debug!(
                                target: "ollama_subprocess",
                                port,
                                error = %e,
                                "ollama stdout drain error; ending task"
                            );
                            break;
                        }
                    }
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let port = config.port;
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            tracing::info!(
                                target: "ollama_subprocess",
                                port,
                                "ollama[stderr]: {}",
                                line
                            );
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::debug!(
                                target: "ollama_subprocess",
                                port,
                                error = %e,
                                "ollama stderr drain error; ending task"
                            );
                            break;
                        }
                    }
                }
            });
        }

        let subprocess = Self {
            port: config.port,
            child: Arc::new(Mutex::new(Some(child))),
        };

        subprocess
            .wait_for_ready(config.readiness_timeout_secs)
            .await?;

        Ok(subprocess)
    }

    /// Base URL of the managed Ollama HTTP server.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Configured port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Returns `true` when the running subprocess responds successfully to
    /// `GET /api/tags`. Returns `false` otherwise — including when the
    /// subprocess has been shut down (no stale cached state).
    pub async fn health_check(&self) -> bool {
        // If the child has been taken (stop() called), the subprocess is
        // definitionally not healthy.
        if self.child.lock().await.is_none() {
            return false;
        }

        let url = format!("{}/api/tags", self.base_url());
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        match client.get(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Pre-warm a model into Ollama's runtime via `POST /api/pull`. The
    /// `name` is the Ollama model identifier (e.g. `qwen2.5:3b`). Idempotent
    /// per Ollama's pull semantics.
    pub async fn model_load(&self, name: &str) -> Result<(), LlmError> {
        let url = format!("{}/api/pull", self.base_url());
        let body = serde_json::json!({ "name": name, "stream": false });
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("ollama /api/pull failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "ollama /api/pull returned {status}: {text}"
            )));
        }
        Ok(())
    }

    /// Remove a model from Ollama's runtime via `DELETE /api/delete`.
    pub async fn model_unload(&self, name: &str) -> Result<(), LlmError> {
        let url = format!("{}/api/delete", self.base_url());
        let body = serde_json::json!({ "name": name });
        let client = reqwest::Client::new();
        let resp = client
            .delete(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Api(format!("ollama /api/delete failed: {e}")))?;

        // 404 is acceptable — the model wasn't present, which is the
        // post-condition we want.
        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api(format!(
                "ollama /api/delete returned {status}: {text}"
            )));
        }
        Ok(())
    }

    /// Shut down the subprocess. Sends a graceful kill via tokio's
    /// `Child::kill` (which delivers SIGKILL on Unix; Ollama responds to
    /// SIGTERM more cleanly, but tokio's portable API uses SIGKILL — the
    /// legacy pool crate uses the same call). Waits for the process to exit
    /// or the grace period to elapse, whichever comes first.
    ///
    /// After this returns, `health_check()` returns `false`.
    pub async fn stop(self) -> Result<(), LlmError> {
        let mut guard = self.child.lock().await;
        let mut child = match guard.take() {
            Some(c) => c,
            None => return Ok(()),
        };

        // Best-effort graceful shutdown: send kill, wait up to grace period.
        if let Err(e) = child.start_kill() {
            warn!(error = %e, "failed to send kill signal to Ollama subprocess");
        }

        let wait_result =
            time::timeout(Duration::from_secs(SHUTDOWN_GRACE_SECS), child.wait()).await;

        match wait_result {
            Ok(Ok(status)) => {
                info!(?status, "Managed Ollama subprocess exited");
                Ok(())
            }
            Ok(Err(e)) => Err(LlmError::Api(format!(
                "wait on Ollama subprocess failed: {e}"
            ))),
            Err(_) => {
                warn!(
                    grace_secs = SHUTDOWN_GRACE_SECS,
                    "Ollama subprocess did not exit within grace period"
                );
                Err(LlmError::Api(format!(
                    "ollama subprocess did not exit within {SHUTDOWN_GRACE_SECS}s"
                )))
            }
        }
    }

    async fn wait_for_ready(&self, timeout_secs: u64) -> Result<(), LlmError> {
        let url = format!("{}/api/tags", self.base_url());
        let client = reqwest::Client::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(LlmError::Api(format!(
                    "ollama subprocess did not become ready within {timeout_secs}s"
                )));
            }
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!(port = self.port, "Ollama subprocess ready");
                    return Ok(());
                }
                _ => {
                    time::sleep(Duration::from_millis(READINESS_POLL_INTERVAL_MS)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_uses_documented_port() {
        // Honest-absence: default MUST NOT collide with the system-Ollama
        // default (11434) or the clinic dev value (11435).
        let c = OllamaSubprocessConfig::default();
        assert_eq!(c.port, DEFAULT_OLLAMA_PORT);
        assert_ne!(c.port, 11434, "must not collide with system Ollama");
        assert_ne!(c.port, 11435, "must not collide with clinic dev Ollama");
        assert!(c.binary_path.is_none());
        assert_eq!(c.readiness_timeout_secs, READINESS_TIMEOUT_SECS);
    }

    #[tokio::test]
    async fn start_fails_with_missing_binary() {
        // Honest-absence: start with a deliberately non-existent binary
        // must return Err — NEVER a success with a stale handle.
        let cfg = OllamaSubprocessConfig {
            port: 0, // would race even if spawn worked, but spawn won't
            binary_path: Some("/nonexistent/path/to/ollama-binary-xyz".to_string()),
            readiness_timeout_secs: 1,
        };
        let result = OllamaSubprocess::start(&cfg).await;
        assert!(result.is_err(), "start with missing binary must fail");
    }
}
