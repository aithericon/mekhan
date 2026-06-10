//! Presence — the allocation plane's liveness adapters (docs/35 §1).
//!
//! Two adapters over ONE shared substrate. The allocation plane answers
//! *"who may work, on what, right now, and why"* — and for presence-backed
//! pools (`liveness = presence`) the answer is driven by a liveness SOURCE
//! that mekhan adapts into pool-net admission/reap:
//!
//! - [`runners`] — enrolled instruments/runner daemons. Liveness is the
//!   data-plane heartbeat on `runner.{id}.presence`; admission is automatic
//!   (`acceptance = auto`), trusted facets come from the `runners` DB row.
//! - [`humans`] — roster members (docs/33/34). A person has no daemon, so
//!   liveness splits into INTENT (`human.{member}.availability`, the durable
//!   toggle) and LIVENESS (`human.{member}.presence`, the session heartbeat);
//!   pools are `acceptance = consent` and trusted facets come from
//!   `roster_members`.
//!
//! Both adapters speak the SAME engine dialect — `presence_acquire` units over
//! the cross-net bridge, bare `presence_expired` signals, the claim bridge for
//! consent pools — and share the same mechanics: an in-memory entry map with an
//! absent→present acquire edge, a TTL sweep, grow-eager/shrink-lazy slot
//! deltas, and JetStream publication. That shared substrate lives in [`core`];
//! everything subject-grammar-, trust-source-, or policy-specific stays in the
//! per-kind module.

pub mod core;
pub mod humans;
pub mod runners;

pub use humans::{spawn_human_presence_controller, HumanPresence, HumanPresenceSnapshot};
pub use runners::{spawn_presence_controller, RunnerPresence};

pub(crate) use core::inject_claim;
