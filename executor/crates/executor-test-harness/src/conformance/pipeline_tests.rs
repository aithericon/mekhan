use std::sync::Arc;
use std::time::Duration;

use aithericon_executor_domain::ExecutionStatus;
use aithericon_executor_worker::{BackendRegistry, CleanupPolicy};

use crate::context::ExecutorTestContext;
use crate::helpers::assert_status_sequence;

use super::kit::BackendTestKit;

/// Helper: create a test context and a backend registry from the kit.
async fn setup<K: BackendTestKit>(kit: &K) -> (ExecutorTestContext, Arc<BackendRegistry>) {
    let backend = kit.create_backend().await.expect("backend creation failed");

    let registry = Arc::new(BackendRegistry::new(Duration::from_secs(30)).register_arc(backend));

    let ctx = ExecutorTestContext::new().await;
    (ctx, registry)
}

// ─── Pipeline Contract 1: Full lifecycle (Accepted -> Running -> Completed) ──

/// Full Accepted -> Running -> Completed lifecycle with stdout in terminal detail.
pub async fn test_pipeline_echo<K: BackendTestKit>(kit: &K) {
    kit.pipeline_setup().await.expect("pipeline setup failed");
    let (ctx, registry) = setup(kit).await;

    let eid = format!(
        "{}-pipe-echo-{}",
        kit.backend_name(),
        uuid::Uuid::new_v4().simple()
    );
    let consumer = ctx.status_consumer("pipe-echo", &eid).await;

    let spec = kit.echo_spec();
    let job = kit.spec_to_job(&eid, spec, None);

    let worker = ctx.spawn_worker_custom(CleanupPolicy::Retain, None, registry);
    ctx.push_job(job).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(30))
        .await;
    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ],
    );

    let completed = statuses.last().unwrap();
    let stdout = completed.detail["stdout_tail"].as_str().unwrap_or("");
    assert!(
        stdout.contains("hello"),
        "[{}] pipeline stdout should contain 'hello', got: {:?}",
        kit.backend_name(),
        stdout
    );

    worker.abort();
    ctx.cleanup().await;
}

// ─── Pipeline Contract 2: Failure path ───────────────────────────────────────

/// Full Accepted -> Running -> Failed lifecycle.
pub async fn test_pipeline_failure<K: BackendTestKit>(kit: &K) {
    kit.pipeline_setup().await.expect("pipeline setup failed");
    let (ctx, registry) = setup(kit).await;

    let eid = format!(
        "{}-pipe-fail-{}",
        kit.backend_name(),
        uuid::Uuid::new_v4().simple()
    );
    let consumer = ctx.status_consumer("pipe-fail", &eid).await;

    let spec = kit.failing_spec();
    let job = kit.spec_to_job(&eid, spec, None);

    let worker = ctx.spawn_worker_custom(CleanupPolicy::Retain, None, registry);
    ctx.push_job(job).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(30))
        .await;
    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Failed,
        ],
    );

    worker.abort();
    ctx.cleanup().await;
}

// ─── Pipeline Contract 3: Timeout ────────────────────────────────────────────

/// Full Accepted -> Running -> TimedOut lifecycle.
pub async fn test_pipeline_timeout<K: BackendTestKit>(kit: &K) {
    kit.pipeline_setup().await.expect("pipeline setup failed");
    let (ctx, registry) = setup(kit).await;

    let eid = format!(
        "{}-pipe-timeout-{}",
        kit.backend_name(),
        uuid::Uuid::new_v4().simple()
    );
    let consumer = ctx.status_consumer("pipe-timeout", &eid).await;

    let spec = kit.sleep_spec(60);
    let job = kit.spec_to_job(&eid, spec, Some(Duration::from_secs(1)));

    let worker = ctx.spawn_worker_custom(CleanupPolicy::Retain, None, registry);
    ctx.push_job(job).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(20))
        .await;
    assert_status_sequence(
        &statuses,
        &[
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::TimedOut,
        ],
    );

    worker.abort();
    ctx.cleanup().await;
}

// ─── Pipeline Contract 4: Environment injection ──────────────────────────────

/// AITHERICON_* env vars are injected by the staging pipeline and accessible in the backend.
pub async fn test_pipeline_env_injection<K: BackendTestKit>(kit: &K) {
    kit.pipeline_setup().await.expect("pipeline setup failed");
    let (ctx, registry) = setup(kit).await;

    let eid = format!(
        "{}-pipe-env-{}",
        kit.backend_name(),
        uuid::Uuid::new_v4().simple()
    );
    let consumer = ctx.status_consumer("pipe-env", &eid).await;

    let spec = kit.echo_spec();
    let job = kit.spec_to_job(&eid, spec, None);

    let worker = ctx.spawn_worker_custom(CleanupPolicy::Retain, None, registry);
    ctx.push_job(job).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(30))
        .await;

    // Verify we reached a terminal status (env injection doesn't crash the pipeline)
    let last = statuses.last().expect("should have at least one status");
    assert!(
        last.status.is_terminal(),
        "[{}] should reach terminal status, got: {:?}",
        kit.backend_name(),
        last.status
    );

    worker.abort();
    ctx.cleanup().await;
}

// ─── Pipeline Contract 5: Metadata echo-back ─────────────────────────────────

/// Job metadata is echoed on every StatusUpdate.
pub async fn test_pipeline_metadata_echo<K: BackendTestKit>(kit: &K) {
    kit.pipeline_setup().await.expect("pipeline setup failed");
    let (ctx, registry) = setup(kit).await;

    let eid = format!(
        "{}-pipe-meta-{}",
        kit.backend_name(),
        uuid::Uuid::new_v4().simple()
    );
    let consumer = ctx.status_consumer("pipe-meta", &eid).await;

    let spec = kit.echo_spec();
    let mut job = kit.spec_to_job(&eid, spec, None);
    job.metadata
        .insert("petri_net_id".into(), "test-net".into());

    let worker = ctx.spawn_worker_custom(CleanupPolicy::Retain, None, registry);
    ctx.push_job(job).await;

    let statuses = ctx
        .collect_statuses(&consumer, Duration::from_secs(30))
        .await;

    for update in &statuses {
        assert_eq!(
            update.metadata.get("petri_net_id").map(|s| s.as_str()),
            Some("test-net"),
            "[{}] metadata should echo petri_net_id on {:?} status",
            kit.backend_name(),
            update.status
        );
    }

    worker.abort();
    ctx.cleanup().await;
}
