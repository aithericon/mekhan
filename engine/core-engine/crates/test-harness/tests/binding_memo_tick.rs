//! P1 increment 2 measurement — per-tick re-payment of a *waiting* guarded join.
//!
//! The equi-join index (increment 1) collapses `==`-correlated guards, but a
//! non-equi guard (the stand-in here for the presence-pool
//! `satisfies(claim, caps)` grant) still pays the full `m^k` cross-product.
//! The live engine re-runs the eval loop on every incoming event, so a join
//! that is *examined but never enabled* re-pays that whole scan on every tick.
//!
//! This drives exactly that shape at L1: a waiting `m^2` join (`t_wait`, guard
//! always false) sits next to a self-loop (`t_churn`) that fires once per
//! `evaluate_until_quiescent(1)` call — mirroring the live "one eval per
//! incoming event" cadence. `t_churn` only ever touches its own place, so it
//! never invalidates `t_wait`'s input places. With the negative-binding memo,
//! `t_wait`'s scan is paid once and then skipped on every later tick; without
//! it, every tick re-pays `m^2`.
//!
//! Run with `cargo test -p petri-test-harness --test binding_memo_tick -- --ignored --nocapture`.
//!
//! Measured (debug build, m=150 → 22,500 combos/scan): the cold tick pays the
//! scan once (~650 ms either way), but the **warm** per-tick cost is
//! **254 µs with the memo vs 624,110 µs without** (~2,460×), and the gap grows
//! with m^2 while the memo stays flat — the waiting join's cost is decoupled
//! from the event/tick rate.

use aithericon_sdk::prelude::*;
use petri_domain::{PlaceId, TokenColor};
use petri_test_harness::fixtures::{TestContext, TestScenario};

const M: usize = 150; // tokens at each of the join's two input places
const TICKS: usize = 12; // unrelated eval ticks driven past the waiting join

type Ctx = TestContext<
    petri_test_harness::doubles::MockEventRepository,
    petri_test_harness::doubles::MockTopologyRepository,
    petri_test_harness::doubles::MockStateProjection,
>;

fn pid(scenario: &TestScenario, name: &str) -> PlaceId {
    scenario.places.get(name).expect("place").clone()
}

fn build_waiting_join_net() -> TestScenario {
    let mut ctx = Context::new("binding-memo-tick");

    let a = ctx.state::<DynamicToken>("a", "A");
    let b = ctx.state::<DynamicToken>("b", "B");
    let sink = ctx.state::<DynamicToken>("sink", "Sink");
    let churn = ctx.state::<DynamicToken>("churn", "Churn");

    // The waiting join: needs a token from BOTH a and b, with a non-equi guard
    // that is always false (k > k' AND k' > k is unsatisfiable). The equi-join
    // index extracts nothing here, so each evaluation pays the full m^2 scan and
    // never binds — the join sits "waiting" forever.
    ctx.transition("t_wait", "Waiting Join")
        .auto_input("x", &a)
        .auto_input("y", &b)
        .guard("x.k > y.k && y.k > x.k")
        .auto_output("out", &sink)
        .logic(r#"#{ out: #{ k: x.k } }"#);

    // The churn self-loop: fires once per eval tick, touching only `churn`, so
    // it never invalidates the join's input places (a, b).
    ctx.transition("t_churn", "Churn")
        .auto_input("c", &churn)
        .auto_output("c2", &churn)
        .logic(r#"#{ c2: #{ n: c.n + 1 } }"#);

    TestScenario::from_sdk(ctx.build())
}

#[tokio::test]
#[ignore = "perf measurement, run manually with --ignored --nocapture"]
async fn waiting_join_cost_is_decoupled_from_tick_rate() {
    let scenario = build_waiting_join_net();
    let ctx: Ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    // Seed M tokens at each join input — disjoint key ranges so no pair could
    // ever satisfy even an equality guard; the always-false guard fails them all.
    for i in 0..M {
        ctx.service
            .create_token(
                pid(&scenario, "a"),
                TokenColor::Data(serde_json::json!({ "k": i })),
            )
            .await
            .unwrap();
        ctx.service
            .create_token(
                pid(&scenario, "b"),
                TokenColor::Data(serde_json::json!({ "k": M + i })),
            )
            .await
            .unwrap();
    }
    // One churn token to drive the ticks.
    ctx.service
        .create_token(
            pid(&scenario, "churn"),
            TokenColor::Data(serde_json::json!({ "n": 0 })),
        )
        .await
        .unwrap();

    // Cold tick: the join's m^2 scan is paid here (the memo records the verdict).
    let cold_start = std::time::Instant::now();
    ctx.service.evaluate_until_quiescent(1).await.unwrap();
    let cold = cold_start.elapsed();

    // Warm ticks: the join stays "waiting" and its inputs never change, so a
    // churn tick touches only `churn`. WITH the memo the join is skipped here;
    // WITHOUT it every one of these re-pays the full m^2 scan.
    let warm_start = std::time::Instant::now();
    for _ in 1..TICKS {
        ctx.service.evaluate_until_quiescent(1).await.unwrap();
    }
    let warm = warm_start.elapsed();

    // Correctness: the join never fired; its inputs are untouched; churn looped.
    let marking = ctx.service.get_marking().await;
    assert_eq!(marking.token_count(&pid(&scenario, "a")), M, "a untouched");
    assert_eq!(marking.token_count(&pid(&scenario, "b")), M, "b untouched");
    assert_eq!(
        marking.token_count(&pid(&scenario, "sink")),
        0,
        "join never fired"
    );
    assert_eq!(
        marking.token_count(&pid(&scenario, "churn")),
        1,
        "churn conserved"
    );

    let warm_per_tick_us = warm.as_micros() as f64 / (TICKS - 1) as f64;
    println!(
        "waiting-join m={M} arity=2 ({} combos/scan): cold tick {:?}, warm {:.1} µs/tick over {} ticks",
        M * M,
        cold,
        warm_per_tick_us,
        TICKS - 1,
    );
}
