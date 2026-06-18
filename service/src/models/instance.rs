use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct WorkflowInstance {
    pub id: Uuid,
    pub template_id: Uuid,
    pub template_version: i32,
    pub net_id: String,
    pub status: String,
    pub created_by: Uuid,
    /// Last mutator (`subject_as_uuid()`). NULL for projector-driven status
    /// transitions (FE renders "System") and for pre-Phase-2 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    /// Advanced on every mutation (Phase 2). DEFAULT NOW() at row birth.
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub current_step: Option<String>,
    pub metadata: serde_json::Value,
    /// Structured result envelope (`{ ok: true, value }` /
    /// `{ ok: false, error: { reason, value } }`) declared by the workflow's
    /// End/Failure result binding. NULL until the instance reaches a terminal
    /// state, and stays NULL for workflows with no result binding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Categorizes the instance. `live` (default) is a production run.
    /// `draft` is a user-initiated experimental run hidden from default
    /// list views. `test_run` is spawned by the template-test runner.
    pub mode: String,
    /// Set when `mode = 'test_run'`: the test this instance is running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_id: Option<Uuid>,
    /// Set when a test was promoted from this instance — points back at the
    /// instance whose event log seeded the test's fixture. Audit-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_instance_id: Option<Uuid>,
    /// SubWorkflow hierarchy: the instance that ran the SubWorkflow node whose
    /// `spawn_net` effect created this child net. NULL for top-level
    /// instances. See migration `20240130000000`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_instance_id: Option<Uuid>,
    /// The SubWorkflow `WorkflowNode.id` in the parent graph that spawned this
    /// child (the spawn transition is `t_{parent_node_id}_spawn`). NULL for
    /// top-level instances.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_node_id: Option<String>,
    /// Top-of-tree instance id (the parent's root, or the parent itself), so a
    /// whole sub-workflow tree is reachable in one query. NULL for top-level
    /// instances.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_instance_id: Option<Uuid>,
    /// Parent net's spawn-event sequence; orders sibling children when a
    /// Loop/Map spawns the same SubWorkflow node multiple times (one child per
    /// iteration). NULL for top-level instances.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_seq: Option<i64>,
    /// Per-run compiled graph for a DRAFT dev-run (`mode = 'draft'`). A draft
    /// compiles from the live Y.Doc, so the template's `graph` column is stale
    /// (it only updates on publish). The instance Workflow view prefers this
    /// snapshot so it renders what actually ran. NULL for live/test_run
    /// instances, which read the immutable published template version instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_snapshot: Option<serde_json::Value>,
    /// Per-run compiled `interface_json` for a DRAFT dev-run — the sibling of
    /// [`Self::graph_snapshot`]. The step-executions projector and the per-node
    /// interface drawer prefer this over the (stale-for-drafts)
    /// `template.interface_json`. NULL for live/test_run instances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_snapshot: Option<serde_json::Value>,
    /// The caller's effective object role on THIS instance (`owner|admin|
    /// editor|viewer`), resolved by the Phase-3 ACL resolver in `get_instance`.
    /// NOT a database column — `#[sqlx(default)]` lets the `SELECT *` row map
    /// satisfy `FromRow`; the handler fills it in after the access check. Lets
    /// the SPA gate edit affordances (Cancel ≥ editor) without a second call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(default)]
    pub my_effective_role: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    Created,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl InstanceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for InstanceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Instance with template name, returned by list queries (JOIN with workflow_templates).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct InstanceListItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub template_version: i32,
    pub net_id: String,
    pub status: String,
    pub created_by: Uuid,
    /// Last mutator (`subject_as_uuid()`). NULL for projector-driven status
    /// transitions and pre-Phase-2 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub current_step: Option<String>,
    pub metadata: serde_json::Value,
    pub template_name: String,
    pub mode: String,
    pub test_id: Option<Uuid>,
    /// The caller's effective role on this instance (`owner|admin|editor|
    /// viewer`), annotated by `list_instances` so the SPA can hide stale edit
    /// affordances. Not a DB column — `#[sqlx(default)]` keeps `FromRow`
    /// working; the handler fills it after the row fetch. The backend still
    /// enforces on every mutate path regardless of this hint.
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_effective_role: Option<String>,
}

impl crate::auth::AclAnnotated for InstanceListItem {
    fn acl_id(&self) -> Uuid {
        self.id
    }
    fn set_my_effective_role(&mut self, role: Option<String>) {
        self.my_effective_role = role;
    }
}

// --- API request/response types ---

/// A typed token seed for a single `Start` block in the template. The token
/// must be a JSON object matching the Start's declared `initial` port shape
/// (required fields present, kinds compatible). See `FieldKind::accepts`.
///
/// Snake-case wire fields to match the surrounding `CreateInstanceRequest`
/// (`start_tokens`, `template_id`, etc.).
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct StartToken {
    /// `WorkflowNode.id` of the Start block this token seeds.
    pub start_block_id: String,
    /// JSON object whose keys match the Start's `initial` port field names.
    pub token: serde_json::Value,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateInstanceRequest {
    pub template_id: Uuid,
    /// Typed seeds for each Start block in the template. A Start with a
    /// non-empty `initial` port requires a matching entry here; otherwise the
    /// API returns 400. Starts with an empty `initial` port can be omitted
    /// (each gets a default `{}` token with system fields injected).
    #[serde(default)]
    pub start_tokens: Vec<StartToken>,
    /// Free-form audit metadata stored on the instance row. Unlike pre-typed-ports
    /// behavior, this is NOT merged into initial Petri tokens — token shape is
    /// driven solely by `start_tokens`.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// `live` (default) | `draft` | `test_run`. `test_run` is reserved for
    /// the test runner — callers requesting it directly are rejected.
    #[serde(default)]
    pub mode: Option<String>,
    /// Per-instance resource/pool bindings: `slot_key -> resource_id` (Phase
    /// C). The HIGHEST-precedence binding tier — overrides the per-workspace
    /// default, platform auto-bind, and home-workspace baseline for any slot in
    /// the template's auto-derived requirements manifest. Slot keys are the
    /// binding aliases surfaced by `GET /templates/{id}/requirements`. Omitted /
    /// `None` ⇒ every slot resolves through the lower tiers (a template with no
    /// requirements manifest ignores this entirely and launches as today).
    #[serde(default)]
    pub bindings: Option<std::collections::HashMap<String, Uuid>>,
}

#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct ListInstancesQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
    pub template_id: Option<Uuid>,
    /// Filter by template version chain: instances of ANY version in the
    /// family. Accepts the chain root (`base_template_id`) or any version
    /// row's id — both resolve through `COALESCE(base_template_id, id)`.
    /// Unlike `template_id`, which pins one exact version row.
    pub template_family: Option<Uuid>,
    pub status: Option<String>,
    /// Filter by `mode`. Default behavior when omitted is to return only
    /// `live` instances; pass `mode=any` to include drafts and test runs,
    /// or `mode=draft` / `mode=test_run` to scope explicitly.
    pub mode: Option<String>,
}

fn default_page() -> i64 {
    1
}
fn default_per_page() -> i64 {
    20
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceStateResponse {
    pub instance_id: Uuid,
    pub net_id: String,
    pub status: String,
    pub events: Vec<serde_json::Value>,
    pub event_count: usize,
    pub marking: serde_json::Value,
    pub engine: EngineStatus,
    pub enabled_transitions: Vec<String>,
    pub current_step: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EngineStatus {
    pub available: bool,
    #[schema(value_type = Option<String>)]
    pub run_mode: Option<petri_api_types::RunMode>,
}
