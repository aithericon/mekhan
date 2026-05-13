use std::path::{Path, PathBuf};

use aithericon_executor_domain::ExecutorError;
use tracing::debug;

/// Check whether the `uv` binary is available on PATH.
///
/// Used by the cache layer to prefer `uv` (10-100× faster venv/install) over
/// the stdlib `venv` + `pip` path. Falls back gracefully when absent.
pub fn uv_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("uv")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Create a virtualenv using `uv venv` — much faster than `python -m venv`.
pub async fn create_virtualenv_uv(
    python_cmd: &str,
    venv_dir: &Path,
) -> Result<PathBuf, ExecutorError> {
    let output = tokio::process::Command::new("uv")
        .arg("venv")
        .arg(venv_dir)
        .arg("--python")
        .arg(python_cmd)
        .output()
        .await
        .map_err(|e| ExecutorError::StagingFailed(format!("failed to run uv venv: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ExecutorError::StagingFailed(format!(
            "uv venv failed: {stderr}"
        )));
    }

    let python_bin = venv_dir.join("bin").join("python");
    debug!(python_bin = %python_bin.display(), "uv virtualenv created");
    Ok(python_bin)
}

/// Install pip requirements via `uv pip install` into an existing virtualenv.
pub async fn install_requirements_uv(
    venv_dir: &Path,
    requirements: &[String],
    uv_cache_dir: Option<&Path>,
) -> Result<(), ExecutorError> {
    if requirements.is_empty() {
        return Ok(());
    }

    let venv_python = venv_dir.join("bin").join("python");
    let mut cmd = tokio::process::Command::new("uv");
    cmd.args(["pip", "install", "--python"])
        .arg(&venv_python)
        .args(requirements);
    if let Some(cache) = uv_cache_dir {
        cmd.env("UV_CACHE_DIR", cache);
    }
    let output = cmd.output().await.map_err(|e| {
        ExecutorError::StagingFailed(format!("failed to run uv pip install: {e}"))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ExecutorError::StagingFailed(format!(
            "uv pip install failed: {stderr}"
        )));
    }

    debug!(count = requirements.len(), "uv pip requirements installed");
    Ok(())
}

/// Install a local Python package via `uv pip install` into an existing virtualenv.
///
/// Same isolation strategy as [`install_local_package`]: copy the package source
/// to a per-venv `sdk-build/` dir before invoking the installer, so concurrent
/// builds against a shared read-only source tree don't race on setuptools' build dir.
pub async fn install_local_package_uv(
    venv_dir: &Path,
    package_dir: &Path,
    uv_cache_dir: Option<&Path>,
) -> Result<(), ExecutorError> {
    let venv_python = venv_dir.join("bin").join("python");
    let build_copy = venv_dir.join("sdk-build");
    if build_copy.exists() {
        tokio::fs::remove_dir_all(&build_copy).await.ok();
    }
    copy_dir_recursive(package_dir, &build_copy).await.map_err(|e| {
        ExecutorError::StagingFailed(format!(
            "failed to stage package '{}' into '{}': {e}",
            package_dir.display(),
            build_copy.display()
        ))
    })?;

    let mut cmd = tokio::process::Command::new("uv");
    cmd.args(["pip", "install", "--python"])
        .arg(&venv_python)
        .arg(&build_copy);
    if let Some(cache) = uv_cache_dir {
        cmd.env("UV_CACHE_DIR", cache);
    }
    let output = cmd.output().await.map_err(|e| {
        ExecutorError::StagingFailed(format!(
            "failed to run uv pip install for local package '{}': {e}",
            package_dir.display()
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ExecutorError::StagingFailed(format!(
            "uv local package install failed: {stderr}"
        )));
    }

    debug!(package = %package_dir.display(), "local package installed via uv");
    Ok(())
}

/// Resolve the Python interpreter's full version string (e.g. `"Python 3.11.9"`).
///
/// Used as part of the venv cache hash key so that a system Python upgrade
/// invalidates cached venvs that depend on the old interpreter.
pub async fn python_version_string(python_cmd: &str) -> Result<String, ExecutorError> {
    let output = tokio::process::Command::new(python_cmd)
        .arg("-V")
        .output()
        .await
        .map_err(|e| {
            ExecutorError::StagingFailed(format!("failed to query '{python_cmd} -V': {e}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ExecutorError::StagingFailed(format!(
            "'{python_cmd} -V' failed: {stderr}"
        )));
    }

    // Some pythons write version to stderr (older 2.x); accept either.
    let combined = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).into_owned()
    } else {
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    Ok(combined.trim().to_string())
}

/// Create a virtualenv at the given directory using the specified Python command.
///
/// Returns the path to the Python binary inside the virtualenv.
pub async fn create_virtualenv(
    python_cmd: &str,
    venv_dir: &Path,
) -> Result<PathBuf, ExecutorError> {
    let output = tokio::process::Command::new(python_cmd)
        .args(["-m", "venv", &venv_dir.to_string_lossy()])
        .output()
        .await
        .map_err(|e| {
            ExecutorError::StagingFailed(format!(
                "failed to run '{python_cmd} -m venv': {e}"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ExecutorError::StagingFailed(format!(
            "virtualenv creation failed: {stderr}"
        )));
    }

    let python_bin = venv_dir.join("bin").join("python");
    debug!(python_bin = %python_bin.display(), "virtualenv created");
    Ok(python_bin)
}

/// Install pip requirements into an existing virtualenv.
pub async fn install_requirements(
    venv_dir: &Path,
    requirements: &[String],
) -> Result<(), ExecutorError> {
    install_requirements_with_cache(venv_dir, requirements, None).await
}

/// Install pip requirements with an optional shared wheel cache directory.
///
/// When `pip_cache_dir` is set, `PIP_CACHE_DIR` is exported to pip so wheels
/// downloaded for one venv build are reused on subsequent builds even when the
/// venv-level cache misses.
pub async fn install_requirements_with_cache(
    venv_dir: &Path,
    requirements: &[String],
    pip_cache_dir: Option<&Path>,
) -> Result<(), ExecutorError> {
    if requirements.is_empty() {
        return Ok(());
    }

    let pip_bin = venv_dir.join("bin").join("pip");
    let mut cmd = tokio::process::Command::new(&pip_bin);
    cmd.arg("install").arg("--quiet").args(requirements);
    if let Some(cache) = pip_cache_dir {
        cmd.env("PIP_CACHE_DIR", cache);
    }
    let output = cmd.output().await.map_err(|e| {
        ExecutorError::StagingFailed(format!("failed to run pip install: {e}"))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ExecutorError::StagingFailed(format!(
            "pip install failed: {stderr}"
        )));
    }

    debug!(count = requirements.len(), "pip requirements installed");
    Ok(())
}

/// Install a local Python package into an existing virtualenv.
///
/// Used to auto-install the aithericon SDK when `sdk: true` in config.
/// Copies the package source to a per-venv temp dir before running pip so
/// concurrent jobs that share a read-only source tree (e.g. on NFS, where
/// multiple Slurm nodes point at the same SDK path) don't race on
/// setuptools' `build/` directory inside the source tree and fail with
/// `[Errno 17] File exists: .../aithericon-0.1.0.dist-info`.
pub async fn install_local_package(
    venv_dir: &Path,
    package_dir: &Path,
) -> Result<(), ExecutorError> {
    install_local_package_with_cache(venv_dir, package_dir, None).await
}

pub async fn install_local_package_with_cache(
    venv_dir: &Path,
    package_dir: &Path,
    pip_cache_dir: Option<&Path>,
) -> Result<(), ExecutorError> {
    let pip_bin = venv_dir.join("bin").join("pip");
    let build_copy = venv_dir.join("sdk-build");
    if build_copy.exists() {
        tokio::fs::remove_dir_all(&build_copy).await.ok();
    }
    copy_dir_recursive(package_dir, &build_copy).await.map_err(|e| {
        ExecutorError::StagingFailed(format!(
            "failed to stage package '{}' into '{}': {e}",
            package_dir.display(),
            build_copy.display()
        ))
    })?;
    let mut cmd = tokio::process::Command::new(&pip_bin);
    cmd.args(["install", "--quiet"]).arg(&build_copy);
    if let Some(cache) = pip_cache_dir {
        cmd.env("PIP_CACHE_DIR", cache);
    }
    let output = cmd.output().await.map_err(|e| {
        ExecutorError::StagingFailed(format!(
            "failed to install local package '{}': {e}",
            package_dir.display()
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ExecutorError::StagingFailed(format!(
            "local package install failed: {stderr}"
        )));
    }

    debug!(package = %package_dir.display(), "local package installed into venv");
    Ok(())
}

async fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((src, dst)) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&src).await?;
        while let Some(entry) = entries.next_entry().await? {
            let ft = entry.file_type().await?;
            let name = entry.file_name();
            // Skip build artifacts that collide with setuptools output.
            if matches!(name.to_str(), Some("build") | Some("dist") | Some("__pycache__")) {
                continue;
            }
            if name.to_string_lossy().ends_with(".egg-info") {
                continue;
            }
            let src_path = src.join(&name);
            let dst_path = dst.join(&name);
            if ft.is_dir() {
                tokio::fs::create_dir_all(&dst_path).await?;
                stack.push((src_path, dst_path));
            } else if ft.is_file() {
                tokio::fs::copy(&src_path, &dst_path).await?;
            }
        }
    }
    Ok(())
}
