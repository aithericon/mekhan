#[cfg(feature = "python")]
mod venv_cache_tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use aithericon_executor_domain::{
        ExecutionJob, ExecutionStatus, InputDeclaration, InputSource, JobPriority,
        OutputDeclaration,
    };
    use aithericon_executor_python::cache::VenvCache;
    use aithericon_executor_python::{PythonBackend, PythonConfig};
    use aithericon_executor_test_harness::context::ExecutorTestContext;
    use aithericon_executor_test_harness::helpers::assert_status_sequence;
    use aithericon_executor_worker::{BackendRegistry, CleanupPolicy};
    use tempfile::TempDir;
    use uuid::Uuid;

    /// Build a registry whose Python backend uses a fresh VenvCache at `cache_root`.
    /// `prefer_uv=false` keeps tests deterministic — they exercise the pip path
    /// regardless of whether `uv` is installed on the host.
    fn python_cache_registry(cache_root: PathBuf) -> Arc<BackendRegistry> {
        let cache = VenvCache::new(cache_root, false, None).expect("venv cache init");
        let backend = PythonBackend::new().with_venv_cache(Arc::new(cache));
        Arc::new(BackendRegistry::new(Duration::from_secs(60)).register(backend))
    }

    fn venv_job(
        eid: &str,
        code: &str,
        requirements: Vec<String>,
        inputs: Vec<InputDeclaration>,
        outputs: Vec<OutputDeclaration>,
    ) -> ExecutionJob {
        let mut config = PythonConfig {
            script: aithericon_executor_python::INLINE_SCRIPT_NAME.into(),
            python: "python3".into(),
            requirements,
            virtualenv: true,
            env: HashMap::new(),
            working_dir: None,
            inherit_env: true,
            sdk: false,
        };
        // Stage inline script via the same convention as PythonConfig::inline_spec.
        let mut inputs = inputs;
        inputs.insert(
            0,
            InputDeclaration {
                name: aithericon_executor_python::INLINE_SCRIPT_NAME.into(),
                source: InputSource::Raw {
                    content: code.into(),
                },
                required: true,
            },
        );
        config.script = aithericon_executor_python::INLINE_SCRIPT_NAME.into();
        ExecutionJob {
            execution_id: eid.into(),
            spec: config.into_spec_with_io(inputs, outputs),
            metadata: HashMap::new(),
            timeout: None,
            priority: JobPriority::Medium,
            stream_events: None,
            feed_chunks: false,
            wrapped_secrets: None,
        }
    }

    fn count_hash_subdirs(cache_root: &PathBuf) -> usize {
        std::fs::read_dir(cache_root)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        // Skip wheels/, .staging/, and *.lock / *.spec.json / *.last_used files.
                        !name.starts_with('.')
                            && name != "wheels"
                            && !name.contains('.')
                            && e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    /// Cache miss → cache hit: second run is much faster, cache dir persists.
    #[tokio::test]
    async fn cache_miss_then_hit() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let tmp = TempDir::new().expect("tmpdir");
        let cache_root = tmp.path().join("python-venvs");
        let ctx = ExecutorTestContext::new().await;

        let eid1 = format!("venv-miss-{}", Uuid::new_v4().simple());
        let eid2 = format!("venv-hit-{}", Uuid::new_v4().simple());
        let consumer1 = ctx.status_consumer("venv-miss", &eid1).await;
        let consumer2 = ctx.status_consumer("venv-hit", &eid2).await;
        let worker = ctx.spawn_worker_custom(
            CleanupPolicy::Immediate,
            None,
            python_cache_registry(cache_root.clone()),
        );

        let code = "print('hello from cached venv')";

        let t0 = Instant::now();
        ctx.push_job(venv_job(&eid1, code, vec![], vec![], vec![]))
            .await;
        let statuses1 = ctx
            .collect_statuses(&consumer1, Duration::from_secs(60))
            .await;
        let miss_elapsed = t0.elapsed();

        assert_status_sequence(
            &statuses1,
            &[
                ExecutionStatus::Accepted,
                ExecutionStatus::Running,
                ExecutionStatus::Completed,
            ],
        );

        assert_eq!(
            count_hash_subdirs(&cache_root),
            1,
            "exactly one cache entry should exist after first run"
        );

        let t1 = Instant::now();
        ctx.push_job(venv_job(&eid2, code, vec![], vec![], vec![]))
            .await;
        let statuses2 = ctx
            .collect_statuses(&consumer2, Duration::from_secs(60))
            .await;
        let hit_elapsed = t1.elapsed();

        assert_status_sequence(
            &statuses2,
            &[
                ExecutionStatus::Accepted,
                ExecutionStatus::Running,
                ExecutionStatus::Completed,
            ],
        );

        assert_eq!(
            count_hash_subdirs(&cache_root),
            1,
            "second run reuses the cache; no new entry"
        );

        // Hit should be substantially faster than miss.
        // Conservative bound: hit < (miss * 0.75). Real-world ratio is closer to 0.1×.
        assert!(
            hit_elapsed * 4 < miss_elapsed * 3,
            "expected cache hit ({hit_elapsed:?}) to be faster than miss ({miss_elapsed:?})"
        );

        worker.abort();
        ctx.cleanup().await;
    }

    /// Run dir cleanup must not delete the cache target. The symlink at
    /// {run_dir}/venv goes away with the run dir; the cache survives.
    #[tokio::test]
    async fn cleanup_does_not_nuke_cache() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let tmp = TempDir::new().expect("tmpdir");
        let cache_root = tmp.path().join("python-venvs");
        let ctx = ExecutorTestContext::new().await;

        let eid = format!("venv-cleanup-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("venv-cleanup", &eid).await;
        let worker = ctx.spawn_worker_custom(
            CleanupPolicy::Immediate,
            None,
            python_cache_registry(cache_root.clone()),
        );

        ctx.push_job(venv_job(&eid, "print(1)", vec![], vec![], vec![]))
            .await;
        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(60))
            .await;
        assert_eq!(
            statuses.last().map(|s| s.status),
            Some(ExecutionStatus::Completed)
        );

        // Run dir cleanup runs *after* the terminal status is published, so we
        // poll briefly rather than assert synchronously.
        let run_dir = ctx.run_dir_for(&eid).root;
        let deadline = Instant::now() + Duration::from_secs(5);
        while run_dir.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            !run_dir.exists(),
            "run dir {} should be cleaned up by Immediate policy",
            run_dir.display()
        );
        assert_eq!(
            count_hash_subdirs(&cache_root),
            1,
            "cache should still hold the venv after run-dir cleanup"
        );

        // Spot-check: the cached venv's python binary actually exists.
        let hash_dirs: Vec<_> = std::fs::read_dir(&cache_root)
            .unwrap()
            .flatten()
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                !n.starts_with('.') && n != "wheels" && !n.contains('.')
            })
            .collect();
        assert_eq!(hash_dirs.len(), 1);
        assert!(hash_dirs[0].path().join("bin/python").exists());

        worker.abort();
        ctx.cleanup().await;
    }

    /// Different requirements → different cache hashes → two cache entries.
    /// Both empty `requirements: []` and a sentinel value would collide in
    /// the venv lookup; we use distinct PYTHONHASHSEED env vars instead?
    /// No — env doesn't affect the hash. The hash is keyed on (python_version,
    /// requirements, sdk_marker). To force a second cache entry we need either
    /// different python or different requirements.
    ///
    /// We pass `requirements = ["pip"]` (already present, no-op install, fast)
    /// to differentiate from `requirements = []`.
    #[tokio::test]
    async fn distinct_requirements_get_distinct_cache_entries() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let tmp = TempDir::new().expect("tmpdir");
        let cache_root = tmp.path().join("python-venvs");
        let ctx = ExecutorTestContext::new().await;

        let eid_a = format!("venv-a-{}", Uuid::new_v4().simple());
        let eid_b = format!("venv-b-{}", Uuid::new_v4().simple());
        let consumer_a = ctx.status_consumer("venv-a", &eid_a).await;
        let consumer_b = ctx.status_consumer("venv-b", &eid_b).await;
        let worker = ctx.spawn_worker_custom(
            CleanupPolicy::Immediate,
            None,
            python_cache_registry(cache_root.clone()),
        );

        ctx.push_job(venv_job(&eid_a, "print(1)", vec![], vec![], vec![]))
            .await;
        let statuses_a = ctx
            .collect_statuses(&consumer_a, Duration::from_secs(60))
            .await;
        assert_eq!(
            statuses_a.last().map(|s| s.status),
            Some(ExecutionStatus::Completed)
        );

        ctx.push_job(venv_job(
            &eid_b,
            "print(2)",
            vec!["pip".into()],
            vec![],
            vec![],
        ))
        .await;
        let statuses_b = ctx
            .collect_statuses(&consumer_b, Duration::from_secs(120))
            .await;
        assert_eq!(
            statuses_b.last().map(|s| s.status),
            Some(ExecutionStatus::Completed)
        );

        assert_eq!(
            count_hash_subdirs(&cache_root),
            2,
            "two distinct requirement sets should yield two cache entries"
        );

        worker.abort();
        ctx.cleanup().await;
    }

    /// End-to-end proof that a real pip-installed dependency survives the
    /// cache lifecycle: pip install on first run → cache hit on second run →
    /// the package is importable from inside the user script in both cases.
    ///
    /// Uses `six` (single-file, pure-Python, ~80 KB) so the install is fast
    /// and reliable on flaky CI networks. The test asserts the actual version
    /// string appears in stdout, not just that the job succeeded — that way
    /// a silent broken-import would fail the assertion instead of passing
    /// because Python crashed gracefully.
    #[tokio::test]
    async fn requirements_install_and_reuse() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let tmp = TempDir::new().expect("tmpdir");
        let cache_root = tmp.path().join("python-venvs");
        let ctx = ExecutorTestContext::new().await;

        let eid_miss = format!("venv-req-miss-{}", Uuid::new_v4().simple());
        let eid_hit = format!("venv-req-hit-{}", Uuid::new_v4().simple());
        let consumer_miss = ctx.status_consumer("venv-req-miss", &eid_miss).await;
        let consumer_hit = ctx.status_consumer("venv-req-hit", &eid_hit).await;
        let worker = ctx.spawn_worker_custom(
            CleanupPolicy::Immediate,
            None,
            python_cache_registry(cache_root.clone()),
        );

        // Importing `six` and printing the version proves the package is
        // (a) installed in the cached venv and (b) on sys.path when the
        // executor runs the script via the cached interpreter.
        let code = "import six; print(f'six={six.__version__}')";
        let requirements = vec!["six".to_string()];

        // ---- Cache miss: real pip install ----
        let t0 = Instant::now();
        ctx.push_job(venv_job(
            &eid_miss,
            code,
            requirements.clone(),
            vec![],
            vec![],
        ))
        .await;
        let statuses_miss = ctx
            .collect_statuses(&consumer_miss, Duration::from_secs(180))
            .await;
        let miss_elapsed = t0.elapsed();

        let terminal_miss = statuses_miss.last().expect("at least one status");
        if terminal_miss.status != ExecutionStatus::Completed {
            panic!(
                "first run did not complete: status={:?} detail={}",
                terminal_miss.status,
                serde_json::to_string_pretty(&terminal_miss.detail).unwrap_or_default()
            );
        }
        let stdout_miss = terminal_miss.detail["stdout_tail"].as_str().unwrap_or("");
        assert!(
            stdout_miss.contains("six="),
            "first run stdout should show 'six=...'; got: {stdout_miss:?}"
        );

        assert_eq!(
            count_hash_subdirs(&cache_root),
            1,
            "one cache entry after the install"
        );

        // ---- Cache hit: no install, same import works ----
        let t1 = Instant::now();
        ctx.push_job(venv_job(&eid_hit, code, requirements, vec![], vec![]))
            .await;
        let statuses_hit = ctx
            .collect_statuses(&consumer_hit, Duration::from_secs(60))
            .await;
        let hit_elapsed = t1.elapsed();

        let terminal_hit = statuses_hit.last().expect("at least one status");
        assert_eq!(
            terminal_hit.status,
            ExecutionStatus::Completed,
            "second run should complete from cache"
        );
        let stdout_hit = terminal_hit.detail["stdout_tail"].as_str().unwrap_or("");
        assert!(
            stdout_hit.contains("six="),
            "second run stdout should still show 'six=...'; got: {stdout_hit:?}"
        );

        assert_eq!(
            count_hash_subdirs(&cache_root),
            1,
            "cache hit should not create a new entry"
        );

        // Cache hit should be dramatically faster — pip install of `six` is
        // network-dependent, but typically ~3-8s. Hit should be sub-second.
        assert!(
            hit_elapsed * 3 < miss_elapsed,
            "cache hit ({hit_elapsed:?}) should be at least 3× faster than the install miss ({miss_elapsed:?})"
        );

        worker.abort();
        ctx.cleanup().await;
    }

    /// Pre-warm path: a caller (e.g. CI bake step) builds a cache entry
    /// directly via VenvCache::resolve, then a worker job with matching
    /// requirements observes a cache hit. The stats counters reflect both
    /// events (1 miss from warm, 1 hit from job).
    ///
    /// This is the unit-of-behavior that the `aithericon-executor warm-venv`
    /// subcommand wraps: it parses a requirements file and calls resolve.
    #[tokio::test]
    async fn warm_then_use_observes_cache_hit() {
        use aithericon_executor_python::cache::BuildRequest;

        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let tmp = TempDir::new().expect("tmpdir");
        let cache_root = tmp.path().join("python-venvs");
        let ctx = ExecutorTestContext::new().await;

        // Step 1: pre-warm — equivalent to `aithericon-executor warm-venv`.
        let cache = Arc::new(VenvCache::new(cache_root.clone(), false, None).expect("cache init"));
        let reqs: Vec<String> = vec![];
        let req = BuildRequest {
            python: "python3",
            requirements: &reqs,
            sdk_path: None,
        };
        let warmed = cache.resolve(req).await.expect("warm resolve");
        assert!(
            warmed.join("bin/python").exists(),
            "warm path should produce a usable venv"
        );

        let s_after_warm = cache.stats();
        assert_eq!(s_after_warm.misses, 1);
        assert_eq!(s_after_warm.hits, 0);
        assert!(s_after_warm.build_duration_ms_total > 0);
        assert_eq!(s_after_warm.builds_in_flight, 0);

        // Step 2: run a worker job with matching requirements — should hit cache.
        // We hand the SAME Arc<VenvCache> to the backend so stats accumulate.
        let backend = PythonBackend::new().with_venv_cache(cache.clone());
        let registry = Arc::new(BackendRegistry::new(Duration::from_secs(60)).register(backend));

        let eid = format!("warm-hit-{}", Uuid::new_v4().simple());
        let consumer = ctx.status_consumer("warm-hit", &eid).await;
        let worker = ctx.spawn_worker_custom(CleanupPolicy::Immediate, None, registry);

        ctx.push_job(venv_job(&eid, "print('warm hit')", vec![], vec![], vec![]))
            .await;
        let statuses = ctx
            .collect_statuses(&consumer, Duration::from_secs(60))
            .await;
        assert_eq!(
            statuses.last().map(|s| s.status),
            Some(ExecutionStatus::Completed),
            "job should complete from pre-warmed cache"
        );

        let s_after_job = cache.stats();
        assert_eq!(
            s_after_job.misses, 1,
            "no new misses — job hit the warmed entry"
        );
        assert_eq!(s_after_job.hits, 1, "job recorded as a cache hit");
        assert!((s_after_job.hit_ratio() - 0.5).abs() < 1e-9);

        // And the cache directory has exactly one hash entry.
        assert_eq!(count_hash_subdirs(&cache_root), 1);

        worker.abort();
        ctx.cleanup().await;
    }

    /// Two jobs with the same hash, started back-to-back: even with worker
    /// concurrency=2, the flock + atomic-rename pattern must produce exactly
    /// one cache entry and leave no stale `.staging/` dirs.
    #[tokio::test]
    async fn parallel_jobs_same_hash_dedupe() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();

        let tmp = TempDir::new().expect("tmpdir");
        let cache_root = tmp.path().join("python-venvs");
        let ctx = ExecutorTestContext::new().await;

        let eid_a = format!("venv-par-a-{}", Uuid::new_v4().simple());
        let eid_b = format!("venv-par-b-{}", Uuid::new_v4().simple());
        let consumer_a = ctx.status_consumer("venv-par-a", &eid_a).await;
        let consumer_b = ctx.status_consumer("venv-par-b", &eid_b).await;
        let worker = ctx.spawn_worker_custom(
            CleanupPolicy::Immediate,
            None,
            python_cache_registry(cache_root.clone()),
        );

        // Push both before collecting either — they race for the worker's
        // concurrency slots and the cache lock.
        ctx.push_job(venv_job(&eid_a, "print('a')", vec![], vec![], vec![]))
            .await;
        ctx.push_job(venv_job(&eid_b, "print('b')", vec![], vec![], vec![]))
            .await;

        let statuses_a = ctx
            .collect_statuses(&consumer_a, Duration::from_secs(60))
            .await;
        let statuses_b = ctx
            .collect_statuses(&consumer_b, Duration::from_secs(60))
            .await;

        assert_eq!(
            statuses_a.last().map(|s| s.status),
            Some(ExecutionStatus::Completed)
        );
        assert_eq!(
            statuses_b.last().map(|s| s.status),
            Some(ExecutionStatus::Completed)
        );

        assert_eq!(
            count_hash_subdirs(&cache_root),
            1,
            "identical hashes must collapse to one cache entry under contention"
        );

        let staging = cache_root.join(".staging");
        let leftover = std::fs::read_dir(&staging)
            .map(|i| i.flatten().count())
            .unwrap_or(0);
        assert_eq!(
            leftover, 0,
            ".staging/ should be empty after both jobs finish"
        );

        worker.abort();
        ctx.cleanup().await;
    }
}
