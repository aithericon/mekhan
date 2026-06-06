//! Failure-path finalizer drain: a net that holds a resource and then FAILS
//! permanently must still release that resource before it is torn down.
//!
//! This is the engine-only proof for the "leased net that fails permanently
//! never strands its lease" fix. A `LeaseScope` holds one capacity unit across
//! its interior and releases it via `t_<id>_exit` — but that exit is gated on
//! the body's SUCCESS token, so when any interior step throws permanently the
//! exit can never fire and the single held token would sit in the pool's
//! `in_use` forever (event-sourced → survives restart → strands the runner).
//!
//! The fix is a **finalizer** transition (`Transition::finalizer == true`):
//! never selected during normal evaluation, fired ONLY during the engine's
//! post-failure drain (`evaluate_until_quiescent`'s permanent-error arm). It
//! consumes the still-parked held token and emits the release. These tests
//! drive the WHOLE mechanism through the real service:
//!
//!   * FAILURE PATH — a body step throws; the net is reported failed AND the
//!     finalizer fired, so the held token moved to the release sink (exactly
//!     once). This is the strand that previously leaked.
//!   * SUCCESS / NO-FIRE — with no failure, the finalizer is invisible even
//!     though its input (the held token) is continuously available, so it never
//!     steals the lease mid-run.

use aithericon_sdk::prelude::*;
use petri_domain::{Marking, PlaceId, TokenColor};
use petri_test_harness::fixtures::{TestContext, TestScenario};

type Ctx = TestContext<
    petri_test_harness::doubles::MockEventRepository,
    petri_test_harness::doubles::MockTopologyRepository,
    petri_test_harness::doubles::MockStateProjection,
>;

async fn new_ctx(scenario: &TestScenario) -> Ctx {
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

fn data(v: serde_json::Value) -> TokenColor {
    TokenColor::Data(v)
}

fn release_grant_ids(marking: &Marking, scenario: &TestScenario) -> Vec<String> {
    marking
        .tokens_at(&pid(scenario, "release"))
        .iter()
        .filter_map(|t| match &t.color {
            TokenColor::Data(v) => v.get("grant_id").and_then(|g| g.as_str()).map(String::from),
            _ => None,
        })
        .collect()
}

/// A minimal lease-shaped net: a `held` token (the lease) parked across the
/// interior, a `body` token the work runs on, a `t_exit` that releases on body
/// SUCCESS, and a `t_finally` FINALIZER that releases on failure. `t_boom`
/// stands in for a body step that throws permanently.
fn build_leased_net(body_throws: bool) -> TestScenario {
    let mut ctx = Context::new("finalizer-drain-test");

    let held = ctx.state::<DynamicToken>("held", "Held Lease");
    let body = ctx.state::<DynamicToken>("body", "Body Token");
    let body_out = ctx.state::<DynamicToken>("body_out", "Body Done");
    let release = ctx.state::<DynamicToken>("release", "Release Out (pool inbox stand-in)");
    let out = ctx.state::<DynamicToken>("out", "Scope Output");

    // The body step. When `body_throws`, it fails permanently (the strand
    // trigger); otherwise it forwards the token to `body_out`.
    let body_step = ctx.transition("t_body", "Body Step").auto_input("body", &body);
    if body_throws {
        body_step
            .logic_rhai(r#"throw "body step failed permanently""#)
            .done();
    } else {
        body_step
            .auto_output("body_out", &body_out)
            .logic_rhai(r#"#{ body_out: body }"#)
            .done();
    }

    // Success-path release: consumes the body-completion token AND the held
    // lease, emits the release. Gated on body success (the body_out token),
    // exactly like a LeaseScope's `t_<id>_exit`.
    ctx.transition("t_exit", "Exit (release on success)")
        .auto_input("input", &body_out)
        .auto_input("held", &held)
        .auto_output("out", &out)
        .auto_output("release", &release)
        .logic_rhai(r#"#{ out: input, release: #{ grant_id: held.grant_id } }"#)
        .done();

    // FINALIZER: failure-path release. Consumes the SAME single held token and
    // emits the release. Marked `.finalizer()` so it is never selected in
    // normal evaluation (the held token is available the whole run) — only the
    // post-failure drain fires it.
    ctx.transition("t_finally", "Release on failure")
        .auto_input("held", &held)
        .auto_output("release", &release)
        .finalizer()
        .logic_rhai(r#"#{ release: #{ grant_id: held.grant_id } }"#)
        .done();

    TestScenario::from_sdk(ctx.build())
}

/// FAILURE PATH: the body throws → the net is failed, and the finalizer drain
/// releases the held lease exactly once (the strand that used to leak forever).
#[tokio::test]
async fn permanently_failed_leased_net_releases_via_finalizer() {
    let scenario = build_leased_net(/* body_throws = */ true);
    let ctx = new_ctx(&scenario).await;

    ctx.service
        .create_token(pid(&scenario, "held"), data(serde_json::json!({ "grant_id": "g1" })))
        .await
        .unwrap();
    ctx.service
        .create_token(pid(&scenario, "body"), data(serde_json::json!({ "x": 1 })))
        .await
        .unwrap();

    let result = ctx.service.evaluate_until_quiescent(50).await.expect("evaluate");

    // The net failed permanently on the throwing body step.
    assert!(
        result.failure_reached.is_some(),
        "the throwing body step must permanently fail the net"
    );

    let marking = ctx.service.get_marking().await;

    // The lease was RELEASED on the failure path: the held token is gone and a
    // single release carrying its grant_id landed on the pool-inbox stand-in.
    assert_eq!(
        marking.token_count(&pid(&scenario, "held")),
        0,
        "the held lease must be consumed by the finalizer on failure (not stranded)"
    );
    assert_eq!(
        release_grant_ids(&marking, &scenario),
        vec!["g1".to_string()],
        "exactly one release carrying the held grant_id must be emitted on failure"
    );
}

/// SUCCESS / NO-FIRE: with no failure the finalizer is never selected even
/// though its input (the held token) is continuously enabled — the normal exit
/// releases the lease, and the finalizer must not double-release or fire early.
#[tokio::test]
async fn finalizer_does_not_fire_without_a_failure() {
    let scenario = build_leased_net(/* body_throws = */ false);
    let ctx = new_ctx(&scenario).await;

    ctx.service
        .create_token(pid(&scenario, "held"), data(serde_json::json!({ "grant_id": "g2" })))
        .await
        .unwrap();
    ctx.service
        .create_token(pid(&scenario, "body"), data(serde_json::json!({ "x": 1 })))
        .await
        .unwrap();

    let result = ctx.service.evaluate_until_quiescent(50).await.expect("evaluate");
    assert!(
        result.failure_reached.is_none(),
        "the success path must not fail the net"
    );

    let marking = ctx.service.get_marking().await;

    // Released exactly once — by `t_exit`, not the finalizer — and not twice.
    assert_eq!(
        marking.token_count(&pid(&scenario, "held")),
        0,
        "the success-path exit consumes the held lease"
    );
    assert_eq!(
        release_grant_ids(&marking, &scenario),
        vec!["g2".to_string()],
        "exactly one release on success — the finalizer must NOT also fire"
    );
    assert_eq!(
        marking.token_count(&pid(&scenario, "out")),
        1,
        "the scope output is produced on the success path"
    );
}
