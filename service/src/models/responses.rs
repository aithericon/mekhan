//! Typed response envelopes for handlers that previously returned
//! `serde_json::Value`. Wire format is preserved byte-for-byte; these structs
//! only carry the shape into the OpenAPI spec so frontend codegen can produce
//! real types instead of `unknown` bags.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::catalogue::model::CatalogueEntry;
use crate::handlers::process_live::LogRow;

/// Response shape for `GET /api/v1/instances/{id}/events`.
///
/// Mirrors the literal `json!({ "net_id": ..., "events": [...], "event_count": ... })`
/// envelope the handler previously emitted. `events` stays `Vec<serde_json::Value>`
/// because the petri-lab event shape is heterogeneous (one of many event types).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct InstanceEventsResponse {
    pub net_id: String,
    pub events: Vec<serde_json::Value>,
    pub event_count: usize,
}

/// Response shape for `GET /api/v1/processes/{process_id}/logs/tail`.
///
/// Frontend reads `body.logs[]` directly — keep the single-field envelope.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LogsTailResponse {
    pub logs: Vec<LogRow>,
}

/// Response shape for `GET /api/v1/processes/{process_id}/artifacts/list`.
///
/// Frontend reads `body.entries[]` directly — keep the single-field envelope.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ArtifactsListResponse {
    pub entries: Vec<CatalogueEntry>,
}

/// Response shape for `GET /api/v1/tasks`.
///
/// `tasks` is `Vec<serde_json::Value>` because each task is a
/// `HumanTask`-shaped JSON built by `to_human_task_json` from heterogeneous DB
/// rows — the right level of typing for this endpoint.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TaskListResponse {
    pub tasks: Vec<serde_json::Value>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
    pub has_next: bool,
    pub has_previous: bool,
}

/// Response shape for `POST /api/v1/files/upload/{id}/{node_id}`.
///
/// The handler returns S3 metadata after a successful upload.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct FileUploadResponse {
    pub key: String,
    pub filename: String,
    pub content_type: String,
    pub size: usize,
}

/// Response shape for `GET /api/v1/instances/{id}/step-executions`.
///
/// One row per `(workflow node, execution iteration)` for an instance.
/// Materialized by the step-executions projection consumer
/// (`service/src/projections/step_executions/`). The frontend keys on
/// `node_id` to overlay runtime info onto each node card on the canvas.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StepExecutionResponse {
    pub node_id: String,
    pub iteration_index: i32,
    pub node_kind: String,
    /// `"pending" | "running" | "completed" | "failed" | "skipped"`.
    pub status: String,
    /// `{ "<producer_node_id>": <envelope> }` grouped by upstream owner of
    /// each read-arc place this step consumed.
    pub inputs: Option<serde_json::Value>,
    /// The envelope deposited at the node's `data_port` (parking nodes) or
    /// `workflow_terminals[*]` (End nodes).
    pub outputs: Option<serde_json::Value>,
    /// Decision branch identifier: `"edge:<edge_id>"` for the output that
    /// received the token. `None` for non-branching nodes.
    pub branch_taken: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub duration_ms: Option<i64>,
    /// `EffectFailed` payload (error_message, retryable, ...) for failed steps.
    pub error: Option<serde_json::Value>,
}

/// Response shape for `GET /api/v1/instances/{id}/allocations` and
/// `GET /api/v1/clusters/{id}/leases`.
///
/// One row of the `allocations` projection table — a resource grant on the
/// Petri substrate: either a `datacenter_lease` (an external-cluster
/// Slurm/Nomad/HTTP allocation held by a LeaseScope / Loop body) or a
/// `concurrency_limit_grant` (an admission against one of our own worker pools).
/// Materialized field-for-field by the allocations projection consumer
/// (sequence-guarded upsert keyed on `(net_id, grant_id, kind)`), with a
/// computed `duration_ms` overlaid the same way `StepExecutionResponse` does.
///
/// Every nullable column mirrors a `NULL`-able DB column: pool grants carry no
/// `cluster_resource_id` / `scheduler_flavor`; pool-management nets carry no
/// `instance_id`; timing/accounting fields fill in as the grant progresses.
#[derive(Debug, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct AllocationResponse {
    pub id: Uuid,
    /// `"datacenter_lease" | "concurrency_limit_grant"`.
    pub kind: String,
    pub net_id: String,
    /// Resolved owning instance; `None` for pool-management nets.
    pub instance_id: Option<Uuid>,
    /// Workflow node / LeaseScope container id that holds the grant.
    pub node_id: Option<String>,
    /// Engine grant key (`instance_id:node_id`); equals the accounting signal key.
    pub grant_id: String,
    /// Datacenter resource; `None` for `concurrency_limit_grant`.
    pub cluster_resource_id: Option<Uuid>,
    /// `"slurm" | "nomad" | "http"`; `None` for pool grants.
    pub scheduler_flavor: Option<String>,
    /// Slurm jobid / Nomad dispatched job id.
    pub alloc_id: Option<String>,
    /// Placement host.
    pub node: Option<String>,
    /// `lease-<sanitized grant_id>`.
    pub executor_namespace: Option<String>,
    /// `"pending" | "held" | "released" | "failed" | "expired"`.
    pub status: String,
    pub requested_at: Option<chrono::DateTime<chrono::Utc>>,
    pub acquired_at: Option<chrono::DateTime<chrono::Utc>>,
    pub released_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expiry: Option<chrono::DateTime<chrono::Utc>>,
    pub exit_code: Option<i32>,
    pub queue_wait_ms: Option<i64>,
    pub elapsed_ms: Option<i64>,
    /// Rounded CPU-seconds (the engine payload float is rounded to `i64`).
    pub cpu_seconds: Option<i64>,
    pub gpu_seconds: Option<i64>,
    pub peak_rss_bytes: Option<i64>,
    pub requested_tres: Option<serde_json::Value>,
    pub allocated_tres: Option<serde_json::Value>,
    pub last_error: Option<String>,
    pub last_sequence: i64,
    /// `released_at - acquired_at`, or `now - acquired_at` while `held`.
    /// `None` until the grant is acquired. Computed by the handler — not a
    /// column, so it defaults to `None` when read via `FromRow`.
    #[sqlx(default)]
    pub duration_ms: Option<i64>,
}

impl AllocationResponse {
    /// Overlay the computed `duration_ms` (the row's own column is absent, so
    /// `FromRow` left it `None`): `released_at - acquired_at` for a finished
    /// grant, or `now - acquired_at` for one still `held`.
    pub fn with_duration(mut self) -> Self {
        self.duration_ms = match (self.acquired_at, self.released_at) {
            (Some(a), Some(r)) => Some((r - a).num_milliseconds()),
            (Some(a), None) if self.status == "held" => {
                Some((chrono::Utc::now() - a).num_milliseconds())
            }
            _ => None,
        };
        self
    }
}

/// Alias for the cluster-leases view (`GET /api/v1/clusters/{id}/leases`),
/// which returns the same `allocations` shape filtered to `datacenter_lease`
/// rows for one datacenter resource. Same wire shape as [`AllocationResponse`].
pub type LeaseResponse = AllocationResponse;

/// One spawned sub-workflow child run of a parent instance, returned by
/// `GET /api/v1/instances/{id}/children`. A SubWorkflow node runs its child as
/// a separate engine net; the causality ingest registers each spawn as a
/// first-class child `workflow_instances` row (see migration
/// `20240130000000`). A SubWorkflow inside a Loop/Map spawns one child per
/// iteration, so multiple rows can share `parent_node_id` — ordered by
/// `spawn_seq` (the spawn order, i.e. iteration order).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct InstanceChild {
    /// The child instance id (open `/instances/{id}` to drill in).
    pub id: Uuid,
    /// The SubWorkflow `WorkflowNode.id` in the parent graph that spawned this
    /// child. Group children by this to attach them to a node on the canvas.
    pub parent_node_id: Option<String>,
    /// Parent net's spawn-event sequence; orders sibling children of the same
    /// node (one per Loop/Map iteration).
    pub spawn_seq: Option<i64>,
    /// The child's resolved template id + version (its own graph to render).
    pub template_id: Uuid,
    pub template_version: i32,
    /// Child template display name (JOINed from `workflow_templates`).
    pub template_name: String,
    /// `"created" | "running" | "completed" | "failed" | "cancelled"`.
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}
