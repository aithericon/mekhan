//! Pure fold from a staging net's event log into a [`StagingUpdate`].
//!
//! A staging net (`staging-<staging_id>`, built by
//! [`crate::petri::staging_net::build_staging_net`]) fires the engine's
//! `stage_template` inline effect exactly once. We fold its terminal event into a
//! single update keyed by the `template_stagings` row id:
//!
//! - `EffectCompleted { effect_handler_id: "stage_template", effect_result }`
//!   (the NORMAL path — the handler reports BOTH cluster success AND a non-fatal
//!   cluster failure here): read `staging_id` / `status` / `remote_ref` / `error`
//!   straight off `effect_result`.
//! - `EffectFailed { effect_handler_id: "stage_template", error_message }` (the
//!   FATAL path — a config/parse error returned `Err`): mark `failed` with
//!   `error_message`. `staging_id` isn't in the event, so it's recovered from the
//!   net id (`staging-<uuid>`).
//!
//! Pure: identical `(events, net_id)` → identical output. Reused by the consumer
//! (online) and tests (offline replay).

use chrono::{DateTime, Utc};
use uuid::Uuid;

use petri_domain::{DomainEvent, PersistedEvent};

/// One projected staging outcome — the fields the consumer upserts onto the
/// `template_stagings` row identified by `staging_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagingUpdate {
    pub staging_id: Uuid,
    /// `"staged"` | `"failed"`.
    pub status: String,
    pub remote_ref: Option<String>,
    pub last_error: Option<String>,
    /// Event timestamp of the success (None on failure).
    pub staged_at: Option<DateTime<Utc>>,
    /// Engine event sequence of the folded terminal event (upsert guard).
    pub last_sequence: u64,
}

/// Recover the `template_stagings` row id from a `staging-<uuid>` net id.
fn staging_id_from_net(net_id: &str) -> Option<Uuid> {
    net_id
        .strip_prefix("staging-")
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// Project a staging net's event stream into at most one [`StagingUpdate`].
/// `None` when no terminal `stage_template` event is present yet.
pub fn project_staging(events: &[PersistedEvent], net_id: &str) -> Option<StagingUpdate> {
    let mut out: Option<StagingUpdate> = None;
    for ev in events {
        match &ev.event {
            DomainEvent::EffectCompleted {
                effect_handler_id,
                effect_result,
                ..
            } if effect_handler_id == "stage_template" => {
                // Prefer the echoed staging_id; fall back to the net id.
                let staging_id = effect_result
                    .get("staging_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .or_else(|| staging_id_from_net(net_id));
                let Some(staging_id) = staging_id else {
                    continue;
                };
                let status = effect_result
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("staged")
                    .to_string();
                let remote_ref = effect_result
                    .get("remote_ref")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let last_error = effect_result
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let staged_at = (status == "staged").then_some(ev.timestamp);
                out = Some(StagingUpdate {
                    staging_id,
                    status,
                    remote_ref,
                    last_error,
                    staged_at,
                    last_sequence: ev.sequence,
                });
            }
            // Fatal path: the handler returned Err (bad config / unparseable
            // request). staging_id isn't journaled — recover it from the net id.
            DomainEvent::EffectFailed {
                effect_handler_id,
                error_message,
                ..
            } if effect_handler_id == "stage_template" => {
                let Some(staging_id) = staging_id_from_net(net_id) else {
                    continue;
                };
                out = Some(StagingUpdate {
                    staging_id,
                    status: "failed".to_string(),
                    remote_ref: None,
                    last_error: Some(error_message.clone()),
                    staged_at: None,
                    last_sequence: ev.sequence,
                });
            }
            _ => {}
        }
    }
    out
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use petri_domain::TransitionId;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).single().expect("valid ts")
    }

    const STAGING: &str = "33333333-3333-3333-3333-333333333333";
    fn net() -> String {
        format!("staging-{STAGING}")
    }

    fn effect_completed(seq: u64, ts_secs: i64, result: serde_json::Value) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: ts(ts_secs),
            event: DomainEvent::EffectCompleted {
                transition_id: TransitionId("t_stage".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "stage_template".to_string(),
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
    fn staged_success_carries_remote_ref_and_staged_at() {
        let ev = effect_completed(
            1,
            500,
            serde_json::json!({
                "staging_id": STAGING, "status": "staged",
                "remote_ref": "petri-mumax3-worker", "slug": "petri-mumax3-worker",
            }),
        );
        let u = project_staging(&[ev], &net()).expect("update");
        assert_eq!(u.staging_id, Uuid::parse_str(STAGING).unwrap());
        assert_eq!(u.status, "staged");
        assert_eq!(u.remote_ref.as_deref(), Some("petri-mumax3-worker"));
        assert_eq!(u.staged_at, Some(ts(500)));
        assert_eq!(u.last_error, None);
        assert_eq!(u.last_sequence, 1);
    }

    #[test]
    fn cluster_failure_is_failed_data_not_an_error() {
        let ev = effect_completed(
            2,
            600,
            serde_json::json!({
                "staging_id": STAGING, "status": "failed",
                "error": "nomad PUT /v1/job returned 500", "slug": "x",
            }),
        );
        let u = project_staging(&[ev], &net()).expect("update");
        assert_eq!(u.status, "failed");
        assert_eq!(
            u.last_error.as_deref(),
            Some("nomad PUT /v1/job returned 500")
        );
        assert_eq!(u.staged_at, None);
    }

    #[test]
    fn fatal_effect_failed_marks_failed_from_net_id() {
        let ev = PersistedEvent {
            sequence: 1,
            timestamp: ts(100),
            event: DomainEvent::EffectFailed {
                transition_id: TransitionId("t_stage".into()),
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "stage_template".to_string(),
                error_message: "missing request.slug".to_string(),
                tokens_consumed: true,
                input_data: None,
                retryable: false,
            },
            hash: String::new(),
            previous_hash: None,
        };
        let u = project_staging(&[ev], &net()).expect("update");
        assert_eq!(u.staging_id, Uuid::parse_str(STAGING).unwrap());
        assert_eq!(u.status, "failed");
        assert_eq!(u.last_error.as_deref(), Some("missing request.slug"));
    }

    #[test]
    fn missing_staging_id_falls_back_to_net_id() {
        let ev = effect_completed(1, 100, serde_json::json!({ "status": "staged" }));
        let u = project_staging(&[ev], &net()).expect("update");
        assert_eq!(u.staging_id, Uuid::parse_str(STAGING).unwrap());
    }

    #[test]
    fn no_stage_event_yields_none() {
        assert!(project_staging(&[], &net()).is_none());
    }
}
