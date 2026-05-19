//! Stable identifiers for long-lived infrastructure nets the compiler bridges
//! to (deployed once by ops, not spawned per-instance).

/// The long-lived scheduler net a `Scheduled` AutomatedStep submits jobs to.
/// Its `job_inbox` `bridge_in` accepts `SchedulerSubmitInput` and replies on
/// the named `result` / `failure` channels. Must match the canonical
/// `scheduler-net` contract (engine/sdk/examples/scheduler_net.rs,
/// engine/sdk/examples/common/scheduler_bridge.rs::connect_to_scheduler),
/// which is what ops deploys (`--example scheduler_net --net-id scheduler-net`).
pub const SCHEDULER_NET_ID: &str = "scheduler-net";

/// The scheduler net's inbound queue place (`scheduler_bridge.rs` bridges to
/// `job_inbox`, the `bridge_in::<SchedulerSubmitInput>` place).
pub const SCHEDULER_JOB_QUEUE: &str = "job_inbox";
