//! Stable identifiers for long-lived infrastructure nets the compiler bridges
//! to (deployed once by ops, not spawned per-instance).

/// The long-lived scheduler net a `Scheduled` AutomatedStep submits jobs to.
/// Its `job_queue` `bridge_in` accepts `SchedulerSubmitInput` and replies on
/// the named `result` / `failure` channels. Mirrors the BO demo's
/// `scheduler-net` (engine/sdk/examples/scheduler_net.rs).
pub const SCHEDULER_NET_ID: &str = "mekhan-scheduler";

/// The scheduler net's inbound queue place.
pub const SCHEDULER_JOB_QUEUE: &str = "job_queue";
