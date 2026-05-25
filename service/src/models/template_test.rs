use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::models::instance::StartToken;

// --- Database rows ---

/// A test attached to a logical template family. `template_id` is the family
/// root (the row's `id` when `base_template_id` is NULL, else the
/// `base_template_id`), resolved by `handlers::template_tests::family_root`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct TemplateTest {
    pub id: Uuid,
    pub template_id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub start_tokens: serde_json::Value,
    pub human_answers: serde_json::Value,
    pub assertions: serde_json::Value,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_against_version: Option<i32>,
    pub last_run_passed: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct TemplateTestRun {
    pub id: Uuid,
    pub test_id: Uuid,
    pub instance_id: Uuid,
    pub template_version: i32,
    pub status: String,
    pub failure_detail: Option<serde_json::Value>,
    pub final_scope: Option<serde_json::Value>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestRunStatus {
    Passed,
    Failed,
    Error,
}

impl TestRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Error => "error",
        }
    }
}

// --- Assertion DSL ---

/// A single check evaluated against the runner's synthetic post-instance scope
/// (`{ result, steps.<slug>.output }`). `path` is a dot-pathed JSON pointer
/// into that scope (e.g. `"result.value.invoice_amount"`,
/// `"steps.review.output.approved"`). Evaluation is intentionally data-driven —
/// no DSL parser. See `handlers::template_tests::runner::eval_assertion`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Assertion {
    pub path: String,
    pub op: AssertOp,
    /// Right-hand side. For `Exists`/`NotExists` this is ignored; for
    /// `Matches` it must be a string (regex); for `Contains` it can be a
    /// scalar (substring on string actual / membership on array actual).
    #[serde(default)]
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssertOp {
    Eq,
    Neq,
    Exists,
    NotExists,
    Gt,
    Gte,
    Lt,
    Lte,
    /// Regex match. `actual` is coerced to a string; `value` is the pattern.
    Matches,
    /// For string `actual`: substring containment. For array `actual`: any
    /// element JSON-equals `value`. Anything else: never matches.
    Contains,
}

// --- API request/response types ---

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateTemplateTestRequest {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub start_tokens: Vec<StartToken>,
    /// Map of `<node_slug>` → form data captured by the human-task UI.
    /// Missing slugs fail the run at the first un-stubbed `human.request`.
    #[serde(default)]
    pub human_answers: serde_json::Value,
    #[serde(default)]
    pub assertions: Vec<Assertion>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateTemplateTestRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_tokens: Option<Vec<StartToken>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_answers: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertions: Option<Vec<Assertion>>,
}

/// Aggregate result for `run-all` (also returned to the publication gate).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RunAllResponse {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub errored: usize,
    pub runs: Vec<TemplateTestRun>,
}

/// Failure shape returned by the publication gate when one or more enabled
/// tests block publish. Surfaces in the editor's `PublishGateModal`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublishGateBlockedResponse {
    pub failing_tests: Vec<FailingTestInfo>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FailingTestInfo {
    pub test_id: Uuid,
    pub name: String,
    pub reason: String,
    pub run_id: Option<Uuid>,
}
