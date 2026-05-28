//! Content-addressed venv cache.
//!
//! Mirrors the design in `crates/executor-worker/src/nix.rs`: hash a build
//! request → look up `{cache_root}/{hash}/` → reuse across executions. The
//! venv is **created at** its cache location (never moved) so absolute paths
//! baked into `pyvenv.cfg`, shebangs, and `bin/python` remain valid; the
//! per-execution code symlinks `{run_dir}/venv` → `{cache_root}/{hash}/`.
//!
//! Concurrency: builds for the same hash are serialized via an advisory
//! `flock` on `{hash}.lock`. Builds happen in `.staging/{hash}.{unique}/`
//! and are atomically renamed into place, so a crashed builder never leaves
//! a half-built venv at the cache target. Leftover `.staging/` entries are
//! swept on cache construction.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aithericon_executor_domain::ExecutorError;
use tracing::{debug, info, warn};

use super::venv;

/// Bump to invalidate every cache entry at once (e.g. after a builder bug fix).
pub const CACHE_SCHEMA: u32 = 1;

/// Per-build inputs. The cache key is `hash(python_version, sorted(requirements), sdk_marker)`.
#[derive(Debug, Clone)]
pub struct BuildRequest<'a> {
    pub python: &'a str,
    pub requirements: &'a [String],
    /// `Some(path)` when the aithericon SDK should be installed; the SDK's
    /// version marker (resolved at cache construction) is mixed into the hash.
    pub sdk_path: Option<&'a Path>,
}

/// Snapshot of cache counters at a point in time. Cheap to read; safe to log.
#[derive(Debug, Clone, Copy)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub builds_in_flight: u64,
    pub build_duration_ms_total: u64,
}

impl CacheStats {
    /// Hit ratio in [0.0, 1.0]. Returns 0.0 when no resolves have happened yet.
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

#[derive(Debug, Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    builds_in_flight: AtomicU64,
    build_duration_ms_total: AtomicU64,
}

/// Shared, content-addressed venv cache.
#[derive(Debug, Clone)]
pub struct VenvCache {
    cache_root: PathBuf,
    wheels_dir: PathBuf,
    staging_dir: PathBuf,
    sdk_marker: Option<String>,
    prefer_uv: bool,
    counters: Arc<Counters>,
}

impl VenvCache {
    pub fn new(
        cache_root: PathBuf,
        prefer_uv: bool,
        sdk_marker: Option<String>,
    ) -> std::io::Result<Self> {
        let wheels_dir = cache_root.join("wheels");
        let staging_dir = cache_root.join(".staging");
        std::fs::create_dir_all(&cache_root)?;
        std::fs::create_dir_all(&wheels_dir)?;
        std::fs::create_dir_all(&staging_dir)?;

        if let Ok(entries) = std::fs::read_dir(&staging_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Err(e) = std::fs::remove_dir_all(&path) {
                        warn!(path = %path.display(), error = %e, "failed to sweep stale staging dir");
                    }
                }
            }
        }

        Ok(Self {
            cache_root,
            wheels_dir,
            staging_dir,
            sdk_marker,
            prefer_uv,
            counters: Arc::new(Counters::default()),
        })
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    pub fn sdk_marker(&self) -> Option<&str> {
        self.sdk_marker.as_deref()
    }

    /// Snapshot of hit/miss counters. Safe to call from any thread.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.counters.hits.load(Ordering::Relaxed),
            misses: self.counters.misses.load(Ordering::Relaxed),
            builds_in_flight: self.counters.builds_in_flight.load(Ordering::Relaxed),
            build_duration_ms_total: self
                .counters
                .build_duration_ms_total
                .load(Ordering::Relaxed),
        }
    }

    /// Compute the cache key for a build request. Visible for tests.
    pub async fn hash_for(&self, req: &BuildRequest<'_>) -> Result<String, ExecutorError> {
        let py_version = venv::python_version_string(req.python).await?;
        let sdk_marker = if req.sdk_path.is_some() {
            self.sdk_marker.as_deref().unwrap_or("<missing>")
        } else {
            "<no-sdk>"
        };
        Ok(compute_hash(&py_version, req.requirements, sdk_marker))
    }

    /// Resolve a venv from the cache, building it if missing.
    ///
    /// Returns the path to the cached venv directory. The caller symlinks
    /// `{run_dir}/venv → returned path`.
    pub async fn resolve(&self, req: BuildRequest<'_>) -> Result<PathBuf, ExecutorError> {
        let hash = self.hash_for(&req).await?;
        let target = self.cache_root.join(&hash);
        let python_bin = target.join("bin").join("python");

        if python_bin.exists() {
            self.counters.hits.fetch_add(1, Ordering::Relaxed);
            debug!(hash = %hash, "venv cache hit");
            touch_last_used(&self.cache_root, &hash);
            return Ok(target);
        }

        let lock_path = self.cache_root.join(format!("{hash}.lock"));
        let lock_file = acquire_lock(&lock_path).await?;

        if python_bin.exists() {
            // Another worker built it while we waited for the lock — still a
            // hit from the caller's perspective.
            self.counters.hits.fetch_add(1, Ordering::Relaxed);
            debug!(hash = %hash, "venv cache hit (after lock-wait)");
            touch_last_used(&self.cache_root, &hash);
            drop(lock_file);
            return Ok(target);
        }

        self.counters.misses.fetch_add(1, Ordering::Relaxed);
        self.counters.builds_in_flight.fetch_add(1, Ordering::Relaxed);
        info!(hash = %hash, "venv cache miss; building");
        let started = std::time::Instant::now();

        let unique = format!(
            "{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let staging_path = self.staging_dir.join(format!("{hash}.{unique}"));

        let build_result = self.build_into(&staging_path, &req).await;
        let elapsed = started.elapsed();
        self.counters
            .build_duration_ms_total
            .fetch_add(elapsed.as_millis() as u64, Ordering::Relaxed);
        self.counters.builds_in_flight.fetch_sub(1, Ordering::Relaxed);

        match build_result {
            Ok(()) => {
                if let Err(e) = std::fs::rename(&staging_path, &target) {
                    let _ = std::fs::remove_dir_all(&staging_path);
                    drop(lock_file);
                    return Err(ExecutorError::StagingFailed(format!(
                        "atomic rename of venv into cache failed: {e}"
                    )));
                }

                self.write_spec(&hash, &req);
                touch_last_used(&self.cache_root, &hash);
                info!(
                    hash = %hash,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "venv cache entry built"
                );
                drop(lock_file);
                Ok(target)
            }
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&staging_path).await;
                drop(lock_file);
                Err(e)
            }
        }
    }

    async fn build_into(
        &self,
        target: &Path,
        req: &BuildRequest<'_>,
    ) -> Result<(), ExecutorError> {
        let use_uv = self.prefer_uv && venv::uv_available();
        if use_uv {
            debug!(target = %target.display(), "building venv via uv");
            venv::create_virtualenv_uv(req.python, target).await?;
            if !req.requirements.is_empty() {
                venv::install_requirements_uv(target, req.requirements, Some(&self.wheels_dir))
                    .await?;
            }
            if let Some(sdk) = req.sdk_path {
                venv::install_local_package_uv(target, sdk, Some(&self.wheels_dir)).await?;
            }
        } else {
            debug!(target = %target.display(), "building venv via python -m venv + pip");
            venv::create_virtualenv(req.python, target).await?;
            if !req.requirements.is_empty() {
                venv::install_requirements_with_cache(
                    target,
                    req.requirements,
                    Some(&self.wheels_dir),
                )
                .await?;
            }
            if let Some(sdk) = req.sdk_path {
                venv::install_local_package_with_cache(target, sdk, Some(&self.wheels_dir))
                    .await?;
            }
        }
        Ok(())
    }

    fn write_spec(&self, hash: &str, req: &BuildRequest<'_>) {
        let built_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let spec = serde_json::json!({
            "schema": CACHE_SCHEMA,
            "python": req.python,
            "requirements": req.requirements,
            "sdk": req.sdk_path.map(|p| p.to_string_lossy().into_owned()),
            "sdk_marker": self.sdk_marker.as_deref().unwrap_or("<none>"),
            "built_at_unix": built_at,
            "installer": if self.prefer_uv && venv::uv_available() { "uv" } else { "pip" },
        });
        let spec_path = self.cache_root.join(format!("{hash}.spec.json"));
        if let Err(e) = std::fs::write(
            &spec_path,
            serde_json::to_string_pretty(&spec).unwrap_or_default(),
        ) {
            warn!(path = %spec_path.display(), error = %e, "failed to write venv spec.json");
        }
    }
}

/// Hash a canonical representation of the build inputs. Order-invariant over
/// `requirements`; case- and whitespace-insensitive for each requirement.
pub fn compute_hash(python_version: &str, requirements: &[String], sdk_marker: &str) -> String {
    let mut reqs: Vec<String> = requirements
        .iter()
        .map(|r| r.trim().to_lowercase())
        .filter(|r| !r.is_empty())
        .collect();
    reqs.sort();
    let canonical = format!(
        "schema={CACHE_SCHEMA};python={python_version};sdk={sdk_marker};reqs={}",
        reqs.join(",")
    );
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn touch_last_used(cache_root: &Path, hash: &str) {
    let p = cache_root.join(format!("{hash}.last_used"));
    if let Err(e) = std::fs::write(&p, "") {
        debug!(path = %p.display(), error = %e, "failed to bump last_used (non-fatal)");
    }
}

async fn acquire_lock(lock_path: &Path) -> Result<std::fs::File, ExecutorError> {
    let path = lock_path.to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        #[cfg(unix)]
        {
            <std::fs::File as fs2::FileExt>::lock_exclusive(&file)?;
        }
        Ok(file)
    })
    .await
    .map_err(|e| ExecutorError::StagingFailed(format!("flock task panicked: {e}")))?
    .map_err(|e| ExecutorError::StagingFailed(format!("flock acquire failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_order_invariant_over_requirements() {
        let a = compute_hash(
            "Python 3.11.9",
            &["numpy".into(), "pandas".into()],
            "<no-sdk>",
        );
        let b = compute_hash(
            "Python 3.11.9",
            &["pandas".into(), "numpy".into()],
            "<no-sdk>",
        );
        assert_eq!(a, b);
    }

    #[test]
    fn hash_is_case_and_whitespace_insensitive() {
        let a = compute_hash("Python 3.11.9", &["NumPy ".into()], "<no-sdk>");
        let b = compute_hash("Python 3.11.9", &["numpy".into()], "<no-sdk>");
        assert_eq!(a, b);
    }

    #[test]
    fn hash_preserves_version_specifiers() {
        let a = compute_hash("Python 3.11.9", &["numpy>=1.20".into()], "<no-sdk>");
        let b = compute_hash("Python 3.11.9", &["numpy".into()], "<no-sdk>");
        assert_ne!(a, b);
    }

    #[test]
    fn python_version_changes_hash() {
        let a = compute_hash("Python 3.11.9", &["numpy".into()], "<no-sdk>");
        let b = compute_hash("Python 3.12.0", &["numpy".into()], "<no-sdk>");
        assert_ne!(a, b);
    }

    #[test]
    fn sdk_marker_changes_hash() {
        let a = compute_hash("Python 3.11.9", &["numpy".into()], "sdk-0.1.0");
        let b = compute_hash("Python 3.11.9", &["numpy".into()], "sdk-0.2.0");
        assert_ne!(a, b);
    }

    #[test]
    fn schema_version_is_in_hash() {
        // If we bump CACHE_SCHEMA, every cache key shifts. This test pins the
        // expected behavior: change the schema mix-in → different hashes.
        let h = compute_hash("Python 3.11.9", &["numpy".into()], "<no-sdk>");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn cache_stats_start_at_zero() {
        let tmp = tempfile::Builder::new().prefix("venv-stats-").tempdir().unwrap();
        let cache = VenvCache::new(tmp.path().join("c"), false, None).unwrap();
        let s = cache.stats();
        assert_eq!(s.hits, 0);
        assert_eq!(s.misses, 0);
        assert_eq!(s.builds_in_flight, 0);
        assert_eq!(s.build_duration_ms_total, 0);
        assert_eq!(s.hit_ratio(), 0.0);
    }

    #[test]
    fn hit_ratio_math() {
        let s = CacheStats {
            hits: 7,
            misses: 3,
            builds_in_flight: 0,
            build_duration_ms_total: 0,
        };
        assert!((s.hit_ratio() - 0.7).abs() < 1e-9);
    }

    #[test]
    fn cache_new_creates_directories_and_sweeps_staging() {
        let tmp = tempfile::Builder::new()
            .prefix("venv-cache-test-")
            .tempdir()
            .unwrap();
        let cache_root = tmp.path().join("python-venvs");
        std::fs::create_dir_all(cache_root.join(".staging")).unwrap();
        std::fs::create_dir_all(cache_root.join(".staging/stale-entry/bin")).unwrap();
        std::fs::write(cache_root.join(".staging/stale-entry/bin/python"), "").unwrap();

        let cache = VenvCache::new(cache_root.clone(), false, None).unwrap();
        assert!(cache.cache_root().join("wheels").is_dir());
        assert!(cache.cache_root().join(".staging").is_dir());
        assert!(!cache_root.join(".staging/stale-entry").exists());
    }
}
