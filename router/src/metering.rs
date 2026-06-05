//! Metering — one record per completed/cancelled/errored request.
//!
//! The record is published on `inference.metering.{request_id}` (the
//! doc-11-canonical subject; the P5 Postgres projector subscribes to the same
//! and folds it into `inference_request_log`, the GDPR processing record). For
//! the MVP the event is the transport only — durable persistence + the
//! executor-side identity-header injection that makes rows attributable are
//! P5. Absent NATS, metering is a no-op.

use chrono::{DateTime, Utc};
use inference_core::{InferenceRequestLog, Usage};
use tracing::warn;

/// The terminal disposition of a request, before usage is known.
#[derive(Debug, Clone, Copy)]
pub enum MeterStatus {
    Completed,
    Cancelled,
    UpstreamError,
}

impl MeterStatus {
    fn as_str(self) -> &'static str {
        match self {
            MeterStatus::Completed => "completed",
            MeterStatus::Cancelled => "cancelled",
            MeterStatus::UpstreamError => "upstream_error",
        }
    }
}

/// Request-scoped metering context captured at admission; `finish` stamps the
/// terminal record.
#[derive(Debug, Clone)]
pub struct MeterContext {
    pub request_id: String,
    pub tenant: String,
    pub instance_id: Option<String>,
    pub step_id: Option<String>,
    pub model: String,
    pub replica_id: String,
    pub replica_base_url: String,
    pub residency_zone: Option<String>,
    pub slo_tier: Option<String>,
    pub started_at: DateTime<Utc>,
}

impl MeterContext {
    pub fn finish(&self, usage: Option<Usage>, status: MeterStatus) -> InferenceRequestLog {
        // A completed request with no parseable usage is `unmetered` (the
        // client didn't ask for `stream_options.include_usage`, or the upstream
        // omitted it) — surfaced so the audit ledger never silently claims 0.
        let status_str = match (status, usage) {
            (MeterStatus::Completed, None) => "unmetered".to_string(),
            (s, _) => s.as_str().to_string(),
        };
        let u = usage.unwrap_or_default();
        InferenceRequestLog {
            request_id: self.request_id.clone(),
            tenant: self.tenant.clone(),
            instance_id: self.instance_id.clone(),
            step_id: self.step_id.clone(),
            model: self.model.clone(),
            replica_id: self.replica_id.clone(),
            replica_base_url: self.replica_base_url.clone(),
            residency_zone: self.residency_zone.clone(),
            slo_tier: self.slo_tier.clone(),
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            status: status_str,
            started_at: self.started_at,
            finished_at: Utc::now(),
        }
    }
}

/// Publish a metering record on `inference.metering.{request_id}`. No-op when
/// NATS is not configured.
pub async fn publish_meter(client: &Option<async_nats::Client>, record: &InferenceRequestLog) {
    let Some(client) = client else {
        return;
    };
    let subject = format!("inference.metering.{}", record.request_id);
    match serde_json::to_vec(record) {
        Ok(bytes) => {
            if let Err(e) = client.publish(subject, bytes.into()).await {
                warn!(request_id = %record.request_id, error = %e, "failed to publish metering record");
            }
        }
        Err(e) => warn!(error = %e, "failed to serialize metering record"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> MeterContext {
        MeterContext {
            request_id: "req-1".into(),
            tenant: "dev".into(),
            instance_id: Some("inst-1".into()),
            step_id: None,
            model: "m1".into(),
            replica_id: "replica-0".into(),
            replica_base_url: "http://a".into(),
            residency_zone: Some("eu-west".into()),
            slo_tier: None,
            started_at: Utc::now(),
        }
    }

    #[test]
    fn completed_with_usage_is_metered() {
        let rec = ctx().finish(
            Some(Usage {
                prompt_tokens: 3,
                completion_tokens: 4,
                total_tokens: 7,
            }),
            MeterStatus::Completed,
        );
        assert_eq!(rec.status, "completed");
        assert_eq!(rec.total_tokens, 7);
    }

    #[test]
    fn completed_without_usage_is_unmetered() {
        let rec = ctx().finish(None, MeterStatus::Completed);
        assert_eq!(rec.status, "unmetered");
        assert_eq!(rec.total_tokens, 0);
    }

    #[test]
    fn cancelled_keeps_partial_usage() {
        let rec = ctx().finish(
            Some(Usage {
                prompt_tokens: 5,
                completion_tokens: 1,
                total_tokens: 6,
            }),
            MeterStatus::Cancelled,
        );
        assert_eq!(rec.status, "cancelled");
        assert_eq!(rec.total_tokens, 6);
    }
}
