use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct HpiProcess {
    pub process_id: String,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub status: String,
    pub owner: Option<String>,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Workflow instance that produced this process (NULL for petri-lab
    /// scenarios created outside a mekhan instance, or unlinked legacy rows).
    pub instance_id: Option<Uuid>,
    /// Engine net id ("mekhan-{instance_id}") for the producing instance.
    pub net_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct HpiTask {
    pub id: String,
    pub process_id: String,
    pub title: String,
    pub status: String,
    pub assignee: Option<String>,
    pub detail: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct HpiMetric {
    pub process_id: String,
    pub key: String,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct HpiMetricSummary {
    pub key: String,
    pub count: i64,
    pub min_value: f64,
    pub max_value: f64,
    pub avg_value: f64,
    pub last_value: f64,
    pub last_timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct HpiLog {
    pub id: i64,
    pub process_id: String,
    pub level: String,
    pub source: Option<String>,
    pub message: String,
    pub detail: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}

// API response types

#[derive(Debug, Serialize, ToSchema)]
pub struct ProcessDetail {
    #[serde(flatten)]
    pub process: HpiProcess,
    pub tasks: Vec<HpiTask>,
    pub recent_metrics: Vec<HpiMetric>,
    pub recent_logs: Vec<HpiLog>,
    pub artifact_count: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProcessStats {
    pub total: i64,
    pub active: i64,
    pub completed: i64,
    pub failed: i64,
}

// Update request

#[derive(Debug, Deserialize, ToSchema)]
pub struct ProcessUpdateRequest {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub owner: Option<String>,
}
