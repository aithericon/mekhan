//! Pure fold from a node-pool actuation net's event log into a [`NodeReplicaUpdate`]
//! (model-pool docs/31 Phase 2, Loop 1).
//!
//! A near-verbatim clone of [`crate::projections::model_replicas::projector`]: a
//! `node-pool-<id>-<gen>` net (built by
//! [`crate::autoscaler::node_actuate::build_node_pool_net`]) fires the engine's
//! `stage_template` inline effect exactly once. We fold its terminal event into a
//! single update keyed by the `node_replicas` row id:
//!
//! - `EffectCompleted { stage_template, effect_result }` with `status == "staged"`
//!   (the NORMAL success): record the cluster `node_slug` + CLEAR `last_error`.
//!   Status is left untouched — Loop 1 owns `provisioning → active` (driven by the
//!   FleetLiveness C-aggregate, not "registered").
//! - `EffectCompleted` with `status == "failed"` (a non-fatal cluster error as
//!   DATA): mark the row `failed` + record the error.
//! - `EffectFailed { stage_template, error_message }` (a fatal config/parse error):
//!   mark `failed`. The id isn't journaled — recover it from the net id.
//!
//! Pure: identical `(events, net_id)` → identical output.
//!
//! NOTE: this projection NEVER sets `observed_nodes` / `observed_slots` — those are
//! FleetLiveness-driven in Loop 1 (DERIVED-B; a `stage_template` success proves
//! "registered", not "serving"). The projector only carries the registration
//! outcome (slug/error).

use uuid::Uuid;

use petri_domain::{DomainEvent, PersistedEvent};

/// One projected node-pool-actuation outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeReplicaUpdate {
    pub pool_resource_id: Uuid,
    /// `Some("failed")` only on failure; `None` on success (don't clobber Loop 1's
    /// `active`/`provisioning`).
    pub status: Option<String>,
    /// Cluster-side reference on success (recorded as the node slug if unset).
    pub node_slug: Option<String>,
    /// Cleared (`None`) on success; the error on failure.
    pub last_error: Option<String>,
    /// Engine event sequence of the folded terminal event.
    pub last_sequence: u64,
}

/// Recover the `node_replicas` row id from a `node-pool-<uuid>-<gen>` net id.
fn pool_id_from_net(net_id: &str) -> Option<Uuid> {
    let rest = net_id.strip_prefix("node-pool-")?;
    // Bare `<uuid>` parses directly; the generation-discriminated
    // `<uuid>-<generation>` form has a trailing all-digits generation to strip from
    // the last `-`.
    Uuid::parse_str(rest).ok().or_else(|| {
        rest.rsplit_once('-').and_then(|(head, gen)| {
            (!gen.is_empty() && gen.bytes().all(|b| b.is_ascii_digit()))
                .then(|| Uuid::parse_str(head).ok())
                .flatten()
        })
    })
}

/// Project a node-pool-actuation net's event stream into at most one
/// [`NodeReplicaUpdate`]. `None` until a terminal `stage_template` event is present.
pub fn project_node_pool(events: &[PersistedEvent], net_id: &str) -> Option<NodeReplicaUpdate> {
    let mut out: Option<NodeReplicaUpdate> = None;
    for ev in events {
        match &ev.event {
            DomainEvent::EffectCompleted {
                effect_handler_id,
                effect_result,
                ..
            } if effect_handler_id == "stage_template" => {
                let pool_id = effect_result
                    .get("staging_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .or_else(|| pool_id_from_net(net_id));
                let Some(pool_id) = pool_id else {
                    continue;
                };
                let cluster_status = effect_result
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("staged");
                if cluster_status == "failed" {
                    out = Some(NodeReplicaUpdate {
                        pool_resource_id: pool_id,
                        status: Some("failed".to_string()),
                        node_slug: None,
                        last_error: effect_result
                            .get("error")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        last_sequence: ev.sequence,
                    });
                } else {
                    out = Some(NodeReplicaUpdate {
                        pool_resource_id: pool_id,
                        status: None, // Loop 1 owns provisioning→active
                        node_slug: effect_result
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
                let Some(pool_id) = pool_id_from_net(net_id) else {
                    continue;
                };
                out = Some(NodeReplicaUpdate {
                    pool_resource_id: pool_id,
                    status: Some("failed".to_string()),
                    node_slug: None,
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

    const POOL: &str = "55555555-5555-5555-5555-555555555555";
    fn net() -> String {
        format!("node-pool-{POOL}-1717000015000")
    }

    #[test]
    fn pool_id_recovered_from_generation_and_legacy_net_ids() {
        let want = Uuid::parse_str(POOL).unwrap();
        assert_eq!(pool_id_from_net(&net()), Some(want));
        assert_eq!(pool_id_from_net(&format!("node-pool-{POOL}")), Some(want));
        // Not a node-pool net.
        assert_eq!(pool_id_from_net("model-replica-abc"), None);
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
    fn success_records_node_slug_and_leaves_status() {
        let ev = effect_completed(
            1,
            serde_json::json!({ "staging_id": POOL, "status": "staged", "remote_ref": "node-pool-aabbccdd" }),
        );
        let u = project_node_pool(&[ev], &net()).expect("update");
        assert_eq!(u.pool_resource_id, Uuid::parse_str(POOL).unwrap());
        assert_eq!(u.status, None); // Loop 1 owns active
        assert_eq!(u.node_slug.as_deref(), Some("node-pool-aabbccdd"));
        assert_eq!(u.last_error, None);
    }

    #[test]
    fn cluster_failure_marks_failed() {
        let ev = effect_completed(
            2,
            serde_json::json!({ "staging_id": POOL, "status": "failed", "error": "nomad 500" }),
        );
        let u = project_node_pool(&[ev], &net()).expect("update");
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
        let u = project_node_pool(&[ev], &net()).expect("update");
        assert_eq!(u.pool_resource_id, Uuid::parse_str(POOL).unwrap());
        assert_eq!(u.status.as_deref(), Some("failed"));
    }

    #[test]
    fn no_terminal_event_yields_none() {
        assert!(project_node_pool(&[], &net()).is_none());
    }
}
