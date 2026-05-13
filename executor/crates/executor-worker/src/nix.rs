use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, info, warn};

use aithericon_executor_domain::{ExecutionJob, ExecutorError, RunContext};

use crate::staging::StagingHook;

/// Nix dependency specification, deserialized from `spec.config["nix"]`.
#[derive(Debug, Clone, Deserialize)]
pub struct NixSpec {
    /// Nixpkgs attribute paths (e.g. `["python311", "python311Packages.numpy"]`).
    pub packages: Vec<String>,

    /// Optional nixpkgs commit hash or channel for pinning (e.g. `"nixos-24.05"` or a commit SHA).
    /// When absent, uses the system `<nixpkgs>`.
    #[serde(default)]
    pub nixpkgs_pin: Option<String>,
}

/// Configuration for the Nix environment hook.
#[derive(Debug, Clone, Deserialize)]
pub struct NixConfig {
    /// Whether Nix environment resolution is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Directory for caching Nix expressions and store path lookups.
    /// Defaults to `{base_dir}/nix-envs/`.
    #[serde(default)]
    pub cache_dir: Option<String>,
}

/// Staging hook that resolves Nix environments for tasks declaring `nix` dependencies.
///
/// When a job's `spec.config` contains a `"nix"` key, this hook:
/// 1. Parses the [`NixSpec`] (package list + optional nixpkgs pin)
/// 2. Computes a content hash of the spec
/// 3. Checks the local cache for a previously-built environment
/// 4. If cache miss, generates a Nix expression and runs `nix-build`
/// 5. Enriches `RunContext.env` with PATH (and PYTHONPATH if applicable)
/// 6. Stores the store path in `backend_state` for nsjail composition
///
/// When the job has no `"nix"` key in config, this hook is a no-op.
pub struct NixEnvironmentHook {
    /// Directory for caching expressions and resolved store paths.
    pub cache_dir: PathBuf,
    /// Optional path to the aithericon Python SDK package.
    /// When set and `spec.config.sdk == true`, the SDK is built into
    /// the Nix environment so no venv is needed.
    pub sdk_path: Option<PathBuf>,
}

impl NixEnvironmentHook {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            sdk_path: None,
        }
    }

    pub fn with_sdk_path(mut self, path: PathBuf) -> Self {
        self.sdk_path = Some(path);
        self
    }
}

#[async_trait]
impl StagingHook for NixEnvironmentHook {
    fn name(&self) -> &'static str {
        "nix_environment"
    }

    async fn stage(
        &self,
        _job: &ExecutionJob,
        mut ctx: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // Resolve NixSpec from either explicit nix config or pip requirements.
        //
        // Priority:
        // 1. Explicit spec.config.nix.packages — use as-is
        // 2. spec.config.requirements (pip names) — auto-translate to nixpkgs attrs
        // 3. Neither — no-op
        let nix_spec = if let Some(v) = ctx.spec.config.get("nix") {
            serde_json::from_value::<NixSpec>(v.clone()).map_err(|e| {
                ExecutorError::StagingFailed(format!("invalid nix spec in config: {e}"))
            })?
        } else if let Some(reqs) = ctx.spec.config.get("requirements") {
            let reqs: Vec<String> = serde_json::from_value(reqs.clone()).unwrap_or_default();
            if reqs.is_empty() {
                // No explicit nix, no requirements — but still provide Python + SDK
                // if the backend is "python"
                if ctx.spec.backend == "python" {
                    nix_spec_from_requirements(&[], &ctx.spec.config)
                } else {
                    return Ok(ctx);
                }
            } else {
                nix_spec_from_requirements(&reqs, &ctx.spec.config)
            }
        } else if ctx.spec.backend == "python" {
            // Python backend with no requirements — still provide Python interpreter
            nix_spec_from_requirements(&[], &ctx.spec.config)
        } else {
            return Ok(ctx);
        };

        if nix_spec.packages.is_empty() {
            debug!("nix spec has no packages, skipping");
            return Ok(ctx);
        }

        // Clear requirements so PythonBackend doesn't also pip-install them
        if let Some(obj) = ctx.spec.config.as_object_mut() {
            obj.insert("requirements".into(), serde_json::json!([]));
        }

        // Auto-include SDK when spec.config.sdk == true and we have an sdk_path
        let local_packages = if self.sdk_path.is_some()
            && ctx.spec.config.get("sdk").and_then(|v| v.as_bool()).unwrap_or(false)
        {
            let sdk = self.sdk_path.as_ref().unwrap();
            info!(sdk_path = %sdk.display(), "including aithericon SDK in nix environment");
            vec![sdk.clone()]
        } else {
            vec![]
        };

        // Resolve (cache hit or build)
        let store_path =
            resolve_environment(&nix_spec, &local_packages, &self.cache_dir).await?;
        info!(store_path = %store_path.display(), "nix environment resolved");

        // Enrich PATH
        let bin_dir = store_path.join("bin");
        if bin_dir.exists() {
            let existing_path = ctx.env.get("PATH").cloned().unwrap_or_default();
            let new_path = if existing_path.is_empty() {
                bin_dir.to_string_lossy().into_owned()
            } else {
                format!("{}:{}", bin_dir.display(), existing_path)
            };
            ctx.env.insert("PATH".into(), new_path);
        }

        // Enrich PYTHONPATH if python site-packages exist
        if let Some(site_packages) = find_python_site_packages(&store_path) {
            let existing = ctx.env.get("PYTHONPATH").cloned().unwrap_or_default();
            let new_val = if existing.is_empty() {
                site_packages.to_string_lossy().into_owned()
            } else {
                format!("{}:{}", site_packages.display(), existing)
            };
            ctx.env.insert("PYTHONPATH".into(), new_val);
        }

        // Store in backend_state for nsjail composition
        let nix_state = serde_json::json!({
            "store_path": store_path.to_string_lossy(),
        });
        if ctx.backend_state.is_null() {
            ctx.backend_state = serde_json::json!({ "nix": nix_state });
        } else {
            ctx.backend_state["nix"] = nix_state;
        }

        Ok(ctx)
    }
}

/// Map a pip package name to a nixpkgs Python package attribute name.
///
/// Most pip names map directly (lowercase). Known exceptions are handled explicitly.
fn pip_to_nixpkgs(pip_name: &str) -> String {
    // Strip version specifiers (e.g. "numpy>=1.20" → "numpy")
    let name = pip_name
        .split(&['>', '<', '=', '!', '~', ';'][..])
        .next()
        .unwrap_or(pip_name)
        .trim();

    // Known pip→nixpkgs name mismatches
    match name.to_lowercase().as_str() {
        "pillow" => "pillow".into(),
        "pyyaml" => "pyyaml".into(),
        "opencv-python" | "opencv-python-headless" => "opencv4".into(),
        "python-dateutil" => "python-dateutil".into(),
        "beautifulsoup4" => "beautifulsoup4".into(),
        other => other.into(),
    }
}

/// Build a NixSpec from pip requirements, auto-detecting the Python version from config.
fn nix_spec_from_requirements(
    requirements: &[String],
    config: &serde_json::Value,
) -> NixSpec {
    // Detect Python version from config.python field (e.g. "python3.11" → "python311")
    let python_attr = config
        .get("python")
        .and_then(|v| v.as_str())
        .map(|p| {
            // "python3.11" → "python311", "python3" → "python3"
            p.replace('.', "").replace("python3", "python3")
        })
        .unwrap_or_else(|| "python311".into());

    let mut packages = vec![python_attr.clone()];
    let pkg_prefix = format!("{python_attr}Packages");

    for req in requirements {
        let nix_name = pip_to_nixpkgs(req);
        packages.push(format!("{pkg_prefix}.{nix_name}"));
    }

    NixSpec {
        packages,
        nixpkgs_pin: None,
    }
}

/// Compute a deterministic content hash for a NixSpec + local packages.
///
/// Sorts packages alphabetically, combines with nixpkgs pin and local paths,
/// and produces a 16-hex-digit hash string. Uses Rust's `DefaultHasher`
/// (SipHash-2-4) which is deterministic within a Rust version — sufficient
/// for a local cache key.
pub fn content_hash(spec: &NixSpec, local_packages: &[PathBuf]) -> String {
    let mut packages = spec.packages.clone();
    packages.sort();
    let mut canonical = format!(
        "nixpkgs_pin={};packages={}",
        spec.nixpkgs_pin.as_deref().unwrap_or("<nixpkgs>"),
        packages.join(",")
    );
    if !local_packages.is_empty() {
        let mut locals: Vec<_> = local_packages
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        locals.sort();
        canonical.push_str(&format!(";local={}", locals.join(",")));
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Generate a Nix expression that builds a merged environment from the given packages.
///
/// Uses `python3XX.withPackages` for Python packages (properly resolves transitive
/// deps like joblib) and `buildEnv` for any non-Python packages.
///
/// Package naming convention:
/// - `"python311"` → the interpreter (determines the Python version)
/// - `"python311Packages.numpy"` → Python package (goes into `withPackages`)
/// - anything else → non-Python package (goes into `buildEnv.paths`)
pub fn generate_expression(spec: &NixSpec, local_packages: &[PathBuf]) -> String {
    let nixpkgs_import = match &spec.nixpkgs_pin {
        Some(pin) => {
            if pin.len() == 40 && pin.chars().all(|c| c.is_ascii_hexdigit()) {
                format!(
                    "import (fetchTarball \"https://github.com/NixOS/nixpkgs/archive/{pin}.tar.gz\") {{}}"
                )
            } else {
                format!(
                    "import (fetchTarball \"https://github.com/NixOS/nixpkgs/archive/refs/heads/{pin}.tar.gz\") {{}}"
                )
            }
        }
        None => "import <nixpkgs> {}".to_string(),
    };

    // Separate Python interpreter, Python packages, and non-Python packages
    let python_attr = spec
        .packages
        .iter()
        .find(|p| p.starts_with("python3") && !p.contains("Packages"))
        .cloned()
        .unwrap_or_else(|| "python311".to_string());

    // Extract the pythonXXPackages prefix to strip it (e.g. "python311Packages.numpy" → "numpy")
    let py_pkg_prefix = format!("{python_attr}Packages.");
    let mut py_packages: Vec<String> = Vec::new();
    let mut other_packages: Vec<String> = Vec::new();

    for pkg in &spec.packages {
        if let Some(name) = pkg.strip_prefix(&py_pkg_prefix) {
            py_packages.push(format!("      ps.{name}"));
        } else if pkg.contains("Packages.") {
            // Different Python version prefix — use as-is in buildEnv
            other_packages.push(format!("    pkgs.{pkg}"));
        } else if *pkg != python_attr {
            // Non-Python package
            other_packages.push(format!("    pkgs.{pkg}"));
        }
        // python_attr itself is handled by withPackages
    }

    // Build local Python package derivations
    let mut local_defs = String::new();
    for (i, pkg_path) in local_packages.iter().enumerate() {
        let var_name = format!("local-pkg-{i}");
        let path_str = pkg_path.to_string_lossy();
        local_defs.push_str(&format!(
            r#"  {var_name} = pkgs.{python_attr}.pkgs.buildPythonPackage {{
    pname = "local-pkg-{i}";
    version = "0.0.0";
    src = {path_str};
    format = "pyproject";
    nativeBuildInputs = [ pkgs.{python_attr}.pkgs.setuptools ];
    propagatedBuildInputs = [ pkgs.{python_attr}.pkgs.grpcio pkgs.{python_attr}.pkgs.protobuf ];
    doCheck = false;
  }};
"#
        ));
        py_packages.push(format!("      {var_name}"));
    }

    // Build the withPackages Python environment
    let py_pkg_list = py_packages.join("\n");
    let python_env = format!(
        r#"  pythonEnv = pkgs.{python_attr}.withPackages (ps: [
{py_pkg_list}
    ]);"#
    );

    // Combine into buildEnv
    let mut all_paths = vec!["    pythonEnv".to_string()];
    all_paths.extend(other_packages);

    let paths_str = all_paths.join("\n");

    format!(
        r#"let
  pkgs = {nixpkgs_import};
{python_env}
{local_defs}in
pkgs.buildEnv {{
  name = "aithericon-env";
  paths = [
{paths_str}
  ];
  ignoreCollisions = true;
}}"#
    )
}

/// Resolve a Nix environment: check cache, build if needed, return store path.
async fn resolve_environment(
    spec: &NixSpec,
    local_packages: &[PathBuf],
    cache_dir: &Path,
) -> Result<PathBuf, ExecutorError> {
    let hash = content_hash(spec, local_packages);

    // Ensure cache directory exists
    tokio::fs::create_dir_all(cache_dir).await.map_err(|e| {
        ExecutorError::StagingFailed(format!("failed to create nix cache dir: {e}"))
    })?;

    let store_path_file = cache_dir.join(format!("{hash}.store_path"));
    let expr_file = cache_dir.join(format!("{hash}.nix"));
    let gcroot_link = cache_dir.join(format!("{hash}.gcroot"));

    // Fast path: check cache
    if let Ok(cached_path) = tokio::fs::read_to_string(&store_path_file).await {
        let cached = PathBuf::from(cached_path.trim());
        if cached.exists() {
            debug!(hash = %hash, path = %cached.display(), "nix cache hit");
            return Ok(cached);
        }
        warn!(hash = %hash, path = %cached.display(), "cached store path missing (GC'd?), rebuilding");
    }

    // Cache miss: generate expression and build
    let expression = generate_expression(spec, local_packages);
    tokio::fs::write(&expr_file, &expression).await.map_err(|e| {
        ExecutorError::StagingFailed(format!("failed to write nix expression: {e}"))
    })?;

    debug!(hash = %hash, expr = %expr_file.display(), "nix cache miss, building");

    let store_path = nix_build(&expr_file).await?;

    // Write cache entry
    tokio::fs::write(&store_path_file, store_path.to_string_lossy().as_bytes())
        .await
        .map_err(|e| {
            ExecutorError::StagingFailed(format!("failed to write nix cache entry: {e}"))
        })?;

    // Create GC root to protect from nix-collect-garbage
    nix_add_gcroot(&store_path, &gcroot_link).await;

    Ok(store_path)
}

/// Run `nix-build` on an expression file and return the resulting store path.
async fn nix_build(expr_path: &Path) -> Result<PathBuf, ExecutorError> {
    let output = tokio::process::Command::new("nix-build")
        .arg(expr_path)
        .arg("--no-out-link")
        .output()
        .await
        .map_err(|e| {
            ExecutorError::StagingFailed(format!(
                "failed to run nix-build (is nix installed?): {e}"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ExecutorError::StagingFailed(format!(
            "nix-build failed:\n{stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let store_path = stdout
        .trim()
        .lines()
        .last()
        .ok_or_else(|| {
            ExecutorError::StagingFailed("nix-build produced no output".into())
        })?;

    Ok(PathBuf::from(store_path))
}

/// Create a GC root symlink to protect a store path from garbage collection.
///
/// Non-fatal: if this fails, the env will just be rebuilt on next cache miss.
async fn nix_add_gcroot(store_path: &Path, link_path: &Path) {
    let result = tokio::process::Command::new("nix-store")
        .args(["--add-root"])
        .arg(link_path)
        .args(["-r"])
        .arg(store_path)
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            debug!(link = %link_path.display(), "nix GC root created");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("nix-store --add-root failed (non-fatal): {stderr}");
        }
        Err(e) => {
            warn!("nix-store --add-root failed (non-fatal): {e}");
        }
    }
}

/// Find the Python site-packages directory inside a Nix buildEnv store path.
///
/// Looks for `lib/python*/site-packages` in the store path.
fn find_python_site_packages(store_path: &Path) -> Option<PathBuf> {
    let lib_dir = store_path.join("lib");
    let read_dir = std::fs::read_dir(&lib_dir).ok()?;

    for entry in read_dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("python") {
            let site_packages = entry.path().join("site-packages");
            if site_packages.exists() {
                return Some(site_packages);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> NixSpec {
        NixSpec {
            packages: vec![
                "python311".into(),
                "python311Packages.numpy".into(),
                "python311Packages.gpytorch".into(),
            ],
            nixpkgs_pin: None,
        }
    }

    #[test]
    fn content_hash_is_deterministic() {
        let spec = sample_spec();
        let h1 = content_hash(&spec, &[]);
        let h2 = content_hash(&spec, &[]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn content_hash_is_order_independent() {
        let spec1 = NixSpec {
            packages: vec!["b".into(), "a".into(), "c".into()],
            nixpkgs_pin: None,
        };
        let spec2 = NixSpec {
            packages: vec!["a".into(), "c".into(), "b".into()],
            nixpkgs_pin: None,
        };
        assert_eq!(content_hash(&spec1, &[]), content_hash(&spec2, &[]));
    }

    #[test]
    fn content_hash_differs_with_pin() {
        let mut spec = sample_spec();
        let h_unpinned = content_hash(&spec, &[]);
        spec.nixpkgs_pin = Some("nixos-24.05".into());
        let h_pinned = content_hash(&spec, &[]);
        assert_ne!(h_unpinned, h_pinned);
    }

    #[test]
    fn generate_expression_unpinned() {
        let spec = sample_spec();
        let expr = generate_expression(&spec, &[]);
        assert!(expr.contains("import <nixpkgs> {}"));
        assert!(expr.contains("withPackages"), "should use withPackages for Python deps");
        assert!(expr.contains("ps.numpy"), "numpy should be in withPackages closure");
        assert!(expr.contains("ps.gpytorch"), "gpytorch should be in withPackages closure");
        assert!(expr.contains("buildEnv"));
        assert!(expr.contains("ignoreCollisions = true"));
    }

    #[test]
    fn generate_expression_pinned_commit() {
        let spec = NixSpec {
            packages: vec!["hello".into()],
            nixpkgs_pin: Some("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2".into()),
        };
        let expr = generate_expression(&spec, &[]);
        assert!(expr.contains("fetchTarball"));
        assert!(expr.contains("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"));
        assert!(!expr.contains("refs/heads"));
    }

    #[test]
    fn generate_expression_pinned_channel() {
        let spec = NixSpec {
            packages: vec!["hello".into()],
            nixpkgs_pin: Some("nixos-24.05".into()),
        };
        let expr = generate_expression(&spec, &[]);
        assert!(expr.contains("fetchTarball"));
        assert!(expr.contains("refs/heads/nixos-24.05"));
    }

    #[test]
    fn nix_spec_deserialize_minimal() {
        let json = serde_json::json!({
            "packages": ["python311", "python311Packages.numpy"]
        });
        let spec: NixSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec.packages.len(), 2);
        assert!(spec.nixpkgs_pin.is_none());
    }

    #[test]
    fn nix_spec_deserialize_with_pin() {
        let json = serde_json::json!({
            "packages": ["hello"],
            "nixpkgs_pin": "nixos-24.05"
        });
        let spec: NixSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec.nixpkgs_pin.as_deref(), Some("nixos-24.05"));
    }

    /// Integration test: requires `nix-build` on PATH.
    /// Skipped automatically when Nix is not installed.
    #[tokio::test]
    async fn integration_resolve_and_cache_python_env() {
        // Skip if nix-build is not available
        if std::process::Command::new("nix-build")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: nix-build not found");
            return;
        }

        let cache_dir = std::env::temp_dir().join(format!(
            "nix-integration-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&cache_dir);

        let spec = NixSpec {
            packages: vec![
                "python311".into(),
                "python311Packages.numpy".into(),
            ],
            nixpkgs_pin: None,
        };

        // First resolve — may build or hit Nix's own cache
        let t0 = std::time::Instant::now();
        let store_path = resolve_environment(&spec, &[], &cache_dir).await.unwrap();
        let first_duration = t0.elapsed();
        eprintln!("first resolve: {:?} -> {}", first_duration, store_path.display());

        // Store path exists and has bin/python3
        assert!(store_path.join("bin/python3").exists(), "python3 binary missing");

        // Cache files were written
        let hash = content_hash(&spec, &[]);
        assert!(
            cache_dir.join(format!("{hash}.store_path")).exists(),
            "cache store_path file missing"
        );
        assert!(
            cache_dir.join(format!("{hash}.nix")).exists(),
            "cache nix expression file missing"
        );

        // Second resolve — must be a cache hit (fast)
        let t1 = std::time::Instant::now();
        let store_path_2 = resolve_environment(&spec, &[], &cache_dir).await.unwrap();
        let second_duration = t1.elapsed();
        eprintln!("second resolve (cached): {:?}", second_duration);

        assert_eq!(store_path, store_path_2, "cache returned different path");
        // Cache hit should be well under 100ms (it's just a file read + exists check)
        assert!(
            second_duration.as_millis() < 100,
            "cache hit took too long: {:?}",
            second_duration
        );

        // PYTHONPATH discovery works
        let site_packages = find_python_site_packages(&store_path);
        assert!(site_packages.is_some(), "site-packages not found");
        let site_packages = site_packages.unwrap();
        assert!(
            site_packages.join("numpy").exists(),
            "numpy not in site-packages"
        );

        // Actually run Python with numpy using the resolved env
        let python = store_path.join("bin/python3");
        let output = tokio::process::Command::new(&python)
            .env("PYTHONPATH", &site_packages)
            .args(["-c", "import numpy; print(f'numpy {numpy.__version__}')"])
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "python failed: {}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.starts_with("numpy "), "unexpected output: {stdout}");
        eprintln!("python output: {}", stdout.trim());

        // Cleanup
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    #[tokio::test]
    async fn hook_is_noop_without_nix_key() {
        use aithericon_executor_backend::ProcessConfig;
        use aithericon_executor_domain::{JobPriority, RunDirectory};
        use std::collections::HashMap;
        use std::time::Duration;

        let job = ExecutionJob {
            execution_id: "test-nix-noop".into(),
            spec: ProcessConfig {
                command: "echo".into(),
                args: vec!["hello".into()],
                env: Default::default(),
                working_dir: None,
                inherit_env: true,
            }
            .into_spec(),
            metadata: HashMap::new(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        };

        let ctx = RunContext {
            execution_id: "test-nix-noop".into(),
            spec: job.spec.clone(),
            run_dir: RunDirectory::new(&PathBuf::from("/tmp"), "test-nix-noop"),
            timeout: Duration::from_secs(60),
            env: HashMap::new(),
            metadata: HashMap::new(),
            staged_inputs: HashMap::new(),
            expected_outputs: HashMap::new(),
            staged_events: Vec::new(),
            backend_state: serde_json::Value::Null,
        };

        let hook = NixEnvironmentHook::new(PathBuf::from("/tmp/nix-test-cache"));
        let result = hook.stage(&job, ctx).await.unwrap();

        // No changes to env or backend_state
        assert!(!result.env.contains_key("PATH"));
        assert!(!result.env.contains_key("PYTHONPATH"));
        assert!(result.backend_state.is_null());
    }

    #[test]
    fn pip_to_nixpkgs_strips_version_specifiers() {
        assert_eq!(pip_to_nixpkgs("numpy>=1.20"), "numpy");
        assert_eq!(pip_to_nixpkgs("scipy==1.11.0"), "scipy");
        assert_eq!(pip_to_nixpkgs("pandas~=2.0"), "pandas");
        assert_eq!(pip_to_nixpkgs("scikit-learn"), "scikit-learn");
    }

    #[test]
    fn pip_to_nixpkgs_handles_known_aliases() {
        assert_eq!(pip_to_nixpkgs("Pillow"), "pillow");
        assert_eq!(pip_to_nixpkgs("PyYAML"), "pyyaml");
        assert_eq!(pip_to_nixpkgs("opencv-python"), "opencv4");
    }

    #[test]
    fn nix_spec_from_requirements_basic() {
        let config = serde_json::json!({});
        let spec = nix_spec_from_requirements(
            &["numpy".into(), "scipy".into()],
            &config,
        );
        assert!(spec.packages.contains(&"python311".to_string()));
        assert!(spec.packages.contains(&"python311Packages.numpy".to_string()));
        assert!(spec.packages.contains(&"python311Packages.scipy".to_string()));
    }

    #[test]
    fn nix_spec_from_requirements_empty_still_has_python() {
        let config = serde_json::json!({});
        let spec = nix_spec_from_requirements(&[], &config);
        assert_eq!(spec.packages, vec!["python311".to_string()]);
    }
}
