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
        .logic(
            r#"#{ hold: #{ grant_id: job.job_id, gpu_id: cap.gpu_id, job_id: job.job_id } }"#,
        );

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
            .create_token(pid(&scenario, "release_inbox"), grant_ref(&format!("job-{i}")))
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
        ctx.service.get_marking().await.token_count(&pid(&scenario, "in_use")),
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
    assert_eq!(marking.token_count(&pid(&scenario, "in_use")), 1, "job-1 still holds");

    // job-1 releases normally; pool fully restored.
    ctx.service
        .create_token(pid(&scenario, "release_inbox"), grant_ref("job-1"))
        .await
        .unwrap();
    run_to_quiescent_checking(&ctx, &scenario).await;
    assert_eq!(
        ctx.service.get_marking().await.token_count(&pid(&scenario, "pool")),
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
                .create_token(pid(scenario, "release_inbox"), grant_ref(&format!("job-{i}")))
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
