//! Resource-pool net — a long-lived, shared capacity pool contended for by
//! many independent workflow instances. This is the direct generalization of
//! `scheduler_net`: instead of relaying jobs to a scheduler, it grants and
//! reclaims a fixed pool of capacity tokens.
//!
//! ## The thesis: resources are places, claims are tokens
//!
//! A pool of N units is a place holding N capacity tokens. The engine's own
//! rules give us, for free, what DAG schedulers bolt on:
//!
//! - **Admission control** — `t_grant` needs a token in BOTH `claim_inbox` and
//!   `pool`; an empty pool simply leaves it disabled, so claims queue.
//! - **Mutex** — at most N capacity tokens exist, so at most N holds are active.
//!   One-transition-per-step firing means grants never race.
//! - **Conservation** — `count(pool) + count(in_use) == N` always.
//! - **Replay-safe reclamation** — `t_reap` consumes a journaled
//!   `lease_expired` signal, never a wall clock.
//!
//! ## Topology
//!
//! ```text
//! [claim_inbox: bridge_in] + [pool] ─(t_grant)─▶ [grant_outbox: reply "grant"]
//!     (consumes one capacity token; emits ONLY the bridge reply → no taint)
//!
//! [register_inbox: bridge_in] ─(t_register)─▶ [in_use]   (CLEAN hold, grant_id+unit_id)
//!     (the holder echoes its grant over a plain bridge once granted)
//!
//! [release_inbox: bridge_in] + [in_use]   ─(t_release, correlate grant_id)─▶ [pool] + [done]
//! [lease_expired: signal]    + [in_use]   ─(t_reap,    correlate grant_id)─▶ [pool] + [done]
//! ```
//!
//! Why the split grant / register: a transition that consumes the routed claim
//! taints every internal output it produces with the claim's reply routing
//! (`firing.rs::route_output_tokens`). If `t_grant` also produced the `in_use`
//! hold, recycling capacity from that hold would carry a stale "grant" channel
//! that collides with the next claim and wedges the pool. So `t_grant` emits
//! only the bridge reply, and the hold is registered separately over a clean
//! bridge — keeping every recycled capacity token clean. See docs/14.
//!
//! `lease_expired` is a plain signal place: reaping is driven by a journaled
//! signal token, never a wall clock, which keeps it replay-safe. In production a
//! durable lease timer (`ctx.delay`) armed at register time feeds that signal;
//! the pure-semantics verification (conservation, mutex, crash-reap, replay
//! determinism) lives in `core-engine/crates/test-harness/tests/resource_pool.rs`
//! and the cross-net contention proof in
//! `core-engine/crates/test-harness/src/integration/resource_pool.rs`.
//!
//! Deployed standalone this net seeds the pool with POOL_CAPACITY units and is
//! driven by instance nets that bridge claims into `claim_inbox`, register into
//! `register_inbox`, and release into `release_inbox`; inject `lease_expired`
//! (`aithericon inject ...`) to simulate a crashed holder.
//!
//! ## Deploy
//!
//! ```bash
//! cargo run -p aithericon-sdk --example resource_pool_net -- --deploy --net-id resource-pool-net
//! ```
//!
//! ## Net ID: `resource-pool-net`

use aithericon_sdk::prelude::*;

/// Number of capacity units in the pool (e.g. GPUs).
const POOL_CAPACITY: usize = 2;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// A unit of capacity sitting in the pool, or held in `in_use`.
#[token]
struct Capacity {
    unit_id: String,
}

/// A claim request arriving from an instance net. `grant_id` correlates the
/// later registration/release/reap back to the right hold; the instance mints
/// it and echoes it.
#[token]
struct ClaimRequest {
    grant_id: String,
}

/// The grant returned to the requesting instance — which unit it got.
#[token]
struct Grant {
    grant_id: String,
    unit_id: String,
}

/// Hold registration: the holder echoes its grant back over a PLAIN bridge once
/// it has the grant. Because this token never carried reply routing, consuming
/// the resulting `in_use` hold keeps the recycled capacity token CLEAN — the
/// crux that avoids the reply-routing taint (see docs/14).
#[token]
struct HoldReg {
    grant_id: String,
    unit_id: String,
}

/// Held capacity, tagged with who holds it. Lives in `in_use` for observability
/// and as the reap/release correlation target.
#[token]
struct Hold {
    grant_id: String,
    unit_id: String,
}

/// A release request (fire-and-forget) echoing the grant_id.
#[token]
struct ReleaseRequest {
    grant_id: String,
}

/// Terminal record of a freed unit, for observability.
#[token]
struct Freed {
    grant_id: String,
    unit_id: String,
    outcome: String,
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // Shared capacity. Seeded with POOL_CAPACITY distinct CLEAN units.
    let pool = ctx.state::<Capacity>("pool", "GPU Pool");
    let in_use = ctx.state::<Hold>("in_use", "In Use");
    let done = ctx.state::<Freed>("done", "Freed Units");

    // Cross-net inboxes: instance nets bridge claim / register / release here.
    let claim_inbox = ctx.bridge_in::<ClaimRequest>("claim_inbox", "Claim Inbox");
    let register_inbox = ctx.bridge_in::<HoldReg>("register_inbox", "Register Inbox");
    let release_inbox = ctx.bridge_in::<ReleaseRequest>("release_inbox", "Release Inbox");

    // Grant reply channel: routes the grant back to the requesting instance via
    // the "grant" channel embedded in the claim token's reply metadata.
    let grant_outbox = ctx.bridge_reply_channel::<Grant>("grant_outbox", "Grant Outbox", "grant");

    // Lease expiry signal. A journaled token here — injected externally in M1,
    // produced by a durable lease timer in M2 — drives crash reclamation.
    let lease_expired = ctx.signal::<ReleaseRequest>("lease_expired", "Lease Expired");

    // t_grant — claim admission. Fires only when a claim AND free capacity are
    // both present; otherwise the claim waits in `claim_inbox` (backpressure).
    // Emits ONLY the bridge grant reply: a transition that consumed the routed
    // claim would taint any internal output it produced with the claim's reply
    // routing, so we deliberately record NO local hold here (see docs/14). The
    // hold is registered separately over a clean bridge.
    ctx.scope("Grant", |ctx| {
        ctx.transition("t_grant", "Grant Capacity")
            .auto_input("claim", &claim_inbox)
            .auto_input("cap", &pool)
            .auto_output("grant", &grant_outbox)
            .logic(r#"#{ grant: #{ grant_id: claim.grant_id, unit_id: cap.unit_id } }"#);
    });

    // t_register — record the hold. The holder echoes its grant over a PLAIN
    // bridge, so the token (and therefore the `in_use` hold) carries no reply
    // routing; consuming it in t_release/t_reap keeps the recycled capacity clean.
    ctx.transition("t_register", "Register Hold")
        .auto_input("reg", &register_inbox)
        .auto_output("hold", &in_use)
        .logic(r#"#{ hold: #{ grant_id: reg.grant_id, unit_id: reg.unit_id } }"#);

    // t_release — body finished: return capacity, matched to the hold by grant_id.
    ctx.scope("Release", |ctx| {
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

        // t_reap — lease expired (holder crashed): reclaim the capacity,
        // matched to the hold by grant_id.
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

    // Seed the pool with POOL_CAPACITY units.
    for i in 0..POOL_CAPACITY {
        ctx.seed_one(
            &pool,
            Capacity {
                unit_id: format!("gpu-{i}"),
            },
        );
    }
}

fn main() {
    aithericon_sdk::run(
        "resource-pool-net",
        "Shared capacity pool — claim/grant/release/reap on the event-sourced \
         Petri-net substrate. Generalizes the scheduler relay pattern for contended resources.",
        definition,
    );
}
