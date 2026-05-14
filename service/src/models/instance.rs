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
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub current_step: Option<String>,
    pub metadata: serde_json::Value,
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
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub current_step: Option<String>,
    pub metadata: serde_json::Value,
    pub template_name: String,
}

// --- API request/response types ---

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateInstanceRequest {
    pub template_id: Uuid,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct ListInstancesQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
    pub template_id: Option<Uuid>,
    pub status: Option<String>,
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
