//! Parameterized **token-pool net** builder (R3, tokens backend).
//!
//! A `token_pool` *resource* (R1) of capacity N is realized as a long-lived
//! Petri net of N clean capacity tokens. This is the mekhan-side port of
//! `engine/sdk/examples/resource_pool_net.rs`, generalized so the net id and
//! capacity are parameters and so the grant reply matches the **typed lease**
//! R2's compiled instances expect.
//!
//! ## The contract this net implements (must line up with R1 + R2)
//!
//! A registry-resolved pooled `AutomatedStep` (compiled by R2's
//! `lower_automated_step_pooled`, alias branch) bridges to this net's
//! `well_known::pool_net_id(resource_id)` = `pool-<id>`, and:
//!
//! - **claim** → `claim_inbox` carries `ClaimRequest { grant_id, request }`
//!   (R2's `t_claim` logic: `#{ grant_id: gid, request: <claim-schema-shaped> }`).
//!   v1 grants exactly one unit per claim; the `request` field is accepted but
//!   not yet used to size the grant (weighted `units > 1` is a documented
//!   follow-up — see `t_grant`).
//! - **grant reply** ("grant" channel) → `Grant { grant_id, unit_id }`. R2
//!   declared the instance's `p_<id>_grant_inbox` place schema as
//!   `Lease__token_pool` = R1's [`TokenPoolLease`] = `{ unit_id }`, and
//!   correlates `t_acquire` on `grant_id`. So the body-visible lease is
//!   `{ unit_id }` and `grant_id` rides for correlation. **`unit_id`, not
//!   `gpu_id`** — that is the one field-name change vs. the SDK example.
//! - **register** → `register_inbox` carries `HoldReg { grant_id, unit_id }`
//!   over a PLAIN bridge (R2's `t_acquire` sets `reg: grant`, i.e. the whole
//!   `{ grant_id, unit_id }` lease).
//! - **release** → `release_inbox` carries `ReleaseRequest { grant_id }` (R2's
//!   `t_to_output` / `t_to_error`: `#{ grant_id: held.grant_id }`).
//!
//! ## Reply-routing taint avoidance (docs/14) — preserved EXACTLY
//!
//! `t_grant` consumes the routed claim, so it emits ONLY the bridge grant reply
//! (no internal hold) — otherwise the hold would carry the claim's stale
//! "grant" reply routing and wedge the pool when recycled. The holder registers
//! its hold separately over a PLAIN bridge (`t_register`), and `t_release` /
//! `t_reap` recycle that CLEAN hold. See the SDK example's module doc.

use aithericon_sdk::effects;
use aithericon_sdk::scenario::ScenarioDefinition;
use aithericon_sdk::{Context, DynamicToken};
use serde_json::json;
use uuid::Uuid;

use crate::compiler::well_known;

/// Build the AIR `ScenarioDefinition` for a `token_pool` resource's backing
/// net. Net id at deploy time is [`well_known::pool_net_id`]; the scenario
/// `name` is set to that id for log/inspection clarity.
///
/// Seeds `capacity` clean capacity tokens labelled `unit-0 .. unit-{N-1}`.
pub fn build_token_pool_net(resource_id: Uuid, capacity: u32) -> ScenarioDefinition {
    let net_id = well_known::pool_net_id(resource_id);
    let mut ctx = Context::new(net_id).description(format!(
        "Token pool for resource {resource_id} (capacity {capacity}). Claim/grant/register/\
         release/reap on the event-sourced Petri substrate; grant reply is the typed \
         Lease__token_pool {{ unit_id }} R2's compiled steps consume."
    ));

    // Shared capacity + observable hold + terminal record. All DynamicToken
    // (schemaless) — the pool net only routes; schema enforcement lives on the
    // instance side (R2 typed the grant inbox as Lease__token_pool).
    let pool: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("pool", "Capacity Pool");
    let in_use: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("in_use", "In Use");
    let done: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("done", "Freed Units");

    // Cross-net inboxes — names are the shared `well_known::POOL_*_INBOX`
    // constants the R2 instance bridges target.
    let claim_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_CLAIM_INBOX, "Claim Inbox");
    let register_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_REGISTER_INBOX, "Register Inbox");
    let release_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_RELEASE_INBOX, "Release Inbox");

    // Grant reply channel: routes the grant back to the claiming instance's
    // `p_<id>_grant_inbox` via the "grant" channel carried on the claim token.
    let grant_outbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_reply_channel("grant_outbox", "Grant Outbox", "grant");

    // Lease-expiry signal: a journaled token here (injected externally, or by a
    // durable timer in a later milestone) reaps a crashed holder. Replay-safe —
    // never a wall clock.
    let lease_expired: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.signal("lease_expired", "Lease Expired");

    // t_grant — admission. Fires only when a claim AND free capacity both
    // exist; an empty pool leaves it disabled so claims queue (backpressure).
    // Emits ONLY the grant reply. The grant is the typed lease `{ unit_id }`
    // plus `grant_id` for correlation.
    //
    // v1: one unit per claim. `claim.request` (the {units?} the R2 step carries)
    // is intentionally NOT read here — weighted/multi-unit grants are a
    // follow-up; a present `request` field is simply ignored, never a fault.
    ctx.scope("Grant", |ctx| {
        ctx.transition("t_grant", "Grant Capacity")
            .auto_input("claim", &claim_inbox)
            .auto_input("cap", &pool)
            .auto_output("grant", &grant_outbox)
            .logic(r#"#{ grant: #{ grant_id: claim.grant_id, unit_id: cap.unit_id } }"#);
    });

    // t_register — record the hold over the PLAIN register bridge, so the
    // `in_use` hold carries no reply routing and recycling stays clean.
    ctx.transition("t_register", "Register Hold")
        .auto_input("reg", &register_inbox)
        .auto_output("hold", &in_use)
        .logic(r#"#{ hold: #{ grant_id: reg.grant_id, unit_id: reg.unit_id } }"#);

    ctx.scope("Release", |ctx| {
        // t_release — body finished: return the (clean) unit, matched by grant_id.
        ctx.transition("t_release", "Release Capacity")
            .auto_input("req", &release_inbox)
            .auto_input("held", &in_use)
            .correlate("req", "held", "grant_id")
            .auto_output("cap", &pool)
            .auto_output("done", &done)
            .logic(
                r#"#{
                    cap:  #{ unit_id: held.unit_id },
                    done: #{ grant_id: held.grant_id, unit_id: held.unit_id, outcome: "released" }
                }"#,
            );

        // t_reap — holder crashed (lease expired): reclaim the unit, by grant_id.
        ctx.transition("t_reap", "Reap Expired Lease")
            .auto_input("exp", &lease_expired)
            .auto_input("held", &in_use)
            .correlate("exp", "held", "grant_id")
            .auto_output("cap", &pool)
            .auto_output("done", &done)
            .logic(
                r#"#{
                    cap:  #{ unit_id: held.unit_id },
                    done: #{ grant_id: held.grant_id, unit_id: held.unit_id, outcome: "reaped" }
                }"#,
            );
    });

    // Seed N clean capacity tokens.
    for i in 0..capacity {
        ctx.seed_one(
            &pool,
            DynamicToken(json!({ "unit_id": format!("unit-{i}") })),
        );
    }

    ctx.build()
}

/// Idempotently ensure a `token_pool` resource's backing net is deployed +
/// running on the engine.
///
/// Idempotency: probe the engine for the net's current run mode first
/// ([`PetriClient::try_get_run_mode`], which returns `None` when the engine has
/// no such net loaded — 404 / connection error). If it's already `Running`,
/// no-op. Otherwise (re)deploy the scenario and set it `Running`. Re-deploying
/// an existing net is harmless — the engine replaces the topology — and a pool
/// net carries no per-instance state to clobber (its only state is the seeded
/// capacity, re-seeded identically), so this is safe to call on every create
/// AND version bump of the resource.
///
/// **Engine-down behavior:** a failed deploy/activate is logged as a WARNING
/// and SWALLOWED — it does NOT fail the resource CRUD. Rationale: a
/// `token_pool` resource is a durable workspace record; its backing net is
/// re-derivable from `(resource_id, capacity)` at any time. Failing resource
/// creation because the engine is momentarily unreachable would be surprising
/// and would strand the user (the DB row + Vault secret already landed). The
/// belt-and-suspenders R3 follow-up — re-`ensure` at template publish when the
/// alias is referenced — covers the gap if the create-time deploy was skipped.
/// (The probe itself can't distinguish "engine down" from "net not yet
/// deployed", so a transient engine outage simply defers deployment to the
/// next create/version/publish that calls this.)
pub async fn ensure_token_pool_net_deployed(
    petri: &crate::petri::client::PetriClient,
    resource_id: Uuid,
    capacity: u32,
) {
    let net_id = well_known::pool_net_id(resource_id);

    if matches!(
        petri.try_get_run_mode(&net_id).await,
        Some(petri_api_types::RunMode::Running)
    ) {
        tracing::debug!(net_id, "token-pool net already deployed + running; no-op");
        return;
    }

    let air = match serde_json::to_value(build_token_pool_net(resource_id, capacity)) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(net_id, %e, "failed to serialize token-pool net AIR");
            return;
        }
    };

    if let Err(e) = crate::petri::instance::deploy_instance(
        petri,
        &net_id,
        &air,
        petri_api_types::DispatchOptions::default(),
        None,
    )
    .await
    {
        tracing::warn!(
            net_id,
            capacity,
            %e,
            "failed to deploy token-pool net to the engine — resource CRUD still \
             succeeded; the net will be (re)deployed on the next resource version \
             or at template publish when the alias is referenced"
        );
        return;
    }
    tracing::info!(net_id, capacity, "deployed + activated token-pool net");
}

// ===========================================================================
// R4b — datacenter lease-adapter net (scheduler backend)
// ===========================================================================

/// The fully-resolved per-cluster connection mekhan threads into the
/// lease-adapter net's `effect_config`.
///
/// mekhan owns Vault + the resolver: it reads the datacenter resource's
/// `public_config` (non-secret connection fields, inline) and builds
/// `{{secret:<vault_path>#<field>}}` templates for each secret field. The engine
/// is the *consumer* — at fire time `firing.rs` runs `resolve_secrets` over the
/// effect_config, unwrapping each `{{secret:…}}` just-in-time (the secret never
/// lands in AIR or the event log). The engine's `ClusterRegistry` parses the
/// resulting object to build a per-`(resource_id, resource_version)` client
/// lazily on first fire — `scheduler_flavor` picks the leg and the two
/// correlation keys are the cache key (docs/16 §2; docs/13 option A).
///
/// All per-flavor fields are `Option` — the resource carries only the fields its
/// flavor needs (publish-time flavor-validation in R1 guarantees the required
/// ones are present before this struct is ever built). [`Self::effect_config`]
/// emits ONLY the keys the flavor needs, so a slurm cluster's net never carries
/// (placeholder) `allocator_url`/`nomad_*` keys and vice-versa.
#[derive(Debug, Clone)]
pub struct DatacenterConnection {
    /// Cluster identity — `resource_id` + `resource_version` are the
    /// `ClusterRegistry` cache key (every flavor carries both, inline/non-secret).
    pub resource_id: Uuid,
    pub resource_version: i32,
    /// Allocator dialect: `"slurm"`, `"nomad"`, or `"http"` — the discriminant.
    pub scheduler_flavor: String,

    // http leg (unchanged from today)
    pub allocator_url: Option<String>,
    /// `{{secret:<vault_path>#token}}` template (http bearer token).
    pub token_secret_ref: Option<String>,

    // slurm leg
    pub ssh_host: Option<String>,
    pub ssh_port: Option<u16>,
    pub ssh_user: Option<String>,
    pub ssh_known_hosts: Option<String>,
    pub template_dir: Option<String>,
    /// `{{secret:<vault_path>#ssh_key}}` template (inline PEM private key).
    pub ssh_key_secret_ref: Option<String>,

    // nomad leg
    pub nomad_addr: Option<String>,
    pub nomad_region: Option<String>,
    /// `{{secret:<vault_path>#nomad_token}}` template (optional — omitted if the
    /// cluster carries no nomad_token).
    pub nomad_token_secret_ref: Option<String>,
}

impl DatacenterConnection {
    /// Emit the flavor-conditional effect_config baked onto both lease effect
    /// transitions. Only the keys the flavor needs are emitted; every flavor
    /// carries `scheduler_flavor` + the `resource_id`/`resource_version`
    /// correlation keys (docs/16 §2.1).
    pub fn effect_config(&self) -> serde_json::Value {
        // Correlation keys + discriminant — on EVERY flavor.
        let mut cfg = json!({
            "scheduler_flavor": self.scheduler_flavor,
            "resource_id": self.resource_id.to_string(),
            "resource_version": self.resource_version,
        });
        let obj = cfg.as_object_mut().expect("json! object");

        macro_rules! put {
            ($key:literal, $opt:expr) => {
                if let Some(v) = &$opt {
                    obj.insert($key.to_string(), json!(v));
                }
            };
        }

        match self.scheduler_flavor.as_str() {
            "slurm" => {
                put!("ssh_host", self.ssh_host);
                put!("ssh_port", self.ssh_port);
                put!("ssh_user", self.ssh_user);
                put!("ssh_known_hosts", self.ssh_known_hosts);
                put!("template_dir", self.template_dir);
                put!("ssh_key", self.ssh_key_secret_ref);
            }
            "nomad" => {
                put!("nomad_addr", self.nomad_addr);
                put!("nomad_region", self.nomad_region);
                put!("nomad_token", self.nomad_token_secret_ref);
            }
            // "http" + any unknown flavor → the generic HTTP allocator leg
            // (unchanged from today). The engine's flavor dispatch defaults the
            // same way.
            _ => {
                put!("allocator_url", self.allocator_url);
                put!("token", self.token_secret_ref);
            }
        }

        cfg
    }

    /// Build a [`DatacenterConnection`] from a datacenter resource's resolved
    /// `public_config` + identity. `vault_path` is the per-version secret base
    /// (caller computes via [`crate::handlers::resources::vault_path_for`]).
    ///
    /// Returns `None` when the flavor's required connection field is missing
    /// (caller skips — R1 create/publish validation is the authoritative gate).
    /// This is the single source of the public-config → connection mapping,
    /// shared by the resource-create adapter-net deploy
    /// (`ensure_pool_net_for_kind`) and the B-staging resolver
    /// (`crate::petri::staging_net::resolve_datacenter_connection`), so the two
    /// can never drift on secret-ref shape or required-field gates.
    pub(crate) fn from_public_config(
        resource_id: Uuid,
        resource_version: i32,
        vault_path: &str,
        public: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<Self> {
        let scheduler_flavor = public
            .get("scheduler_flavor")
            .and_then(|v| v.as_str())
            .unwrap_or("http")
            .to_string();

        let secret_ref = |field: &str| format!("{{{{secret:{vault_path}#{field}}}}}");
        let s = |k: &str| public.get(k).and_then(|v| v.as_str()).map(String::from);
        let port = |k: &str| {
            public
                .get(k)
                .and_then(|v| v.as_u64())
                .and_then(|n| u16::try_from(n).ok())
        };

        let required_present = match scheduler_flavor.as_str() {
            "slurm" => public.get("ssh_host").and_then(|v| v.as_str()).is_some(),
            "nomad" => public.get("nomad_addr").and_then(|v| v.as_str()).is_some(),
            _ => public
                .get("allocator_url")
                .and_then(|v| v.as_str())
                .is_some(),
        };
        if !required_present {
            return None;
        }

        Some(DatacenterConnection {
            resource_id,
            resource_version,
            scheduler_flavor: scheduler_flavor.clone(),

            allocator_url: s("allocator_url"),
            token_secret_ref: matches!(scheduler_flavor.as_str(), "http")
                .then(|| secret_ref("token")),

            ssh_host: s("ssh_host"),
            ssh_port: port("ssh_port"),
            ssh_user: s("ssh_user"),
            ssh_known_hosts: s("ssh_known_hosts"),
            template_dir: s("template_dir"),
            ssh_key_secret_ref: (scheduler_flavor == "slurm").then(|| secret_ref("ssh_key")),

            nomad_addr: s("nomad_addr"),
            nomad_region: s("nomad_region"),
            nomad_token_secret_ref: (scheduler_flavor == "nomad"
                && public.contains_key(crate::handlers::resources::NOMAD_TOKEN_SENTINEL))
            .then(|| secret_ref("nomad_token")),
        })
    }
}

/// Build the AIR `ScenarioDefinition` for a `datacenter` resource's
/// lease-adapter net — the `scheduler` backend's per-resource net, parallel to
/// [`build_token_pool_net`].
///
/// Same net-id scheme (`well_known::pool_net_id(resource_id)` = `pool-<id>`) and
/// the SAME cross-net inbox names (`POOL_{CLAIM,REGISTER,RELEASE}_INBOX`, reply
/// channel `"grant"`) as the token pool — so the R2 instance claim contract
/// works UNCHANGED regardless of which backend kind the alias resolved to. The
/// KIND decides what the net IS: instead of an in-net capacity pool, this net
/// holds a *lease* against an external allocator, calling the R4a engine effects
/// (`resource_lease_acquire` / `resource_lease_release`).
///
/// ## Contract (lines up with R1 `DatacenterLease` + R2 + R4a)
///
/// Every `Scheduled` step (standalone or inside a `LeaseScope`) bridges to
/// this net's `well_known::pool_net_id(resource_id)` = `pool-<id>`, and:
///
/// - **claim** → `claim_inbox` carries `ClaimRequest { grant_id, request }`.
///   `t_request` fires `resource_lease_acquire` (effect_config = the resolved
///   connection `{ allocator_url, token }`). The effect POSTs the request to
///   the allocator and emits the typed lease `{ grant_id, node, gpu_uuid,
///   alloc_id, expiry }` on its `"lease"` output port → routed to `grant_outbox`
///   (reply channel `"grant"`). So the grant reply the instance's
///   `p_<id>_grant_inbox` (typed `Lease__datacenter` in R2) receives IS the lease.
/// - **register** → `register_inbox` carries the lease echoed back over a PLAIN
///   bridge (R2's `t_acquire` sets `reg: grant`, i.e. the whole lease). `t_register`
///   records a CLEAN `in_use` hold carrying `{ grant_id, alloc_id, node, gpu_uuid,
///   expiry }` — `alloc_id` is the load-bearing field: release/reap need it, and
///   it lives on the hold, NOT on the bare `{ grant_id }` release request.
/// - **release** → `release_inbox` carries `ReleaseRequest { grant_id }`.
///   `alloc_id` is joined IN from the `in_use` hold: `t_release_prep` consumes
///   `{ release_inbox, in_use }` correlated on `grant_id` → a combined
///   `{ grant_id, alloc_id }` on `release_prep`, which `t_release` (effect
///   `resource_lease_release`, same connection) consumes on its `"release"` port
///   → DELETEs the allocation at the allocator.
/// - **lease_expired** (signal) + `in_use` correlated on `grant_id` → `t_reap`:
///   the allocator's TTL already reclaimed the allocation, so reap just DROPS the
///   hold. It does NOT re-call release (the alloc is already gone; the R4a DELETE
///   is 404-tolerant, but firing an effect on the reap path would need the same
///   prep-join and buys nothing — the lease is dead either way).
///
/// ## Reply-routing-taint discipline (mirrors `build_token_pool_net`)
///
/// `t_request` consumes the routed claim and emits ONLY the grant reply (the
/// effect's `"lease"` output → `grant_outbox`); it produces NO local hold. The
/// hold is registered separately over the PLAIN `register_inbox` bridge, so the
/// `in_use` hold (and anything recycled from it) carries no stale reply routing.
///
/// `token_secret_ref` is the `{{secret:<vault_path>#token}}` template for the
/// datacenter's Vault token — the engine resolves it just-in-time at fire time
/// (`firing.rs` `resolve_secrets`), so the secret never enters the AIR/event log.
pub fn build_datacenter_lease_adapter_net(conn: &DatacenterConnection) -> ScenarioDefinition {
    let resource_id = conn.resource_id;
    let net_id = well_known::pool_net_id(resource_id);
    let scheduler_flavor = conn.scheduler_flavor.as_str();
    let mut ctx = Context::new(net_id).description(format!(
        "Datacenter lease adapter for resource {resource_id} (flavor {scheduler_flavor}). \
         Holds a lease against an external cluster via the resource_lease engine effects; \
         grant reply is the typed Lease__datacenter the R2 compiled steps consume."
    ));

    // The full per-flavor connection passed to BOTH effect transitions. Secret
    // fields are `{{secret:…}}` templates resolved at fire time by the engine
    // (`firing.rs` `resolve_secrets`), so they never enter the AIR/event log.
    // `scheduler_flavor` + the `resource_id`/`resource_version` correlation keys
    // let the engine's `ClusterRegistry` build (and cache) the right per-cluster
    // `ClusterClient` lazily on first fire.
    let effect_config = conn.effect_config();

    // Observable hold + terminal records (all DynamicToken — the adapter net
    // only routes; the typed-lease schema lives on the instance side in R2).
    let in_use: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("in_use", "In Use");
    let done: aithericon_sdk::PlaceHandle<DynamicToken> = ctx.state("done", "Released Leases");

    // Cross-net inboxes — the SAME shared names as the token pool.
    let claim_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_CLAIM_INBOX, "Claim Inbox");
    let register_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_REGISTER_INBOX, "Register Inbox");
    let release_inbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_in(well_known::POOL_RELEASE_INBOX, "Release Inbox");

    // Grant reply channel — the effect's "lease" output is routed here.
    let grant_outbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_reply_channel("grant_outbox", "Grant Outbox", "grant");

    // Fail reply channel — `t_lease_died` routes a held-allocation-death token
    // back to the claiming instance's loop over the SAME claim token's reply
    // routing (the loop's `claim_out` carries both a "grant" and a "fail"
    // channel). This is the fail-fast path (docs/16 §7): when the held salloc /
    // dispatched drain-executor dies mid-lease the watcher signals `lease_failed`
    // here, and this routes a `{ grant_id }` failure token to the loop's
    // `p_<loop>_lease_failed` inbox so the loop aborts instead of enqueuing the
    // next iteration into a now-dead NATS namespace.
    let fail_outbox: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.bridge_reply_channel("fail_outbox", "Lease-Failed Outbox", "fail");

    // Lease-expiry signal (journaled → replay-safe reap).
    let lease_expired: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.signal("lease_expired", "Lease Expired");

    // Held-allocation-death signal. The per-cluster watcher routes the held
    // alloc's TERMINAL signal here (via the acquire effect's stamped routing
    // meta — the `failed` status route targets `lease_failed`) when the salloc /
    // dispatched drain-executor dies mid-lease. Distinct from `lease_expired`
    // (a clean TTL reap that silently drops the hold): a death must be SURFACED
    // back to the loop as a failure. Journaled → replay-safe.
    let lease_failed: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.signal("lease_failed", "Lease Failed (held alloc died)");

    // Internal place joining release_inbox + in_use before the release effect.
    let release_prep: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.state("release_prep", "Release Prep (grant_id + alloc_id)");

    // Internal place catching `t_request`'s `_error` token on an acquire failure.
    // The engine's `_error`-port path consumes the claim, records `EffectFailed`,
    // and routes the raw error token HERE (carrying the consumed claim's reply
    // routing per `firing.rs` `route_output_tokens` internal-place branch) INSTEAD
    // of NetFailing the whole adapter net — so one claimant's bad acquire never
    // takes down the SHARED pool. `t_request_failed` reshapes it onto the `fail`
    // reply channel back to that one claimant.
    let request_error: aithericon_sdk::PlaceHandle<DynamicToken> =
        ctx.state("request_error", "Acquire Error (raw _error token)");

    // t_request — acquire effect. Consumes the routed claim, fires
    // resource_lease_acquire (effect reads the claim on its "request" port +
    // the resolved connection from effect_config), and emits ONLY the lease on
    // the "lease" port → grant_outbox (the grant reply). NO local hold here.
    ctx.transition("t_request", "Request Lease")
        .auto_input("request", &claim_inbox)
        .auto_output("lease", &grant_outbox)
        .auto_output("_error", &request_error)
        .effect_with_config(
            effects::RESOURCE_LEASE_ACQUIRE.handler_id,
            effect_config.clone(),
        );

    // t_request_failed — acquire effect FAILED (e.g. allocator returned 500
    // 'parameterized job not found'). The engine routed the raw error token to
    // `request_error`, which carries the consumed claim's reply routing. Reshape
    // it onto the `fail` reply channel so the SPECIFIC claiming instance's
    // lease-failed inbox receives `{ grant_id, error, phase }` and aborts (the
    // instance side's `t_<id>_claim_abort` in `lease_bridge.rs`). `grant_id` is
    // nested under the effect's `request` input port in the raw error token.
    // The shared pool net is UNAFFECTED — it consumed the claim and keeps serving.
    ctx.transition("t_request_failed", "Acquire Failed (notify claimant)")
        .auto_input("err", &request_error)
        .auto_output("notify", &fail_outbox)
        .logic(
            r#"#{ notify: #{
                grant_id: err.inputs.request.grant_id,
                error: err.error,
                phase: "acquire"
            } }"#,
        );

    // t_register — record the lease hold over the PLAIN register bridge. Keep
    // the WHOLE echoed lease (esp. alloc_id) so release/reap can reclaim.
    ctx.transition("t_register", "Register Lease Hold")
        .auto_input("reg", &register_inbox)
        .auto_output("hold", &in_use)
        .logic(
            // `alloc_id` is load-bearing (release/reap DELETE key); the rest is
            // adapter-side traceability. `node`/`expiry`/`scheduler` ride from the
            // echoed lease when present (Rhai yields `()` for an absent optional —
            // harmless on this observational hold). `gpu_uuid` is gone.
            r#"#{ hold: #{
                grant_id: reg.grant_id,
                alloc_id: reg.alloc_id,
                node: reg.node,
                expiry: reg.expiry,
                scheduler: reg.scheduler
            } }"#,
        );

    ctx.scope("Release", |ctx| {
        // t_release_prep — the release request is just `{ grant_id }`; the
        // alloc_id needed to DELETE the allocation lives on the in_use hold.
        // Join them (correlate grant_id) into `{ grant_id, alloc_id }` for the
        // effect, and record the freed lease in `done`.
        ctx.transition("t_release_prep", "Join Release + Hold")
            .auto_input("req", &release_inbox)
            .auto_input("held", &in_use)
            .correlate("req", "held", "grant_id")
            .auto_output("release", &release_prep)
            .auto_output("done", &done)
            .logic(
                r#"#{
                    release: #{ grant_id: held.grant_id, alloc_id: held.alloc_id },
                    done:    #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "released" }
                }"#,
            );

        // t_release — release effect: DELETE the allocation at the allocator.
        // Reads `{ grant_id, alloc_id }` on the "release" port; the handler's
        // `{ grant_id }` "released" output is recorded in `done` (an observable
        // terminal — the instance already released on its own side, so this is
        // adapter-side bookkeeping, not routed back).
        ctx.transition("t_release", "Release Lease")
            .auto_input("release", &release_prep)
            .auto_output("released", &done)
            .effect_with_config(
                effects::RESOURCE_LEASE_RELEASE.handler_id,
                effect_config.clone(),
            );

        // t_reap — lease expired (allocator TTL already reclaimed the alloc).
        // Just DROP the hold; do NOT re-call release — the allocation is already
        // gone.
        //
        // Correlate on `scheduler_job_id`, NOT `grant_id`: the `exp` token is a
        // watcher-injected SIGNAL, and the watcher payload carries
        // `scheduler_job_id` (= the dispatched Nomad job id / Slurm job id) but
        // NOT `grant_id` — the grant_id rides as the signal's `signal_key`
        // (sibling causality meta), which never lands in the token color. The
        // held hold's `alloc_id` IS that same dispatched-job / slurm-job id (the
        // acquire effect stores `dispatched_job_id` / slurm job id as the lease's
        // `alloc_id`), so `exp.scheduler_job_id == held.alloc_id` is the value
        // that actually correlates. Matching on `grant_id` here compared
        // `()` (absent) against the held string and NEVER bound — the hold leaked.
        ctx.transition("t_reap", "Reap Expired Lease")
            .auto_input("exp", &lease_expired)
            .auto_input("held", &in_use)
            .guard("exp.scheduler_job_id == held.alloc_id")
            .auto_output("done", &done)
            .logic(
                r#"#{ done: #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "reaped" } }"#,
            );

        // t_lease_done — drain a CLEAN drain-executor terminal whose hold was
        // ALREADY released. The completion route (resource_lease handler) sends
        // the executor's clean `completed` status here as a `lease_expired`
        // signal so it never falls back to `lease_failed` (the false-failure fix).
        // But on a normal release `t_release_prep` consumes `in_use` BEFORE that
        // completion signal arrives, so `t_reap` (which correlates an `in_use`
        // hold) has no binding and the token would otherwise sit in
        // `lease_expired` forever. This 1-input transition drains that orphan
        // (records it in `done`). It can NEVER steal a real TTL reap: `t_reap`
        // (2 inputs) binds the same — newest — `lease_expired` token, so both
        // share an enabling time and the engine's specificity rule (more input
        // arcs wins at equal time, evaluation.rs select_next_transition) makes
        // `t_reap` win whenever a hold exists; `t_lease_done` only fires when
        // `t_reap` is disabled (hold gone).
        ctx.transition("t_lease_done", "Lease Terminal Drain (released)")
            .auto_input("exp", &lease_expired)
            .auto_output("done", &done)
            // `exp` is a watcher signal; it carries `scheduler_job_id`, not
            // `grant_id` (that rides as `signal_key`). Record the alloc id for
            // traceability — `exp.grant_id` was always `()` here.
            .logic(
                r#"#{ done: #{ alloc_id: exp.scheduler_job_id, outcome: "lease_done" } }"#,
            );

        // t_lease_died — held-allocation death (docs/16 §7). The watcher routed
        // the held alloc's terminal signal to `lease_failed`; consume it + the
        // matching `in_use` hold, DROP the hold (the alloc is already dead — no
        // release call, like reap), record the death in `done`, AND route a
        // `{ grant_id }` failure token back to the claiming loop over the "fail"
        // reply channel so it fails fast.
        //
        // Correlation key = `scheduler_job_id`, NOT `grant_id`. The `fail` token
        // is a watcher-injected SIGNAL whose payload carries `scheduler_job_id`
        // (the dispatched Nomad job id / Slurm job id) but NOT `grant_id` — the
        // grant_id rides as the signal's `signal_key` (sibling causality meta)
        // and never enters the token color, so a `fail.grant_id == held.grant_id`
        // guard compared `()` against the held string and NEVER bound: held-alloc
        // deaths were silently dropped instead of failing the loop fast. The
        // held hold's `alloc_id` is that SAME dispatched-job / slurm-job id (the
        // acquire effect stores it as the lease `alloc_id`), so
        // `fail.scheduler_job_id == held.alloc_id` is the field pair that
        // actually correlates the death signal to its hold — and grant_id (which
        // the loop's fail inbox needs) is recovered from the matched `held`.
        ctx.transition("t_lease_died", "Lease Died (held alloc failure)")
            .auto_input("fail", &lease_failed)
            .auto_input("held", &in_use)
            .guard("fail.scheduler_job_id == held.alloc_id")
            .auto_output("notify", &fail_outbox)
            .auto_output("done", &done)
            .logic(
                r#"#{
                    notify: #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "lease_failed" },
                    done:   #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "lease_failed" }
                }"#,
            );
    });

    ctx.build()
}

/// Idempotently ensure a `datacenter` resource's lease-adapter net is deployed +
/// running. Parallel to [`ensure_token_pool_net_deployed`]: probe-then-deploy
/// via [`crate::petri::instance::deploy_instance`], engine-down failures are
/// logged + SWALLOWED (the resource is durable; the net is re-derivable from the
/// resolved [`DatacenterConnection`]). Re-deploying is harmless — the adapter net
/// carries no per-instance seed state.
pub async fn ensure_datacenter_adapter_deployed(
    petri: &crate::petri::client::PetriClient,
    conn: &DatacenterConnection,
) {
    let resource_id = conn.resource_id;
    let scheduler_flavor = conn.scheduler_flavor.as_str();
    let net_id = well_known::pool_net_id(resource_id);

    if matches!(
        petri.try_get_run_mode(&net_id).await,
        Some(petri_api_types::RunMode::Running)
    ) {
        tracing::debug!(
            net_id,
            "datacenter lease-adapter net already deployed + running; no-op"
        );
        return;
    }

    let air = match serde_json::to_value(build_datacenter_lease_adapter_net(conn)) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(net_id, %e, "failed to serialize datacenter lease-adapter net AIR");
            return;
        }
    };

    if let Err(e) = crate::petri::instance::deploy_instance(
        petri,
        &net_id,
        &air,
        petri_api_types::DispatchOptions::default(),
        None,
    )
    .await
    {
        tracing::warn!(
            net_id,
            scheduler_flavor,
            %e,
            "failed to deploy datacenter lease-adapter net to the engine — resource CRUD \
             still succeeded; the net will be (re)deployed on the next resource version \
             or at template publish when the alias is referenced"
        );
        return;
    }
    tracing::info!(
        net_id,
        scheduler_flavor,
        "deployed + activated datacenter lease-adapter net"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn air(resource_id: Uuid, capacity: u32) -> serde_json::Value {
        serde_json::to_value(build_token_pool_net(resource_id, capacity))
            .expect("pool net serializes to AIR")
    }

    fn place<'a>(air: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
        air["places"].as_array()?.iter().find(|p| p["id"] == id)
    }

    fn transition<'a>(air: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
        air["transitions"]
            .as_array()?
            .iter()
            .find(|t| t["id"] == id)
    }

    /// The cross-net contract places exist with the right kinds + names the R2
    /// instance bridges target.
    #[test]
    fn topology_matches_r2_contract() {
        let a = air(Uuid::nil(), 2);

        // Inboxes are bridge_in with the well-known names. (AIR serializes the
        // place kind under the `type` key.)
        for name in [
            well_known::POOL_CLAIM_INBOX,
            well_known::POOL_REGISTER_INBOX,
            well_known::POOL_RELEASE_INBOX,
        ] {
            let p = place(&a, name).unwrap_or_else(|| panic!("missing place {name}"));
            assert_eq!(p["type"], "bridge_in", "{name} kind");
        }

        // Grant outbox routes the "grant" reply channel (a `state` place with
        // `bridge_reply` set).
        let grant = place(&a, "grant_outbox").expect("grant_outbox");
        assert_eq!(grant["bridge_reply"], true);
        assert_eq!(grant["bridge_reply_channel"], "grant");

        // lease_expired is a signal place (journaled reap, replay-safe).
        assert_eq!(place(&a, "lease_expired").unwrap()["type"], "signal");

        // The four transitions exist.
        for t in ["t_grant", "t_register", "t_release", "t_reap"] {
            assert!(transition(&a, t).is_some(), "missing transition {t}");
        }
    }

    /// The grant reply must be `{ grant_id, unit_id }` — `unit_id` is R1's
    /// `TokenPoolLease` field + R2's `Lease__token_pool` schema. This is the
    /// load-bearing field-name alignment.
    #[test]
    fn grant_reply_is_typed_lease_unit_id() {
        let a = air(Uuid::nil(), 1);
        let logic = transition(&a, "t_grant").unwrap()["logic"].to_string();
        assert!(
            logic.contains("grant_id: claim.grant_id") && logic.contains("unit_id: cap.unit_id"),
            "t_grant must reply the typed lease {{ grant_id, unit_id }}: {logic}"
        );
        // register echoes the lease; release/reap correlate on grant_id.
        let reg = transition(&a, "t_register").unwrap()["logic"].to_string();
        assert!(reg.contains("reg.grant_id") && reg.contains("reg.unit_id"));
    }

    /// Capacity is seeded as N clean `{ unit_id }` tokens.
    #[test]
    fn seeds_capacity_clean_unit_tokens() {
        let a = air(Uuid::nil(), 3);
        let pool = place(&a, "pool").expect("pool place");
        let seeded = pool["initial_tokens"].as_array().expect("initial_tokens");
        assert_eq!(seeded.len(), 3, "capacity tokens seeded");
        // ScenarioToken::Data is untagged → serializes as the bare JSON object.
        let labels: Vec<&str> = seeded
            .iter()
            .filter_map(|t| t["unit_id"].as_str())
            .collect();
        assert_eq!(labels, vec!["unit-0", "unit-1", "unit-2"]);
    }

    /// Net id (and scenario name) derive from the resource id via the shared
    /// `well_known::pool_net_id` — the same id R2's claim bridge targets.
    #[test]
    fn name_is_pool_net_id() {
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let a = air(id, 1);
        assert_eq!(a["name"], format!("pool-{id}"));
        assert_eq!(a["name"], well_known::pool_net_id(id));
    }

    // -----------------------------------------------------------------------
    // R4b — datacenter lease-adapter net
    // -----------------------------------------------------------------------

    fn http_conn(resource_id: Uuid) -> DatacenterConnection {
        DatacenterConnection {
            resource_id,
            resource_version: 1,
            scheduler_flavor: "http".to_string(),
            allocator_url: Some("http://allocator.test/leases".to_string()),
            token_secret_ref: Some("{{secret:resources/ws/dc/v1#token}}".to_string()),
            ssh_host: None,
            ssh_port: None,
            ssh_user: None,
            ssh_known_hosts: None,
            template_dir: None,
            ssh_key_secret_ref: None,
            nomad_addr: None,
            nomad_region: None,
            nomad_token_secret_ref: None,
        }
    }

    fn dc_air(resource_id: Uuid) -> serde_json::Value {
        serde_json::to_value(build_datacenter_lease_adapter_net(&http_conn(resource_id)))
            .expect("datacenter adapter net serializes to AIR")
    }

    /// The adapter shares the EXACT cross-net contract (inbox names, grant reply
    /// channel) with the token pool, so the R2 instance claim works unchanged,
    /// and the net name is `pool-<id>`.
    #[test]
    fn datacenter_adapter_shares_pool_contract() {
        let id = Uuid::parse_str("22222222-3333-4444-5555-666666666666").unwrap();
        let a = dc_air(id);

        assert_eq!(a["name"], well_known::pool_net_id(id));

        for name in [
            well_known::POOL_CLAIM_INBOX,
            well_known::POOL_REGISTER_INBOX,
            well_known::POOL_RELEASE_INBOX,
        ] {
            let p = place(&a, name).unwrap_or_else(|| panic!("missing place {name}"));
            assert_eq!(p["type"], "bridge_in", "{name} kind");
        }
        let grant = place(&a, "grant_outbox").expect("grant_outbox");
        assert_eq!(grant["bridge_reply"], true);
        assert_eq!(grant["bridge_reply_channel"], "grant");
        assert_eq!(place(&a, "lease_expired").unwrap()["type"], "signal");
    }

    /// t_request is an EFFECT transition firing `resource_lease_acquire`, with
    /// effect_config carrying allocator_url + the {{secret:…}} token template,
    /// and its lease output routed to the "grant" reply channel.
    #[test]
    fn t_request_fires_acquire_effect_with_connection_config() {
        let a = dc_air(Uuid::nil());
        let t = transition(&a, "t_request").expect("t_request");

        // Effect transition (logic.type == "effect") with the acquire handler.
        assert_eq!(t["logic"]["type"], "effect");
        assert_eq!(t["logic"]["handler_id"], "resource_lease_acquire");

        // effect_config carries the resolved connection. token is the
        // {{secret:…}} template (resolved by the engine at fire time, never in
        // the AIR plaintext).
        let cfg = &t["logic"]["config"];
        assert_eq!(cfg["allocator_url"], "http://allocator.test/leases");
        assert_eq!(cfg["token"], "{{secret:resources/ws/dc/v1#token}}");

        // Discriminant + the two ClusterRegistry cache/correlation keys ride on
        // EVERY flavor (the http leg uses Uuid::nil here).
        assert_eq!(cfg["scheduler_flavor"], "http");
        assert_eq!(cfg["resource_id"], Uuid::nil().to_string());
        assert_eq!(cfg["resource_version"], 1);
        // The http flavor emits NO slurm/nomad keys.
        assert!(cfg.get("ssh_host").is_none());
        assert!(cfg.get("nomad_addr").is_none());

        // Input on "request" (← claim_inbox), output "lease" → grant_outbox.
        let in_ports: Vec<&str> = t["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["port"].as_str().unwrap())
            .collect();
        assert!(in_ports.contains(&"request"), "inputs: {in_ports:?}");
        let out_to_grant = t["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["port"] == "lease" && o["place"] == "grant_outbox");
        assert!(out_to_grant, "lease output must route to grant_outbox: {t}");
    }

    /// A slurm cluster's effect_config carries the SSH connection (with the
    /// inline-PEM secret as a `{{secret:…}}` template) + the correlation keys, and
    /// NONE of the http/nomad keys.
    #[test]
    fn slurm_effect_config_carries_ssh_connection() {
        let id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let conn = DatacenterConnection {
            resource_id: id,
            resource_version: 7,
            scheduler_flavor: "slurm".to_string(),
            allocator_url: None,
            token_secret_ref: None,
            ssh_host: Some("login.hpc.test".to_string()),
            ssh_port: Some(2222),
            ssh_user: Some("runner".to_string()),
            ssh_known_hosts: Some("accept".to_string()),
            template_dir: Some("/opt/jobs".to_string()),
            ssh_key_secret_ref: Some("{{secret:resources/ws/dc/v7#ssh_key}}".to_string()),
            nomad_addr: None,
            nomad_region: None,
            nomad_token_secret_ref: None,
        };
        let cfg = conn.effect_config();
        assert_eq!(cfg["scheduler_flavor"], "slurm");
        assert_eq!(cfg["resource_id"], id.to_string());
        assert_eq!(cfg["resource_version"], 7);
        assert_eq!(cfg["ssh_host"], "login.hpc.test");
        assert_eq!(cfg["ssh_port"], 2222);
        assert_eq!(cfg["ssh_user"], "runner");
        assert_eq!(cfg["ssh_known_hosts"], "accept");
        assert_eq!(cfg["template_dir"], "/opt/jobs");
        assert_eq!(cfg["ssh_key"], "{{secret:resources/ws/dc/v7#ssh_key}}");
        // No http / nomad leg keys leaked.
        assert!(cfg.get("allocator_url").is_none());
        assert!(cfg.get("token").is_none());
        assert!(cfg.get("nomad_addr").is_none());
    }

    /// A nomad cluster's effect_config carries the Nomad address/region + the
    /// optional token template (here present), and NO ssh/http keys.
    #[test]
    fn nomad_effect_config_carries_nomad_connection() {
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let conn = DatacenterConnection {
            resource_id: id,
            resource_version: 3,
            scheduler_flavor: "nomad".to_string(),
            allocator_url: None,
            token_secret_ref: None,
            ssh_host: None,
            ssh_port: None,
            ssh_user: None,
            ssh_known_hosts: None,
            template_dir: None,
            ssh_key_secret_ref: None,
            nomad_addr: Some("http://nomad.test:4646".to_string()),
            nomad_region: Some("global".to_string()),
            nomad_token_secret_ref: Some("{{secret:resources/ws/dc/v3#nomad_token}}".to_string()),
        };
        let cfg = conn.effect_config();
        assert_eq!(cfg["scheduler_flavor"], "nomad");
        assert_eq!(cfg["resource_id"], id.to_string());
        assert_eq!(cfg["resource_version"], 3);
        assert_eq!(cfg["nomad_addr"], "http://nomad.test:4646");
        assert_eq!(cfg["nomad_region"], "global");
        assert_eq!(
            cfg["nomad_token"],
            "{{secret:resources/ws/dc/v3#nomad_token}}"
        );
        assert!(cfg.get("ssh_host").is_none());
        assert!(cfg.get("allocator_url").is_none());
    }

    /// The optional nomad_token is OMITTED entirely when the cluster carries no
    /// secret (an unauthenticated dev Nomad) — not emitted as null.
    #[test]
    fn nomad_effect_config_omits_absent_token() {
        let conn = DatacenterConnection {
            resource_id: Uuid::nil(),
            resource_version: 1,
            scheduler_flavor: "nomad".to_string(),
            allocator_url: None,
            token_secret_ref: None,
            ssh_host: None,
            ssh_port: None,
            ssh_user: None,
            ssh_known_hosts: None,
            template_dir: None,
            ssh_key_secret_ref: None,
            nomad_addr: Some("http://nomad.test:4646".to_string()),
            nomad_region: None,
            nomad_token_secret_ref: None,
        };
        let cfg = conn.effect_config();
        assert!(
            cfg.get("nomad_token").is_none(),
            "absent token must be omitted, not null"
        );
        assert!(cfg.get("nomad_region").is_none());
    }

    /// t_register keeps alloc_id (+ the rest of the lease) on the in_use hold —
    /// release/reap need alloc_id, which the bare `{grant_id}` release request
    /// lacks.
    #[test]
    fn in_use_hold_carries_alloc_id() {
        let a = dc_air(Uuid::nil());
        let reg = transition(&a, "t_register").expect("t_register");
        let logic = reg["logic"]["source"].as_str().expect("rhai source");
        assert!(
            logic.contains("alloc_id: reg.alloc_id") && logic.contains("grant_id: reg.grant_id"),
            "t_register hold must carry grant_id + alloc_id: {logic}"
        );
        // The hold lands in in_use.
        let to_in_use = reg["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "in_use");
        assert!(to_in_use, "t_register must output to in_use");
    }

    /// Release threads alloc_id from the in_use hold (NOT the bare release
    /// request) into the release effect: a prep transition joins
    /// release_inbox + in_use (correlate grant_id) → release_prep, then the
    /// release effect fires `resource_lease_release` on its "release" port.
    #[test]
    fn release_joins_alloc_id_from_hold_then_fires_release_effect() {
        let a = dc_air(Uuid::nil());

        // Prep transition consumes release_inbox + in_use, emits {grant_id, alloc_id}.
        // (`ctx.scope` only tags group_id for visualization — it does NOT
        // prefix transition ids, unlike `scoped_prefix`.)
        let prep = transition(&a, "t_release_prep").expect("t_release_prep");
        let in_places: Vec<&str> = prep["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["place"].as_str().unwrap())
            .collect();
        assert!(
            in_places.contains(&well_known::POOL_RELEASE_INBOX) && in_places.contains(&"in_use"),
            "prep must consume release_inbox + in_use, got {in_places:?}"
        );
        let prep_logic = prep["logic"]["source"].as_str().unwrap();
        assert!(
            prep_logic.contains("alloc_id: held.alloc_id")
                && prep_logic.contains("grant_id: held.grant_id"),
            "prep must build {{grant_id, alloc_id}} from the hold: {prep_logic}"
        );

        // The release EFFECT fires resource_lease_release on its "release" port,
        // with the same connection config.
        let rel = transition(&a, "t_release").expect("t_release");
        assert_eq!(rel["logic"]["type"], "effect");
        assert_eq!(rel["logic"]["handler_id"], "resource_lease_release");
        assert_eq!(
            rel["logic"]["config"]["allocator_url"],
            "http://allocator.test/leases"
        );
        let rel_in: Vec<&str> = rel["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["port"].as_str().unwrap())
            .collect();
        assert!(
            rel_in.contains(&"release"),
            "release effect input port: {rel_in:?}"
        );
    }

    /// Held-allocation death (docs/16 §7): the adapter net has a `lease_failed`
    /// SIGNAL place + a `fail_outbox` reply-channel ("fail") + a `t_lease_died`
    /// transition that consumes `{lease_failed, in_use}` (correlate grant_id),
    /// drops the hold (no release call — the alloc is already dead), and routes a
    /// failure token over the "fail" channel back to the claiming loop.
    #[test]
    fn lease_died_routes_failure_over_fail_channel() {
        let a = dc_air(Uuid::nil());

        // lease_failed is a journaled signal place (replay-safe), distinct from
        // lease_expired (the clean TTL reap).
        assert_eq!(place(&a, "lease_failed").unwrap()["type"], "signal");

        // fail_outbox is a reply-channel ("fail") place.
        let fail = place(&a, "fail_outbox").expect("fail_outbox");
        assert_eq!(fail["bridge_reply"], true);
        assert_eq!(fail["bridge_reply_channel"], "fail");

        // t_lease_died consumes lease_failed + in_use, correlated on grant_id.
        let died = transition(&a, "t_lease_died").expect("t_lease_died");
        // It is a plain rhai transition — drops the hold, NO release effect (the
        // held alloc is already dead, like reap).
        assert_eq!(died["logic"]["type"], "rhai");
        let in_places: Vec<&str> = died["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["place"].as_str().unwrap())
            .collect();
        assert!(
            in_places.contains(&"lease_failed") && in_places.contains(&"in_use"),
            "t_lease_died consumes lease_failed + in_use, got {in_places:?}"
        );
        // It routes a notify token to fail_outbox (the fail reply channel).
        let to_fail = died["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "fail_outbox");
        assert!(to_fail, "t_lease_died must route to fail_outbox: {died}");
    }

    /// A clean drain-executor terminal that arrives AFTER the lease was released
    /// must not pile up in `lease_expired`. `t_lease_done` (1 input) drains the
    /// orphan token; `t_reap` (2 inputs) stays more specific so a real TTL reap
    /// with a live hold still reclaims the hold first (engine specificity rule).
    #[test]
    fn lease_done_drains_orphan_terminal_without_stealing_reap() {
        let a = dc_air(Uuid::nil());

        let done = transition(&a, "t_lease_done").expect("t_lease_done");
        assert_eq!(done["logic"]["type"], "rhai");
        let in_places: Vec<&str> = done["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["place"].as_str().unwrap())
            .collect();
        assert_eq!(
            in_places,
            vec!["lease_expired"],
            "t_lease_done must consume ONLY lease_expired (1 input): {in_places:?}"
        );
        let to_done = done["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["place"] == "done");
        assert!(to_done, "t_lease_done must record in done: {done}");

        // t_reap must stay 2-input so it out-specifies t_lease_done when a hold
        // exists (so a real TTL reap is never drained as an orphan).
        let reap = transition(&a, "t_reap").expect("t_reap");
        assert_eq!(
            reap["inputs"].as_array().unwrap().len(),
            2,
            "t_reap must keep 2 inputs (lease_expired + in_use) to out-specify t_lease_done"
        );
    }

    /// On an acquire-effect FAILURE the adapter SURVIVES (no NetFailed) and
    /// routes the failure to the claimant: t_request has an _error output arc
    /// to request_error, and t_request_failed reshapes that raw error token
    /// onto the 'fail' reply channel (fail_outbox).
    #[test]
    fn acquire_failure_routes_to_fail_channel_without_netfail() {
        let a = dc_air(Uuid::nil());
        let req = transition(&a, "t_request").expect("t_request");
        let err_arc = req["outputs"].as_array().unwrap().iter()
            .any(|o| o["port"] == "_error" && o["place"] == "request_error");
        assert!(err_arc, "t_request must route _error to request_error: {req}");
        let f = transition(&a, "t_request_failed").expect("t_request_failed");
        assert_eq!(f["logic"]["type"], "rhai");
        let in_places: Vec<&str> = f["inputs"].as_array().unwrap().iter()
            .map(|x| x["place"].as_str().unwrap()).collect();
        assert!(in_places.contains(&"request_error"), "inputs: {in_places:?}");
        let to_fail = f["outputs"].as_array().unwrap().iter()
            .any(|o| o["place"] == "fail_outbox");
        assert!(to_fail, "t_request_failed must route to fail_outbox: {f}");
        let src = f["logic"]["source"].as_str().unwrap();
        assert!(src.contains("err.inputs.request.grant_id") && src.contains("err.error"),
            "notify must carry grant_id + error: {src}");
    }

    /// t_reap drops the expired hold without re-calling release (the allocator
    /// TTL already reclaimed the alloc).
    #[test]
    fn reap_drops_hold_without_release_effect() {
        let a = dc_air(Uuid::nil());
        let reap = transition(&a, "t_reap").expect("t_reap");
        // Reap is a plain rhai transition (not an effect) — it just drops the hold.
        assert_eq!(reap["logic"]["type"], "rhai");
        let in_places: Vec<&str> = reap["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["place"].as_str().unwrap())
            .collect();
        assert!(
            in_places.contains(&"lease_expired") && in_places.contains(&"in_use"),
            "reap consumes lease_expired + in_use, got {in_places:?}"
        );
    }
}
