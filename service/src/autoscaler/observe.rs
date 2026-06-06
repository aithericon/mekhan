//! C-weighted observed-capacity source for the node-fleet scaler (docs/31 Phase 2,
//! Loop 1, DERIVED-B).
//!
//! `serving_runner_counts` answers "which MODELS are live" as a per-runner
//! head-count and CANNOT C-weight (its `RunnerPresenceSnapshot` source has no
//! `concurrency` field). [`FleetLiveness::snapshot`] is the ONLY place already
//! carrying each runner's per-engine concurrency budget `C`. So Loop 1's observed
//! capacity sums `C` there — and the two signals are NEVER merged: one answers
//! "which models are live" (the picker/AND-gate), the other "how much engine
//! capacity exists" (this).
//!
//! Pool membership is not on the [`FleetSnapshotEntry`]; it is joined on the runner
//! UUID against [`RunnerPresence::pool_membership`] (the `pool_alias` =
//! `resources.path` of the runner's `runner_group`, which for a node-pool node is
//! the pool's own alias).

use std::collections::HashMap;

use uuid::Uuid;

use crate::fleet::{CapacityKind, FleetLiveness};
use crate::runners_presence::RunnerPresence;

/// The C-weighted observed capacity of one node pool, identified by its alias
/// (`resources.path`): `Σ entry.concurrency` over present RUNNER nodes tagged to
/// the pool (DERIVED-B). Returns both the head-count of present pool nodes and the
/// summed slot budget so Loop 1 can record `observed_nodes` AND `observed_slots`.
///
/// Joins the two registries on the runner UUID: FleetLiveness carries `C`, the
/// presence map carries the pool alias. A runner present in only one of the two
/// (heartbeat-mirror skew for one tick) is simply not counted — fail-soft, never a
/// hard error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolObserved {
    /// Head-count of present runner nodes tagged to the pool.
    pub nodes: u32,
    /// `Σ C` over those nodes — the engine slot capacity Loop 1 scales against.
    pub slots: u32,
}

/// Observe one pool's live capacity. `pool_alias` is the `node_pool` resource's
/// `resources.path` (the same string a node's `runner_group` carries).
pub async fn pool_serving_capacity(
    fleet: &FleetLiveness,
    runner_presence: &RunnerPresence,
    pool_alias: &str,
) -> PoolObserved {
    // runner UUID → its pool alias (present runners only).
    let membership: HashMap<Uuid, String> = runner_presence.pool_membership().await;

    let mut observed = PoolObserved::default();
    for entry in fleet.snapshot().await {
        // Only enrolled instrument runners carry a per-engine `C`; workers are
        // competing-consumer executors normalised to C=1 and never node-pool nodes.
        if !matches!(entry.kind, CapacityKind::Runner) {
            continue;
        }
        // FleetSnapshotEntry.id is the runner UUID as a string.
        let Ok(runner_id) = Uuid::parse_str(&entry.id) else {
            continue;
        };
        if membership.get(&runner_id).map(String::as_str) == Some(pool_alias) {
            observed.nodes += 1;
            observed.slots = observed.slots.saturating_add(entry.concurrency);
        }
    }
    observed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sums_c_over_pool_tagged_runners_only() {
        let fleet = FleetLiveness::new();
        let presence = RunnerPresence::new();

        // Two runners in pool "gpu-eu" (C=8 each), one in "gpu-us" (C=4), one worker.
        let r1 = Uuid::new_v4();
        let r2 = Uuid::new_v4();
        let r3 = Uuid::new_v4();
        fleet
            .upsert_runner(r1.to_string(), vec!["python".into()], 8)
            .await;
        fleet
            .upsert_runner(r2.to_string(), vec!["python".into()], 8)
            .await;
        fleet
            .upsert_runner(r3.to_string(), vec!["python".into()], 4)
            .await;

        // Mirror the presence pool membership for the same runners.
        presence.test_set_membership(r1, "gpu-eu", true).await;
        presence.test_set_membership(r2, "gpu-eu", true).await;
        presence.test_set_membership(r3, "gpu-us", true).await;

        let eu = pool_serving_capacity(&fleet, &presence, "gpu-eu").await;
        assert_eq!(eu, PoolObserved { nodes: 2, slots: 16 });

        let us = pool_serving_capacity(&fleet, &presence, "gpu-us").await;
        assert_eq!(us, PoolObserved { nodes: 1, slots: 4 });

        // An unknown pool observes nothing (fail-soft to empty).
        let none = pool_serving_capacity(&fleet, &presence, "gpu-ap").await;
        assert_eq!(none, PoolObserved::default());
    }

    #[tokio::test]
    async fn absent_runner_is_not_counted() {
        let fleet = FleetLiveness::new();
        let presence = RunnerPresence::new();

        let r1 = Uuid::new_v4();
        fleet
            .upsert_runner(r1.to_string(), vec!["python".into()], 8)
            .await;
        // In the pool but NOT present → omitted from membership.
        presence.test_set_membership(r1, "gpu-eu", false).await;

        let eu = pool_serving_capacity(&fleet, &presence, "gpu-eu").await;
        assert_eq!(eu, PoolObserved::default());
    }
}
