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

/// Deterministic backing-net id for a registry-resolved pool resource. A pooled
/// AutomatedStep (`Inline { pool: { alias } }`) whose alias resolves to a
/// `token_pool` resource `<resource_id>` bridges its claim/register/release
/// handshake to this id. R3 (tokens backend) deploys a net with exactly this id
/// via `build_token_pool_net`; the resource *kind* decides what that net IS, but
/// the id scheme is shared so the compiler stays backend-agnostic. Pure function
/// of the resource id ⇒ replay-safe + diff-stable in the AIR.
///
/// The prototype's single well-known global (`resource-pool-net`) is gone — the
/// consolidation pivot requires every pool to be a named `token_pool` resource.
pub fn pool_net_id(resource_id: uuid::Uuid) -> String {
    format!("pool-{resource_id}")
}

/// The pool net's claim queue (`bridge_in::<ClaimRequest>("claim_inbox", …)`).
/// A `ClaimRequest { grant_id }` deposited here is matched against a free
/// capacity token by `t_grant`, which replies a `Grant { grant_id, gpu_id }`
/// on the `"grant"` channel — or queues (backpressure) when the pool is empty.
pub const POOL_CLAIM_INBOX: &str = "claim_inbox";

/// The pool net's hold-registration queue
/// (`bridge_in::<HoldReg>("register_inbox", …)`). Once granted, the holder
/// echoes its `HoldReg { grant_id, gpu_id }` here over a PLAIN bridge so the
/// pool records an observable `in_use` hold (and can reap it on crash) WITHOUT
/// the reply-routing taint — see the "Keep capacity tokens clean" rule in
/// `docs/14` and the split-grant/register rationale in the SDK example.
pub const POOL_REGISTER_INBOX: &str = "register_inbox";

/// The pool net's release queue
/// (`bridge_in::<ReleaseRequest>("release_inbox", …)`). On EVERY body exit
/// (success or error) the holder bridges a `ReleaseRequest { grant_id }` here;
/// `t_release` correlates it to the `in_use` hold by `grant_id` and returns a
/// clean capacity token to the pool. A forgotten release strands capacity and
/// deadlocks the pool under contention — the compiler enforces "every exit
/// arcs to release_out" structurally (`lower_automated_step_pooled`).
pub const POOL_RELEASE_INBOX: &str = "release_inbox";
