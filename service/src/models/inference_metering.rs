//! Model-pool P5 (docs/29 §7') — the inference metering / GDPR processing-record
//! read model.
//!
//! One [`InferenceRequestLogRow`] per metered inference request, materialized by
//! the `inference_metering` projector
//! (`crate::projections::inference_metering`) off the `INFERENCE_METERING`
//! JetStream stream the router publishes to. Columns mirror the
//! `inference_request_log` table 1:1 (see
//! `migrations/20240148000000_inference_request_log.sql`); the `record.tenant →
//! tenant_id` and `record.model → model_id` renames happen at the upsert binds
//! in the projector.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// One `inference_request_log` row — the durable GDPR processing record + the
/// Control-Plane audit-ledger read. Token counts are stored `BIGINT` (`i64`).
#[derive(Clone, Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct InferenceRequestLogRow {
    pub request_id: String,
    pub tenant_id: String,
    pub instance_id: Option<String>,
    pub step_id: Option<String>,
    pub model_id: String,
    pub replica_id: String,
    pub replica_base_url: String,
    pub residency_zone: Option<String>,
    pub slo_tier: Option<String>,
    pub status: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}
