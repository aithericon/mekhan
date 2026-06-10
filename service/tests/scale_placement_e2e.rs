//! Placement scaling e2e (live Postgres + live NATS): drives the REAL
//! [`mekhan_service::autoscaler::placement::reconcile_placement`] against a seeded
//! `model_states` policy + seeded runner interface catalog(s) + an in-memory
//! presence snapshot, asserting the published NATS load commands (via the
//! [`common::nats_spy`] spy) and the `model_replicas` reconciliation row.
//!
//! ## What's real vs. faked
//!
//! - **Real**: `reconcile_placement` (the whole tick: policy scan →
//!   `serving_runner_catalogs` zone inventory → `plan_placements` →
//!   `apply_plan`/`publish_model_command` → `upsert_status` → `apply_load_timing`),
//!   the Postgres `model_states`/`runner_interfaces`/`model_replicas` round-trips,
//!   and the CORE-NATS `runner.{id}.load` publish/subscribe.
//! - **Faked (offline seams)**: presence is injected via
//!   `RunnerPresence::inject_present_for_test` (no heartbeat); the runner interface
//!   catalog is written directly into `runner_interfaces` (no live node-agent); and
//!   demand is a tiny in-process [`ConstDemand`] `DemandSource` so a `keep_warm`
//!   policy reads `demand > 0` without a live router `/metrics` scrape.
//!
//! ## Infra
//!
//! Needs the shared test Postgres + the test NATS (the same broker
//! `common::test_app` `.expect()`s). It does NOT need a full `just dev` stack
//! (engine / executor / router) — every external the reconciler touches is seeded
//! or faked above. Infra-gated implicitly via `create_test_db()` / `MekhanNats`
//! (which panic with a "run the test infra" message), the same convention as the
//! neighbouring `model_agent_catalog_e2e.rs`.

mod common;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use sqlx::PgPool;
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
use common::nats_spy::{CapturedCommand, NatsCommandSpy};

// ── tiny test models we standardize on ───────────────────────────────────────
const LLAMA: &str = "llama3.2:1b";
const ZONE: &str = "eu-west";

// ── a const demand source (keep_warm needs demand > 0 to place) ──────────────

/// A fixed per-model demand signal so a reactive (`keep_warm`) policy reads
/// `demand > 0` and places, without a live router `/metrics` scrape. `inflight`
/// returns `None` (unknown) so headroom fails-soft to "available" (= the runner's
/// advertised `C`), keeping the slot a placement candidate.
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

/// A demand source the test FLIPS between ticks, modelling the real
/// `PrometheusDemandSource` whose per-model demand is a momentary value (in-flight
/// + the one-shot starved-counter delta): a burst of starved requests reads `> 0`
/// on ONE scrape, then `0` on the next once the delta is consumed. Lets a test
/// reproduce "woken by a transient demand edge, then the very next tick reads 0"
/// — the exact shape that used to flap a `scale_to_zero` model straight back to
/// sleep before it could serve.
struct SwitchableDemand(Mutex<f64>);

impl SwitchableDemand {
    fn new(initial: f64) -> Self {
        Self(Mutex::new(initial))
    }
    fn set(&self, v: f64) {
        *self.0.lock().unwrap() = v;
    }
}

#[async_trait]
impl DemandSource for SwitchableDemand {
    async fn demand_for(&self, _model_id: &str) -> Option<f64> {
        Some(*self.0.lock().unwrap())
    }
    async fn inflight_for(&self, _model_id: &str) -> Option<f64> {
        None
    }
}

// ── local helpers ────────────────────────────────────────────────────────────

/// Connect a `MekhanNats` for the service-under-test to publish on, plus a raw
/// `async_nats::Client` for the spy — both on the shared test broker.
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

/// Rewrite a seeded runner's interface catalog so `model_id` becomes RESIDENT
/// (present in `catalog.models` as a base with budget `c`) while staying in
/// `pulled`. This is the offline stand-in for "the runner finished its cold load
/// and re-advertised the base resident" — the signal `reconcile_placement` reads
/// as `now_resident` (and the loaded-set head-count reads as `observed += 1`).
async fn make_base_resident(db: &PgPool, runner_id: Uuid, model_id: &str, c: u32) {
    let catalog = json!({
        "topics": [],
        "services": [],
        "actions": [],
        "models": [{ "model_id": model_id, "kind": "base", "max_num_seqs": c }],
        "pulled": [model_id],
        "residency_zone": ZONE,
    });
    sqlx::query("UPDATE runner_interfaces SET catalog = $2 WHERE runner_id = $1")
        .bind(runner_id)
        .bind(&catalog)
        .execute(db)
        .await
        .expect("update runner catalog to resident");
}

/// Count of `Unload{Base}` commands captured for `base` (idle-eviction sleeps).
fn unload_base_count(captured: &[CapturedCommand], base: &str) -> usize {
    captured
        .iter()
        .filter(|c| {
            matches!(
                &c.command,
                ModelCommand::Unload { target: LoadTarget::Base { model_id } } if model_id == base
            )
        })
        .count()
}

/// Force the model's `last_actuated_at` (the warm-window anchor) into the past so a
/// subsequent zero-demand tick is no longer inside the warm window — the offline
/// stand-in for "the warm window has elapsed" (we can't wait the real 120s).
async fn backdate_last_actuated(db: &PgPool, workspace_id: Uuid, model_id: &str, secs_ago: i64) {
    let ts = chrono::Utc::now() - chrono::Duration::seconds(secs_ago);
    sqlx::query(
        "UPDATE model_replicas SET last_actuated_at = $3 \
         WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(model_id)
    .bind(ts)
    .execute(db)
    .await
    .expect("backdate last_actuated_at");
}

/// All `LoadBase` commands captured for `base`, grouped by target runner.
fn load_base_runners(captured: &[CapturedCommand], base: &str) -> Vec<Uuid> {
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

/// Read the cold-load timing trio off the `model_replicas` row:
/// `(load_started_at IS SOME, last_load_duration_ms)`.
async fn read_load_timing(
    db: &PgPool,
    workspace_id: Uuid,
    model_id: &str,
) -> (bool, Option<i64>) {
    let row: Option<(Option<chrono::DateTime<chrono::Utc>>, Option<i64>)> = sqlx::query_as(
        "SELECT load_started_at, last_load_duration_ms \
         FROM model_replicas WHERE workspace_id = $1 AND model_id = $2",
    )
    .bind(workspace_id)
    .bind(model_id)
    .fetch_optional(db)
    .await
    .expect("read model_replicas load timing");
    match row {
        Some((started, dur)) => (started.is_some(), dur),
        None => (false, None),
    }
}

// ── S1 COLD ──────────────────────────────────────────────────────────────────

/// One runner has the base PULLED but NOT resident; `keep_warm` policy, demand>0,
/// `desired_replicas = 1`. Tick 1 (COLD): exactly one `LoadBase` to that runner is
/// published; `load_started_at` is stamped (cold-load measurement begins); the
/// `model_replicas` row is `active` with `observed = 0` (no runner advertises the
/// base resident yet). Then the runner re-advertises the base resident and tick 2
/// observes residency: `observed → 1`, the measurement FINISHes
/// (`last_load_duration_ms` becomes `Some(>= 0)`, `load_started_at` clears).
#[tokio::test]
async fn s1_cold_load_starts_measurement_then_finishes_on_residency() {
    let db = common::create_test_db().await;
    let presence = RunnerPresence::new();
    let (nats, spy_client) = connect_nats().await;
    let spy = NatsCommandSpy::start(spy_client).await;

    let ws = Uuid::new_v4();

    // A runner that has the base PULLED to disk but NOT resident (empty `models`).
    let runner = seed_model_runner(
        &db,
        &presence,
        SeedRunnerSpec {
            workspace_id: ws,
            models: vec![], // nothing resident yet
            pulled: vec![LLAMA.to_string()],
            residency_zone: Some(ZONE.to_string()),
            ..Default::default()
        },
    )
    .await;

    // keep_warm, desired 1, zoned — demand>0 makes it place this tick.
    seed_model_policy(
        &db,
        SeedPolicySpec::base(ws, LLAMA, "keep_warm", ZONE, 1),
    )
    .await;

    let demand = ConstDemand(1.0);

    // ── Tick 1: COLD ─────────────────────────────────────────────────────────
    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick 1");

    // Exactly one LoadBase to the pulled runner.
    let captured = spy
        .wait_for(1, Duration::from_secs(2))
        .await
        .expect("a LoadBase published on the cold tick");
    let loaders = load_base_runners(&captured, LLAMA);
    assert_eq!(
        loaders,
        vec![runner.runner_id],
        "exactly one LoadBase to the pulled runner"
    );

    // Row is active, observed 0 (base not resident yet), and the cold-load
    // measurement has STARTED (None → Some).
    let (status, desired, observed) = read_replica_status(&db, ws, LLAMA)
        .await
        .expect("model_replicas row written");
    assert_eq!(status, "active");
    assert_eq!(desired, 1);
    assert_eq!(observed, 0, "no runner advertises the base resident yet");
    let (started, dur) = read_load_timing(&db, ws, LLAMA).await;
    assert!(started, "load_started_at stamped on the cold publish");
    assert_eq!(dur, None, "no completed cold load yet");

    // ── Now the base becomes resident: refresh the catalog + re-tick ──────────
    make_base_resident(&db, runner.runner_id, LLAMA, 8).await;
    spy.drain().await; // ignore the idempotent wake the next tick re-publishes

    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick 2");

    // observed converges to 1, measurement FINISHED: load_started_at cleared,
    // last_load_duration_ms is now Some(>= 0).
    let (status, _desired, observed) = read_replica_status(&db, ws, LLAMA)
        .await
        .expect("model_replicas row still present");
    assert_eq!(status, "active");
    assert_eq!(observed, 1, "the now-resident runner is observed");
    let (started, dur) = read_load_timing(&db, ws, LLAMA).await;
    assert!(!started, "load_started_at cleared once the base is resident");
    let dur = dur.expect("last_load_duration_ms recorded once resident");
    assert!(dur >= 0, "a completed cold-load duration is non-negative");
}

// ── S2 WARM ──────────────────────────────────────────────────────────────────

/// The base is already RESIDENT on the runner (not just pulled). Reconcile is
/// idempotent: NO cold-load measurement is ever started (`load_started_at` stays
/// None across two ticks), and the spy shows no Load STORM — at most one
/// idempotent wake per tick to the same single runner, never a duplicated set or
/// a growing count.
#[tokio::test]
async fn s2_warm_resident_is_idempotent_no_cold_measurement_no_storm() {
    let db = common::create_test_db().await;
    let presence = RunnerPresence::new();
    let (nats, spy_client) = connect_nats().await;
    let spy = NatsCommandSpy::start(spy_client).await;

    let ws = Uuid::new_v4();

    // A runner with the base ALREADY RESIDENT (advertised in `models`).
    let runner = seed_model_runner(
        &db,
        &presence,
        SeedRunnerSpec {
            workspace_id: ws,
            models: vec![SeedModel::base(LLAMA, 8)],
            pulled: vec![LLAMA.to_string()],
            residency_zone: Some(ZONE.to_string()),
            ..Default::default()
        },
    )
    .await;

    seed_model_policy(
        &db,
        SeedPolicySpec::base(ws, LLAMA, "keep_warm", ZONE, 1),
    )
    .await;
    let demand = ConstDemand(1.0);

    // ── Tick 1 ────────────────────────────────────────────────────────────────
    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick 1");

    // A resident base is woken (idempotent) — that is allowed, but it must NOT
    // start a cold measurement.
    let after_t1 = spy.wait_for(1, Duration::from_secs(2)).await;
    if let Ok(captured) = &after_t1 {
        // Every wake targets the single resident runner; no other runner.
        for rid in load_base_runners(captured, LLAMA) {
            assert_eq!(rid, runner.runner_id, "wake only the resident runner");
        }
    }
    let t1_count = spy.drain().await.len();
    assert!(
        t1_count <= 1,
        "at most one idempotent wake on tick 1, saw {t1_count}"
    );

    // The row reflects an active, observed-1 placement with NO cold measurement.
    let (status, _desired, observed) = read_replica_status(&db, ws, LLAMA)
        .await
        .expect("model_replicas row");
    assert_eq!(status, "active");
    assert_eq!(observed, 1, "the resident runner is observed");
    let (started, dur) = read_load_timing(&db, ws, LLAMA).await;
    assert!(!started, "warm wake does NOT start a cold measurement");
    assert_eq!(dur, None, "no cold load was ever measured for a warm base");

    // ── Tick 2: still idempotent, no storm ───────────────────────────────────
    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick 2");
    let after_t2 = spy.wait_for(1, Duration::from_millis(500)).await;
    if let Ok(captured) = &after_t2 {
        for rid in load_base_runners(captured, LLAMA) {
            assert_eq!(rid, runner.runner_id, "tick 2 wake only the resident runner");
        }
    }
    let t2_count = spy.drain().await.len();
    assert!(
        t2_count <= 1,
        "tick 2 publishes at most one idempotent wake, saw {t2_count} (no Load storm)"
    );

    // Timing state is unchanged: never started, never measured.
    let (started, dur) = read_load_timing(&db, ws, LLAMA).await;
    assert!(!started, "load_started_at still None after two warm ticks");
    assert_eq!(dur, None, "still no cold-load duration after two warm ticks");
}

// ── S3 SPREAD-TO-2 ───────────────────────────────────────────────────────────

/// Two runners both have the base PULLED, policy `desired_replicas = 2` ⇒
/// placement spreads: a `LoadBase` to BOTH distinct runners on the cold tick.
/// Then, with one runner now RESIDENT (re-advertised) and `desired_replicas = 2`,
/// only the SHORTFALL (the still-pulled runner) gets a new cold load — the
/// resident runner is at most woken, never re-cold-loaded onto a fresh runner.
#[tokio::test]
async fn s3_spread_to_two_then_only_shortfall_gets_new_load() {
    let db = common::create_test_db().await;
    let presence = RunnerPresence::new();
    let (nats, spy_client) = connect_nats().await;
    let spy = NatsCommandSpy::start(spy_client).await;

    let ws = Uuid::new_v4();

    // Two runners, both with the base PULLED but NOT resident.
    let pulled_spec = |models: Vec<SeedModel>| SeedRunnerSpec {
        workspace_id: ws,
        models,
        pulled: vec![LLAMA.to_string()],
        residency_zone: Some(ZONE.to_string()),
        ..Default::default()
    };
    let r1 = seed_model_runner(&db, &presence, pulled_spec(vec![])).await;
    let r2 = seed_model_runner(&db, &presence, pulled_spec(vec![])).await;

    // keep_warm, desired 2 — spread across both runners.
    seed_model_policy(
        &db,
        SeedPolicySpec::base(ws, LLAMA, "keep_warm", ZONE, 2),
    )
    .await;
    let demand = ConstDemand(1.0);

    // ── Tick 1: spread to 2 distinct runners ─────────────────────────────────
    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick 1 (spread)");

    let captured = spy
        .wait_for(2, Duration::from_secs(2))
        .await
        .expect("two LoadBase published across the spread");
    let mut loaders = load_base_runners(&captured, LLAMA);
    loaders.sort();
    let mut expected = vec![r1.runner_id, r2.runner_id];
    expected.sort();
    assert_eq!(
        loaders, expected,
        "a LoadBase to BOTH distinct pulled runners"
    );

    // Row: desired 2, both runners loading (observed still 0 — neither resident).
    let (status, desired, observed) = read_replica_status(&db, ws, LLAMA)
        .await
        .expect("model_replicas row");
    assert_eq!(status, "active");
    assert_eq!(desired, 2);
    assert_eq!(observed, 0, "neither runner advertises the base resident yet");

    spy.drain().await;

    // ── One runner becomes resident; desired still 2 → only the shortfall ─────
    make_base_resident(&db, r1.runner_id, LLAMA, 8).await;

    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick 2 (shortfall)");

    // Wait for the publishes to settle, then classify by runner.
    let _ = spy.wait_for(1, Duration::from_secs(1)).await;
    let captured = spy.drain().await;
    let loaders = load_base_runners(&captured, LLAMA);
    let mut by_runner: HashMap<Uuid, usize> = HashMap::new();
    for rid in &loaders {
        *by_runner.entry(*rid).or_default() += 1;
    }
    // The still-pulled runner r2 MUST get a (cold) load — it is the shortfall.
    assert!(
        by_runner.get(&r2.runner_id).copied().unwrap_or(0) >= 1,
        "the still-pulled runner (the shortfall) gets a new Load"
    );
    // r1 is resident: it is at most WOKEN (idempotent), never spread onto a fresh
    // runner. Across the whole spread, exactly the two seeded runners appear — no
    // third runner is ever targeted.
    let distinct: std::collections::HashSet<Uuid> = loaders.iter().copied().collect();
    assert!(
        distinct.is_subset(&[r1.runner_id, r2.runner_id].into_iter().collect()),
        "only the two seeded runners are ever targeted"
    );

    // observed converges toward 2 once r1 is resident (1 so far this tick; the
    // second lands when r2 finishes its load — out of band).
    let (_status, desired, observed) = read_replica_status(&db, ws, LLAMA)
        .await
        .expect("model_replicas row after shortfall tick");
    assert_eq!(desired, 2);
    assert_eq!(observed, 1, "the now-resident runner is observed");
}

// ── S4 SCALE-FROM-ZERO: WAKE THEN HOLD WARM (regression for the flap) ─────────

/// The bug this locks: a `scale_to_zero` model with `idle_evict` is woken by a
/// momentary demand edge (a burst of starved requests → `demand > 0` on ONE
/// scrape), then the NEXT tick reads `demand == 0` (the one-shot starved delta is
/// consumed). Pre-fix, that zero tick idle-evicted the model RIGHT BACK to sleep —
/// it flapped and could never stay resident long enough to serve the client's
/// cold-start retries. Post-fix, the WARM WINDOW (default 120s, anchored on the
/// placement's `last_actuated_at`) holds it resident: tick 2 must NOT publish an
/// `Unload`, and the row must stay `active`, not `sleeping`.
///
/// The previous tests all used a CONSTANT non-zero demand, so they never returned
/// to zero and never exercised idle-eviction at all — which is exactly why the
/// harness missed this.
#[tokio::test]
async fn s4_scale_to_zero_wake_then_holds_warm_no_immediate_evict() {
    let db = common::create_test_db().await;
    let presence = RunnerPresence::new();
    let (nats, spy_client) = connect_nats().await;
    let spy = NatsCommandSpy::start(spy_client).await;

    let ws = Uuid::new_v4();

    // A runner with the base RESIDENT (the woken state); scale_to_zero + idle_evict.
    let runner = seed_model_runner(
        &db,
        &presence,
        SeedRunnerSpec {
            workspace_id: ws,
            models: vec![SeedModel::base(LLAMA, 8)],
            pulled: vec![LLAMA.to_string()],
            residency_zone: Some(ZONE.to_string()),
            ..Default::default()
        },
    )
    .await;

    seed_model_policy(
        &db,
        SeedPolicySpec {
            idle_evict: true, // opt in to idle-eviction — the thing that flapped
            ..SeedPolicySpec::base(ws, LLAMA, "scale_to_zero", ZONE, 1)
        },
    )
    .await;

    // Demand spikes once (the starved-from-zero edge), then decays to zero.
    let demand = SwitchableDemand::new(1.0);

    // ── Tick 1: demand > 0 → wake/place, stamp the warm-window anchor ──────────
    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick 1 (wake)");
    let _ = spy.wait_for(1, Duration::from_secs(1)).await;
    let t1 = spy.drain().await;
    for rid in load_base_runners(&t1, LLAMA) {
        assert_eq!(rid, runner.runner_id, "wake targets the resident runner");
    }
    let (status, _desired, _observed) = read_replica_status(&db, ws, LLAMA)
        .await
        .expect("model_replicas row after wake");
    assert_eq!(status, "active", "demand>0 places the model active");

    // ── Tick 2: demand decays to 0 (edge consumed) → WARM WINDOW must hold ─────
    demand.set(0.0);
    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick 2 (zero demand, within warm window)");
    // Give any (erroneous) Unload time to land before we assert its absence.
    let _ = spy.wait_for(1, Duration::from_millis(500)).await;
    let t2 = spy.drain().await;
    assert_eq!(
        unload_base_count(&t2, LLAMA),
        0,
        "a just-woken scale_to_zero model must NOT be idle-evicted within the warm window"
    );
    let (status, _desired, _observed) = read_replica_status(&db, ws, LLAMA)
        .await
        .expect("model_replicas row after the zero-demand tick");
    assert_eq!(
        status, "active",
        "the model stays resident (active) through the warm window, not sleeping"
    );
}

// ── S5 WARM WINDOW EXPIRES → idle-evict sleeps the model ──────────────────────

/// The complement of S4: once the warm window has actually elapsed and demand is
/// still zero, idle-eviction DOES fire — the model is genuinely idle, so it sleeps
/// (an `Unload{Base}` is published and the row goes `sleeping`). Proves the warm
/// window is a finite hold, not a permanent pin, and that the eviction path itself
/// still works.
#[tokio::test]
async fn s5_scale_to_zero_evicts_after_warm_window_elapses() {
    let db = common::create_test_db().await;
    let presence = RunnerPresence::new();
    let (nats, spy_client) = connect_nats().await;
    let spy = NatsCommandSpy::start(spy_client).await;

    let ws = Uuid::new_v4();

    let runner = seed_model_runner(
        &db,
        &presence,
        SeedRunnerSpec {
            workspace_id: ws,
            models: vec![SeedModel::base(LLAMA, 8)],
            pulled: vec![LLAMA.to_string()],
            residency_zone: Some(ZONE.to_string()),
            ..Default::default()
        },
    )
    .await;

    seed_model_policy(
        &db,
        SeedPolicySpec {
            idle_evict: true,
            ..SeedPolicySpec::base(ws, LLAMA, "scale_to_zero", ZONE, 1)
        },
    )
    .await;

    // Place once under demand to create the row + warm-window anchor.
    let demand = SwitchableDemand::new(1.0);
    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick 1 (wake)");
    let _ = spy.wait_for(1, Duration::from_secs(1)).await;
    spy.drain().await;

    // Age the warm-window anchor well past the default 120s window.
    backdate_last_actuated(&db, ws, LLAMA, 600).await;

    // ── Zero demand + elapsed warm window → idle-evict sleeps the model ────────
    demand.set(0.0);
    reconcile_placement(&db, &nats, &presence, Some(&demand))
        .await
        .expect("reconcile tick 2 (zero demand, warm window elapsed)");
    let captured = spy
        .wait_for(1, Duration::from_secs(2))
        .await
        .expect("an Unload published once genuinely idle past the warm window");
    assert_eq!(
        unload_base_count(&captured, LLAMA),
        1,
        "exactly one idle-eviction Unload{{Base}} once the warm window has elapsed"
    );
    // The Unload targets the resident runner.
    assert!(
        captured.iter().any(|c| c.runner_id == runner.runner_id),
        "the idle-eviction targets the resident runner"
    );
    let (status, _desired, _observed) = read_replica_status(&db, ws, LLAMA)
        .await
        .expect("model_replicas row after eviction");
    assert_eq!(status, "sleeping", "the idle model is slept");
}
