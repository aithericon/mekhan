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

// ── presence_pool (Phase 3) ──────────────────────────────────────────────────
//
// A `presence_pool` resource is a capacity-LESS pool: its capacity is driven by
// runner *presence*, not a seeded count. Its backing net reuses the SAME net id
// scheme (`pool_net_id` = `pool-<resource_id>`) and the SAME claim/register/
// release inboxes + `"grant"` reply channel as the token pool — so the R2
// instance claim handshake is byte-for-byte identical regardless of which pool
// KIND the alias resolved to. What differs is admission: instead of seeded
// capacity tokens, mekhan's presence controller INJECTS one pool unit per live
// runner (via `presence_acquire`) and DROPS it when the runner's presence lease
// expires (via the `presence_expired` signal). See `petri/presence_pool_net.rs`.

/// The presence pool's runner-admit inbox
/// (`bridge_in::<PresenceAcquire>("presence_acquire", …)`). mekhan's presence
/// controller deposits a `{ runner_id, executor_namespace, caps }` token here
/// (cross-net bridge subject `petri.bridge.pool-<rid>.presence_acquire`) when a
/// runner first checks in; `t_presence_acquire` turns it into ONE free pool unit
/// (`unit_id == runner_id`). One unit per runner — re-acquire of a still-present
/// runner is idempotent at the controller (it tracks which runners it admitted).
pub const POOL_PRESENCE_ACQUIRE_INBOX: &str = "presence_acquire";

/// The presence pool's runner-expiry SIGNAL place
/// (`signal::<PresenceExpired>("presence_expired", …)`). mekhan injects a BARE
/// `{ runner_id }` token here (signal subject
/// `petri.signal.pool-<rid>.presence_expired`) when a runner's presence lease
/// lapses. Signals carry NO reply routing — they are injected routing-less. The
/// net reaps the matching unit: `t_reap_free` drops a FREE unit (capacity
/// shrinks); `t_reap_held` drops a HELD unit AND fails its holding instance over
/// the `"fail"` reply channel resolved from the hold's carried routing.
pub const POOL_PRESENCE_EXPIRED_SIGNAL: &str = "presence_expired";

/// The presence pool's lease-failed reply channel
/// (`bridge_reply_channel("fail_outbox", …, "fail")`). `t_reap_held` emits a
/// `{ runner_id, unit_id }` failure token on THIS channel when a runner whose
/// unit is currently HELD by an instance expires — routed back to the holding
/// instance's lease-failed inbox so it fails fast instead of running against a
/// now-dead runner namespace. The routing is resolved from the HELD unit's
/// carried `"fail"` channel (the instance registers its hold over a bridge whose
/// `bridge_out_reply_channels` is limited to `&[("fail", <lease-failed place>)]`
/// — never `"grant"`, preserving the reply-routing-taint rule). The "grant"
/// channel lives only on `grant_outbox`.
pub const POOL_FAIL_CHANNEL: &str = "fail";
