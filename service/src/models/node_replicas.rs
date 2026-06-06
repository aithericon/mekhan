//! Model-pool reconciliation (docs/31 Phase 1) — the node-fleet reconciliation
//! row + the PURE node-count decision math.
//!
//! Loop 1 (`crate::autoscaler::node_actuate`, Phase 2) reconciles ONE
//! [`NodeReplicaRow`] per `node_pool` capacity resource: each tick it reads the
//! pool config, computes a desired NODE count ([`compute_node_target`]) — gated by
//! the same durable cooldown ([`crate::models::model_replicas::in_cooldown`])
//! anchored on `last_actuated_at` — observes the live C-weighted capacity from
//! FleetLiveness (`Σ present-node C`, DERIVED-B; NOT the staging effect result),
//! actuates via a generated `node-pool-<id>-<gen>` one-shot net, and upserts the
//! row.
//!
//! This is a near-verbatim clone of [`crate::models::model_replicas`]: the row
//! shape mirrors `ModelReplicaRow` (pool instead of policy, nodes instead of
//! replicas, plus the C-weighted `observed_slots`), and the decision fn is
//! `compute_target` re-targeted onto `NodePoolPolicy`'s `[min_nodes, max_nodes]`.
//! `in_cooldown` is REUSED as-is (re-exported from `model_replicas`).
//!
//! The decision functions here are pure + table-driven-testable: no DB, no clock
//! beyond the `now` passed in.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use aithericon_resources::types::NodePoolPolicy;

// `in_cooldown` is identical for both reconciliation rows (it gates purely on
// `last_actuated_at` + `cooldown_secs`), so node pools reuse it verbatim rather
// than duplicating the window math.
pub use crate::models::model_replicas::in_cooldown;

/// Terminal + transient states of a node-pool row's reconciliation. Stored as TEXT
/// (DB CHECK enforces the set — see `20240151000000_node_replicas.sql`). Mirrors
/// [`crate::models::model_replicas::status`].
pub mod status {
    pub const PROVISIONING: &str = "provisioning";
    pub const ACTIVE: &str = "active";
    pub const SCALING: &str = "scaling";
    pub const DRAINING: &str = "draining";
    pub const STOPPED: &str = "stopped";
    pub const FAILED: &str = "failed";
}

/// One `node_replicas` row — Loop 1's durable reconciliation target + Control-Plane
/// read. `desired_nodes`/`observed_nodes`/`observed_slots` are stored `INT`; the
/// loop works in `u32` and converts at the edges.
///
/// `observed_nodes` is the live head-count of present pool nodes; `observed_slots`
/// is the C-weighted aggregate (`Σ present-node C`) from FleetLiveness — the
/// capacity Loop 1 scales against (DERIVED-B). Both are roster-driven; the outcome
/// projector NEVER writes them.
#[derive(Clone, Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct NodeReplicaRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    /// The `node_pool` resource this row reconciles (UNIQUE — one row/pool).
    pub pool_resource_id: Uuid,
    /// Resolved `datacenter` resource UUID (the pool carries an alias; the loop
    /// resolves it before the upsert).
    pub datacenter_resource_id: Uuid,
    /// Native job NAME registered on the cluster (Nomad service-job id for the
    /// generic engine fleet). `None` until first actuation.
    pub node_slug: Option<String>,
    /// Last desired NODE count the loop drove.
    pub desired_nodes: i32,
    /// Live count of present pool nodes (head-count from FleetLiveness).
    pub observed_nodes: i32,
    /// Live C-weighted capacity (`Σ present-node C`) from FleetLiveness — the
    /// aggregate Loop 1 scales against (DERIVED-B).
    pub observed_slots: i32,
    /// One of `status::*`.
    pub status: String,
    /// HARD residency zone recorded for the Control-Plane read + audit (the SINGLE
    /// zone source — DERIVED-A).
    pub residency_zone: Option<String>,
    pub last_error: Option<String>,
    /// Anchors the durable cooldown gate (survives a mekhan restart).
    pub last_actuated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The PURE desired-NODE-count decision, clamped to `[min_nodes, max_nodes]`.
///
/// Clone of [`crate::models::model_replicas::compute_target`] re-targeted onto the
/// `node_pool`'s node bounds. `demand` here is the aggregate model demand routed to
/// the pool, already converted to C-units / `C` by the caller (Loop 1 computes
/// `ceil(Σ demand / max_num_seqs)` before passing it in), so this fn only applies
/// the bound clamp + the `manual_override` precedence:
///
/// - `manual_override` (if present) is the operator's pinned node count → clamped.
/// - else `demand` (the C-unit-derived desired node count) → clamped.
/// - `None`/`None` ⇒ no decision yet (the loop no-ops this tick).
///
/// Always clamps to `[min_nodes, max_nodes]` so the `ceil(demand/C)` arithmetic
/// can never exceed the pool's declared capacity envelope.
pub fn compute_node_target(
    pool: &NodePoolPolicy,
    demand: Option<f64>,
    manual_override: Option<u32>,
) -> Option<u32> {
    let raw = match manual_override {
        Some(m) => m,
        None => match demand {
            Some(d) => d.ceil().max(0.0) as u32,
            None => return None,
        },
    };
    Some(raw.clamp(pool.min_nodes, pool.max_nodes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).single().expect("valid ts")
    }

    fn pool(min: u32, max: u32) -> NodePoolPolicy {
        NodePoolPolicy {
            datacenter_resource_id: "dev-nomad".to_string(),
            residency_zone: "eu-west".to_string(),
            gpu_class: "a100-80gb".to_string(),
            max_num_seqs: 8,
            engine_spec: json!({ "image": "vllm/vllm-openai:latest", "gpus": 1 }),
            min_nodes: min,
            max_nodes: max,
            cooldown_secs: None,
        }
    }

    #[test]
    fn manual_override_wins_and_clamps() {
        let p = pool(1, 3);
        // Override pins the node count, clamped to the pool envelope.
        assert_eq!(compute_node_target(&p, Some(99.0), Some(2)), Some(2));
        assert_eq!(compute_node_target(&p, None, Some(5)), Some(3)); // clamp high
        assert_eq!(compute_node_target(&p, None, Some(0)), Some(1)); // clamp low
    }

    #[test]
    fn demand_derives_ceil_node_count_clamped() {
        let p = pool(0, 4);
        assert_eq!(compute_node_target(&p, Some(2.1), None), Some(3)); // ceil(2.1)=3
        assert_eq!(compute_node_target(&p, Some(0.0), None), Some(0)); // no demand → 0
        assert_eq!(compute_node_target(&p, Some(100.0), None), Some(4)); // clamp high
    }

    #[test]
    fn no_demand_no_override_is_no_decision() {
        let p = pool(0, 4);
        assert_eq!(compute_node_target(&p, None, None), None);
    }

    #[test]
    fn cooldown_reused_from_model_replicas() {
        let now = ts(1_000);
        // No prior actuation → never gated.
        assert!(!in_cooldown(None, Some(60), now));
        // Within the window → gated.
        assert!(in_cooldown(Some(ts(990)), Some(60), now)); // 990+60=1050 > 1000
                                                            // Past the window → free.
        assert!(!in_cooldown(Some(ts(900)), Some(60), now)); // 900+60=960 < 1000
    }
}
