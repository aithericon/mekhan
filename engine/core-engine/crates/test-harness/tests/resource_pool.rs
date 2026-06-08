//! M1 — In-net resource-pool primitive: conservation + replay-safe reap.
//!
//! This is the engine-only proof for the "resources are places, claims are
//! tokens" thesis (see `docs/14-resource-pool-net-design.md` and the
//! `resource_pool_net` SDK example). A pool of N capacity tokens is contended
//! for by M jobs. The engine's own rules do the work:
//!
//!   * ADMISSION — `t_grant` needs a token in BOTH `jobs` and `pool`; when the
//!     pool is empty it simply isn't enabled. (`find_valid_binding` requires a
//!     token at every input place.)
//!   * MUTEX — at most N capacity tokens exist, so at most N holds can be
//!     active simultaneously. `select_next_transition` fires one transition per
//!     step, so grants never race.
//!   * CONSERVATION — every grant moves one token `pool → in_use` and every
//!     release/reap moves one token `in_use → pool`, so the invariant
//!     `count(pool) + count(in_use) == N` holds after EVERY step.
//!   * REPLAY-SAFE REAP — the lease reaper (`t_reap`) consumes a journaled
//!     `lease_expired(grant_id)` signal, NOT a wall-clock guard. Re-running the
//!     recorded schedule in `ExecutionMode::Replay` yields a byte-identical
//!     event sequence and marking. If `t_reap` instead read `now()`, a replay
//!     at a different instant would diverge — and this test would catch it.
//!
//! The net here is pure-Rhai with an *injectable* `lease_expired` signal, so it
//! runs fully in-process with no Clockmaster. The deployable `resource_pool_net`
//! example arms a real durable timer (`ctx.delay`) that feeds that same signal;
//! M2 exercises the timer against the live engine.

use aithericon_sdk::prelude::*;
use petri_application::ExecutionMode;
use petri_domain::{DomainEvent, Marking, PersistedEvent, PlaceId, TokenColor};
use petri_test_harness::fixtures::{TestContext, TestScenario};

const N_CAPACITY: usize = 2;
const M_JOBS: usize = 5;

// ---------------------------------------------------------------------------
// Net definition (SDK DSL). Pure Rhai, no seeds — the driver seeds explicitly
// so the live and replay runs apply an identical schedule.
// ---------------------------------------------------------------------------

/// Build the in-net pool: pool (capacity), jobs, in_use (held capacity),
/// release_inbox + lease_expired (injectable signals), done (sink).
fn build_pool_net() -> TestScenario {
    let mut ctx = Context::new("resource-pool-m1");

    let pool = ctx.state::<DynamicToken>("pool", "GPU Pool");
    let jobs = ctx.state::<DynamicToken>("jobs", "Pending Jobs");
    let in_use = ctx.state::<DynamicToken>("in_use", "In Use");
    let release_inbox = ctx.signal::<DynamicToken>("release_inbox", "Release Requests");
    let lease_expired = ctx.signal::<DynamicToken>("lease_expired", "Lease Expired");
    let done = ctx.state::<DynamicToken>("done", "Done");

    // t_grant — claim: consume one job + one capacity token, mint a hold.
    // No guard → homogeneous pool, deterministic FIFO binding.
    ctx.transition("t_grant", "Grant Capacity")
        .auto_input("job", &jobs)
        .auto_input("cap", &pool)
        .auto_output("hold", &in_use)
        .logic(r#"#{ hold: #{ grant_id: job.job_id, gpu_id: cap.gpu_id, job_id: job.job_id } }"#);

    // t_release — body finished: return the held capacity to the pool.
    // Correlates the release request to the matching hold by grant_id.
    ctx.transition("t_release", "Release Capacity")
        .auto_input("req", &release_inbox)
        .auto_input("held", &in_use)
        .correlate("req", "held", "grant_id")
        .auto_output("cap", &pool)
        .auto_output("done", &done)
        .logic(
            r#"#{
                cap: #{ gpu_id: held.gpu_id },
                done: #{ grant_id: held.grant_id, gpu_id: held.gpu_id, job_id: held.job_id, outcome: "released" }
            }"#,
        );

    // t_reap — lease expired (holder crashed without releasing): reclaim the
    // capacity. Driven by a journaled signal, never a clock → replay-safe.
    ctx.transition("t_reap", "Reap Expired Lease")
        .auto_input("exp", &lease_expired)
        .auto_input("held", &in_use)
        .correlate("exp", "held", "grant_id")
        .auto_output("cap", &pool)
        .auto_output("done", &done)
        .logic(
            r#"#{
                cap: #{ gpu_id: held.gpu_id },
                done: #{ grant_id: held.grant_id, gpu_id: held.gpu_id, job_id: held.job_id, outcome: "reaped" }
            }"#,
        );

    TestScenario::from_sdk(ctx.build())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

type Ctx = TestContext<
    petri_test_harness::doubles::MockEventRepository,
    petri_test_harness::doubles::MockTopologyRepository,
    petri_test_harness::doubles::MockStateProjection,
>;

async fn new_ctx(scenario: &TestScenario) -> Ctx {
    // The scenario carries no seeds, so build() only initialises the net.
    TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await
}

fn pid(scenario: &TestScenario, name: &str) -> PlaceId {
    scenario
        .places
        .get(name)
        .unwrap_or_else(|| panic!("unknown place {name}"))
        .clone()
}

fn cap(gpu_id: &str) -> TokenColor {
    TokenColor::Data(serde_json::json!({ "gpu_id": gpu_id }))
}

fn job(job_id: &str) -> TokenColor {
    TokenColor::Data(serde_json::json!({ "job_id": job_id }))
}

fn grant_ref(grant_id: &str) -> TokenColor {
    TokenColor::Data(serde_json::json!({ "grant_id": grant_id }))
}

/// `count(pool) + count(in_use)` — the conserved capacity total.
fn capacity_total(marking: &Marking, scenario: &TestScenario) -> usize {
    marking.token_count(&pid(scenario, "pool")) + marking.token_count(&pid(scenario, "in_use"))
}

/// Single-step to quiescence, asserting the conservation invariant after every
/// firing AND that the live in-use count never exceeds N (the mutex).
async fn run_to_quiescent_checking(ctx: &Ctx, scenario: &TestScenario) {
    loop {
        let before = ctx.service.get_events().await.len();
        let result = ctx
            .service
            .evaluate_until_quiescent(1)
            .await
            .expect("evaluate");
        let marking = ctx.service.get_marking().await;

        assert_eq!(
            capacity_total(&marking, scenario),
            N_CAPACITY,
            "capacity conservation violated: pool={} in_use={}",
            marking.token_count(&pid(scenario, "pool")),
            marking.token_count(&pid(scenario, "in_use")),
        );
        assert!(
            marking.token_count(&pid(scenario, "in_use")) <= N_CAPACITY,
            "in_use exceeded pool capacity (mutex violated)"
        );

        let after = ctx.service.get_events().await.len();
        if matches!(
            result.final_state,
            petri_application::EvaluateFinalState::Quiescent
        ) && after == before
        {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Replay digests (mirrors petri-application/src/integration_tests.rs)
// ---------------------------------------------------------------------------

fn token_color_json(color: &TokenColor) -> serde_json::Value {
    match color {
        TokenColor::Data(v) => v.clone(),
        TokenColor::Integer(n) => serde_json::json!(n),
        TokenColor::Unit => serde_json::Value::Null,
    }
}

fn sorted_produced(produced: &[(PlaceId, petri_domain::Token)]) -> String {
    let mut payloads: Vec<String> = produced
        .iter()
        .map(|(_, t)| token_color_json(&t.color).to_string())
        .collect();
    payloads.sort();
    payloads.join("|")
}

/// Variant tag + salient payload per event, in log order. Two runs that fire
/// the same transitions in the same order with the same data are identical.
fn event_digest(events: &[PersistedEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match &e.event {
            DomainEvent::NetInitialized { .. } => "NetInitialized".to_string(),
            DomainEvent::TokenCreated {
                place_id, token, ..
            } => format!(
                "TokenCreated({},{})",
                place_id,
                token_color_json(&token.color)
            ),
            DomainEvent::TransitionFired {
                transition_id,
                produced_tokens,
                consumed_tokens,
                ..
            } => format!(
                "TransitionFired({},consumed={},produced=[{}])",
                transition_id,
                consumed_tokens.len(),
                sorted_produced(produced_tokens)
            ),
            DomainEvent::NetCompleted { .. } => "NetCompleted".to_string(),
            other => format!("{:?}", std::mem::discriminant(other)),
        })
        .collect()
}

fn marking_digest(marking: &Marking, scenario: &TestScenario) -> Vec<(String, Vec<String>)> {
    ["pool", "in_use", "jobs", "done"]
        .iter()
        .map(|name| {
            let p = pid(scenario, name);
            let mut datas: Vec<String> = marking
                .tokens_at(&p)
                .iter()
                .map(|t| token_color_json(&t.color).to_string())
                .collect();
            datas.sort(); // compare the multiset; intra-place order isn't a contract
            (name.to_string(), datas)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Conservation + mutex + full drain: N=2 capacity, M=5 jobs. Drives the whole
/// lifecycle (grants gated by capacity, releases interleaved) and asserts the
/// conserved total holds after every single step, the pool returns to N, and
/// all M jobs reach `done`.
#[tokio::test]
async fn pool_conserves_capacity_and_drains_all_jobs() {
    let scenario = build_pool_net();
    let ctx = new_ctx(&scenario).await;

    // Seed N capacity tokens and M jobs.
    for i in 0..N_CAPACITY {
        ctx.service
            .create_token(pid(&scenario, "pool"), cap(&format!("gpu-{i}")))
            .await
            .unwrap();
    }
    for i in 0..M_JOBS {
        ctx.service
            .create_token(pid(&scenario, "jobs"), job(&format!("job-{i}")))
            .await
            .unwrap();
    }

    // First wave: grants fill the pool (2 holds), then stall — no releases yet.
    run_to_quiescent_checking(&ctx, &scenario).await;
    let marking = ctx.service.get_marking().await;
    assert_eq!(
        marking.token_count(&pid(&scenario, "in_use")),
        N_CAPACITY,
        "exactly N jobs should be holding capacity; the rest queue"
    );
    assert_eq!(
        marking.token_count(&pid(&scenario, "jobs")),
        M_JOBS - N_CAPACITY,
        "the remaining jobs wait — visible backpressure"
    );

    // Release each granted job in turn; every release frees a slot that an
    // unwaiting job immediately claims. Drive to completion.
    for i in 0..M_JOBS {
        ctx.service
            .create_token(
                pid(&scenario, "release_inbox"),
                grant_ref(&format!("job-{i}")),
            )
            .await
            .unwrap();
        run_to_quiescent_checking(&ctx, &scenario).await;
    }

    let marking = ctx.service.get_marking().await;
    assert_eq!(
        marking.token_count(&pid(&scenario, "pool")),
        N_CAPACITY,
        "all capacity returned to the pool"
    );
    assert_eq!(marking.token_count(&pid(&scenario, "in_use")), 0);
    assert_eq!(marking.token_count(&pid(&scenario, "jobs")), 0);
    assert_eq!(
        marking.token_count(&pid(&scenario, "done")),
        M_JOBS,
        "every job completed"
    );
}

/// Crash recovery via lease reap: a holder never releases; injecting the
/// journaled `lease_expired` signal reclaims its capacity, conservation intact.
#[tokio::test]
async fn expired_lease_is_reaped_and_capacity_reclaimed() {
    let scenario = build_pool_net();
    let ctx = new_ctx(&scenario).await;

    for i in 0..N_CAPACITY {
        ctx.service
            .create_token(pid(&scenario, "pool"), cap(&format!("gpu-{i}")))
            .await
            .unwrap();
    }
    // Two jobs, both will be granted (fills the pool).
    for i in 0..2 {
        ctx.service
            .create_token(pid(&scenario, "jobs"), job(&format!("job-{i}")))
            .await
            .unwrap();
    }
    run_to_quiescent_checking(&ctx, &scenario).await;
    assert_eq!(
        ctx.service
            .get_marking()
            .await
            .token_count(&pid(&scenario, "in_use")),
        2
    );

    // job-0's holder "crashes": no release ever arrives. The lease timer would
    // fire in production; here we inject the journaled expiry signal directly.
    ctx.service
        .create_token(pid(&scenario, "lease_expired"), grant_ref("job-0"))
        .await
        .unwrap();
    run_to_quiescent_checking(&ctx, &scenario).await;

    let marking = ctx.service.get_marking().await;
    assert_eq!(
        marking.token_count(&pid(&scenario, "pool")),
        1,
        "reaped capacity is back in the pool"
    );
    assert_eq!(
        marking.token_count(&pid(&scenario, "in_use")),
        1,
        "job-1 still holds"
    );

    // job-1 releases normally; pool fully restored.
    ctx.service
        .create_token(pid(&scenario, "release_inbox"), grant_ref("job-1"))
        .await
        .unwrap();
    run_to_quiescent_checking(&ctx, &scenario).await;
    assert_eq!(
        ctx.service
            .get_marking()
            .await
            .token_count(&pid(&scenario, "pool")),
        N_CAPACITY
    );
}

/// Replay determinism: the same scripted schedule (seeds, grants, a reap, and
/// releases) produces a byte-identical event sequence and final marking whether
/// run live or in `ExecutionMode::Replay`. This locks the invariant that NO
/// transition — especially `t_reap` — depends on wall-clock time.
#[tokio::test]
async fn schedule_is_replay_deterministic() {
    // A single scripted schedule applied to whichever service is passed in.
    async fn drive(ctx: &Ctx, scenario: &TestScenario) {
        for i in 0..N_CAPACITY {
            ctx.service
                .create_token(pid(scenario, "pool"), cap(&format!("gpu-{i}")))
                .await
                .unwrap();
        }
        for i in 0..3 {
            ctx.service
                .create_token(pid(scenario, "jobs"), job(&format!("job-{i}")))
                .await
                .unwrap();
        }
        ctx.service.evaluate_until_quiescent(50).await.unwrap();
        // job-0 reaped (crash), job-1 + job-2 released normally.
        ctx.service
            .create_token(pid(scenario, "lease_expired"), grant_ref("job-0"))
            .await
            .unwrap();
        ctx.service.evaluate_until_quiescent(50).await.unwrap();
        for i in 1..3 {
            ctx.service
                .create_token(
                    pid(scenario, "release_inbox"),
                    grant_ref(&format!("job-{i}")),
                )
                .await
                .unwrap();
            ctx.service.evaluate_until_quiescent(50).await.unwrap();
        }
    }

    // Live run.
    let scenario = build_pool_net();
    let live = new_ctx(&scenario).await;
    drive(&live, &scenario).await;
    let live_events = live.service.get_events().await;
    let live_marking = live.service.get_marking().await;

    // Replay run — fresh service, replay mode, identical schedule.
    let replay = new_ctx(&scenario).await;
    replay.service.set_execution_mode(ExecutionMode::Replay);
    drive(&replay, &scenario).await;
    let replay_events = replay.service.get_events().await;
    let replay_marking = replay.service.get_marking().await;

    assert_eq!(
        event_digest(&live_events),
        event_digest(&replay_events),
        "live and replay must emit an identical event sequence"
    );
    assert_eq!(
        marking_digest(&live_marking, &scenario),
        marking_digest(&replay_marking, &scenario),
        "live and replay must reach a byte-identical final marking"
    );
}

// ===========================================================================
// Offer dispatch (docs/33, P1) — engine-side drive proof.
//
// `Dispatch::Offer` is the pull-mode counterpart of the push-mode pool above.
// In push mode `t_grant` auto-fires the instant a claim AND free capacity are
// both present — the net *pushes* the grant. In OFFER mode the control plane
// (mekhan, off-engine) runs `satisfies(requirements, caps)` ONCE to match a
// unit, then PARKS an offer token in the net reserving that unit. The grant is
// NOT minted yet. Binding waits for a UNIT-INITIATED claim: the matched unit
// posts `presence_claim {grant_id, unit_id}`, which fires `t_claim` to mint the
// grant and consume the offer. First claim wins; any further claim for the same
// grant_id finds no parked offer and no-ops (the offer was implicitly rescinded
// the moment it was consumed). This is the same conservation/reap machinery as
// push mode — only the *binding trigger* differs (parked offer + claim, not an
// auto-firing grant), so reap parity must hold for a held offer-unit too.
//
// The engine workspace cannot import mekhan's `build_offer_net` (separate cargo
// workspace, mekhan emits AIR only). So — exactly as `build_pool_net` above
// replicates the deployable `resource_pool_net` SDK example in the engine's own
// SDK DSL — this builds the offer topology in the SDK directly. The off-engine
// `satisfies` match-once is modelled by the driver: it picks the unit whose caps
// satisfy the requirement and posts an offer reserving that unit_id. What the
// engine net itself proves is the *binding discipline*: parked-offer → no
// auto-bind, claim-driven first-wins, rescind-on-consume, and reap parity.
// ===========================================================================

/// Build the in-net OFFER topology. Pure Rhai, injectable signals — runs fully
/// in-process, mirroring `build_pool_net`.
///
/// Places: `pool` (capacity units tagged with `caps`); `offers` (PARKED offers —
/// grant_id + reserved unit_id + caps; an offer reserves a unit, which leaves
/// `pool` when the offer is posted, and NO transition auto-binds it);
/// `presence_claim` (signal: a unit-initiated claim `{grant_id, unit_id}`);
/// `in_use` (held capacity); `grants` (emitted grants — the "grant channel" sink,
/// what a push-mode `grant_outbox` would carry back); `release_inbox` (signal:
/// `{grant_id}`); `presence_expired` (signal: a unit dropped out `{grant_id}`);
/// `done` (sink for released/reaped/failed records).
///
/// Transitions: `t_claim` (offer + matching claim, correlate grant_id → grant +
/// hold; fires ONLY on a claim; consumes the offer = rescind); `t_release`
/// (release + hold → recycle unit); `t_reap_held` (presence_expired + hold →
/// recycle unit + `fail` record; reap parity with push mode).
fn build_offer_net() -> TestScenario {
    let mut ctx = Context::new("offer-dispatch-m1");

    let pool = ctx.state::<DynamicToken>("pool", "Capacity Pool");
    let offers = ctx.state::<DynamicToken>("offers", "Parked Offers");
    let presence_claim = ctx.signal::<DynamicToken>("presence_claim", "Unit Claims");
    let in_use = ctx.state::<DynamicToken>("in_use", "In Use");
    let grants = ctx.state::<DynamicToken>("grants", "Emitted Grants");
    let release_inbox = ctx.signal::<DynamicToken>("release_inbox", "Release Requests");
    let presence_expired = ctx.signal::<DynamicToken>("presence_expired", "Unit Dropped");
    let done = ctx.state::<DynamicToken>("done", "Done");

    // t_claim — the OFFER bind. Requires a parked offer AND a unit-initiated
    // claim carrying the same grant_id. There is deliberately NO auto-bind
    // transition (no `t_grant` reading pool + offer): an offer just sits parked
    // until its unit claims it. Consuming the offer here implicitly rescinds it
    // for any later claim — first claim wins, deterministically (one transition
    // fires per step). Emits the grant on the grant channel and records the hold.
    ctx.transition("t_claim", "Claim Offer")
        .auto_input("offer", &offers)
        .auto_input("claim", &presence_claim)
        .correlate("offer", "claim", "grant_id")
        .auto_output("grant", &grants)
        .auto_output("hold", &in_use)
        .logic(
            r#"#{
                grant: #{ grant_id: offer.grant_id, unit_id: offer.unit_id, caps: offer.caps },
                hold:  #{ grant_id: offer.grant_id, unit_id: offer.unit_id, caps: offer.caps }
            }"#,
        );

    // t_release — holder finished: return the reserved unit to the pool.
    ctx.transition("t_release", "Release Hold")
        .auto_input("req", &release_inbox)
        .auto_input("held", &in_use)
        .correlate("req", "held", "grant_id")
        .auto_output("cap", &pool)
        .auto_output("done", &done)
        .logic(
            r#"#{
                cap:  #{ unit_id: held.unit_id, caps: held.caps },
                done: #{ grant_id: held.grant_id, unit_id: held.unit_id, outcome: "released" }
            }"#,
        );

    // t_reap_held — the held unit dropped out (presence expired) before
    // releasing: reclaim its capacity, matched by grant_id. This is the OFFER
    // analogue of push-mode `t_reap` — proving reap parity: a held offer-unit is
    // reaped exactly like a held push-mode hold. Emits a `fail` record.
    ctx.transition("t_reap_held", "Reap Held Unit")
        .auto_input("exp", &presence_expired)
        .auto_input("held", &in_use)
        .correlate("exp", "held", "grant_id")
        .auto_output("cap", &pool)
        .auto_output("done", &done)
        .logic(
            r#"#{
                cap:  #{ unit_id: held.unit_id, caps: held.caps },
                done: #{ grant_id: held.grant_id, unit_id: held.unit_id, outcome: "reaped" }
            }"#,
        );

    TestScenario::from_sdk(ctx.build())
}

// --- offer-net helpers -----------------------------------------------------

/// A capacity unit carrying its capabilities (the `caps` the off-engine
/// `satisfies(requirements, caps)` matcher reads).
fn unit(unit_id: &str, caps: serde_json::Value) -> TokenColor {
    TokenColor::Data(serde_json::json!({ "unit_id": unit_id, "caps": caps }))
}

/// The off-engine match-once result, parked as an offer: a grant_id reserving a
/// specific unit (already chosen by `satisfies`) with that unit's caps.
fn offer(grant_id: &str, unit_id: &str, caps: serde_json::Value) -> TokenColor {
    TokenColor::Data(serde_json::json!({ "grant_id": grant_id, "unit_id": unit_id, "caps": caps }))
}

/// A unit-initiated claim against a parked offer.
fn claim(grant_id: &str, unit_id: &str) -> TokenColor {
    TokenColor::Data(serde_json::json!({ "grant_id": grant_id, "unit_id": unit_id }))
}

/// `satisfies(requirements, caps)` — the SAME predicate the push-mode matcher
/// uses, applied here by the driver to pick which unit an offer reserves. A unit
/// satisfies a requirement when it offers every required capability.
fn satisfies(requirements: &[&str], caps: &serde_json::Value) -> bool {
    let have: Vec<&str> = caps
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    requirements.iter().all(|r| have.contains(r))
}

/// Drive one tick set to quiescence and return how many events were appended.
async fn run_to_quiescent(ctx: &Ctx) -> usize {
    loop {
        let before = ctx.service.get_events().await.len();
        let result = ctx
            .service
            .evaluate_until_quiescent(1)
            .await
            .expect("evaluate");
        let after = ctx.service.get_events().await.len();
        if matches!(
            result.final_state,
            petri_application::EvaluateFinalState::Quiescent
        ) && after == before
        {
            return after;
        }
    }
}

/// Match-once + post-offer: run `satisfies` over the pool, pick the first unit
/// that matches, REMOVE it from the pool (reserve it), and park an offer for it.
/// Returns the reserved unit_id. Panics if nothing matches (caller's invariant).
async fn post_offer(
    ctx: &Ctx,
    scenario: &TestScenario,
    grant_id: &str,
    requirements: &[&str],
) -> String {
    let marking = ctx.service.get_marking().await;
    let pool_place = pid(scenario, "pool");
    let (reserved_unit, reserved_caps) = marking
        .tokens_at(&pool_place)
        .iter()
        .find_map(|t| {
            let v = token_color_json(&t.color);
            let caps = v.get("caps").cloned().unwrap_or(serde_json::Value::Null);
            if satisfies(requirements, &caps) {
                Some((
                    v.get("unit_id")
                        .and_then(|u| u.as_str())
                        .unwrap()
                        .to_string(),
                    caps,
                ))
            } else {
                None
            }
        })
        .expect("satisfies found no matching unit in the pool");

    // Reserve: remove the matched unit token from the pool.
    let units = marking.tokens_at(&pool_place);
    let token_id = units
        .iter()
        .find(|t| {
            token_color_json(&t.color)
                .get("unit_id")
                .and_then(|u| u.as_str())
                == Some(reserved_unit.as_str())
        })
        .map(|t| t.id.clone())
        .expect("reserved unit token present");
    ctx.service
        .remove_token(pool_place.clone(), Some(token_id), None, None)
        .await
        .expect("reserve unit out of pool");

    // Park the offer (the match-once result). NO grant minted yet.
    ctx.service
        .create_token(
            pid(scenario, "offers"),
            offer(grant_id, &reserved_unit, reserved_caps),
        )
        .await
        .expect("park offer");

    reserved_unit
}

// --- offer-net tests -------------------------------------------------------

/// The core offer/claim discipline:
///   1. post an offer  → it PARKS, no grant emitted (no auto-bind);
///   2. a unit claim   → `t_claim` fires: grant emitted, offer consumed;
///   3. a SECOND claim → no-op (offer already rescinded; deterministic 1st-wins);
///   4. release        → the reserved unit recycles back to the pool.
#[tokio::test]
async fn offer_parks_then_binds_on_unit_claim_first_wins() {
    let scenario = build_offer_net();
    let ctx = new_ctx(&scenario).await;

    // A pool of two units with distinct capabilities.
    ctx.service
        .create_token(
            pid(&scenario, "pool"),
            unit("unit-a", serde_json::json!(["gpu", "cuda"])),
        )
        .await
        .unwrap();
    ctx.service
        .create_token(
            pid(&scenario, "pool"),
            unit("unit-b", serde_json::json!(["cpu"])),
        )
        .await
        .unwrap();

    // (1) Post an offer requiring "gpu" — satisfies() matches unit-a only.
    let reserved = post_offer(&ctx, &scenario, "g1", &["gpu"]).await;
    assert_eq!(reserved, "unit-a", "satisfies matched the gpu unit");

    run_to_quiescent(&ctx).await;
    let m = ctx.service.get_marking().await;
    assert_eq!(
        m.token_count(&pid(&scenario, "offers")),
        1,
        "offer PARKS — no unit-initiated claim yet"
    );
    assert_eq!(
        m.token_count(&pid(&scenario, "grants")),
        0,
        "NO grant minted: offer mode does not auto-bind"
    );
    assert_eq!(
        m.token_count(&pid(&scenario, "in_use")),
        0,
        "nothing held until the unit claims"
    );
    // The reserved unit is out of the pool; the non-matching unit remains.
    assert_eq!(m.token_count(&pid(&scenario, "pool")), 1);

    // (2) The matched unit claims its offer.
    ctx.service
        .create_token(pid(&scenario, "presence_claim"), claim("g1", "unit-a"))
        .await
        .unwrap();
    run_to_quiescent(&ctx).await;

    let m = ctx.service.get_marking().await;
    assert_eq!(
        m.token_count(&pid(&scenario, "grants")),
        1,
        "claim bound the offer → exactly one grant emitted"
    );
    assert_eq!(
        m.token_count(&pid(&scenario, "in_use")),
        1,
        "the unit now holds capacity"
    );
    assert_eq!(
        m.token_count(&pid(&scenario, "offers")),
        0,
        "the offer was consumed (rescinded) on bind"
    );
    let granted = m.tokens_at(&pid(&scenario, "grants"));
    let gv = token_color_json(&granted[0].color);
    assert_eq!(gv["grant_id"], "g1");
    assert_eq!(gv["unit_id"], "unit-a");

    // (3) A SECOND claim for the same grant_id — the offer is gone, so there is
    // nothing for t_claim to bind against: deterministic first-wins, the late
    // claim no-ops. (It sits unconsumed in the signal place, harmlessly.)
    let grants_before = m.token_count(&pid(&scenario, "grants"));
    let in_use_before = m.token_count(&pid(&scenario, "in_use"));
    ctx.service
        .create_token(pid(&scenario, "presence_claim"), claim("g1", "unit-a"))
        .await
        .unwrap();
    run_to_quiescent(&ctx).await;

    let m = ctx.service.get_marking().await;
    assert_eq!(
        m.token_count(&pid(&scenario, "grants")),
        grants_before,
        "no SECOND grant — first claim already won, offer rescinded"
    );
    assert_eq!(
        m.token_count(&pid(&scenario, "in_use")),
        in_use_before,
        "no second hold minted"
    );

    // (4) Release: the reserved unit recycles back to the pool.
    ctx.service
        .create_token(pid(&scenario, "release_inbox"), grant_ref("g1"))
        .await
        .unwrap();
    run_to_quiescent(&ctx).await;

    let m = ctx.service.get_marking().await;
    assert_eq!(m.token_count(&pid(&scenario, "in_use")), 0, "hold released");
    assert_eq!(
        m.token_count(&pid(&scenario, "pool")),
        2,
        "the reserved unit is recycled — pool whole again"
    );
    let dones = m.tokens_at(&pid(&scenario, "done"));
    assert_eq!(dones.len(), 1);
    assert_eq!(token_color_json(&dones[0].color)["outcome"], "released");
}

/// Reap parity: a HELD offer-unit whose holder drops out (presence_expired) is
/// reclaimed by `t_reap_held`, emitting a `fail`-style record — exactly as
/// push-mode `t_reap` reclaims a crashed hold. Proves the offer hold is reapable
/// on the same journaled-signal discipline (no wall clock).
#[tokio::test]
async fn held_offer_unit_is_reaped_on_presence_expired() {
    let scenario = build_offer_net();
    let ctx = new_ctx(&scenario).await;

    ctx.service
        .create_token(
            pid(&scenario, "pool"),
            unit("unit-a", serde_json::json!(["gpu"])),
        )
        .await
        .unwrap();

    // Post an offer and let the unit claim it → it is now HELD.
    post_offer(&ctx, &scenario, "g9", &["gpu"]).await;
    run_to_quiescent(&ctx).await;
    ctx.service
        .create_token(pid(&scenario, "presence_claim"), claim("g9", "unit-a"))
        .await
        .unwrap();
    run_to_quiescent(&ctx).await;
    assert_eq!(
        ctx.service
            .get_marking()
            .await
            .token_count(&pid(&scenario, "in_use")),
        1,
        "unit-a holds capacity before it drops out"
    );

    // The holding unit drops out without releasing — presence_expired fires the
    // reap. No release ever arrives.
    ctx.service
        .create_token(pid(&scenario, "presence_expired"), grant_ref("g9"))
        .await
        .unwrap();
    run_to_quiescent(&ctx).await;

    let m = ctx.service.get_marking().await;
    assert_eq!(
        m.token_count(&pid(&scenario, "in_use")),
        0,
        "the held unit was reaped"
    );
    assert_eq!(
        m.token_count(&pid(&scenario, "pool")),
        1,
        "reaped capacity returned to the pool — reap parity with push mode"
    );
    let dones = m.tokens_at(&pid(&scenario, "done"));
    assert_eq!(dones.len(), 1);
    assert_eq!(
        token_color_json(&dones[0].color)["outcome"],
        "reaped",
        "reap emits the fail/reaped record"
    );
}
