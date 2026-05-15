# Postgres Execution Backend — Design Proposal

**Status:** Proposal (sub-phase 2.2 A1, Cloud AI OS migration)
**Cross-reference:** `/Users/amd/dev/online-clinic/plan/cloud-layer-phase-2.md` § 2.2
**Implementation slice:** B2 (this doc is design-only; no Rust source lands here)

---

## 1. Goal & Non-Goals

**Goal.** Add a new `PostgresBackend` to aithericon-executor that runs a parametrised SQL query against a tenant-scoped Postgres database and returns the result rows as a JSONB array in the `outputs` map. The backend exists so cloud-layer petri-net workflows (and downstream consumers such as the clinic `data_gathering` pipeline-engine step kind, currently in-process) can move SQL into the same execution substrate that already handles process, HTTP, LLM, and document jobs — closing the gap that today forces tenant-data queries to run outside the audited executor path.

**Non-goals.**

- **Not a generic ORM.** No relationship mapping, no schema discovery, no `INSERT … RETURNING` fluent builder. The backend executes one SQL statement per job.
- **Not for cross-tenant queries.** Every query is tenant-scoped via Postgres RLS; admin-tier "see everything" queries are explicitly out of scope and must use a separate, future, audit-flagged backend.
- **Not a long-running session.** Each job acquires a connection from a pool, runs in a single short-lived transaction, releases. No `LISTEN`, no `COPY` streaming, no advisory locks.
- **Not a migration runner.** DDL is rejected; only `SELECT` (initial scope) and explicitly-allow-listed DML kinds (later iterations) are permitted.

## 2. JSON Job Spec (`type: "postgres"`)

Following the convention in `docs/job-model.md`, the per-job config is JSON, deserialised from `ExecutionSpec.config` by the backend's `prepare()` hook. Field shape:

| Field | Type | Default | Description |
|---|---|---|---|
| `query` | `string` | required | The SQL statement (parametrised `$1`, `$2`, … placeholders only — never string-interpolated values). Supports `{{input:NAME}}` template substitution **for input file content only** (e.g., a query loaded from a staged file); placeholder values come from `params`. |
| `params` | `[json]` | `[]` | Ordered values bound to `$1`, `$2`, …. Each entry is a JSON scalar, array, or object; the backend maps them to Postgres types via `sqlx` (`Value::String` → `text`, `Value::Number` → `int8`/`float8`, etc.). |
| `projection` | `[string]` | required | Ordered list of column names expected in the result rows. The backend verifies each column exists in the row description; an unexpected or missing column is a `BackendError`. |
| `row_limit` | `uint64` | `10000` | Maximum number of rows materialised. If the query returns more, the backend fails closed with `BackendError { message: "row_limit exceeded" }`. |
| `statement_timeout_ms` | `uint64` | `5000` | Sent as `SET LOCAL statement_timeout = $1` inside the same transaction. Capped at job-level `timeout`. |
| `read_only` | `bool` | `true` | When `true`, the backend issues `SET LOCAL transaction_read_only = on` before the user query. Initial scope hard-locks this to `true`; later iterations may relax it for an allow-listed call-site set. |
| `pool` | `string` | `"default"` | Names which connection pool to draw from (see § 3). Multiple pools may be registered when a single executor talks to multiple Postgres instances. |

### Worked example

```json
{
  "execution_id": "patient-chart-fetch-1",
  "spec": {
    "type": "postgres",
    "config": {
      "query": "SELECT id, name, dob FROM patients WHERE clinic_id = $1 AND active = $2 ORDER BY name LIMIT $3",
      "params": ["00000000-0000-0000-0000-000000000001", true, 50],
      "projection": ["id", "name", "dob"],
      "row_limit": 100,
      "statement_timeout_ms": 2000,
      "read_only": true,
      "pool": "clinic_primary"
    },
    "outputs": [
      { "name": "rows", "required": true },
      { "name": "row_count", "required": true }
    ]
  },
  "metadata": {
    "petri_net_id": "net-data-gathering-42",
    "tenant_id": "00000000-0000-0000-0000-000000000001",
    "jwt_subject": "cloud-layer:tenant-context"
  }
}
```

The example UUID above is illustrative-only — real jobs propagate `tenant_id` from the cloud-layer JWT (§ 4), never hardcode it.

## 3. Executor-Level TOML Stanza (pool registration)

Per `docs/configuration.md`, the executor process loads `executor.toml` plus `EXECUTOR_*` env vars. Per-job state belongs in JSON `ExecutionSpec.config`; **shared pool definitions** (connection strings, pool sizing) belong in the executor process config. Add a new `[backends.postgres]` table mirroring the existing `[cancel]` and `[storage]` shape:

```toml
[backends.postgres]
# Default pool used when a job omits `config.pool`.
default_pool = "clinic_primary"

[backends.postgres.pools.clinic_primary]
url_env  = "EXECUTOR_PG_CLINIC_PRIMARY_URL"   # PG connection string lives in env, never the file
max_connections = 16
min_connections = 2
acquire_timeout_secs = 5
idle_timeout_secs    = 300
# Application-name surfaced in pg_stat_activity for audit.
application_name = "aithericon-executor"

[backends.postgres.pools.cloud_layer_registry]
url_env  = "EXECUTOR_PG_CLOUD_LAYER_URL"
max_connections = 4
```

Env-var equivalents follow the documented `EXECUTOR_BACKENDS__POSTGRES__DEFAULT_POOL` / `EXECUTOR_BACKENDS__POSTGRES__POOLS__CLINIC_PRIMARY__MAX_CONNECTIONS` double-underscore-nesting convention. Connection strings themselves are **never** in the TOML file — only the env-var name that holds them, matching how `AuthConfig::Bearer` does `token_env` rather than inline `token`.

## 4. Rust Trait Signature

The backend implements `aithericon_executor_backend::traits::ExecutionBackend` per `docs/backend-trait.md`. Struct + impl skeleton (illustrative — actual code lands in B2):

```rust
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;

pub struct PostgresBackend {
    pools: Arc<HashMap<String, PgPool>>,   // registered at executor startup
    default_pool: String,
}

impl PostgresBackend {
    pub fn new(pools: HashMap<String, PgPool>, default_pool: String) -> Self {
        Self { pools: Arc::new(pools), default_pool }
    }
}

#[async_trait::async_trait]
impl ExecutionBackend for PostgresBackend {
    fn name(&self) -> &'static str { "postgres" }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "postgres"
    }

    async fn prepare(
        &self,
        _job: &ExecutionJob,
        mut run_context: RunContext,
    ) -> Result<RunContext, ExecutorError> {
        // 1. Resolve {{input:NAME}} in raw config.
        // 2. Deserialize into `PostgresConfig`.
        // 3. Validate: read_only must be true (initial scope); projection non-empty;
        //    statement_timeout_ms <= run_context.timeout; pool name registered.
        // 4. Store validated config in run_context.backend_state.
        Ok(run_context)
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        // See § 5 for the tenant-context-then-query transaction wrapper.
        unimplemented!("see § 5 / § 6")
    }
}
```

**Pool sharing.** `PostgresBackend` is constructed once at executor startup with all named pools materialised. Pools are `sqlx::PgPool` (clonable, `Arc`-internally) shared across job invocations via `Arc<HashMap<…>>`. The backend itself is registered in `BackendRegistry` like any other (`docs/backend-trait.md` § "Adding a New Backend").

## 5. Multi-Tenant JWT Context Propagation (security invariant)

The cloud-layer issues each workflow execution a JWT containing `tenant_id`. That tenant scope is propagated to Postgres via `SET LOCAL app.tenant_id`, evaluated by the database's RLS policies (defined in the cloud-layer schema and the clinic schema). The backend MUST make the SET-then-query ordering **un-bypassable**.

**Where `tenant_id` comes from.** The executor's `RunContext.metadata` map carries `tenant_id` (populated by the cloud-layer's job dispatcher from the verified JWT). The backend reads it from there. **No other source is consulted** — no env var, no config field, no JWT re-decode. Single-source ensures the JWT verification done by the cloud-layer is the only point of trust.

**Transaction wrapper pattern.** Every query runs inside a transaction. First two statements are SET-LOCALs; third is the user query. All three use the `sqlx::query!()` macro form (no raw `sqlx::query()` allowed — matches the project-wide enforcement in clinic and aligns with the macro-form discipline `feedback_sqlx_macro_only_strict`):

```rust
let tenant_id: uuid::Uuid = run_context.metadata
    .get("tenant_id")
    .ok_or_else(|| ExecutorError::Config(
        "postgres backend requires metadata.tenant_id (cloud-layer JWT subject)".into()
    ))?
    .parse()
    .map_err(|e| ExecutorError::Config(format!("tenant_id is not a UUID: {e}")))?;

let pool = self.pools
    .get(config.pool.as_deref().unwrap_or(&self.default_pool))
    .ok_or_else(|| ExecutorError::Config(format!("unknown pool: {}", config.pool)))?;

let mut tx = pool.begin().await.map_err(|e| ExecutorError::Backend(e.to_string()))?;

// SET LOCAL is transaction-scoped; cannot leak to pool-recycled connection.
sqlx::query!(
    "SELECT set_config('app.tenant_id', $1, true)",
    tenant_id.to_string()
)
.fetch_one(&mut *tx)
.await?;

sqlx::query!(
    "SELECT set_config('statement_timeout', $1, true)",
    format!("{}ms", config.statement_timeout_ms)
)
.fetch_one(&mut *tx)
.await?;

// Then the user query, parametrised — params bound via sqlx::query_with at runtime
// because the SQL is dynamic. (See § 7 trip-wire on macro-vs-dynamic-SQL.)
let rows = sqlx::query_with(&config.query, build_arguments(&config.params)?)
    .fetch_all(&mut *tx)
    .await?;

tx.commit().await?;
```

**Fail-closed on missing tenant.** If `metadata.tenant_id` is absent or unparseable, the backend returns `ExecutorError::Config` in `prepare()` — the job never reaches the database. There is no fallback to "run without tenant context"; a tenant-less query is a contract violation.

**Transaction scope guarantee.** `SET LOCAL` is scoped to the current transaction by Postgres definition. Because all three statements share the same `tx`, and the user query never runs outside that `tx`, the context cannot be skipped or leaked to a subsequent connection-pool checkout. RLS policies in the database consume `current_setting('app.tenant_id', true)` and filter rows accordingly.

## 6. JSONB Output Convention

The job declares the expected projection in `config.projection`. After the query runs:

- The backend reads each row, validates that exactly the projection columns are present (extra columns → `BackendError`; missing columns → `BackendError`).
- Each row is serialised to a `serde_json::Value::Object` keyed by column name, with values converted via sqlx's standard `Json`/scalar mappings (`text` → string, `int8` → number, `uuid` → string, `jsonb` → nested value, `timestamptz` → ISO-8601 string, etc.).
- The result is written to `ExecutionResult.outputs` as:

```rust
outputs.insert("rows".into(), serde_json::Value::Array(rows));
outputs.insert("row_count".into(), serde_json::Value::Number(rows.len().into()));
```

Both `rows` and `row_count` are always populated. Consumers (the petri-net token shape) treat the `outputs` map identically to how they treat HTTP/LLM/Kreuzberg outputs today — no new token shape needed.

**Error path.** If a projection column doesn't exist in the row description, the backend returns `ExecutionOutcome::BackendError { message: "column 'foo' missing from query result; projection requires ['foo','bar']" }`. No partial results.

**Type coercion failures.** If a row's value cannot be converted to JSON (rare: e.g., custom Postgres composite types not registered with sqlx), the backend fails closed with `BackendError`. No silent string-cast fallback.

## 7. Certification Plan (binding for B2)

Per `feedback_act2_certification_is_tier_scoped` and `feedback_recipe_as_named_before_close_claim`, each tier is named explicitly with its literal `just` recipe and the bin/test names exercised. **B2 close-report must list each tier separately with stdout shape; "all PASS" framing is forbidden.**

**Tier 1 — compile, format, lint.**
Recipes: `just check`, `just fmt --check`, `just lint`.
Surface: `cargo check -p aithericon-executor-postgres` succeeds; clippy zero warnings on the new crate; `cargo fmt --check` clean.

**Tier 2 — unit tests.**
Recipe: `just test`.
Bin/test names: `cargo test -p aithericon-executor-postgres --lib` — covers config deserialisation round-trip, projection validation, fail-closed-on-missing-tenant path (no DB needed; pure logic).

**Tier 3 — integration tests against a live Postgres.**
Recipe: `just test-integration` (new — wired in B2 to start a transient Postgres via testcontainers or an external `EXECUTOR_TEST_PG_URL`).
Test names: `cargo test -p aithericon-executor-postgres --test integration`.

Required integration test cases (each is a separate `#[tokio::test]` function, named in the close-report):

1. `tenant_isolation_two_tenants_one_pool` — Seeds two `tenant`-tagged rows under stable lookup-keys (`tenant_a_email = "tenant-a@test.example"`, `tenant_b_email = "tenant-b@test.example"` — UUIDs resolved via lookup, never hardcoded). Runs the same SELECT under both `tenant_id` contexts. Asserts tenant A sees its row and **also asserts honest-absence: tenant A does NOT see tenant B's row** (per `_supervision-conventions.md` test discipline).

2. `fail_closed_when_tenant_metadata_missing` — Submits a job whose `metadata` map omits `tenant_id`. Asserts `ExecutorError::Config` is returned and **no SQL statement ran** (verified via a `pg_stat_statements` lookup on the test pool, or an in-test counting wrapper).

3. `jsonb_output_projection_roundtrip` — Submits a query returning mixed types (text, uuid, int, timestamptz, jsonb). Asserts the output `rows` array round-trips through `serde_json` without precision or type loss.

4. `row_limit_enforced` — Query returns more rows than `row_limit`. Asserts `BackendError { message: contains "row_limit" }` and zero rows in output.

5. `read_only_transaction_blocks_writes` — Submits a query with `INSERT INTO …`. Asserts the transaction rejects it (Postgres error code `25006`) and the backend surfaces a `BackendError` rather than silently swallowing.

**Tier-4-adjacent — recipe certification.** Per `feedback_recipe_as_named_before_close_claim`, the new `just test-integration` recipe must be invoked literally once by the B2 implementer with stdout captured; sub-checks running clean ≠ recipe certification.

**Test discipline (forward-binding).**

- **Resolve-or-seed-by-stable-key.** Tests never hardcode UUIDs. Seed fixtures use email/name lookup keys (`tenant-a@test.example`, etc.); UUIDs are resolved at test runtime.
- **Honest-absence assertions.** Every "tenant sees row" assertion is paired with a "tenant does NOT see other tenant's row" assertion in the same test body.
- **No `std::env::set_var` in test bodies.** Tenant ID is passed as a job-metadata parameter, never via global env.
- **No `// SQLX-UNCHECKED-OK:` markers.** All SQL in the backend (including SET-LOCAL calls) uses `sqlx::query!()` macro form. The one exception is `sqlx::query_with(&dynamic_sql, args)` for the user query itself, where the SQL is genuinely runtime-supplied — that path is structurally distinct from the marker-escape-hatch and does not require a comment (it's the documented sqlx mechanism for runtime SQL).

## 8. Trip-Wires for B2 Implementation

Surfacing these now per supervisor-coordination practice so B2's surface-report does not re-discover them:

- **sqlx feature flags.** The new `aithericon-executor-postgres` crate needs `sqlx = { version = "…", features = ["runtime-tokio", "postgres", "uuid", "chrono", "json"] }`. Confirm the workspace `Cargo.toml` resolver settles on a single sqlx version across all crates (no duplicate compile of sqlx-core under feature unification differences). The clinic uses sqlx 0.8.x; aligning prevents diamond-dep mismatches if the executor and clinic ever share a process.

- **Dynamic SQL vs macro-form tension.** The user query is supplied at runtime (`config.query`), so it cannot use `sqlx::query!()` (which requires compile-time SQL). The SET-LOCAL bookend statements *can* and *must* use the macro form — they're hardcoded. Document this asymmetry in the B2 PR; reviewers expecting macro-form everywhere need the rationale up front.

- **`executor-domain` spec-type extension.** The existing `ExecutionSpec.backend` is an open string. Two options for B2: (a) keep `backend: "postgres"` as a plain string and let `PostgresBackend::supports()` match — minimal change, matches existing LLM/HTTP convention; (b) add a strongly-typed `PostgresJobSpec` to `executor-backend-configs`. Recommendation: **option (a)** to match the existing HTTP/LLM/Kreuzberg pattern (none of them are enum variants in `ExecutionSpec` — they all use the open-string discriminant).

- **Connection-pool lifecycle.** Pools are created at executor process startup from `executor.toml` `[backends.postgres.pools.*]` stanzas, owned by the `BackendRegistry`-held `PostgresBackend`, dropped at process shutdown. Pool config (`max_connections`, etc.) lives in TOML; URLs live in env vars. **Trip-wire:** if a pool's env-var-named URL is unset at startup, the executor must fail to start (loud-fail-on-missing-env-var) rather than starting and then failing each job individually.

- **Tenant-context-leak in pool-recycled connections.** `SET LOCAL` is transaction-scoped per Postgres semantics, so it cannot leak across transactions on the same physical connection. **However**, if the backend ever switches to `SET` (without `LOCAL`) — e.g., for a "session-wide setting" optimisation — that protection is lost and a recycled connection could carry a previous tenant's context into a new job. **B2 implementers must never substitute `SET` for `SET LOCAL`.** This is a security-critical invariant; the integration test `tenant_isolation_two_tenants_one_pool` would surface a regression by running both queries on the same pool back-to-back.

- **Hardware grounding.** Dev machine is 128GB M5 Apple Silicon (unified memory). Pool sizing defaults (`max_connections = 16`) are conservative for dev; production sizing is a separate (out-of-scope-for-this-spec) capacity exercise. There is no GPU/VRAM concern for this backend — Postgres latency dominates, not compute.

---

## Open questions deferred to B2 surface-report

- DML allow-list shape (which call-sites get `read_only: false` — likely a separate `INSERT`-specialised backend, not a `read_only: false` flag on this one).
- Migration of clinic-side `data_gathering` step kind to use this backend (sub-phase 2.2 B3/B4, not B2).
- Observability — metrics namespace (`postgres/query_time_ms`, `postgres/rows_returned`) follows the existing `llm/*`, `http/*` convention but exact metric names finalise in B2.
