pub mod assets;
pub mod auth_tokens;
pub mod backends;
pub mod capabilities;
pub mod capacities;
pub mod cloud_layer_proxy;
pub mod clusters;
pub mod container_images;
pub mod demos;
pub mod executions;
pub mod files;
pub mod folders;
pub mod health;
pub mod inference_metering;
pub mod instances;
pub mod job_templates;
pub mod me;
pub mod model_pool;
pub mod model_replicas;
pub mod node_types;
pub mod observability;
pub mod openapi_bundle;
pub mod process_live;
pub mod resources;
pub mod runners;
pub mod task_stream;
pub mod template_tests;
pub mod templates;
pub mod triggers;
pub mod users;
pub mod workers;
pub mod workspaces;
pub mod yjs_sync;

use sqlx::PgPool;
use uuid::Uuid;

use crate::models::error::ApiError;
use crate::models::template::WorkflowTemplate;

/// Fetch a template row by id, returning a 404 `ApiError` when it doesn't exist.
///
/// Collapses the `SELECT * FROM workflow_templates WHERE id = $1` +
/// `fetch_optional` + error-map + `ok_or_else(not_found)` idiom that was
/// previously copy-pasted across the template / instance / yjs handlers. DB
/// errors propagate via `From<sqlx::Error> for ApiError` (→ 500).
pub(crate) async fn require_template(db: &PgPool, id: Uuid) -> Result<WorkflowTemplate, ApiError> {
    sqlx::query_as::<_, WorkflowTemplate>("SELECT * FROM workflow_templates WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| ApiError::not_found("template not found"))
}
