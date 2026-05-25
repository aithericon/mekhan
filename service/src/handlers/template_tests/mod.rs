//! Template tests — CRUD + execution endpoints.
//!
//! Tests attach to a *logical template family* (the family-root `id`,
//! resolved by [`family_root`]) so they survive every new version of the
//! template without re-authoring. The runner (`run`) spawns a synthetic
//! instance with `mode = 'test_run'`, auto-completes human tasks from the
//! test's fixture, captures the final scope, and evaluates the assertion
//! DSL declared in [`crate::models::template_test`].

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::instance::WorkflowInstance;
use crate::models::template::{WorkflowGraph, WorkflowNodeData, WorkflowTemplate};
use crate::models::template_test::{
    CreateTemplateTestRequest, RunAllResponse, TemplateTest, TemplateTestRun,
    UpdateTemplateTestRequest,
};
use crate::AppState;

mod runner;

pub use runner::{run_test, RunContext};

// --- Family resolution -------------------------------------------------------

/// Resolve a template row id to its family root.
///
/// Tests float across the family; storing them against the root keeps a v3
/// of a template addressable by the same `template_id` a freshly-created v1
/// used. Rows created via the `Init` flow have `base_template_id = id`
/// already; defensively fall back to `id` if the column is NULL (the very
/// first row in a chain).
pub async fn family_root(db: &PgPool, template_id: Uuid) -> Result<Uuid, ApiError> {
    let row: Option<(Option<Uuid>, Uuid)> = sqlx::query_as(
        "SELECT base_template_id, id FROM workflow_templates WHERE id = $1",
    )
    .bind(template_id)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;

    let (base, id) = row.ok_or_else(|| ApiError::not_found("template not found"))?;
    Ok(base.unwrap_or(id))
}

/// Fetch the latest *published* row in a template family — the runner's
/// target for test execution. Returns 412 if no published version exists so
/// the caller can surface a "publish a version first" hint.
async fn latest_published_in_family(
    db: &PgPool,
    family_id: Uuid,
) -> Result<WorkflowTemplate, ApiError> {
    sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates \
         WHERE (base_template_id = $1 OR id = $1) AND is_latest = TRUE AND published = TRUE \
         ORDER BY version DESC LIMIT 1",
    )
    .bind(family_id)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::PRECONDITION_FAILED,
            "no published version of this template",
        )
    })
}

// --- CRUD --------------------------------------------------------------------

/// GET /api/templates/{id}/tests — list tests for a template family.
#[utoipa::path(
    get,
    path = "/api/templates/{id}/tests",
    params(("id" = Uuid, Path, description = "Template id (any version)")),
    responses(
        (status = 200, body = Vec<TemplateTest>),
        (status = 404, body = ErrorResponse),
    ),
    tag = "template_tests",
)]
pub async fn list_tests(
    State(state): State<AppState>,
    Path(template_id): Path<Uuid>,
) -> Result<Json<Vec<TemplateTest>>, ApiError> {
    let family = family_root(&state.db, template_id).await?;
    let rows = sqlx::query_as::<_, TemplateTest>(
        "SELECT * FROM template_tests WHERE template_id = $1 ORDER BY created_at ASC",
    )
    .bind(family)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(rows))
}

/// POST /api/templates/{id}/tests — create a new test.
#[utoipa::path(
    post,
    path = "/api/templates/{id}/tests",
    params(("id" = Uuid, Path, description = "Template id (any version)")),
    request_body = CreateTemplateTestRequest,
    responses(
        (status = 201, body = TemplateTest),
        (status = 404, body = ErrorResponse),
        (status = 409, description = "A test with this name already exists", body = ErrorResponse),
    ),
    tag = "template_tests",
)]
pub async fn create_test(
    State(state): State<AppState>,
    user: AuthUser,
    Path(template_id): Path<Uuid>,
    Json(req): Json<CreateTemplateTestRequest>,
) -> Result<(StatusCode, Json<TemplateTest>), ApiError> {
    let family = family_root(&state.db, template_id).await?;

    let start_tokens = serde_json::to_value(&req.start_tokens)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let assertions =
        serde_json::to_value(&req.assertions).map_err(|e| ApiError::internal(e.to_string()))?;

    let row = sqlx::query_as::<_, TemplateTest>(
        r#"
        INSERT INTO template_tests
            (template_id, name, enabled, start_tokens, human_answers, assertions, created_by)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING *
        "#,
    )
    .bind(family)
    .bind(&req.name)
    .bind(req.enabled)
    .bind(&start_tokens)
    .bind(&req.human_answers)
    .bind(&assertions)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db_err) if db_err.constraint().is_some() => {
            ApiError::conflict(format!("test '{}' already exists", req.name))
        }
        _ => ApiError::internal(e.to_string()),
    })?;

    Ok((StatusCode::CREATED, Json(row)))
}

/// PATCH /api/templates/{template_id}/tests/{test_id} — partial update.
#[utoipa::path(
    patch,
    path = "/api/templates/{template_id}/tests/{test_id}",
    params(
        ("template_id" = Uuid, Path, description = "Template id (any version)"),
        ("test_id" = Uuid, Path, description = "Test id"),
    ),
    request_body = UpdateTemplateTestRequest,
    responses(
        (status = 200, body = TemplateTest),
        (status = 404, body = ErrorResponse),
    ),
    tag = "template_tests",
)]
pub async fn update_test(
    State(state): State<AppState>,
    Path((template_id, test_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateTemplateTestRequest>,
) -> Result<Json<TemplateTest>, ApiError> {
    let family = family_root(&state.db, template_id).await?;

    // COALESCE-pattern partial update: each NULL bind preserves the
    // existing column. Encode the optional Vec values to JSON so we can
    // bind a uniform `Option<Value>`; serde_json handles None ⇒ NULL.
    let start_tokens_val = req
        .start_tokens
        .as_ref()
        .map(|v| serde_json::to_value(v))
        .transpose()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let assertions_val = req
        .assertions
        .as_ref()
        .map(|v| serde_json::to_value(v))
        .transpose()
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let updated = sqlx::query_as::<_, TemplateTest>(
        r#"
        UPDATE template_tests SET
            name = COALESCE($3, name),
            enabled = COALESCE($4, enabled),
            start_tokens = COALESCE($5, start_tokens),
            human_answers = COALESCE($6, human_answers),
            assertions = COALESCE($7, assertions),
            updated_at = NOW()
        WHERE id = $1 AND template_id = $2
        RETURNING *
        "#,
    )
    .bind(test_id)
    .bind(family)
    .bind(req.name.as_deref())
    .bind(req.enabled)
    .bind(&start_tokens_val)
    .bind(&req.human_answers)
    .bind(&assertions_val)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("test not found"))?;

    Ok(Json(updated))
}

/// DELETE /api/templates/{template_id}/tests/{test_id}
#[utoipa::path(
    delete,
    path = "/api/templates/{template_id}/tests/{test_id}",
    params(
        ("template_id" = Uuid, Path),
        ("test_id" = Uuid, Path),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = ErrorResponse),
    ),
    tag = "template_tests",
)]
pub async fn delete_test(
    State(state): State<AppState>,
    Path((template_id, test_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let family = family_root(&state.db, template_id).await?;
    let result = sqlx::query("DELETE FROM template_tests WHERE id = $1 AND template_id = $2")
        .bind(test_id)
        .bind(family)
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("test not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- Run ---------------------------------------------------------------------

/// POST /api/templates/{template_id}/tests/{test_id}/run — execute one test.
#[utoipa::path(
    post,
    path = "/api/templates/{template_id}/tests/{test_id}/run",
    params(
        ("template_id" = Uuid, Path),
        ("test_id" = Uuid, Path),
    ),
    responses(
        (status = 200, body = TemplateTestRun),
        (status = 404, body = ErrorResponse),
        (status = 412, description = "No published version", body = ErrorResponse),
    ),
    tag = "template_tests",
)]
pub async fn run_one(
    State(state): State<AppState>,
    user: AuthUser,
    Path((template_id, test_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<TemplateTestRun>, ApiError> {
    let family = family_root(&state.db, template_id).await?;
    let test = sqlx::query_as::<_, TemplateTest>(
        "SELECT * FROM template_tests WHERE id = $1 AND template_id = $2",
    )
    .bind(test_id)
    .bind(family)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("test not found"))?;

    let template = latest_published_in_family(&state.db, family).await?;
    let ctx = RunContext::from_published(&state, &template, user.subject_as_uuid())?;
    let run = run_test(&state, &ctx, &test).await?;
    Ok(Json(run))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct RunAllQuery {
    /// Run only enabled tests (default). `?include_disabled=true` runs every
    /// test — used by the editor's "Run all" button when authors want a
    /// total picture during debugging.
    #[serde(default)]
    pub include_disabled: bool,
}

// --- Promote instance to test -----------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct PromoteToTestRequest {
    /// Name for the new test. Must be unique within the template family.
    pub name: String,
    /// Optional override for the start tokens. When omitted, the runner
    /// best-effort-extracts them from the instance's initial TokenCreated
    /// events; this override lets the UI pre-fill from a known-good source
    /// (e.g. the original `CreateInstanceRequest`).
    #[serde(default)]
    pub start_tokens: Option<Value>,
}

/// POST /api/instances/{id}/promote-to-test — create a `template_tests` row
/// from an existing instance's event log.
///
/// Scoops the instance's start tokens (from the initial Start-place tokens)
/// and human-task completions (from signal-place tokens) into a new test
/// fixture attached to the instance's template family. The user typically
/// completes authoring by adding assertions in the editor afterward.
#[utoipa::path(
    post,
    path = "/api/instances/{id}/promote-to-test",
    params(("id" = Uuid, Path, description = "Source instance id")),
    request_body = PromoteToTestRequest,
    responses(
        (status = 201, body = TemplateTest),
        (status = 404, body = ErrorResponse),
        (status = 409, description = "A test with this name already exists", body = ErrorResponse),
    ),
    tag = "template_tests",
)]
pub async fn promote_instance_to_test(
    State(state): State<AppState>,
    user: AuthUser,
    Path(instance_id): Path<Uuid>,
    Json(req): Json<PromoteToTestRequest>,
) -> Result<(StatusCode, Json<TemplateTest>), ApiError> {
    // Pull source instance + its template so we can resolve slugs and the
    // family root the test will attach to.
    let instance = sqlx::query_as::<_, WorkflowInstance>(
        "SELECT * FROM workflow_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("instance not found"))?;

    let template = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(instance.template_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::internal("source instance references unknown template"))?;
    let graph: WorkflowGraph = serde_json::from_value(template.graph.clone())
        .map_err(|e| ApiError::internal(format!("template graph invalid: {e}")))?;

    let family = template.base_template_id.unwrap_or(template.id);

    // Best-effort start_tokens extraction. The caller can override; otherwise
    // pull tokens deposited on Start-block data places. The compiler emits
    // `p_<start_id>_ready` as the Start's entry place; that's where the
    // parameterized seed lands as a `TokenCreated`.
    let start_tokens = if let Some(override_val) = req.start_tokens.clone() {
        override_val
    } else {
        extract_start_tokens(&state.db, &instance.net_id, &graph).await?
    };

    // Human answers — query signal-place tokens whose token_data carries the
    // engine-injected `{task_id, data, completed_at}` shape. Key the result
    // by node author slug for readability.
    let human_answers = extract_human_answers(&state.db, &instance.net_id, &graph).await?;

    let row = sqlx::query_as::<_, TemplateTest>(
        r#"
        INSERT INTO template_tests
            (template_id, name, enabled, start_tokens, human_answers, assertions, created_by)
        VALUES ($1, $2, TRUE, $3, $4, '[]'::jsonb, $5)
        RETURNING *
        "#,
    )
    .bind(family)
    .bind(&req.name)
    .bind(&start_tokens)
    .bind(&human_answers)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db_err) if db_err.constraint().is_some() => {
            ApiError::conflict(format!("test '{}' already exists", req.name))
        }
        _ => ApiError::internal(e.to_string()),
    })?;

    Ok((StatusCode::CREATED, Json(row)))
}

/// Pull the original Start-block tokens out of the causality event stream.
/// For each Start node in the graph, look for the first `TokenCreated` on
/// its `p_<id>_ready` place — that's the seed `parameterize_air` deposited.
async fn extract_start_tokens(
    db: &PgPool,
    net_id: &str,
    graph: &WorkflowGraph,
) -> Result<Value, ApiError> {
    let mut tokens = Vec::new();
    for node in &graph.nodes {
        if !matches!(node.data, WorkflowNodeData::Start { .. }) {
            continue;
        }
        let place = format!("p_{}_ready", node.id);
        let row: Option<(Option<Value>,)> = sqlx::query_as(
            "SELECT cet.token_data FROM causality_event_tokens cet \
             JOIN causality_events ce ON ce.net_id = cet.net_id AND ce.event_seq = cet.event_seq \
             WHERE cet.net_id = $1 AND cet.place_name = $2 AND cet.role = 'created' \
             ORDER BY ce.event_seq ASC LIMIT 1",
        )
        .bind(net_id)
        .bind(&place)
        .fetch_optional(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
        let token_data = row.and_then(|(d,)| d).unwrap_or(Value::Null);
        // Strip the system fields parameterize_air injects (`_instance_id`,
        // `_template_id`, `_template_version`, `_created_at`, `_created_by`)
        // so re-running the test with this fixture doesn't fight the next
        // launch's freshly-injected values.
        let token = strip_system_fields(token_data);
        tokens.push(serde_json::json!({
            "start_block_id": node.id,
            "token": token,
        }));
    }
    Ok(Value::Array(tokens))
}

fn strip_system_fields(value: Value) -> Value {
    match value {
        Value::Object(mut obj) => {
            obj.retain(|k, _| !k.starts_with('_'));
            Value::Object(obj)
        }
        other => other,
    }
}

/// Reconstruct `human_answers` keyed by node slug. For each HumanTask node,
/// look at tokens deposited on its `sig_<id>` place — those are the engine's
/// signal-injected completions and carry the form data under `data`.
async fn extract_human_answers(
    db: &PgPool,
    net_id: &str,
    graph: &WorkflowGraph,
) -> Result<Value, ApiError> {
    let mut answers = serde_json::Map::new();
    for node in &graph.nodes {
        if !matches!(node.data, WorkflowNodeData::HumanTask { .. }) {
            continue;
        }
        let place = format!("sig_{}", node.id);
        let row: Option<(Option<Value>,)> = sqlx::query_as(
            "SELECT cet.token_data FROM causality_event_tokens cet \
             JOIN causality_events ce ON ce.net_id = cet.net_id AND ce.event_seq = cet.event_seq \
             WHERE cet.net_id = $1 AND cet.place_name = $2 AND cet.role = 'created' \
             ORDER BY ce.event_seq ASC LIMIT 1",
        )
        .bind(net_id)
        .bind(&place)
        .fetch_optional(db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
        if let Some((Some(token_data),)) = row {
            // Engine completion shape: { task_id, data, completed_at }. We
            // care about `data`; a bare-token-shaped payload (no `data` key)
            // is passed through unchanged so an unusual engine variant still
            // produces a usable fixture.
            let answer = token_data
                .get("data")
                .cloned()
                .unwrap_or(token_data);
            answers.insert(node.slug(), answer);
        }
    }
    Ok(Value::Object(answers))
}

/// POST /api/templates/{id}/tests/run-all — run every enabled test for a
/// template family. Used by the editor's "Run all" button and by the
/// publication gate.
#[utoipa::path(
    post,
    path = "/api/templates/{id}/tests/run-all",
    params(
        ("id" = Uuid, Path),
        RunAllQuery,
    ),
    responses(
        (status = 200, body = RunAllResponse),
        (status = 404, body = ErrorResponse),
        (status = 412, description = "No published version", body = ErrorResponse),
    ),
    tag = "template_tests",
)]
pub async fn run_all(
    State(state): State<AppState>,
    user: AuthUser,
    Path(template_id): Path<Uuid>,
    Query(query): Query<RunAllQuery>,
) -> Result<Json<RunAllResponse>, ApiError> {
    let family = family_root(&state.db, template_id).await?;
    let template = latest_published_in_family(&state.db, family).await?;
    let ctx = RunContext::from_published(&state, &template, user.subject_as_uuid())?;

    let sql = if query.include_disabled {
        "SELECT * FROM template_tests WHERE template_id = $1 ORDER BY created_at ASC"
    } else {
        "SELECT * FROM template_tests WHERE template_id = $1 AND enabled = TRUE ORDER BY created_at ASC"
    };
    let tests = sqlx::query_as::<_, TemplateTest>(sql)
        .bind(family)
        .fetch_all(&state.db)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let mut runs = Vec::with_capacity(tests.len());
    let (mut passed, mut failed, mut errored) = (0, 0, 0);
    for test in &tests {
        let run = run_test(&state, &ctx, test).await?;
        match run.status.as_str() {
            "passed" => passed += 1,
            "failed" => failed += 1,
            _ => errored += 1,
        }
        runs.push(run);
    }

    Ok(Json(RunAllResponse {
        total: tests.len(),
        passed,
        failed,
        errored,
        runs,
    }))
}

