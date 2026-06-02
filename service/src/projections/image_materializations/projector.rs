//! Pure fold from a materialize net's event log into a [`MaterializeUpdate`].
//!
//! A materialize net (`materialize-<id>`, built by
//! [`crate::petri::staging_net::build_materialize_image_net`]) fires the engine's
//! `materialize_image` inline effect exactly once. We fold its terminal event
//! into a single update keyed by the `image_materializations` row id:
//!
//! - `EffectCompleted { effect_handler_id: "materialize_image", effect_result }`
//!   (the NORMAL path — BOTH cluster success AND a non-fatal cluster failure):
//!   read `materialize_id` / `status` / `digest` / `sif_path` / `size_bytes` /
//!   `error` straight off `effect_result`.
//! - `EffectFailed { effect_handler_id: "materialize_image", error_message }`
//!   (the FATAL path — a config/parse error returned `Err`): mark `failed`,
//!   recovering the id from the net id (`materialize-<uuid>`).
//!
//! Pure: identical `(events, net_id)` → identical output.

use uuid::Uuid;

use petri_domain::{DomainEvent, PersistedEvent};

/// One projected materialization outcome — the fields the consumer upserts onto
/// the `image_materializations` row identified by `materialize_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializeUpdate {
    pub materialize_id: Uuid,
    /// `"ready"` | `"failed"`.
    pub status: String,
    pub digest: Option<String>,
    pub sif_path: Option<String>,
    pub size_bytes: Option<i64>,
    pub last_error: Option<String>,
}

/// Recover the `image_materializations` row id from a `materialize-<uuid>` net id.
fn materialize_id_from_net(net_id: &str) -> Option<Uuid> {
    net_id
        .strip_prefix("materialize-")
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// Project a materialize net's event stream into at most one [`MaterializeUpdate`].
/// `None` when no terminal `materialize_image` event is present yet.
pub fn project_materialize(events: &[PersistedEvent], net_id: &str) -> Option<MaterializeUpdate> {
    let mut out: Option<MaterializeUpdate> = None;
    for ev in events {
        match &ev.event {
            DomainEvent::EffectCompleted {
                effect_handler_id,
                effect_result,
                ..
            } if effect_handler_id == "materialize_image" => {
                let materialize_id = effect_result
                    .get("materialize_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .or_else(|| materialize_id_from_net(net_id));
                let Some(materialize_id) = materialize_id else {
                    continue;
                };
                let status = effect_result
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ready")
                    .to_string();
                let digest = effect_result
                    .get("digest")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let sif_path = effect_result
                    .get("sif_path")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let size_bytes = effect_result.get("size_bytes").and_then(|v| v.as_i64());
                let last_error = effect_result
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                out = Some(MaterializeUpdate {
                    materialize_id,
                    status,
                    digest,
                    sif_path,
                    size_bytes,
                    last_error,
                });
            }
            DomainEvent::EffectFailed {
                effect_handler_id,
                error_message,
                ..
            } if effect_handler_id == "materialize_image" => {
                let Some(materialize_id) = materialize_id_from_net(net_id) else {
                    continue;
                };
                out = Some(MaterializeUpdate {
                    materialize_id,
                    status: "failed".to_string(),
                    digest: None,
                    sif_path: None,
                    size_bytes: None,
                    last_error: Some(error_message.clone()),
                });
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use petri_domain::TransitionId;

    const MAT: &str = "44444444-4444-4444-4444-444444444444";
    fn net() -> String {
        format!("materialize-{MAT}")
    }

    fn effect_completed(result: serde_json::Value) -> PersistedEvent {
        PersistedEvent {
            sequence: 1,
            timestamp: Utc.timestamp_opt(500, 0).single().unwrap(),
            event: DomainEvent::EffectCompleted {
                transition_id: TransitionId("t_materialize".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "materialize_image".to_string(),
                effect_result: result,
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            hash: String::new(),
            previous_hash: None,
        }
    }

    #[test]
    fn ready_carries_digest_and_path() {
        let ev = effect_completed(serde_json::json!({
            "materialize_id": MAT, "status": "ready",
            "digest": "abc123", "sif_path": "/shared/sif/abc123.sif", "size_bytes": 42,
        }));
        let u = project_materialize(&[ev], &net()).expect("update");
        assert_eq!(u.materialize_id, Uuid::parse_str(MAT).unwrap());
        assert_eq!(u.status, "ready");
        assert_eq!(u.digest.as_deref(), Some("abc123"));
        assert_eq!(u.sif_path.as_deref(), Some("/shared/sif/abc123.sif"));
        assert_eq!(u.size_bytes, Some(42));
        assert_eq!(u.last_error, None);
    }

    #[test]
    fn cluster_failure_is_failed_data() {
        let ev = effect_completed(serde_json::json!({
            "materialize_id": MAT, "status": "failed",
            "error": "apptainer pull returned 500",
        }));
        let u = project_materialize(&[ev], &net()).expect("update");
        assert_eq!(u.status, "failed");
        assert_eq!(u.last_error.as_deref(), Some("apptainer pull returned 500"));
        assert!(u.digest.is_none());
    }

    #[test]
    fn fatal_effect_failed_marks_failed_from_net_id() {
        let ev = PersistedEvent {
            sequence: 1,
            timestamp: Utc.timestamp_opt(100, 0).single().unwrap(),
            event: DomainEvent::EffectFailed {
                transition_id: TransitionId("t_materialize".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "materialize_image".to_string(),
                error_message: "missing image_ref".to_string(),
                tokens_consumed: true,
                input_data: None,
                retryable: false,
            },
            hash: String::new(),
            previous_hash: None,
        };
        let u = project_materialize(&[ev], &net()).expect("update");
        assert_eq!(u.materialize_id, Uuid::parse_str(MAT).unwrap());
        assert_eq!(u.status, "failed");
        assert_eq!(u.last_error.as_deref(), Some("missing image_ref"));
    }

    #[test]
    fn no_event_yields_none() {
        assert!(project_materialize(&[], &net()).is_none());
    }
}
