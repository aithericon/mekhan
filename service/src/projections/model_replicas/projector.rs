//! Pure fold from a model-replica actuation net's event log into a
//! [`ReplicaUpdate`] (model-pool P4, docs/29 §6').
//!
//! A `model-replica-<id>` net (built by
//! [`crate::autoscaler::actuate::build_model_replica_net`]) fires the engine's
//! `stage_template` inline effect exactly once. We fold its terminal event into a
//! single update keyed by the `model_replicas` row id:
//!
//! - `EffectCompleted { stage_template, effect_result }` with `status == "staged"`
//!   (the NORMAL success): record the cluster `remote_ref` + CLEAR `last_error`.
//!   Status is left untouched — the AUTOSCALER owns `provisioning → active`
//!   (driven by the fleet roster's observed count, not "registered").
//! - `EffectCompleted` with `status == "failed"` (a non-fatal cluster error
//!   reported as DATA): mark the row `failed` + record the error.
//! - `EffectFailed { stage_template, error_message }` (a fatal config/parse error):
//!   mark `failed`. The id isn't journaled — recover it from the net id.
//!
//! Pure: identical `(events, net_id)` → identical output. Reused by the consumer
//! (online) and tests (offline replay).
//!
//! NOTE: this projection NEVER sets `observed_count` — that is roster-derived in
//! the autoscaler loop (a `stage_template` success proves "registered", not
//! "serving"). The projector only carries the registration outcome (slug/error).

use uuid::Uuid;

use petri_domain::{DomainEvent, PersistedEvent};

/// One projected replica-actuation outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaUpdate {
    pub replica_id: Uuid,
    /// `Some("failed")` only on failure; `None` on success (don't clobber the
    /// autoscaler's `active`/`provisioning`).
    pub status: Option<String>,
    /// Cluster-side reference on success (recorded as the replica slug if unset).
    pub remote_ref: Option<String>,
    /// Cleared (`None`) on success; the error on failure.
    pub last_error: Option<String>,
    /// Engine event sequence of the folded terminal event.
    pub last_sequence: u64,
}

/// Recover the `model_replicas` row id from a `model-replica-<uuid>` net id.
fn replica_id_from_net(net_id: &str) -> Option<Uuid> {
    let rest = net_id.strip_prefix("model-replica-")?;
    // Bare `<uuid>` (legacy) parses directly; the generation-discriminated
    // `<uuid>-<generation>` form has a trailing all-digits generation to strip
    // from the last `-`.
    Uuid::parse_str(rest).ok().or_else(|| {
        rest.rsplit_once('-').and_then(|(head, gen)| {
            (!gen.is_empty() && gen.bytes().all(|b| b.is_ascii_digit()))
                .then(|| Uuid::parse_str(head).ok())
                .flatten()
        })
    })
}

/// Project a replica-actuation net's event stream into at most one
/// [`ReplicaUpdate`]. `None` until a terminal `stage_template` event is present.
pub fn project_replica(events: &[PersistedEvent], net_id: &str) -> Option<ReplicaUpdate> {
    let mut out: Option<ReplicaUpdate> = None;
    for ev in events {
        match &ev.event {
            DomainEvent::EffectCompleted {
                effect_handler_id,
                effect_result,
                ..
            } if effect_handler_id == "stage_template" => {
                let replica_id = effect_result
                    .get("staging_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .or_else(|| replica_id_from_net(net_id));
                let Some(replica_id) = replica_id else {
                    continue;
                };
                let cluster_status = effect_result
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("staged");
                if cluster_status == "failed" {
                    out = Some(ReplicaUpdate {
                        replica_id,
                        status: Some("failed".to_string()),
                        remote_ref: None,
                        last_error: effect_result
                            .get("error")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        last_sequence: ev.sequence,
                    });
                } else {
                    out = Some(ReplicaUpdate {
                        replica_id,
                        status: None, // autoscaler owns provisioning→active
                        remote_ref: effect_result
                            .get("remote_ref")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        last_error: None,
                        last_sequence: ev.sequence,
                    });
                }
            }
            DomainEvent::EffectFailed {
                effect_handler_id,
                error_message,
                ..
            } if effect_handler_id == "stage_template" => {
                let Some(replica_id) = replica_id_from_net(net_id) else {
                    continue;
                };
                out = Some(ReplicaUpdate {
                    replica_id,
                    status: Some("failed".to_string()),
                    remote_ref: None,
                    last_error: Some(error_message.clone()),
                    last_sequence: ev.sequence,
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
    use chrono::{DateTime, TimeZone, Utc};
    use petri_domain::TransitionId;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).single().expect("valid ts")
    }

    const REPLICA: &str = "55555555-5555-5555-5555-555555555555";
    // Generation-discriminated net id (the real shape the autoscaler deploys).
    fn net() -> String {
        format!("model-replica-{REPLICA}-1717000015000")
    }

    #[test]
    fn replica_id_recovered_from_generation_and_legacy_net_ids() {
        let want = Uuid::parse_str(REPLICA).unwrap();
        // Generation-discriminated form.
        assert_eq!(replica_id_from_net(&net()), Some(want));
        // Legacy bare-uuid form still parses.
        assert_eq!(
            replica_id_from_net(&format!("model-replica-{REPLICA}")),
            Some(want)
        );
        // Not a replica net.
        assert_eq!(replica_id_from_net("staging-abc"), None);
    }

    fn effect_completed(seq: u64, result: serde_json::Value) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: ts(500),
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
    fn success_records_remote_ref_and_leaves_status() {
        let ev = effect_completed(
            1,
            serde_json::json!({ "staging_id": REPLICA, "status": "staged", "remote_ref": "model-qwen-1" }),
        );
        let u = project_replica(&[ev], &net()).expect("update");
        assert_eq!(u.replica_id, Uuid::parse_str(REPLICA).unwrap());
        assert_eq!(u.status, None); // autoscaler owns active
        assert_eq!(u.remote_ref.as_deref(), Some("model-qwen-1"));
        assert_eq!(u.last_error, None);
    }

    #[test]
    fn cluster_failure_marks_failed() {
        let ev = effect_completed(
            2,
            serde_json::json!({ "staging_id": REPLICA, "status": "failed", "error": "nomad 500" }),
        );
        let u = project_replica(&[ev], &net()).expect("update");
        assert_eq!(u.status.as_deref(), Some("failed"));
        assert_eq!(u.last_error.as_deref(), Some("nomad 500"));
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
        let u = project_replica(&[ev], &net()).expect("update");
        assert_eq!(u.replica_id, Uuid::parse_str(REPLICA).unwrap());
        assert_eq!(u.status.as_deref(), Some("failed"));
    }

    #[test]
    fn no_terminal_event_yields_none() {
        assert!(project_replica(&[], &net()).is_none());
    }
}
