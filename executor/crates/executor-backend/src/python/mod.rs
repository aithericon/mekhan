pub mod cache;
pub mod runner;
pub mod venv;

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use aithericon_executor_domain::{
    ExecutionJob, ExecutionResult, ExecutionSpec, ExecutorError, RunContext,
};

use crate::process::child::run_process;
use crate::process::ProcessConfig;
use crate::traits::{ExecutionBackend, StatusCallback};

use cache::{BuildRequest, VenvCache};

/// Default max output capture: 64 KB per stream.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;

// Re-export config type and constants from the shared configs crate.
pub use aithericon_executor_backend_configs::python::{
    default_python, PythonConfig, INLINE_SCRIPT_NAME,
};

/// Backend that executes Python code with optional virtualenv isolation.
pub struct PythonBackend {
    max_output_bytes: usize,
    venv_cache: Option<Arc<VenvCache>>,
}

impl PythonBackend {
    pub fn new() -> Self {
        Self {
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            venv_cache: None,
        }
    }

    pub fn with_max_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }

    /// Attach a shared content-addressed venv cache. When set, jobs that
    /// request a virtualenv get a per-execution symlink to a cache-resident
    /// venv instead of building one inside the run dir.
    pub fn with_venv_cache(mut self, cache: Arc<VenvCache>) -> Self {
        self.venv_cache = Some(cache);
        self
    }

    /// Discover the aithericon SDK package path.
    ///
    /// Checks (in order):
    /// 1. `AITHERICON_SDK_PATH` environment variable
    /// 2. Relative to the crate manifest directory (development builds)
    fn find_sdk_path() -> Option<std::path::PathBuf> {
        // Explicit env var override
        if let Ok(path) = std::env::var("AITHERICON_SDK_PATH") {
            let p = std::path::PathBuf::from(path);
            if p.join("pyproject.toml").exists() {
                return Some(p);
            }
        }

        // Development fallback: relative to the workspace root
        // CARGO_MANIFEST_DIR points to crates/executor-backend/
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::Path::new(manifest_dir)
            .parent() // crates/
            .and_then(|p| p.parent()); // workspace root

        if let Some(root) = workspace_root {
            let sdk_path = root.join("packages").join("aithericon-sdk");
            if sdk_path.join("pyproject.toml").exists() {
                return Some(sdk_path);
            }
        }

        None
    }
}

impl Default for PythonBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionBackend for PythonBackend {
    async fn prepare(
        &self,
        _job: &ExecutionJob,
        mut run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        let config = PythonConfig::from_spec(&run_context.spec)?;

        // The SDK can only be installed into a virtualenv (no sudo, no global
        // site-packages writes). Treat `sdk: true` as implying `virtualenv:
        // true` so users don't have to toggle both.
        let needs_venv = config.virtualenv || config.sdk;

        // Determine Python binary
        let python_bin = if needs_venv {
            let venv_link = run_context.run_dir.root.join("venv");
            let sdk_path = if config.sdk { Self::find_sdk_path() } else { None };

            if let Some(cache) = self.venv_cache.as_ref() {
                let req = BuildRequest {
                    python: &config.python,
                    requirements: &config.requirements,
                    sdk_path: sdk_path.as_deref(),
                };
                let cached = cache.resolve(req).await?;
                symlink_into_run_dir(&cached, &venv_link).await?;
                info!(
                    cached = %cached.display(),
                    link = %venv_link.display(),
                    "venv resolved from cache"
                );
                cached
                    .join("bin")
                    .join("python")
                    .to_string_lossy()
                    .into_owned()
            } else {
                info!(venv_dir = %venv_link.display(), "creating virtualenv");
                let python_path =
                    venv::create_virtualenv(&config.python, &venv_link).await?;

                if !config.requirements.is_empty() {
                    info!(count = config.requirements.len(), "installing pip requirements");
                    venv::install_requirements(&venv_link, &config.requirements).await?;
                }

                if let Some(sdk) = sdk_path {
                    info!(sdk_path = %sdk.display(), "installing aithericon SDK");
                    venv::install_local_package(&venv_link, &sdk).await?;
                } else if config.sdk {
                    debug!("SDK auto-install skipped: SDK path not found");
                }

                python_path.to_string_lossy().into_owned()
            }
        } else {
            config.python.clone()
        };

        // Find user script in inputs directory (staged by StageInputsHook)
        let user_code_path = run_context.run_dir.inputs_dir.join(&config.script);
        if !user_code_path.exists() {
            return Err(ExecutorError::Config(format!(
                "script '{}' not found in inputs directory ({})",
                config.script,
                run_context.run_dir.inputs_dir.display()
            )));
        }

        // Generate runner template. Declared outputs from the spec are baked
        // into the template so the post-exec sweep can promote matching
        // Python globals to `<name>.json`. Outputs with an explicit `path`
        // are sidecar-style (IPC / file already produced by user code) and
        // not subject to the sweep — declared globals only.
        let declared_outputs: Vec<(String, bool)> = run_context
            .spec
            .outputs
            .iter()
            .filter(|o| o.path.is_none())
            .map(|o| (o.name.clone(), o.required))
            .collect();
        let runner_path = run_context.run_dir.root.join("__runner__.py");
        runner::write_runner(&runner_path, &user_code_path, &declared_outputs).await?;
        debug!(runner = %runner_path.display(), "generated runner template");

        // Store paths in backend_state for execute()
        run_context.backend_state = serde_json::json!({
            "python_bin": python_bin,
            "runner_path": runner_path.to_string_lossy(),
        });

        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        let config = PythonConfig::from_spec(&run_context.spec)?;

        let python_bin = run_context.backend_state["python_bin"]
            .as_str()
            .ok_or_else(|| {
                ExecutorError::StagingFailed("python_bin missing from backend_state".into())
            })?;

        let runner_path = run_context.backend_state["runner_path"]
            .as_str()
            .ok_or_else(|| {
                ExecutorError::StagingFailed("runner_path missing from backend_state".into())
            })?;

        // Build a ProcessConfig and delegate to the shared process execution engine
        let proc_config = ProcessConfig {
            command: python_bin.into(),
            args: vec!["-u".into(), runner_path.into()],
            env: config.env,
            working_dir: Some(run_context.run_dir.root.to_string_lossy().into_owned()),
            inherit_env: config.inherit_env,
        };

        run_process(
            &proc_config,
            run_context,
            self.max_output_bytes,
            &status_cb,
            cancel,
        )
        .await
    }

    fn name(&self) -> &'static str {
        "python"
    }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "python"
    }
}

/// Symlink a cache-resident venv into the per-execution run directory.
///
/// Race-tolerant: an existing entry at `link_path` (left over from a prior
/// run dir if the executor crashed mid-cleanup, or from a stale symlink) is
/// removed before the new symlink is created. The cache target is unaffected.
async fn symlink_into_run_dir(target: &std::path::Path, link_path: &std::path::Path) -> Result<(), ExecutorError> {
    if link_path.symlink_metadata().is_ok() {
        if let Err(e) = tokio::fs::remove_file(link_path).await {
            // Symlinks live as a file entry; if for some reason it's a real dir,
            // fall back to remove_dir_all (best-effort, then surface symlink error).
            warn!(path = %link_path.display(), error = %e, "could not unlink existing venv entry as file; trying remove_dir_all");
            if let Err(e2) = tokio::fs::remove_dir_all(link_path).await {
                return Err(ExecutorError::StagingFailed(format!(
                    "venv link site '{}' is occupied and could not be cleared: {e2}",
                    link_path.display()
                )));
            }
        }
    }

    #[cfg(unix)]
    {
        tokio::fs::symlink(target, link_path).await.map_err(|e| {
            ExecutorError::StagingFailed(format!(
                "failed to symlink venv {} -> {}: {e}",
                link_path.display(),
                target.display()
            ))
        })?;
    }

    #[cfg(not(unix))]
    {
        return Err(ExecutorError::StagingFailed(
            "venv cache requires a Unix host (symlink unsupported)".into(),
        ));
    }

    Ok(())
}
