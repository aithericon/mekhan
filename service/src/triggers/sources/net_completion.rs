//! NetCompletion trigger source (Phase 5d).
//!
//! Fires when an instance of a source template terminates with a matching
//! status. Wired into the existing `lifecycle.rs` listener so the dispatcher
//! sees every completion / cancellation / failure without inventing a parallel
//! subscription.
//!
//! Payload scope handed to `payload_mapping` expressions:
//!   - `payload.source_instance_id`  — terminating instance UUID
//!   - `payload.source_template_id`  — terminating template UUID
//!   - `payload.source_version`      — terminating template version
//!   - `payload.completion_status`   — `"success" | "failure" | "cancelled"`
//!   - `payload.completion_time`     — RFC 3339 UTC
//!
//! `final_token` (proposal §4.3) is intentionally absent in Phase 5d — fetching
//! the terminal token requires walking the instance's event log, which we can
//! add as a follow-up once a concrete consumer needs it.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::template::{CompletionStatus, TriggerSource};
use crate::triggers::dispatcher::TriggerDispatcher;

/// Status discriminant on the wire — matches `CompletionStatus`'s `snake_case`
/// rename so the JSON shape stays consistent across sources.
pub fn status_as_str(s: CompletionStatus) -> &'static str {
    match s {
        CompletionStatus::Success => "success",
        CompletionStatus::Failure => "failure",
        CompletionStatus::Cancelled => "cancelled",
        CompletionStatus::Any => "any",
    }
}

/// Whether a trigger's `on` filter matches an actual terminal status.
fn status_matches(filter: CompletionStatus, actual: &str) -> bool {
    match filter {
        CompletionStatus::Any => true,
        CompletionStatus::Success => actual == "success" || actual == "completed",
        CompletionStatus::Failure => actual == "failure" || actual == "failed",
        CompletionStatus::Cancelled => actual == "cancelled",
    }
}

/// Hook called from the lifecycle listener for every terminal net event.
/// `actual_status` is the wire value out of NATS (`"completed"`, `"cancelled"`,
/// `"failed"`) — we map both `"completed"` and `"success"` to the `Success`
/// filter since the lifecycle subject uses `"completed"`.
pub async fn evaluate(
    dispatcher: &TriggerDispatcher,
    db: &PgPool,
    net_id: &str,
    actual_status: &str,
) {
    // Find the instance row → template_id + version.
    let row = match sqlx::query_as::<_, (Uuid, Uuid, i32)>(
        "SELECT id, template_id, template_version FROM workflow_instances WHERE net_id = $1",
    )
    .bind(net_id)
    .fetch_optional(db)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(net_id, "net_completion trigger lookup failed: {e}");
            return;
        }
    };
    let (instance_id, template_id, template_version) = row;

    for rec in dispatcher.list_all() {
        if !rec.enabled {
            continue;
        }
        let TriggerSource::NetCompletion(ref nc) = rec.source else {
            continue;
        };

        if nc.source_template_id != template_id {
            continue;
        }
        if let Some(v) = nc.source_version {
            if v != template_version {
                continue;
            }
        }
        if !status_matches(nc.on, actual_status) {
            continue;
        }

        let payload = json!({
            "source_instance_id": instance_id,
            "source_template_id": template_id,
            "source_version": template_version,
            "completion_status": actual_status,
            "completion_time": chrono::Utc::now().to_rfc3339(),
        });

        match dispatcher
            .fire(
                &rec.node_id,
                payload,
                petri_api_types::DispatchOptions::default(),
                None,
            )
            .await
        {
            Ok(result) => {
                tracing::info!(
                    node_id = %rec.node_id,
                    source_instance = %instance_id,
                    outcome = ?result.outcome,
                    "net_completion trigger fired"
                );
            }
            Err(e) => {
                tracing::warn!(
                    node_id = %rec.node_id,
                    source_instance = %instance_id,
                    "net_completion trigger fire failed: {e}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_filters() {
        assert!(status_matches(CompletionStatus::Any, "completed"));
        assert!(status_matches(CompletionStatus::Any, "failed"));
        assert!(status_matches(CompletionStatus::Success, "completed"));
        assert!(status_matches(CompletionStatus::Success, "success"));
        assert!(!status_matches(CompletionStatus::Success, "failed"));
        assert!(status_matches(CompletionStatus::Failure, "failed"));
        assert!(status_matches(CompletionStatus::Cancelled, "cancelled"));
        assert!(!status_matches(CompletionStatus::Cancelled, "completed"));
    }
}
