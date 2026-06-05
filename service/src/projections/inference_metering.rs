//! Inference metering audit-ledger ingest (model-pool P5, docs/29 §7').
//!
//! The router publishes one COMPLETE `inference_core::InferenceRequestLog` per
//! request on `inference.metering.{request_id}` (captured by the
//! `INFERENCE_METERING` JetStream stream). Unlike the event-sourced projections
//! (`model_replicas`, `template_stagings`, …) there is NO per-net replay: each
//! message is a self-contained terminal record, so we upsert it directly into
//! `inference_request_log` keyed by `request_id` — idempotent on redelivery.
//!
//! Field→column mapping: `record.tenant → tenant_id`, `record.model →
//! model_id`; token counts bind as `i64` (the BIGINT columns). A malformed
//! payload is recorded as a silent drop + ACKed (never wedges the stream); a DB
//! error is `Nak`'d with a 2s backoff so the record is retried.

use async_nats::jetstream::AckKind;
use futures::StreamExt;
use sqlx::PgPool;

use inference_core::InferenceRequestLog;

use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;

/// Start the inference-metering ingest consumer. Spawned alongside the other
/// projection consumers in `main.rs`. Runs until the message stream ends.
pub async fn start_inference_metering_ingest(nats: MekhanNats, db: PgPool) {
    let consumer = match nats.inference_metering_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create inference_metering consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start inference_metering message stream: {e}");
            return;
        }
    };

    tracing::info!("inference_metering ingest started on inference.metering.>");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("inference_metering ingest message error: {e}");
                continue;
            }
        };

        let subject = msg.subject.as_str();
        let record: InferenceRequestLog = match serde_json::from_slice(&msg.payload) {
            Ok(r) => r,
            Err(e) => {
                record_silent_drop_with(
                    "inference_metering_envelope",
                    &e,
                    serde_json::json!({ "subject": subject }),
                    Some(&msg.payload),
                );
                let _ = msg.ack().await;
                continue;
            }
        };

        match upsert_record(&db, &record).await {
            Ok(()) => {
                let _ = msg.ack().await;
            }
            Err(e) => {
                tracing::error!(
                    request_id = %record.request_id,
                    "inference_metering upsert failed: {e}"
                );
                let _ = msg
                    .ack_with(AckKind::Nak(Some(std::time::Duration::from_secs(2))))
                    .await;
            }
        }
    }

    tracing::warn!("inference_metering ingest stream ended");
}

/// Idempotently persist one metering record. Keyed by `request_id` (PRIMARY
/// KEY); a redelivery folds the terminal fields onto the existing row. The
/// `tenant → tenant_id` / `model → model_id` renames and the `u64 → i64` token
/// casts happen at the binds.
async fn upsert_record(db: &PgPool, record: &InferenceRequestLog) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO inference_request_log \
            (request_id, tenant_id, instance_id, step_id, model_id, replica_id, \
             replica_base_url, residency_zone, slo_tier, status, prompt_tokens, \
             completion_tokens, total_tokens, started_at, finished_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
         ON CONFLICT (request_id) DO UPDATE SET \
            status = EXCLUDED.status, \
            prompt_tokens = EXCLUDED.prompt_tokens, \
            completion_tokens = EXCLUDED.completion_tokens, \
            total_tokens = EXCLUDED.total_tokens, \
            finished_at = EXCLUDED.finished_at",
    )
    .bind(&record.request_id)
    .bind(&record.tenant)
    .bind(record.instance_id.as_deref())
    .bind(record.step_id.as_deref())
    .bind(&record.model)
    .bind(&record.replica_id)
    .bind(&record.replica_base_url)
    .bind(record.residency_zone.as_deref())
    .bind(record.slo_tier.as_deref())
    .bind(&record.status)
    .bind(record.prompt_tokens as i64)
    .bind(record.completion_tokens as i64)
    .bind(record.total_tokens as i64)
    .bind(record.started_at)
    .bind(record.finished_at)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete record published by the router deserializes back into the
    /// SAME `InferenceRequestLog` the projector upserts — proving the wire
    /// shape + the `tenant`/`model` field names line up with the column binds
    /// (`tenant → tenant_id`, `model → model_id`). No live DB needed.
    #[test]
    fn router_record_roundtrips_into_upsert_fields() {
        let json = serde_json::json!({
            "request_id": "req-42",
            "tenant": "acme",
            "instance_id": "inst-7",
            "model": "qwen2.5-7b",
            "replica_id": "replica-0",
            "replica_base_url": "http://10.0.0.1:8000",
            "residency_zone": "eu-west",
            "prompt_tokens": 11,
            "completion_tokens": 22,
            "total_tokens": 33,
            "status": "completed",
            "started_at": "2026-06-05T00:00:00Z",
            "finished_at": "2026-06-05T00:00:01Z"
        });

        let record: InferenceRequestLog = serde_json::from_value(json).expect("parses");

        // The binds the upsert uses, asserted against the parsed record.
        assert_eq!(record.request_id, "req-42");
        assert_eq!(record.tenant, "acme"); // → tenant_id
        assert_eq!(record.model, "qwen2.5-7b"); // → model_id
        assert_eq!(record.instance_id.as_deref(), Some("inst-7"));
        assert_eq!(record.step_id, None); // absent Option field tolerated
        assert_eq!(record.slo_tier, None);
        assert_eq!(record.prompt_tokens as i64, 11);
        assert_eq!(record.completion_tokens as i64, 22);
        assert_eq!(record.total_tokens as i64, 33);
        assert_eq!(record.status, "completed");
    }
}
