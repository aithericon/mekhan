//! Template test runner.
//!
//! Spawns a synthetic workflow instance (`mode = 'test_run'`), auto-completes
//! human tasks from the test's `human_answers` fixture (keyed by node slug
//! with a node-id fallback), waits for the instance to reach a terminal
//! state, then evaluates the assertion DSL against a synthetic scope built
//! from `instance.result` plus per-step outputs from `step_execution`.
//!
//! Engine-side code is untouched — auto-completion lives entirely in this
//! service-side glue. Polling `hpi_tasks` (rather than subscribing to the
//! NATS `human.request.>` stream) sidesteps subscription-ordering races for
//! fast-completing first steps and keeps the runner tolerant to dropped
//! NATS messages between tests.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::error::ApiError;
use crate::models::instance::StartToken;
use crate::models::template::{WorkflowGraph, WorkflowTemplate};
use crate::models::template_test::{Assertion, AssertOp, TemplateTest, TemplateTestRun};
use crate::petri::launcher::{InstanceLauncher, LaunchSpec};
use crate::AppState;

/// Per-run timeout for the whole-test wall clock. Tests are MVP-scoped to
/// short workflows; a hung test should fail fast rather than tie up the
/// runner. The value is intentionally generous — Python venv warmup alone
/// can take several seconds on cold dev.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Polling cadence for the hpi_tasks / instance status loop. Tight enough
/// that a fast workflow completes promptly, loose enough that the runner
/// doesn't dominate dev-DB connections during a `run-all`.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Inputs the runner needs that aren't on the test itself. Built once per
/// run by [`RunContext::from_published`]; the publication-gate path will
/// later build one against an *about-to-publish* AIR without persisting
/// (same shape, different source).
pub struct RunContext {
    pub template_id: Uuid,
    pub template_version: i32,
    pub air_json: Value,
    pub graph: WorkflowGraph,
    pub created_by: Uuid,
}

impl RunContext {
    pub fn from_published(
        _state: &AppState,
        template: &WorkflowTemplate,
        created_by: Uuid,
    ) -> Result<Self, ApiError> {
        let air_json = template
            .air_json
            .clone()
            .ok_or_else(|| ApiError::internal("published template has no AIR JSON"))?;
        let graph: WorkflowGraph = serde_json::from_value(template.graph.clone())
            .map_err(|e| ApiError::internal(format!("template graph invalid: {e}")))?;
        Ok(Self {
            template_id: template.id,
            template_version: template.version,
            air_json,
            graph,
            created_by,
        })
    }
}

/// Drive one test to terminal. Errors only on infrastructure failure (DB
/// write, launcher error); a missing fixture answer or an assertion mismatch
/// returns `Ok(_)` with `status` ∈ {`failed`, `error`} so the caller can
/// surface a structured response rather than a 500.
pub async fn run_test(
    state: &AppState,
    ctx: &RunContext,
    test: &TemplateTest,
) -> Result<TemplateTestRun, ApiError> {
    let started_at = Utc::now();
    let started_instant = Instant::now();
    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{instance_id}");

    // Deserialize the test's stored start tokens into the typed shape the
    // launcher expects.
    let start_tokens: Vec<StartToken> = serde_json::from_value(test.start_tokens.clone())
        .map_err(|e| {
            ApiError::internal(format!("test {} has invalid start_tokens: {e}", test.id))
        })?;

    // Spawn the test_run instance.
    let launcher = InstanceLauncher::new(&state.db, &state.petri);
    let launch_result = launcher
        .launch(LaunchSpec {
            instance_id,
            net_id: net_id.clone(),
            template_id: ctx.template_id,
            template_version: ctx.template_version,
            created_by: ctx.created_by,
            metadata: json!({ "test_id": test.id, "test_name": test.name }),
            air_json: &ctx.air_json,
            graph: &ctx.graph,
            start_tokens: &start_tokens,
            mode: Some("test_run"),
            test_id: Some(test.id),
        })
        .await;

    let instance = match launch_result {
        Ok(inst) => inst,
        Err(e) => {
            // Launch never inserted a row (parameterize/database) or rolled
            // it back (deploy). Record an error run and bail without an
            // instance_id link.
            return persist_run(
                state,
                test,
                ctx.template_version,
                Uuid::nil(),
                "error",
                Some(json!({ "reason": "launch_failed", "detail": e.to_string() })),
                None,
                started_at,
                started_instant.elapsed(),
            )
            .await;
        }
    };

    // Index human_answers by both slug and node_id so the answer-lookup
    // tolerates either keying scheme in the stored fixture.
    let answers_obj = test
        .human_answers
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut answers: HashMap<String, Value> = HashMap::new();
    for (k, v) in answers_obj {
        answers.insert(k, v);
    }

    // Drive the auto-complete loop until the instance reaches a terminal
    // status or the wall-clock timeout fires.
    let mut completed_task_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let terminal_status = loop {
        if started_instant.elapsed() > RUN_TIMEOUT {
            // Leave the engine net running — cancellation lives outside the
            // launcher today, and the test_run mode tag means a retention
            // pass can clean it up later. We just record the error run and
            // bail.
            tracing::warn!(
                test_id = %test.id,
                net_id = %net_id,
                "template test timed out"
            );
            return persist_run(
                state,
                test,
                ctx.template_version,
                instance.id,
                "error",
                Some(json!({ "reason": "timeout", "after_ms": RUN_TIMEOUT.as_millis() })),
                None,
                started_at,
                started_instant.elapsed(),
            )
            .await;
        }

        // Pull every still-pending task for our net that we haven't already
        // answered. The hpi_tasks projection lands these rows from the
        // engine's `human.request` effect via the causality consumer.
        let pending_tasks: Vec<(String, Value)> = sqlx::query_as(
            "SELECT id, detail FROM hpi_tasks \
             WHERE detail->>'net_id' = $1 AND status = 'pending'",
        )
        .bind(&net_id)
        .fetch_all(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("hpi_tasks poll failed: {e}")))?;

        for (task_id, detail) in pending_tasks {
            if !completed_task_ids.insert(task_id.clone()) {
                // Already answered — engine just hasn't projected the
                // completion yet.
                continue;
            }

            let place = match detail.get("place").and_then(Value::as_str) {
                Some(p) => p.to_string(),
                None => {
                    return persist_run(
                        state,
                        test,
                        ctx.template_version,
                        instance.id,
                        "error",
                        Some(json!({
                            "reason": "task_missing_place",
                            "task_id": task_id,
                        })),
                        None,
                        started_at,
                        started_instant.elapsed(),
                    )
                    .await;
                }
            };

            let slug = resolve_task_slug(&ctx.graph, &detail, &place);
            let answer = answers
                .get(&slug)
                .or_else(|| {
                    // Fall back to node_id (the WorkflowNode.id) so authors
                    // can hand-author by either identifier.
                    detail
                        .get("node_id")
                        .and_then(Value::as_str)
                        .and_then(|nid| answers.get(nid))
                })
                .cloned();

            let Some(answer) = answer else {
                return persist_run(
                    state,
                    test,
                    ctx.template_version,
                    instance.id,
                    "error",
                    Some(json!({
                        "reason": "missing_human_answer",
                        "node_slug": slug,
                        "place": place,
                        "task_id": task_id,
                        "hint": "add an entry to human_answers keyed by this slug",
                    })),
                    None,
                    started_at,
                    started_instant.elapsed(),
                )
                .await;
            };

            // Publish the synthetic completion. Subject + payload shape match
            // what `service::tests::causality_e2e` does for live human task
            // completion (kept identical so we ride the same engine path the
            // UI uses).
            let subject = format!("human.completed.{net_id}.{place}");
            let payload = json!({
                "task_id": task_id,
                "data": answer,
                "completed_at": Utc::now().to_rfc3339(),
            });
            if let Err(e) = state
                .nats
                .client()
                .publish(subject, serde_json::to_vec(&payload).unwrap().into())
                .await
            {
                return Err(ApiError::internal(format!(
                    "failed to publish human completion: {e}"
                )));
            }
        }

        // Check whether the instance has terminated.
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(instance.id)
                .fetch_optional(&state.db)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;
        if let Some((status,)) = row {
            if matches!(status.as_str(), "completed" | "failed" | "cancelled") {
                break status;
            }
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    };

    // Build the synthetic scope: { result, steps.<slug>.output }.
    let scope = build_scope(&state.db, &ctx.graph, instance.id).await?;

    // Map instance termination status into a coarse run status before we
    // even look at assertions. A `failed`/`cancelled` instance with passing
    // assertions is still treated as `error` — the assertions never get a
    // chance to be meaningful against an aborted workflow.
    if terminal_status != "completed" {
        return persist_run(
            state,
            test,
            ctx.template_version,
            instance.id,
            "error",
            Some(json!({
                "reason": "instance_did_not_complete",
                "terminal_status": terminal_status,
            })),
            Some(scope),
            started_at,
            started_instant.elapsed(),
        )
        .await;
    }

    // Evaluate assertions; short-circuit on first failure.
    let assertions: Vec<Assertion> = serde_json::from_value(test.assertions.clone())
        .map_err(|e| ApiError::internal(format!("invalid assertions: {e}")))?;
    for (idx, assertion) in assertions.iter().enumerate() {
        match eval_assertion(&scope, assertion) {
            Ok(true) => continue,
            Ok(false) => {
                let detail = json!({
                    "assertion_idx": idx,
                    "path": assertion.path,
                    "op": assertion.op,
                    "expected": assertion.value,
                    "actual": navigate(&scope, &assertion.path).cloned().unwrap_or(Value::Null),
                });
                return persist_run(
                    state,
                    test,
                    ctx.template_version,
                    instance.id,
                    "failed",
                    Some(detail),
                    Some(scope),
                    started_at,
                    started_instant.elapsed(),
                )
                .await;
            }
            Err(reason) => {
                let detail = json!({
                    "assertion_idx": idx,
                    "path": assertion.path,
                    "op": assertion.op,
                    "expected": assertion.value,
                    "error": reason,
                });
                return persist_run(
                    state,
                    test,
                    ctx.template_version,
                    instance.id,
                    "error",
                    Some(detail),
                    Some(scope),
                    started_at,
                    started_instant.elapsed(),
                )
                .await;
            }
        }
    }

    persist_run(
        state,
        test,
        ctx.template_version,
        instance.id,
        "passed",
        None,
        Some(scope),
        started_at,
        started_instant.elapsed(),
    )
    .await
}

// --- Persistence -------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn persist_run(
    state: &AppState,
    test: &TemplateTest,
    template_version: i32,
    instance_id: Uuid,
    status: &str,
    failure_detail: Option<Value>,
    final_scope: Option<Value>,
    started_at: chrono::DateTime<Utc>,
    duration: Duration,
) -> Result<TemplateTestRun, ApiError> {
    let finished_at = Utc::now();
    let duration_ms = duration.as_millis().min(i32::MAX as u128) as i32;

    let run = sqlx::query_as::<_, TemplateTestRun>(
        r#"
        INSERT INTO template_test_runs
            (test_id, instance_id, template_version, status, failure_detail, final_scope, started_at, finished_at, duration_ms)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING *
        "#,
    )
    .bind(test.id)
    .bind(instance_id)
    .bind(template_version)
    .bind(status)
    .bind(&failure_detail)
    .bind(&final_scope)
    .bind(started_at)
    .bind(finished_at)
    .bind(duration_ms)
    .fetch_one(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("failed to persist test run: {e}")))?;

    // Stamp the test row so the publication gate / list view sees a fresh
    // result. `passed=true` only when status='passed' — `failed` and `error`
    // both block publish.
    //
    // Also refresh `reference_scope` on a passing run: a green run is the
    // most reliable evidence of what the synthetic scope actually looks like,
    // so the editor's Available Scope panel stays honest as the template
    // evolves. Skip the refresh on failed/errored runs — that final_scope
    // might be partial (e.g. instance aborted) and would mislead authoring.
    let refresh_scope = status == "passed";
    sqlx::query(
        "UPDATE template_tests SET \
           last_run_at = $2, last_run_against_version = $3, last_run_passed = $4, \
           reference_scope = CASE WHEN $5 THEN $6 ELSE reference_scope END, \
           updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(test.id)
    .bind(finished_at)
    .bind(template_version)
    .bind(status == "passed")
    .bind(refresh_scope)
    .bind(&final_scope)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;

    Ok(run)
}

// --- Slug resolution ---------------------------------------------------------

/// Best-effort `(detail, place)` → author slug. Prefer slug fields embedded
/// in the task detail (the engine writes `node_slug` for HumanTask effects),
/// then strip the compiler's `p_<id>_signal` place naming back to the node id
/// and look up the node's [`WorkflowNode::slug`], and finally fall back to
/// `node_id` from detail or the legacy `sig_<slug>` prefix.
///
/// The middle path (`p_<id>_signal` → node id → slug) is the live engine's
/// shape: `hpi_tasks.detail.place` carries `p_<node_id>_signal`, never
/// `node_id` directly, so the inverse map through `graph.nodes` is the only
/// reliable way to recover the author slug for human_answers lookup.
fn resolve_task_slug(graph: &WorkflowGraph, detail: &Value, place: &str) -> String {
    if let Some(s) = detail.get("node_slug").and_then(Value::as_str) {
        return s.to_string();
    }
    if let Some(inner) = place
        .strip_prefix("p_")
        .and_then(|s| s.strip_suffix("_signal"))
    {
        if let Some(node) = graph.nodes.iter().find(|n| n.id == inner) {
            return node.slug();
        }
    }
    if let Some(node_id) = detail.get("node_id").and_then(Value::as_str) {
        if let Some(node) = graph.nodes.iter().find(|n| n.id == node_id) {
            return node.slug();
        }
    }
    // Last-resort legacy fallback.
    place.strip_prefix("sig_").unwrap_or(place).to_string()
}

// --- Scope construction ------------------------------------------------------

/// Build the synthetic scope assertions see: `{ result, steps.<slug>.output }`.
/// Reads from `workflow_instances.result` and `step_execution.outputs`,
/// keyed by the node's author slug. Also used by `promote_instance_to_test`
/// to seed a fresh test's `reference_scope` from the source instance, so
/// authors author against the exact same shape the runner will later check.
pub(super) async fn build_scope(
    db: &PgPool,
    graph: &WorkflowGraph,
    instance_id: Uuid,
) -> Result<Value, ApiError> {
    let row: Option<(Option<Value>,)> =
        sqlx::query_as("SELECT result FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_optional(db)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    let result = row.and_then(|(r,)| r).unwrap_or(Value::Null);

    // Node-id → author slug, so we can key per-step output by slug for
    // human-readable assertion paths.
    let slug_by_node: HashMap<String, String> = graph
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.slug()))
        .collect();

    let rows: Vec<(String, Option<Value>)> = sqlx::query_as(
        "SELECT node_id, outputs FROM step_execution WHERE instance_id = $1 \
         ORDER BY completed_at NULLS LAST, node_id, iteration_index",
    )
    .bind(instance_id)
    .fetch_all(db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?;

    let mut steps = serde_json::Map::new();
    for (node_id, outputs) in rows {
        let slug = slug_by_node.get(&node_id).cloned().unwrap_or(node_id);
        let mut entry = serde_json::Map::new();
        entry.insert("output".to_string(), outputs.unwrap_or(Value::Null));
        steps.insert(slug, Value::Object(entry));
    }

    Ok(json!({
        "result": result,
        "steps": steps,
    }))
}

// --- Assertion evaluator -----------------------------------------------------

fn eval_assertion(scope: &Value, assertion: &Assertion) -> Result<bool, String> {
    let actual = navigate(scope, &assertion.path);
    match assertion.op {
        AssertOp::Exists => Ok(actual.is_some_and(|v| !v.is_null())),
        AssertOp::NotExists => Ok(actual.map_or(true, Value::is_null)),
        AssertOp::Eq => Ok(actual == Some(&assertion.value)),
        AssertOp::Neq => Ok(actual != Some(&assertion.value)),
        AssertOp::Gt | AssertOp::Gte | AssertOp::Lt | AssertOp::Lte => {
            let a = actual
                .and_then(Value::as_f64)
                .ok_or_else(|| format!("path '{}' is not a number", assertion.path))?;
            let b = assertion
                .value
                .as_f64()
                .ok_or_else(|| "rhs is not a number".to_string())?;
            Ok(match assertion.op {
                AssertOp::Gt => a > b,
                AssertOp::Gte => a >= b,
                AssertOp::Lt => a < b,
                AssertOp::Lte => a <= b,
                _ => unreachable!(),
            })
        }
        AssertOp::Matches => {
            let pattern = assertion
                .value
                .as_str()
                .ok_or_else(|| "Matches rhs must be a string regex".to_string())?;
            let actual_str = actual
                .and_then(Value::as_str)
                .ok_or_else(|| format!("path '{}' is not a string", assertion.path))?;
            let re = regex::Regex::new(pattern).map_err(|e| format!("invalid regex: {e}"))?;
            Ok(re.is_match(actual_str))
        }
        AssertOp::Contains => match actual {
            Some(Value::String(s)) => {
                let needle = assertion
                    .value
                    .as_str()
                    .ok_or_else(|| "Contains rhs must be a string for string actual".to_string())?;
                Ok(s.contains(needle))
            }
            Some(Value::Array(arr)) => Ok(arr.contains(&assertion.value)),
            _ => Ok(false),
        },
    }
}

/// Walk a dot-separated path through a JSON value. `result.value.amount`,
/// `steps.review.output.approved`. Numeric segments index into arrays.
fn navigate<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for segment in path.split('.') {
        cur = match cur {
            Value::Object(map) => map.get(segment)?,
            Value::Array(arr) => {
                let idx: usize = segment.parse().ok()?;
                arr.get(idx)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Value {
        json!({
            "result": { "ok": true, "value": { "amount": 1234.5, "approved": "yes" } },
            "steps": {
                "review": { "output": { "approved": true, "comments": "looks good" } },
                "extract": { "output": { "items": [1, 2, 3] } }
            }
        })
    }

    fn a(path: &str, op: AssertOp, value: Value) -> Assertion {
        Assertion { path: path.to_string(), op, value }
    }

    #[test]
    fn eq_on_string_passes() {
        assert!(eval_assertion(&scope(), &a("result.value.approved", AssertOp::Eq, json!("yes"))).unwrap());
    }

    #[test]
    fn eq_on_string_mismatch_fails() {
        assert!(!eval_assertion(&scope(), &a("result.value.approved", AssertOp::Eq, json!("no"))).unwrap());
    }

    #[test]
    fn nested_step_output_path() {
        assert!(eval_assertion(&scope(), &a("steps.review.output.approved", AssertOp::Eq, json!(true))).unwrap());
    }

    #[test]
    fn array_index_path() {
        assert!(eval_assertion(&scope(), &a("steps.extract.output.items.1", AssertOp::Eq, json!(2))).unwrap());
    }

    #[test]
    fn gt_on_number() {
        assert!(eval_assertion(&scope(), &a("result.value.amount", AssertOp::Gt, json!(1000))).unwrap());
        assert!(!eval_assertion(&scope(), &a("result.value.amount", AssertOp::Gt, json!(9999))).unwrap());
    }

    #[test]
    fn gt_on_non_numeric_errors() {
        let err = eval_assertion(&scope(), &a("result.value.approved", AssertOp::Gt, json!(1))).unwrap_err();
        assert!(err.contains("not a number"), "got: {err}");
    }

    #[test]
    fn exists_handles_missing() {
        assert!(eval_assertion(&scope(), &a("result.value.amount", AssertOp::Exists, Value::Null)).unwrap());
        assert!(!eval_assertion(&scope(), &a("result.value.nope", AssertOp::Exists, Value::Null)).unwrap());
        assert!(eval_assertion(&scope(), &a("result.value.nope", AssertOp::NotExists, Value::Null)).unwrap());
    }

    #[test]
    fn matches_regex() {
        assert!(eval_assertion(&scope(), &a("steps.review.output.comments", AssertOp::Matches, json!("looks .*"))).unwrap());
        assert!(!eval_assertion(&scope(), &a("steps.review.output.comments", AssertOp::Matches, json!("^bad"))).unwrap());
    }

    #[test]
    fn contains_substring_and_array() {
        assert!(eval_assertion(&scope(), &a("steps.review.output.comments", AssertOp::Contains, json!("good"))).unwrap());
        assert!(eval_assertion(&scope(), &a("steps.extract.output.items", AssertOp::Contains, json!(2))).unwrap());
        assert!(!eval_assertion(&scope(), &a("steps.extract.output.items", AssertOp::Contains, json!(99))).unwrap());
    }

    #[test]
    fn navigate_walks_objects_and_arrays() {
        let s = scope();
        assert_eq!(navigate(&s, "result.value.amount").and_then(Value::as_f64), Some(1234.5));
        assert_eq!(navigate(&s, "steps.extract.output.items.0").and_then(Value::as_i64), Some(1));
        assert!(navigate(&s, "no.such.path").is_none());
    }
}
