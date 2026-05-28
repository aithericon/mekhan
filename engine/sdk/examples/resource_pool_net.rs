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
//!                                              + [in_use]   (held, tagged grant_id)
//!                                              + [lease_arm]
//!
//! [lease_arm] ─(ctx.delay LEASE_MS)─▶ [lease_scheduled] ──(timer fires)──▶ [lease_expired]
//!
//! [release_inbox: bridge_in] + [in_use]   ─(t_release, correlate grant_id)─▶ [pool] + [done]
//! [lease_expired: signal]    + [in_use]   ─(t_reap,    correlate grant_id)─▶ [pool] + [done]
//! ```
//!
//! `lease_expired` is a plain signal place here: reaping is driven by a
//! journaled signal token, never a wall clock — which is exactly what keeps it
//! replay-safe. In M2 a durable lease timer (`ctx.delay`) armed at grant time
//! feeds that signal in production; the M1 verification of the pure semantics
//! (conservation, mutex, crash-reap, replay determinism) lives in
//! `core-engine/crates/test-harness/tests/resource_pool.rs`.
//!
//! Deployed standalone this net seeds the pool with POOL_CAPACITY units and is
//! driven interactively: bridge claims into `claim_inbox`, then inject
//! `release_inbox` / `lease_expired` signals (`aithericon inject ...`) to free
//! capacity. M2 wires real instance nets to `claim_inbox` / `release_inbox`
//! over the cross-net bridge so multiple instances genuinely contend.
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
    gpu_id: String,
}

/// A claim request arriving from an instance net. `grant_id` correlates the
/// later release/reap back to the right hold; the instance mints it and echoes
/// it on release.
#[token]
struct ClaimRequest {
    grant_id: String,
}

/// The grant returned to the requesting instance — which unit it got.
#[token]
struct Grant {
    grant_id: String,
    gpu_id: String,
}

/// Held capacity, tagged with who holds it.
#[token]
struct Hold {
    grant_id: String,
    gpu_id: String,
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
    gpu_id: String,
    outcome: String,
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // Shared capacity. Seeded with POOL_CAPACITY distinct units.
    let pool = ctx.state::<Capacity>("pool", "GPU Pool");
    let in_use = ctx.state::<Hold>("in_use", "In Use");
    let done = ctx.state::<Freed>("done", "Freed Units");

    // Cross-net inboxes: instance nets bridge claim/release requests here.
    let claim_inbox = ctx.bridge_in::<ClaimRequest>("claim_inbox", "Claim Inbox");
    let release_inbox = ctx.bridge_in::<ReleaseRequest>("release_inbox", "Release Inbox");

    // Grant reply channel: routes the grant back to the requesting instance via
    // the "grant" channel embedded in the claim token's reply metadata.
    let grant_outbox = ctx.bridge_reply_channel::<Grant>("grant_outbox", "Grant Outbox", "grant");

    // Lease expiry signal. A journaled token here — injected externally in M1,
    // produced by a durable lease timer in M2 — drives crash reclamation.
    let lease_expired = ctx.signal::<ReleaseRequest>("lease_expired", "Lease Expired");

    // t_grant — claim admission. Fires only when a claim AND free capacity are
    // both present; otherwise the claim waits in `claim_inbox` (backpressure).
    ctx.scope("Grant", |ctx| {
        ctx.transition("t_grant", "Grant Capacity")
            .auto_input("claim", &claim_inbox)
            .auto_input("cap", &pool)
            .auto_output("grant", &grant_outbox)
            .auto_output("hold", &in_use)
            .logic(
                r#"#{
                    grant: #{ grant_id: claim.grant_id, gpu_id: cap.gpu_id },
                    hold:  #{ grant_id: claim.grant_id, gpu_id: cap.gpu_id }
                }"#,
            );
    });

    // t_release — body finished: return capacity, matched to the hold by
    // grant_id. Fire-and-forget from the instance's perspective.
    ctx.scope("Release", |ctx| {
        ctx.transition("t_release", "Release Capacity")
            .auto_input("req", &release_inbox)
            .auto_input("held", &in_use)
            .correlate("req", "held", "grant_id")
            .auto_output("cap", &pool)
            .auto_output("done", &done)
            .logic(
                r#"#{
                    cap:  #{ gpu_id: held.gpu_id },
                    done: #{ grant_id: held.grant_id, gpu_id: held.gpu_id, outcome: "released" }
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
                    cap:  #{ gpu_id: held.gpu_id },
                    done: #{ grant_id: held.grant_id, gpu_id: held.gpu_id, outcome: "reaped" }
                }"#,
            );
    });

    // Seed the pool with POOL_CAPACITY units.
    for i in 0..POOL_CAPACITY {
        ctx.seed_one(
            &pool,
            Capacity {
                gpu_id: format!("gpu-{i}"),
            },
        );
    }
}

fn main() {
    aithericon_sdk::run(
        "resource-pool-net",
        "Shared capacity pool — claim/grant/release/reap on the event-sourced \
         Petri-net substrate. Generalizes scheduler-net for contended resources.",
        definition,
    );
}
