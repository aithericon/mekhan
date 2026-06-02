//! Stable identifiers for long-lived infrastructure nets the compiler bridges
//! to (deployed once by ops, not spawned per-instance).

/// Deterministic backing-net id for a registry-resolved pool resource. A pooled
/// AutomatedStep (`Executor { pool: { alias } }`) whose alias resolves to a
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

/// Deterministic net id for a one-shot **staging run** (B-staging, Phase 4). A
/// staging run pushes one job-template *version* onto one *datacenter* cluster;
/// mekhan generates a short-lived Petri net (`build_staging_net`) that fires the
/// `stage_template` engine effect once and completes. Keyed by the
/// `template_stagings` row id (`staging_id`) so each (template_version ×
/// datacenter) staging attempt is its own instance you can drill into, and so
/// the `stage_template` effect_result's echoed `staging_id` correlates straight
/// back to the row the `template_stagings` projection updates. Pure function of
/// the staging row id ⇒ replay-safe + unique per attempt (re-staging the same
/// combo upserts the row → reuses its id → re-deploys the same net id, which the
/// engine replaces).
pub fn staging_net_id(staging_id: uuid::Uuid) -> String {
    format!("staging-{staging_id}")
}

/// Net id for a one-shot image-materialization run (docs/22 container staging).
/// mekhan generates a short-lived net (`build_materialize_image_net`) that fires
/// the `materialize_image` engine effect once and completes. Keyed by the
/// `image_materializations` row id so each (container_image × datacenter) pull is
/// its own drill-in-able instance, and so the effect_result's echoed
/// `materialize_id` correlates back to the row the `image_materializations`
/// projection updates. Pure function of the row id ⇒ replay-safe + unique.
pub fn materialize_net_id(materialize_id: uuid::Uuid) -> String {
    format!("materialize-{materialize_id}")
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
