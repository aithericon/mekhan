//! Fleet liveness — the unified telemetry plane (docs/23 §2, §4; docs/24 S1).
//!
//! Every capacity that executes work — today the anonymous competing-consumer
//! worker pool and the enrolled instrument runners — emits a presence
//! heartbeat. This module folds the two previously-separate advisory trackers
//! (`worker_coverage`'s `BackendCoverage` and the advisory facet of
//! `presence::runners`) into ONE [`FleetLiveness`] registry: a single
//! TTL-swept snapshot and a single `satisfies`-shaped eligibility query
//! (`serves_backend`) over both kinds.
//!
//! ## What this is (telemetry) and is NOT (control)
//!
//! This is the **liveness/telemetry** facet only — purely advisory, with NO
//! side effect on any instance (docs/24 refinement #2). A dropped worker — or
//! a dropped runner — vanishing from this registry NEVER reaps or fails an
//! instance. The runner *capacity-binding* (the inject/expire pool-net edges in
//! [`crate::presence::runners`]) is a SEPARATE control plane that stays
//! runner-only and is untouched: a held runner's death still reaps its held
//! unit there; a worker's death is a redeliverable JetStream hiccup and must
//! stay one. Workers feed this registry and nothing else.
//!
//! [`crate::presence::runners`] owns the runner control binding and MIRRORS each
//! runner's advisory facet (its self-reported backends) into here on every
//! heartbeat; `worker_coverage`'s machinery is absorbed wholesale.

pub mod liveness;

pub use liveness::{spawn_worker_liveness, CapacityKind, FleetLiveness, FleetSnapshotEntry};
