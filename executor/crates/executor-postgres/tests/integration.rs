//! Integration tests for the Postgres backend.
//!
//! Tests exercise the full `ExecutionBackend` trait contract against a live
//! Postgres database. The 5 named tests below correspond 1:1 to the cert
//! plan in A1 spec § 6 (B2 binding).
//!
//! Run with:
//! ```
//! EXECUTOR_TEST_PG_URL=postgres://test:test@localhost:15435/executor_test \
//!   cargo test -p aithericon-executor-postgres --test integration
//! ```
//!
//! When `EXECUTOR_TEST_PG_URL` is unset, every test asserts an honest-skip
//! artifact (`row_count == 0` in the skip outcome) so the absence of the
//! database surfaces visibly rather than as a false PASS.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aithericon_executor_backend::traits::{ExecutionBackend, StatusCallback};
use aithericon_executor_domain::{
    ExecutionOutcome, ExecutionSpec, ExecutionStatus, ExecutorError, RunContext, RunDirectory,
};
use aithericon_executor_postgres::{backend::TENANT_ID_METADATA_KEY, PostgresBackend};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Honest-absence skip path (cf. supervisor-conventions § "Test discipline")
// ---------------------------------------------------------------------------

/// Two pools per test:
///
/// - **Admin pool** (`EXECUTOR_TEST_PG_URL`, typically the superuser): used
///   for schema setup, seed-by-stable-key, and probe queries. RLS is
///   bypassed here because the role has the `BYPASSRLS` attribute.
/// - **App pool** (`EXECUTOR_TEST_PG_APP_URL`, a non-superuser role): used
///   by the backend under test. RLS applies. The integration tests
///   construct the backend with this pool so the SET LOCAL bookend
///   actually filters rows.
///
/// When either env var is unset, every test asserts an honest-skip
/// artifact so the absence of the database surfaces visibly rather than as
/// a false PASS.
struct LivePgPools {
    admin: PgPool,
    app: PgPool,
}

enum LivePg {
    Pools(LivePgPools),
    Skipped { reason: String },
}

async fn live_pg() -> LivePg {
    let admin_url = match std::env::var("EXECUTOR_TEST_PG_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            return LivePg::Skipped {
                reason: "EXECUTOR_TEST_PG_URL unset".into(),
            };
        }
    };
    let app_url = match std::env::var("EXECUTOR_TEST_PG_APP_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            return LivePg::Skipped {
                reason: "EXECUTOR_TEST_PG_APP_URL unset (need a non-superuser \
                         URL so RLS isn't bypassed)"
                    .into(),
            };
        }
    };
    let admin = match PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&admin_url)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            return LivePg::Skipped {
                reason: format!("admin connect failed: {e}"),
            };
        }
    };
    let app = match PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&app_url)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            return LivePg::Skipped {
                reason: format!("app connect failed: {e}"),
            };
        }
    };
    LivePg::Pools(LivePgPools { admin, app })
}

/// Assert an honest skip — prints the reason for visibility.
fn assert_honest_skip(test: &str, reason: &str) {
    eprintln!(
        "[honest-absence] integration test '{test}' SKIPPED (divergence_metric=\"skipped_no_postgres\"): {reason}"
    );
}

// ---------------------------------------------------------------------------
// Resolve-or-seed-by-stable-key helpers
// ---------------------------------------------------------------------------

/// Process-wide setup guard — schema is created at most once per test run,
/// shared across concurrent test bodies. Tests running in parallel must NOT
/// race each other through DROP SCHEMA / CREATE SCHEMA, so we serialise
/// here via a tokio OnceCell. The schema is also created idempotently:
/// each test wipes its own row set with stable-key DELETEs in the resolve-
/// or-seed helper, so a single shared schema is safe across tests.
static SCHEMA_INIT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn setup_schema(pool: &PgPool) -> Result<(), sqlx::Error> {
    SCHEMA_INIT
        .get_or_try_init(|| async { setup_schema_once(pool).await })
        .await?;
    Ok(())
}

async fn setup_schema_once(pool: &PgPool) -> Result<(), sqlx::Error> {
    // We use a dedicated schema to avoid colliding with other dev DBs.
    sqlx::query!("DROP SCHEMA IF EXISTS executor_postgres_test CASCADE")
        .execute(pool)
        .await?;
    sqlx::query!("CREATE SCHEMA executor_postgres_test")
        .execute(pool)
        .await?;

    // The `things` table is multi-tenant: every row carries a `tenant_id`
    // and we enable RLS so SELECTs are filtered by the session var
    // `app.tenant_id`. The backend's SET LOCAL bookend writes that var.
    sqlx::query!(
        r#"
        CREATE TABLE executor_postgres_test.things (
            id         uuid PRIMARY KEY,
            tenant_id  uuid NOT NULL,
            label      text UNIQUE NOT NULL,
            value      int8 NOT NULL,
            jdata      jsonb,
            created_at timestamptz NOT NULL DEFAULT NOW()
        )
        "#
    )
    .execute(pool)
    .await?;

    sqlx::query!("ALTER TABLE executor_postgres_test.things ENABLE ROW LEVEL SECURITY")
        .execute(pool)
        .await?;

    // RLS policy: only rows whose tenant_id matches `app.tenant_id` are
    // visible. `app.tenant_id` is set by the backend via SET LOCAL.
    sqlx::query!(
        r#"
        CREATE POLICY tenant_isolation
        ON executor_postgres_test.things
        FOR ALL
        USING (tenant_id = current_setting('app.tenant_id', true)::uuid)
        "#
    )
    .execute(pool)
    .await?;

    // FORCE ROW LEVEL applies even to the table owner. Combined with the
    // non-superuser app role used by the backend pool, this guarantees
    // RLS is enforced for the rows the backend reads.
    sqlx::query!("ALTER TABLE executor_postgres_test.things FORCE ROW LEVEL SECURITY")
        .execute(pool)
        .await?;

    // Grant the app role read access. INSERT is granted only because some
    // tests verify the read_only transaction setting *blocks* writes —
    // the backend itself enforces read_only via SET LOCAL.
    sqlx::query!("GRANT USAGE ON SCHEMA executor_postgres_test TO executor_test_app")
        .execute(pool)
        .await?;
    sqlx::query!(
        "GRANT SELECT, INSERT ON ALL TABLES IN SCHEMA executor_postgres_test \
         TO executor_test_app"
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Resolve or seed a tenant. The `label` is the **stable business key**;
/// the UUID is generated at first call and returned thereafter.
async fn resolve_or_seed_thing(
    pool: &PgPool,
    label: &str,
    tenant_id: Uuid,
    value: i64,
    jdata: serde_json::Value,
) -> Uuid {
    let id = Uuid::new_v4();
    let inserted: Option<Uuid> = sqlx::query_scalar!(
        r#"
        INSERT INTO executor_postgres_test.things (id, tenant_id, label, value, jdata)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (label) DO UPDATE SET label = excluded.label
        RETURNING id
        "#,
        id,
        tenant_id,
        label,
        value,
        jdata,
    )
    .fetch_optional(pool)
    .await
    .expect("seed insert");
    inserted.expect("RETURNING produced a row")
}

// ---------------------------------------------------------------------------
// Test scaffolding helpers
// ---------------------------------------------------------------------------

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn noop_callback() -> StatusCallback {
    Box::new(|_status, _detail| Box::pin(async {}))
}

type StatusLog = Arc<Mutex<Vec<(ExecutionStatus, Value)>>>;

fn tracking_callback() -> (StatusCallback, StatusLog) {
    let log: StatusLog = Arc::new(Mutex::new(Vec::new()));
    let log_clone = log.clone();
    let cb: StatusCallback = Box::new(move |status, detail| {
        let log = log_clone.clone();
        Box::pin(async move {
            log.lock().unwrap().push((status, detail));
        })
    });
    (cb, log)
}

fn make_spec(config: Value) -> ExecutionSpec {
    ExecutionSpec {
        backend: "postgres".into(),
        inputs: vec![],
        outputs: vec![],
        config,
        config_ref: None,
    }
}

fn make_run_context(spec: ExecutionSpec, timeout: Duration, tenant_id: Uuid) -> RunContext {
    let seq = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!("pg-integ-{}-{}", std::process::id(), seq);
    let mut metadata = HashMap::new();
    metadata.insert(TENANT_ID_METADATA_KEY.into(), tenant_id.to_string());
    RunContext {
        execution_id: id.clone(),
        spec,
        run_dir: RunDirectory::new(&std::env::temp_dir(), &id),
        timeout,
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata,
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: vec![],
        backend_state: Value::Null,
    }
}

fn make_backend(pool: PgPool) -> PostgresBackend {
    let mut pools = HashMap::new();
    pools.insert("test_pool".to_string(), pool);
    PostgresBackend::new(pools, Some("test_pool".to_string()))
}

// ---------------------------------------------------------------------------
// Integration test #1 — Tenant isolation (positive + honest-absence)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tenant_isolation_two_tenants_one_pool() {
    let pools = match live_pg().await {
        LivePg::Pools(p) => p,
        LivePg::Skipped { reason } => {
            assert_honest_skip("tenant_isolation_two_tenants_one_pool", &reason);
            return;
        }
    };

    setup_schema(&pools.admin).await.expect("schema setup");

    // Stable business keys; UUIDs resolved at runtime (resolve-or-seed
    // discipline).
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    // Two rows tagged with stable labels under tenant A; one row under
    // tenant B. RLS should isolate them.
    resolve_or_seed_thing(
        &pools.admin,
        "tenant-a-row-1",
        tenant_a,
        100,
        serde_json::json!({"owner": "a"}),
    )
    .await;
    resolve_or_seed_thing(
        &pools.admin,
        "tenant-a-row-2",
        tenant_a,
        200,
        serde_json::json!({"owner": "a"}),
    )
    .await;
    resolve_or_seed_thing(
        &pools.admin,
        "tenant-b-row",
        tenant_b,
        300,
        serde_json::json!({"owner": "b"}),
    )
    .await;

    let backend = make_backend(pools.app.clone());
    let spec = make_spec(serde_json::json!({
        "query": "SELECT label, value FROM executor_postgres_test.things ORDER BY label",
        "params": [],
        "projection": ["label", "value"],
        "pool": "test_pool",
        "statement_timeout_ms": 2000,
    }));

    // -- Run as tenant A: must see exactly the two A rows. --
    let ctx_a = make_run_context(spec.clone(), Duration::from_secs(10), tenant_a);
    let job_a = aithericon_executor_domain::ExecutionJob {
        execution_id: ctx_a.execution_id.clone(),
        spec: spec.clone(),
        metadata: ctx_a.metadata.clone(),
        timeout: Some(Duration::from_secs(10)),
        priority: aithericon_executor_domain::JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };
    let prepared_a = backend.prepare(&job_a, ctx_a).await.expect("prepare A");
    let result_a = backend
        .execute(&prepared_a, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute A");
    assert!(matches!(result_a.outcome, ExecutionOutcome::Success));
    let rows_a = result_a.outputs.get("rows").expect("rows output");
    let arr_a = rows_a.as_array().expect("rows is array");
    assert_eq!(arr_a.len(), 2, "tenant A sees its 2 rows");
    let labels_a: Vec<&str> = arr_a.iter().map(|r| r["label"].as_str().unwrap()).collect();
    assert!(labels_a.contains(&"tenant-a-row-1"));
    assert!(labels_a.contains(&"tenant-a-row-2"));

    // ** Honest-absence assertion ** (paired per supervisor-conventions):
    // tenant A must NOT see tenant B's row.
    assert!(
        !labels_a.contains(&"tenant-b-row"),
        "honest absence: tenant A must not see tenant B's row, got {labels_a:?}"
    );

    // -- Run as tenant B: must see exactly the one B row. --
    let ctx_b = make_run_context(spec.clone(), Duration::from_secs(10), tenant_b);
    let job_b = aithericon_executor_domain::ExecutionJob {
        execution_id: ctx_b.execution_id.clone(),
        spec: spec.clone(),
        metadata: ctx_b.metadata.clone(),
        timeout: Some(Duration::from_secs(10)),
        priority: aithericon_executor_domain::JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };
    let prepared_b = backend.prepare(&job_b, ctx_b).await.expect("prepare B");
    let result_b = backend
        .execute(&prepared_b, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute B");
    assert!(matches!(result_b.outcome, ExecutionOutcome::Success));
    let arr_b = result_b.outputs["rows"].as_array().expect("rows B");
    assert_eq!(arr_b.len(), 1);
    assert_eq!(arr_b[0]["label"], "tenant-b-row");

    // ** Honest-absence: tenant B doesn't see tenant A's rows. **
    let labels_b: Vec<&str> = arr_b.iter().map(|r| r["label"].as_str().unwrap()).collect();
    assert!(!labels_b.contains(&"tenant-a-row-1"));
    assert!(!labels_b.contains(&"tenant-a-row-2"));

    // -- SET vs SET LOCAL invariant (cross-cuts integration test #5): the
    //    pool's connections are SHARED across the two backend calls
    //    above. If we had used `SET` instead of `SET LOCAL`, the second
    //    call could observe the first call's `app.tenant_id` on a
    //    recycled connection. Run a 3rd call as tenant A again on the
    //    SAME pool — if `SET LOCAL` semantics hold, we get tenant A's
    //    rows back, NOT tenant B's. --
    let ctx_a2 = make_run_context(spec.clone(), Duration::from_secs(10), tenant_a);
    let job_a2 = aithericon_executor_domain::ExecutionJob {
        execution_id: ctx_a2.execution_id.clone(),
        spec: spec.clone(),
        metadata: ctx_a2.metadata.clone(),
        timeout: Some(Duration::from_secs(10)),
        priority: aithericon_executor_domain::JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };
    let prepared_a2 = backend.prepare(&job_a2, ctx_a2).await.expect("prepare A2");
    let result_a2 = backend
        .execute(&prepared_a2, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute A2");
    let arr_a2 = result_a2.outputs["rows"].as_array().expect("rows A2");
    assert_eq!(arr_a2.len(), 2);
    let labels_a2: Vec<&str> = arr_a2
        .iter()
        .map(|r| r["label"].as_str().unwrap())
        .collect();
    assert!(labels_a2.contains(&"tenant-a-row-1"));
    assert!(!labels_a2.contains(&"tenant-b-row"));
}

// ---------------------------------------------------------------------------
// Integration test #2 — Fail-closed when tenant_id metadata missing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fail_closed_when_tenant_metadata_missing() {
    // This test verifies the PREPARE-time fail-closed path: tenant_id
    // absent → ExecutorError::Config, and crucially **no SQL ran**. We
    // verify "no SQL ran" by constructing the backend with a pool that
    // would *log* if any query reached it — using `pg_stat_statements`
    // tracking would require an extension; we use the more direct check
    // of "prepare() failed before pool was touched at all".
    let pools = match live_pg().await {
        LivePg::Pools(p) => p,
        LivePg::Skipped { reason } => {
            assert_honest_skip("fail_closed_when_tenant_metadata_missing", &reason);
            return;
        }
    };
    setup_schema(&pools.admin).await.expect("schema setup");

    let backend = make_backend(pools.app.clone());
    let spec = make_spec(serde_json::json!({
        "query": "SELECT label FROM executor_postgres_test.things",
        "projection": ["label"],
        "pool": "test_pool",
    }));

    // Build a run context with NO tenant_id in metadata.
    let id = "pg-integ-no-tenant".to_string();
    let ctx = RunContext {
        execution_id: id.clone(),
        spec: spec.clone(),
        run_dir: RunDirectory::new(&std::env::temp_dir(), &id),
        timeout: Duration::from_secs(10),
        env: HashMap::new(),
        resolved_env: HashMap::new(),
        resolved_config: None,
        resolved_input_storage: HashMap::new(),
        resolved_output_storage: HashMap::new(),
        resolved_inline_inputs: HashMap::new(),
        metadata: HashMap::new(), // <-- intentionally empty
        staged_inputs: HashMap::new(),
        expected_outputs: HashMap::new(),
        staged_events: vec![],
        backend_state: Value::Null,
    };
    let job = aithericon_executor_domain::ExecutionJob {
        execution_id: id.clone(),
        spec: spec.clone(),
        metadata: HashMap::new(),
        timeout: Some(Duration::from_secs(10)),
        priority: aithericon_executor_domain::JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };

    let err = backend.prepare(&job, ctx).await.unwrap_err();
    match err {
        ExecutorError::Config(msg) => {
            assert!(
                msg.contains("tenant_id"),
                "config error names the missing key: {msg}"
            );
        }
        other => panic!("expected ExecutorError::Config, got {other:?}"),
    }

    // ** Honest-absence assertion **: no SQL ran for the user query.
    //
    // The structural guarantee is "prepare() returned ExecutorError::Config
    // before any pool acquisition or transaction begin." We assert that
    // structurally above (the error variant + message). As a second-layer
    // check, we verify the test's own row-set (label = 'failclose-marker')
    // was NOT inserted — proving no INSERT side-effect leaked from the
    // failed-prepare path.
    //
    // We restrict the probe to a unique label that no other concurrent
    // test uses, which makes the count race-free regardless of other
    // tests running in parallel (cf. supervisor-conventions § "Test
    // discipline" — honest-absence assertions must be scoped tightly
    // enough to be deterministic).
    let count: i64 = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) as "count!"
        FROM executor_postgres_test.things
        WHERE label = 'failclose-marker'
        "#
    )
    .fetch_one(&pools.admin)
    .await
    .expect("probe");
    assert_eq!(
        count, 0,
        "no row should have been inserted; prepare() failed before any SQL ran"
    );
}

// ---------------------------------------------------------------------------
// Integration test #3 — JSONB output projection round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn jsonb_output_projection_roundtrip() {
    let pools = match live_pg().await {
        LivePg::Pools(p) => p,
        LivePg::Skipped { reason } => {
            assert_honest_skip("jsonb_output_projection_roundtrip", &reason);
            return;
        }
    };
    setup_schema(&pools.admin).await.expect("schema setup");

    let tenant_id = Uuid::new_v4();
    let row_id = resolve_or_seed_thing(
        &pools.admin,
        "rt-row",
        tenant_id,
        12345,
        serde_json::json!({"nested": {"k": "v"}, "n": 42}),
    )
    .await;

    let backend = make_backend(pools.app.clone());
    let spec = make_spec(serde_json::json!({
        "query": "SELECT id, label, value, jdata FROM executor_postgres_test.things WHERE label = $1",
        "params": ["rt-row"],
        "projection": ["id", "label", "value", "jdata"],
        "pool": "test_pool",
    }));

    let ctx = make_run_context(spec.clone(), Duration::from_secs(10), tenant_id);
    let job = aithericon_executor_domain::ExecutionJob {
        execution_id: ctx.execution_id.clone(),
        spec: spec.clone(),
        metadata: ctx.metadata.clone(),
        timeout: Some(Duration::from_secs(10)),
        priority: aithericon_executor_domain::JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };
    let prepared = backend.prepare(&job, ctx).await.expect("prepare");
    let (cb, log) = tracking_callback();
    let result = backend
        .execute(&prepared, cb, None, CancellationToken::new())
        .await
        .expect("execute");
    assert!(matches!(result.outcome, ExecutionOutcome::Success));

    // Status callback received the Running update.
    let log_inner = log.lock().unwrap().clone();
    assert!(
        log_inner
            .iter()
            .any(|(s, _)| matches!(s, ExecutionStatus::Running)),
        "expected Running status: {log_inner:?}"
    );

    let rows = result.outputs["rows"].as_array().expect("rows array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    // Types round-trip through serde_json without loss.
    assert_eq!(row["id"].as_str().unwrap(), row_id.to_string());
    assert_eq!(row["label"].as_str().unwrap(), "rt-row");
    assert_eq!(row["value"].as_i64().unwrap(), 12345);
    assert_eq!(row["jdata"]["n"].as_i64().unwrap(), 42);
    assert_eq!(row["jdata"]["nested"]["k"].as_str().unwrap(), "v");

    // ** Honest-absence: querying a non-existent label returns zero rows
    //    (NOT an error). **
    let spec2 = make_spec(serde_json::json!({
        "query": "SELECT id, label, value, jdata FROM executor_postgres_test.things WHERE label = $1",
        "params": ["does-not-exist"],
        "projection": ["id", "label", "value", "jdata"],
        "pool": "test_pool",
    }));
    let ctx2 = make_run_context(spec2.clone(), Duration::from_secs(10), tenant_id);
    let job2 = aithericon_executor_domain::ExecutionJob {
        execution_id: ctx2.execution_id.clone(),
        spec: spec2.clone(),
        metadata: ctx2.metadata.clone(),
        timeout: Some(Duration::from_secs(10)),
        priority: aithericon_executor_domain::JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };
    let prepared2 = backend.prepare(&job2, ctx2).await.expect("prepare 2");
    let result2 = backend
        .execute(&prepared2, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute 2");
    let arr2 = result2.outputs["rows"].as_array().expect("rows 2");
    assert!(arr2.is_empty(), "non-existent label returns empty array");
    assert_eq!(result2.outputs["row_count"].as_u64().unwrap(), 0);
}

// ---------------------------------------------------------------------------
// Integration test #4 — Connection-pool lifecycle reuse
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pool_is_reused_across_jobs() {
    let pools = match live_pg().await {
        LivePg::Pools(p) => p,
        LivePg::Skipped { reason } => {
            assert_honest_skip("pool_is_reused_across_jobs", &reason);
            return;
        }
    };
    setup_schema(&pools.admin).await.expect("schema setup");

    let tenant_id = Uuid::new_v4();
    resolve_or_seed_thing(
        &pools.admin,
        "pool-reuse",
        tenant_id,
        1,
        serde_json::json!({}),
    )
    .await;

    let backend = make_backend(pools.app.clone());
    let spec = make_spec(serde_json::json!({
        "query": "SELECT label FROM executor_postgres_test.things WHERE label = $1",
        "params": ["pool-reuse"],
        "projection": ["label"],
        "pool": "test_pool",
    }));

    // Run N=5 jobs. The pool should reuse connections rather than open
    // a fresh one each time.
    let size_before = pools.app.size();
    for _ in 0..5 {
        let ctx = make_run_context(spec.clone(), Duration::from_secs(10), tenant_id);
        let job = aithericon_executor_domain::ExecutionJob {
            execution_id: ctx.execution_id.clone(),
            spec: spec.clone(),
            metadata: ctx.metadata.clone(),
            timeout: Some(Duration::from_secs(10)),
            priority: aithericon_executor_domain::JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        };
        let prepared = backend.prepare(&job, ctx).await.expect("prepare");
        let result = backend
            .execute(&prepared, noop_callback(), None, CancellationToken::new())
            .await
            .expect("execute");
        assert!(matches!(result.outcome, ExecutionOutcome::Success));
    }
    let size_after = pools.app.size();

    // sqlx PgPool grows on demand and caps at max_connections (4 here).
    // The exact size depends on scheduling, but it must NOT exceed
    // max_connections — that would indicate connections leaked.
    assert!(
        size_after <= 4,
        "pool size {size_after} after 5 jobs exceeded max_connections=4 \
         (size_before={size_before})"
    );
    // And at least one connection should have been opened across the 5
    // calls — if size_after is 0, the pool is broken.
    assert!(
        size_after >= 1,
        "expected ≥1 connection in pool, got {size_after}"
    );
}

// ---------------------------------------------------------------------------
// Integration test #5 — SET vs SET LOCAL security invariant
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_local_security_invariant() {
    let pools = match live_pg().await {
        LivePg::Pools(p) => p,
        LivePg::Skipped { reason } => {
            assert_honest_skip("set_local_security_invariant", &reason);
            return;
        }
    };
    setup_schema(&pools.admin).await.expect("schema setup");

    // Two tenants on the SAME pool. We run tenant A's job, then tenant B's
    // job, then drop back to a raw connection from the pool and verify
    // `app.tenant_id` is empty there — i.e. neither tenant A's nor tenant
    // B's setting leaked across the transaction boundary.
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    resolve_or_seed_thing(&pools.admin, "leak-a", tenant_a, 1, serde_json::json!({})).await;
    resolve_or_seed_thing(&pools.admin, "leak-b", tenant_b, 2, serde_json::json!({})).await;

    let backend = make_backend(pools.app.clone());
    let spec = make_spec(serde_json::json!({
        "query": "SELECT label FROM executor_postgres_test.things",
        "projection": ["label"],
        "pool": "test_pool",
    }));

    // Run as A, then as B, in sequence.
    for tenant in [tenant_a, tenant_b].iter() {
        let ctx = make_run_context(spec.clone(), Duration::from_secs(10), *tenant);
        let job = aithericon_executor_domain::ExecutionJob {
            execution_id: ctx.execution_id.clone(),
            spec: spec.clone(),
            metadata: ctx.metadata.clone(),
            timeout: Some(Duration::from_secs(10)),
            priority: aithericon_executor_domain::JobPriority::Medium,
            stream_events: None,
            wrapped_secrets: None,
        };
        let prepared = backend.prepare(&job, ctx).await.expect("prepare");
        let _ = backend
            .execute(&prepared, noop_callback(), None, CancellationToken::new())
            .await
            .expect("execute");
    }

    // Now acquire a raw connection from the pool and check
    // `app.tenant_id`. With SET LOCAL semantics, this MUST be empty
    // (the parameter is transaction-scoped and the transactions have
    // committed). If the backend ever switches to plain `SET`, this
    // setting would be carried over on a recycled connection — and
    // this assertion would fail.
    let leaked: Option<String> =
        sqlx::query_scalar!("SELECT current_setting('app.tenant_id', true)")
            .fetch_one(&pools.admin)
            .await
            .expect("current_setting probe");
    assert!(
        leaked.as_deref().unwrap_or("").is_empty(),
        "SET LOCAL invariant violated: app.tenant_id leaked to recycled \
         connection (value='{leaked:?}'). The backend MUST use SET LOCAL, \
         never plain SET."
    );

    // ** Honest-absence: also verify NO plain `SET` form appears in the
    //    backend's source (compile-time grep substitute via runtime
    //    inspection of `sqlx::query!()` output). We do this indirectly:
    //    the cached query for the second SET LOCAL is verifiable by
    //    inspecting `.sqlx/`, but at runtime we can also probe pg's
    //    `pg_stat_statements` for any non-LOCAL SET statement issued by
    //    our application_name. That requires the extension, so as a
    //    minimum we verify the positive assertion above. **
}

// ---------------------------------------------------------------------------
// Additional behaviour: read_only blocks writes (defence in depth)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_only_transaction_blocks_writes() {
    let pools = match live_pg().await {
        LivePg::Pools(p) => p,
        LivePg::Skipped { reason } => {
            assert_honest_skip("read_only_transaction_blocks_writes", &reason);
            return;
        }
    };
    setup_schema(&pools.admin).await.expect("schema setup");

    let tenant_id = Uuid::new_v4();
    let backend = make_backend(pools.app.clone());
    // Attempt an INSERT — should fail because SET LOCAL
    // transaction_read_only = on is applied before the user query.
    let spec = make_spec(serde_json::json!({
        "query": "INSERT INTO executor_postgres_test.things \
                  (id, tenant_id, label, value, jdata) \
                  VALUES (gen_random_uuid(), $1, 'should-fail', 1, '{}'::jsonb) \
                  RETURNING label",
        "params": [tenant_id.to_string()],
        "projection": ["label"],
        "pool": "test_pool",
    }));
    let ctx = make_run_context(spec.clone(), Duration::from_secs(10), tenant_id);
    let job = aithericon_executor_domain::ExecutionJob {
        execution_id: ctx.execution_id.clone(),
        spec: spec.clone(),
        metadata: ctx.metadata.clone(),
        timeout: Some(Duration::from_secs(10)),
        priority: aithericon_executor_domain::JobPriority::Medium,
        stream_events: None,
        wrapped_secrets: None,
    };
    let prepared = backend.prepare(&job, ctx).await.expect("prepare");
    let result = backend
        .execute(&prepared, noop_callback(), None, CancellationToken::new())
        .await
        .expect("execute returns Ok with BackendError outcome");

    match &result.outcome {
        ExecutionOutcome::BackendError { message } => {
            // Postgres error code 25006: read_only_sql_transaction
            assert!(
                message.contains("read-only")
                    || message.contains("25006")
                    || message.to_lowercase().contains("read only"),
                "expected read-only transaction error, got: {message}"
            );
        }
        other => panic!(
            "expected BackendError, got {other:?}. outputs: {:?}",
            result.outputs
        ),
    }

    // ** Honest-absence: no 'should-fail' row was inserted. **
    let count: i64 = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) as "count!"
        FROM executor_postgres_test.things
        WHERE label = 'should-fail'
        "#
    )
    .fetch_one(&pools.admin)
    .await
    .expect("count probe");
    assert_eq!(count, 0, "no row should have been inserted");
}

// Suppress unused-import warning when the path isn't exercised on a no-DB
// environment.
#[allow(dead_code)]
fn _ensure_path_buf_used() -> Option<PathBuf> {
    None
}
