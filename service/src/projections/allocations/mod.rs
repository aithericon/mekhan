//! Unified projection of resource allocations into the `allocations` table.
//!
//! Two allocation KINDS share one table (one row per `(net_id, grant_id,
//! kind)`):
//!
//! - `datacenter_lease` (PRIMARY) — a Slurm/Nomad/HTTP cluster lease. Driven by
//!   the engine's `resource_lease_acquire` / `resource_lease_release`
//!   [`EffectCompleted`] events (which fire on the synthetic pool-adapter net
//!   `pool-<resource_id>`), enriched by the terminal accounting signal the
//!   per-cluster watcher publishes (`signal_key == grant_id`).
//! - `token_pool_grant` (BEST-EFFORT) — an admission grant against our own
//!   worker-capacity pool. Driven by the pool net's `t_grant` / `t_release`
//!   [`TransitionFired`] events.
//!
//! Mirrors the `step_executions` projection exactly:
//! - [`projector`]: pure `(events, net_id) → Vec<AllocationRow>` fold. Reused by
//!   tests (offline replay) and by the consumer (online ingest).
//! - [`consumer`]: NATS-driven background task that subscribes to
//!   `petri.events.>`, re-folds the per-net event buffer on each arrival, and
//!   upserts changed rows (sequence-guarded).
//!
//! Unlike `step_executions`, the projector needs no compiler `InterfaceRegistry`
//! — every correlation key (grant_id, cluster_resource_id, node_id,
//! requested_tres) is carried in the engine events themselves (effect_result,
//! consumed input token, and the `pool-<resource_id>` net id).

pub mod consumer;
pub mod projector;

pub use consumer::start_allocations_ingest;
pub use projector::{project_allocations, AllocationKind, AllocationRow, AllocationStatus};
