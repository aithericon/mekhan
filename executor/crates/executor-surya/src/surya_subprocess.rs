//! Managed Surya OCR Python subprocess lifecycle.
//!
//! Pattern: mirrors [`aithericon_executor_llm::OllamaSubprocess`] for
//! Ollama. Spawns `<venv-python> -m surya_pool_server --port <port>` on a
//! configurable port (default 7160), waits for readiness via `GET /health`
//! polling, exposes a health probe, and shuts down cleanly on `stop()`.
//!
//! ## Why HTTP (not stdio JSON-RPC)
//!
//! Mirrors executor-llm's Ollama HTTP shape exactly: spawn a long-running
//! HTTP server subprocess + per-request HTTP client. Same wire-shape
//! (base64-over-JSON envelope) as the legacy `online-clinic/ocr/` Python
//! sidecar — inherited as known-good baseline per the slice plan's
//! Decision B disposition (bundle Python wrapper inside crate).
//!
//! ## Critical: stdio drain from spawn time
//!
//! Per the slice #69 Item 4 lesson (mekhan commit `1d896f0`): when
//! spawning a long-running subprocess with `Stdio::piped()`, drain
//! tasks MUST be launched immediately. Without them, uvicorn's
//! access-log writes fill the pipe buffer (~16-64KB) within minutes
//! of routine probes, after which the subprocess's next write
//! blocks indefinitely and the entire server appears unresponsive.
//!
//! This module spawns two tokio drain tasks (stdout + stderr) from
//! the spawn moment, forwarding each line to tracing with
//! `target = "surya_subprocess"`. Handles are intentionally not
//! retained: tokio tasks aren't cancelled on JoinHandle drop, and
//! EOF-on-pipe-close (after child exit) is the natural termination
//! signal.
//!
//! ## Why 120s default readiness timeout
//!
//! Surya's first invocation downloads ~1-2GB of model weights from
//! HuggingFace (DetectionPredictor + FoundationPredictor +
//! RecognitionPredictor + LayoutPredictor). Cold startup can exceed
//! the 30s timeout that executor-llm's Ollama path uses. Once the
//! models are cached in the venv's `~/.cache/huggingface/hub/`,
//! subsequent startups are ~10-30s.
//!
//! Operator pre-warm via `just surya-venv-setup` + a one-shot
//! invocation is documented as the production path; 120s is the safe
//! default for first-time spawns in dev/cert.
//!
//! ## License isolation (workstream #71)
//!
//! Surya is GPL-3.0 (code) + modified OpenRAIL-M (weights, with $2M
//! revenue/funding commercial-license threshold). Subprocess
//! process-isolation cleanly contains GPL-3.0 — the executor binary
//! itself stays Apache-2.0 because Surya code never links into our
//! process address space. This module owns the license boundary;
//! any future change that pulls Surya into the Rust process (FFI,
//! pyo3, embedded Python) would entangle the binary with GPL-3.0
//! and must be HALTed-and-surfaced.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time;
use tracing::{info, warn};

use crate::port::OcrError;

const READINESS_POLL_INTERVAL_MS: u64 = 500;
/// Default readiness timeout. Surya's first-init model download takes
/// 5-15 min cold (~1-2GB across detection/foundation/recognition/layout
/// predictors). Operators MUST pre-warm via `just surya-venv-setup` +
/// one-shot invocation before relying on the default; cert harnesses
/// honest-skip when the venv is absent.
const READINESS_TIMEOUT_SECS: u64 = 120;
const SHUTDOWN_GRACE_SECS: u64 = 10;
/// Default Surya HTTP port. Adjacent to compute-agent control 7100;
/// distinct from legacy `ocr/` 5050 (which stays alive during
/// transition for non-executor-routed callers; legacy deletion is a
/// separate slice per `feedback_delete_superseded_code`).
const DEFAULT_SURYA_PORT: u16 = 7160;

/// Default venv directory name. Created relative to the just-recipe's
/// CWD (typically `mekhan/executor/`) at `.dev/surya-venv/`. The
/// runtime spawn resolves the venv via [`SuryaSubprocessConfig::venv_path`]
/// (env-overridable to an absolute path).
const DEFAULT_VENV_DIR: &str = ".dev/surya-venv";

/// Configuration for spawning a managed Surya subprocess.
///
/// All fields are env-overridable per the slice plan's Item 0 surface
/// report. Env-reading lives in caller code; this struct takes plain
/// values to keep tests deterministic (per
/// `feedback_test_discipline` — no `std::env::set_var` in test bodies).
#[derive(Debug, Clone)]
pub struct SuryaSubprocessConfig {
    /// TCP port for the Surya HTTP server to bind. Default 7160.
    pub port: u16,

    /// Path to the venv directory containing `bin/python` (Unix) with
    /// the bundled `surya_pool_server` package installed via
    /// `uv pip install -e <crate>/python`. When `None`, defaults to
    /// `.dev/surya-venv/` resolved relative to the process's CWD —
    /// matches the `just surya-venv-setup` convention.
    pub venv_path: Option<PathBuf>,

    /// Optional explicit Python binary override (absolute path). When
    /// `None`, the spawn path uses `<venv_path>/bin/python`. Provided
    /// for tests + alternative deployment runners.
    pub python_binary: Option<PathBuf>,

    /// Optional device hint forwarded to the wrapper as an env var
    /// (`SURYA_DEVICE`). Accepts `"cpu"`, `"cuda"`, `"mps"`, `"auto"`.
    /// When `None`, the wrapper's own auto-detection runs (PyTorch
    /// `cuda` → `mps` → `cpu` priority).
    pub device: Option<String>,

    /// Readiness timeout (seconds). Defaults to
    /// [`READINESS_TIMEOUT_SECS`] (120 — cold-init safe).
    pub readiness_timeout_secs: u64,
}

impl Default for SuryaSubprocessConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_SURYA_PORT,
            venv_path: None,
            python_binary: None,
            device: None,
            readiness_timeout_secs: READINESS_TIMEOUT_SECS,
        }
    }
}

impl SuryaSubprocessConfig {
    /// Resolve the venv's Python binary. Honors `python_binary` override
    /// first, then `<venv_path>/bin/python`, then `<default-venv>/bin/python`.
    /// Returns `Err` if the resolved path doesn't exist on disk so callers
    /// surface "did you run `just surya-venv-setup`?" clearly rather than
    /// failing inside `Command::spawn` with an opaque OS error.
    pub fn resolve_python(&self) -> Result<PathBuf, OcrError> {
        if let Some(explicit) = &self.python_binary {
            if !explicit.exists() {
                return Err(OcrError::Config(format!(
                    "AITHERICON_EXECUTOR_SURYA_BINARY_PATH points at {} which does not exist",
                    explicit.display()
                )));
            }
            return Ok(explicit.clone());
        }
        let venv = self
            .venv_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_VENV_DIR));
        let python = venv.join("bin").join("python");
        if !python.exists() {
            return Err(OcrError::Config(format!(
                "Surya venv python not found at {}. Run `just surya-venv-setup` from the executor workspace, \
                 or set AITHERICON_EXECUTOR_SURYA_VENV_PATH to an absolute venv path.",
                python.display()
            )));
        }
        Ok(python)
    }
}

/// A managed Surya Python subprocess. Holds the `Child` handle and
/// metadata (port, base URL). Drop semantics: dropping without calling
/// `stop()` leaves the child process running in the background per
/// `kill_on_drop(false)` — call `stop()` to guarantee shutdown.
pub struct SuryaSubprocess {
    port: u16,
    child: Arc<Mutex<Option<Child>>>,
}

impl SuryaSubprocess {
    /// Spawn the Surya subprocess and wait for readiness.
    pub async fn start(config: &SuryaSubprocessConfig) -> Result<Self, OcrError> {
        let python = config.resolve_python()?;
        let port_arg = config.port.to_string();

        info!(
            python = %python.display(),
            port = config.port,
            "Spawning managed Surya subprocess"
        );

        let mut cmd = Command::new(&python);
        cmd.arg("-m")
            .arg("surya_pool_server")
            .arg("--port")
            .arg(&port_arg)
            // Pipe stdout/stderr so the drain tasks below can forward
            // line-by-line to tracing. Without the drain the pipe buffer
            // fills (~16-64KB on macOS/Linux) within minutes of routine
            // probes and the subprocess's next write blocks — the same
            // shape as slice #69 Item 4's Ollama deadlock.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Do NOT kill on drop — operator decides shutdown semantics
            // (parity with executor-llm's OllamaSubprocess contract).
            .kill_on_drop(false);

        if let Some(device) = &config.device {
            cmd.env("SURYA_DEVICE", device);
        }

        let mut child = cmd.spawn().map_err(|e| {
            OcrError::Subprocess(format!(
                "failed to spawn Surya subprocess (python={}): {e}",
                python.display()
            ))
        })?;

        // Drain stdout + stderr from spawn time. Each task reads
        // line-by-line and forwards to tracing at info level. When the
        // child exits, the pipes close → drain tasks see EOF → exit
        // cleanly. Handles deliberately not retained.
        if let Some(stdout) = child.stdout.take() {
            let port = config.port;
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            tracing::info!(
                                target: "surya_subprocess",
                                port,
                                "surya[stdout]: {}",
                                line
                            );
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::debug!(
                                target: "surya_subprocess",
                                port,
                                error = %e,
                                "surya stdout drain error; ending task"
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
                                target: "surya_subprocess",
                                port,
                                "surya[stderr]: {}",
                                line
                            );
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::debug!(
                                target: "surya_subprocess",
                                port,
                                error = %e,
                                "surya stderr drain error; ending task"
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

    /// Base URL of the managed Surya HTTP server.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Configured port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Returns `true` when the running subprocess responds successfully
    /// to `GET /health`. Returns `false` otherwise — including when the
    /// subprocess has been shut down (no stale cached state).
    pub async fn health_check(&self) -> bool {
        if self.child.lock().await.is_none() {
            return false;
        }

        let url = format!("{}/health", self.base_url());
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

    /// Shut down the subprocess. Sends a graceful kill via tokio's
    /// `Child::kill` (which delivers SIGKILL on Unix) and waits up to
    /// `SHUTDOWN_GRACE_SECS` for the process to exit. PID-scoped — never
    /// `killall surya`. After this returns, `health_check()` returns
    /// `false`.
    pub async fn stop(self) -> Result<(), OcrError> {
        let mut guard = self.child.lock().await;
        let mut child = match guard.take() {
            Some(c) => c,
            None => return Ok(()),
        };

        if let Err(e) = child.start_kill() {
            warn!(error = %e, "failed to send kill signal to Surya subprocess");
        }

        let wait_result =
            time::timeout(Duration::from_secs(SHUTDOWN_GRACE_SECS), child.wait()).await;

        match wait_result {
            Ok(Ok(status)) => {
                info!(?status, "Managed Surya subprocess exited");
                Ok(())
            }
            Ok(Err(e)) => Err(OcrError::Subprocess(format!(
                "wait on Surya subprocess failed: {e}"
            ))),
            Err(_) => {
                warn!(
                    grace_secs = SHUTDOWN_GRACE_SECS,
                    "Surya subprocess did not exit within grace period"
                );
                Err(OcrError::Subprocess(format!(
                    "surya subprocess did not exit within {SHUTDOWN_GRACE_SECS}s"
                )))
            }
        }
    }

    async fn wait_for_ready(&self, timeout_secs: u64) -> Result<(), OcrError> {
        let url = format!("{}/health", self.base_url());
        let client = reqwest::Client::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(OcrError::Subprocess(format!(
                    "surya subprocess did not become ready within {timeout_secs}s \
                     (cold-init downloads ~1-2GB of models; pre-warm via \
                     `just surya-venv-setup` + one-shot invocation if first run)"
                )));
            }
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!(port = self.port, "Surya subprocess ready");
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
    fn config_default_uses_documented_port_and_timeout() {
        // Honest-absence: default port MUST NOT collide with the legacy
        // ocr/ sidecar (5050), executor-llm's Ollama (11436), compute-agent
        // control listener (7100), executor-llm pool_listener (3301), or
        // mekhan engine (3030).
        let c = SuryaSubprocessConfig::default();
        assert_eq!(c.port, DEFAULT_SURYA_PORT);
        assert_ne!(c.port, 5050, "must not collide with legacy ocr/ sidecar");
        assert_ne!(c.port, 7100, "must not collide with compute-agent control");
        assert_ne!(c.port, 11436, "must not collide with executor-llm Ollama");
        assert_ne!(c.port, 3301, "must not collide with executor-llm pool_listener");
        assert_ne!(c.port, 3030, "must not collide with mekhan engine");
        assert!(c.venv_path.is_none());
        assert!(c.python_binary.is_none());
        assert!(c.device.is_none());
        assert_eq!(c.readiness_timeout_secs, READINESS_TIMEOUT_SECS);
        // Cold-init safety: 120s is the slice plan's tuned default.
        assert!(
            c.readiness_timeout_secs >= 120,
            "readiness_timeout_secs must be at least 120s for Surya cold-init \
             model download"
        );
    }

    #[test]
    fn resolve_python_errors_when_default_venv_missing() {
        // Honest-absence: default venv path resolves to `.dev/surya-venv/`
        // relative to CWD. When that doesn't exist (the test runs from the
        // executor workspace which doesn't have `.dev/surya-venv/` unless
        // `just surya-venv-setup` was run), resolve_python returns Err with
        // an actionable message that names the recipe.
        //
        // This test passes the empty-config path explicitly; it does NOT
        // create or delete the venv (no shared-fs mutation).
        let cfg = SuryaSubprocessConfig::default();
        let result = cfg.resolve_python();
        // If the venv DOES exist (operator pre-warmed it), the function
        // returns Ok — that's also valid; the assertion is symmetric on
        // both branches.
        match result {
            Ok(path) => {
                assert!(
                    path.exists(),
                    "resolve_python returned Ok but the path {path:?} doesn't exist"
                );
            }
            Err(OcrError::Config(msg)) => {
                assert!(
                    msg.contains("just surya-venv-setup"),
                    "Err message must name the recipe operator should run; got: {msg}"
                );
            }
            Err(other) => panic!(
                "resolve_python with default-venv-missing must Err::Config, got {other:?}"
            ),
        }
    }

    #[test]
    fn resolve_python_errors_clearly_on_missing_explicit_binary() {
        // Honest-absence: explicit binary path that doesn't exist surfaces
        // the path in the error so operators can correlate to their env var.
        let cfg = SuryaSubprocessConfig {
            python_binary: Some(PathBuf::from(
                "/definitely/not/a/real/path/to/python-binary-test",
            )),
            ..SuryaSubprocessConfig::default()
        };
        let err = cfg
            .resolve_python()
            .expect_err("missing explicit binary must Err");
        match err {
            OcrError::Config(msg) => {
                assert!(
                    msg.contains("AITHERICON_EXECUTOR_SURYA_BINARY_PATH"),
                    "Err message must name the env var; got: {msg}"
                );
                assert!(
                    msg.contains("/definitely/not/a/real/path"),
                    "Err message must include the offending path; got: {msg}"
                );
            }
            other => panic!("expected OcrError::Config, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_fails_with_missing_binary() {
        // Honest-absence: start with a deliberately non-existent binary
        // must return Err (from resolve_python) — NEVER a success with a
        // stale handle. Host-independent (no Surya install required).
        let cfg = SuryaSubprocessConfig {
            python_binary: Some(PathBuf::from(
                "/nonexistent/path/to/python-binary-xyz",
            )),
            readiness_timeout_secs: 1,
            ..SuryaSubprocessConfig::default()
        };
        let result = SuryaSubprocess::start(&cfg).await;
        assert!(
            result.is_err(),
            "start with missing python binary must fail"
        );
    }
}
