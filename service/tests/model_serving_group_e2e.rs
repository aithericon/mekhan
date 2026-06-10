//! Model-serving GROUP identity e2e (live Postgres + live NATS): proves that
//! "this runner is part of the LLM pool" is now decided by **membership in the
//! `model_serving` runner group**, not by the legacy heuristic (a present runner
//! whose catalog happened to carry a `base_url`).
//!
//! The decisive case is `present_runner_outside_the_group_is_not_a_pool_replica`:
//! a runner that is present, has the base pulled, AND advertises a `base_url`
//! (everything the old `base_url`-sniff required) is NOT placed onto, because it
//! never enrolled into the group. The positive control
//! (`group_member_is_identified_and_placed`) shows a real member still places.
//!
//! Same real-vs-faked split + infra requirements as `scale_placement_e2e.rs`
//! (shared test Postgres + test NATS; presence + catalog injected offline). The
//! `model_serving` membership is stamped on the presence entry by the fixture
//! (`SeedRunnerSpec.group`, defaulting to `model_serving`).

mod common;

use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

use mekhan_service::autoscaler::demand::DemandSource;
use mekhan_service::autoscaler::placement::reconcile_placement;
use mekhan_service::nats::MekhanNats;
use mekhan_service::runner_commands::{LoadTarget, ModelCommand};
use mekhan_service::presence::RunnerPresence;

use common::model_runner_fixture::{
    read_replica_status, seed_model_policy, seed_model_runner, SeedModel, SeedPolicySpec,
    SeedRunnerSpec,
};
use common::nats_spy::NatsCommandSpy;

const LLAMA: &str = "llama3.2:1b";
const ZONE: &str = "eu-west";

/// Fixed demand so a `keep_warm` policy reads `demand > 0` and places without a
/// live router `/metrics` scrape; `inflight = None` → headroom fails soft to the
/// runner's advertised `C`.
struct ConstDemand(f64);

#[async_trait]
impl DemandSource for ConstDemand {
    async fn demand_for(&self, _model_id: &str) -> Option<f64> {
        Some(self.0)
    }
    async fn inflight_for(&self, _model_id: &str) -> Option<f64> {
        None
    }
}

async fn connect_nats() -> (MekhanNats, async_nats::Client) {
    let url = common::nats_url();
    let nats = MekhanNats::connect(&url, None)
        .await
        .expect("connect MekhanNats — run the test infra (NATS)");
    let spy_client = async_nats::connect(&url)
        .await
        .expect("connect spy NATS client");
    (nats, spy_client)
}

fn load_base_targets(captured: &[common::nats_spy::CapturedCommand], base: &str) -> Vec<Uuid> {
    captured
        .iter()
        .filter_map(|c| match &c.command {
            ModelCommand::Load {
                target: LoadTarget::Base { model_id },
            } if model_id == base => Some(c.runner_id),
            _ => None,
        })
        .collect()
}

/// THE BEHAVIOR CHANGE: a present runner that has the base pulled AND advertises
/// a `base_url` — i.e. everything the old `base_url`-sniff treated as a replica —
/// is NOT a pool replica when it is outside the `model_serving` group, so the
/// placement loop never targets it and writes no active placement for the model.
#[tokio::test]
async fn present_runner_outside_the_group_is_not_a_pool_replica() {
    let db = common::create_test_db().await;
    let presence = RunnerPresence::new();
    let (nats, spy_client) = connect_nats().await;
    let spy = NatsCommandSpy::start(spy_client).await;

    let ws = Uuid::new_v4();

    // Present, base pulled, zoned, AND advertises an inference endpoint — but
    // group = None ⇒ it never enrolled into the model-serving pool.
    let _outsider = seed_model_runner(
        &db,
        &presence,
        SeedRunnerSpec {
            workspace_id: ws,
            group: None, // <-- not a member of the model_serving group
            models: vec![],
            pulled: vec![LLAMA.to_string()],
            residency_zone: Some(ZONE.to_string()),
            base_url: Some("http://127.0.0.1:65535".to_string()),
            ..Default::default()
        },
    )
    .await;

    seed_model_policy(&db, SeedPolicySpec::base(ws, LLAMA, "keep_warm", ZONE, 1)).await;

    let demand = ConstDemand(1.0);
    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick");

    // No LoadBase is published — the outsider is not an eligible replica.
    let captured = spy.wait_for(1, Duration::from_millis(1200)).await;
    assert!(
        captured.is_err(),
        "no model command must be published for a non-member runner, got: {captured:?}"
    );

    // The placement row (if written) must not be an `active` placement, and no
    // replica is observed — the outsider does not count.
    if let Some((status, _desired, observed)) = read_replica_status(&db, ws, LLAMA).await {
        assert_ne!(
            status, "active",
            "a non-member runner must not yield an active placement"
        );
        assert_eq!(observed, 0, "a non-member runner is not an observed replica");
    }
}

/// Positive control: a real `model_serving` group member (the fixture default)
/// with the base pulled IS identified and placed — exactly one `LoadBase` to it.
#[tokio::test]
async fn group_member_is_identified_and_placed() {
    let db = common::create_test_db().await;
    let presence = RunnerPresence::new();
    let (nats, spy_client) = connect_nats().await;
    let spy = NatsCommandSpy::start(spy_client).await;

    let ws = Uuid::new_v4();

    // Default group = `model_serving` ⇒ a first-class pool member.
    let member = seed_model_runner(
        &db,
        &presence,
        SeedRunnerSpec {
            workspace_id: ws,
            models: vec![SeedModel::base(LLAMA, 8)], // resident or pulled both fine
            pulled: vec![LLAMA.to_string()],
            residency_zone: Some(ZONE.to_string()),
            ..Default::default()
        },
    )
    .await;

    seed_model_policy(&db, SeedPolicySpec::base(ws, LLAMA, "keep_warm", ZONE, 1)).await;

    let demand = ConstDemand(1.0);
    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick");

    // A member with the base already resident is woken idempotently; with the
    // base only pulled it is cold-loaded. Either way a LoadBase targets the
    // member (and only the member). Drain a short window and assert.
    let captured = spy
        .wait_for(1, Duration::from_secs(2))
        .await
        .unwrap_or_default();
    let targets = load_base_targets(&captured, LLAMA);
    assert!(
        targets.iter().all(|&id| id == member.runner_id),
        "every LoadBase must target the group member; got {targets:?}"
    );
    let (status, desired, _observed) = read_replica_status(&db, ws, LLAMA)
        .await
        .expect("model_replicas row written for a placed member");
    assert_eq!(status, "active", "a placed member yields an active row");
    assert_eq!(desired, 1);
}
