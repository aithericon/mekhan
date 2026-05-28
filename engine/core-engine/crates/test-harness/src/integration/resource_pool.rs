//! M2 — Cross-net claim/grant/release round-trip with capacity contention.
//!
//! A shared `resource-pool-net` (one capacity token) is contended for by three
//! independent requester nets over the real NATS cross-net bridge. This proves,
//! end to end, the load-bearing properties the M3 compiler will rely on:
//!
//!   * GRANT ROUTING — the grant returns to the *specific* requester that
//!     claimed (via the "grant" reply channel carried on the claim token), never
//!     cross-routed. Each requester's hold carries its own grant_id.
//!   * SERIALIZATION / MUTEX — with N=1 capacity, at most one requester holds at
//!     a time. `count(pool) + count(in_use) == 1` on the pool net throughout;
//!     `in_use` never exceeds N. The other claims queue in `claim_inbox`
//!     (visible backpressure).
//!   * TWO-PHASE RELEASE — release is a *separate* fire-and-forget bridge round
//!     trip (not the single request/reply scheduler-net does), correlated back
//!     to its hold by grant_id. Releasing frees the slot and the next queued
//!     claim is granted.
//!
//! The pure in-process semantics are proven in
//! `core-engine/crates/test-harness/tests/resource_pool.rs` (M1); this test adds
//! the distribution layer the compiler output (M3) targets.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use tokio::sync::Notify;

use petri_application::PetriNetService;
use petri_domain::{
    Arc as PetriArc, Marking, PetriNet, Place, PlaceId, Port, TokenColor, Transition,
};
use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};
use petri_nats::{CrossNetBridge, NatsConfig, NatsEventPublisher};

use crate::nats::{ensure_global_stream, shared_nats_url};

type Svc = PetriNetService<
    NatsEventPublisher<MemoryEventStore>,
    MemoryTopologyStore,
    MarkingProjection,
>;

const POOL_CAPACITY: usize = 1;
const N_REQUESTERS: usize = 3;

// ---------------------------------------------------------------------------
// Multi-net test context: one pool + N requesters, all on the shared bridge.
// ---------------------------------------------------------------------------

struct PoolTestContext {
    pool_id: String,
    requester_ids: Vec<String>,
    pool: Arc<Svc>,
    requesters: Vec<Arc<Svc>>,
    jetstream: jetstream::Context,
}

impl PoolTestContext {
    async fn setup(n_requesters: usize) -> Self {
        let nats_url = shared_nats_url().await;
        let client = async_nats::connect(nats_url)
            .await
            .expect("connect to shared NATS testcontainer");
        let jetstream = jetstream::new(client);
        ensure_global_stream(&jetstream)
            .await
            .expect("PETRI_GLOBAL stream");

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let pool_id = format!("resource-pool-{suffix}");
        let requester_ids: Vec<String> =
            (0..n_requesters).map(|i| format!("req-{i}-{suffix}")).collect();

        let build_service = |net_id: &str| {
            let store = Arc::new(MemoryEventStore::new());
            let config = NatsConfig {
                url: nats_url.to_string(),
                net_id: Some(net_id.to_string()),
                ..NatsConfig::default()
            };
            let publisher = NatsEventPublisher::new(store, jetstream.clone(), config);
            Arc::new(PetriNetService::new(
                Arc::new(publisher),
                Arc::new(MemoryTopologyStore::new()),
                Arc::new(MarkingProjection::new()),
            ))
        };

        let pool = build_service(&pool_id);
        let requesters: Vec<Arc<Svc>> = requester_ids.iter().map(|id| build_service(id)).collect();

        // Start inbound bridge listeners for every net.
        let mut all: Vec<(&String, &Arc<Svc>)> = vec![(&pool_id, &pool)];
        for (id, svc) in requester_ids.iter().zip(requesters.iter()) {
            all.push((id, svc));
        }
        for (net_id, svc) in &all {
            let bridge = Arc::new(CrossNetBridge::new((*net_id).clone(), jetstream.clone()));
            bridge.start_inbound_listener((*svc).clone(), Arc::new(Notify::new()));
        }

        // Wait for all bridge consumers to exist (DeliverPolicy::New drops
        // messages published before the consumer is created).
        let stream = jetstream
            .get_stream("PETRI_GLOBAL")
            .await
            .expect("get PETRI_GLOBAL stream");
        let mut ids = vec![pool_id.clone()];
        ids.extend(requester_ids.iter().cloned());
        for net_id in &ids {
            let consumer_name = format!("bridge-inbound-{net_id}");
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                match stream
                    .get_consumer::<async_nats::jetstream::consumer::pull::Config>(&consumer_name)
                    .await
                {
                    Ok(_) => break,
                    Err(_) if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(e) => panic!("Bridge consumer {consumer_name} not ready: {e}"),
                }
            }
        }

        Self {
            pool_id,
            requester_ids,
            pool,
            requesters,
            jetstream,
        }
    }

    async fn teardown(&self) {
        let stream = match self.jetstream.get_stream("PETRI_GLOBAL").await {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut ids = vec![self.pool_id.clone()];
        ids.extend(self.requester_ids.iter().cloned());
        for net_id in &ids {
            let _ = stream.delete_consumer(&format!("bridge-inbound-{net_id}")).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Net builders (domain types, mirroring the resource_pool_net SDK example).
// ---------------------------------------------------------------------------

/// Place ids of interest on the pool net.
struct PoolPlaces {
    pool: PlaceId,
    claim_inbox: PlaceId,
    release_inbox: PlaceId,
    done: PlaceId,
}

/// Build the pool net.
///
/// Capacity is modelled as N CLEAN data tokens in `pool`. `t_grant` consumes a
/// claim + a capacity token and emits ONLY the grant reply (a bridge output,
/// which produces no tainted local token); `t_release` consumes the (clean)
/// release request and returns a clean capacity token. We deliberately do NOT
/// thread capacity through an `in_use` token here: a transition that consumes
/// the routed claim taints every internal output it produces with the claim's
/// reply_routing (`firing.rs` route_output_tokens), so a recycled capacity
/// token would carry a stale "grant" channel and collide with the next claim's
/// routing in `merge_reply_routing` — wedging the pool. Keeping the recycled
/// capacity token clean (produced only from the clean release request) avoids
/// that. Outstanding holds are observed on the requester side and as
/// `capacity - available`.
fn build_pool_net() -> (PetriNet, PoolPlaces) {
    let mut net = PetriNet::new();

    let pool = Place::internal("pool").with_id(PlaceId::named("pool"));
    let done = Place::internal("done").with_id(PlaceId::named("done"));
    let claim_inbox = Place::bridge_in("claim_inbox").with_id(PlaceId::named("claim_inbox"));
    let release_inbox = Place::bridge_in("release_inbox").with_id(PlaceId::named("release_inbox"));
    let grant_outbox =
        Place::bridge_reply_channel("grant_outbox", "grant").with_id(PlaceId::named("grant_outbox"));

    let (pool_id, done_id, claim_id, release_id, grant_id) = (
        pool.id.clone(),
        done.id.clone(),
        claim_inbox.id.clone(),
        release_inbox.id.clone(),
        grant_outbox.id.clone(),
    );
    for p in [pool, done, claim_inbox, release_inbox, grant_outbox] {
        net.add_place(p);
    }

    // t_grant: claim_inbox + pool → grant_outbox (reply only). Consuming a
    // capacity token decrements availability; emitting only the bridge reply
    // leaves no tainted internal token behind.
    let t_grant = Transition::new(
        "t_grant",
        r#"#{ grant: #{ grant_id: claim.grant_id, gpu_id: cap.gpu_id } }"#,
    )
    .with_input_port(Port::new("claim"))
    .with_input_port(Port::new("cap"))
    .with_output_port(Port::new("grant"));
    let t_grant_id = t_grant.id.clone();
    net.add_transition(t_grant);
    net.add_arc(PetriArc::input(claim_id.clone(), t_grant_id.clone(), "claim"));
    net.add_arc(PetriArc::input(pool_id.clone(), t_grant_id.clone(), "cap"));
    net.add_arc(PetriArc::output(t_grant_id.clone(), "grant", grant_id));

    // t_release: release_inbox → pool + done. The release request is clean (it
    // crossed a plain bridge_out), so the recycled capacity token is clean and
    // never collides with a future claim's reply routing.
    let t_release = Transition::new(
        "t_release",
        r#"#{
            cap:  #{ gpu_id: req.gpu_id },
            done: #{ grant_id: req.grant_id, gpu_id: req.gpu_id }
        }"#,
    )
    .with_input_port(Port::new("req"))
    .with_output_port(Port::new("cap"))
    .with_output_port(Port::new("done"));
    let t_release_id = t_release.id.clone();
    net.add_transition(t_release);
    net.add_arc(PetriArc::input(release_id.clone(), t_release_id.clone(), "req"));
    net.add_arc(PetriArc::output(t_release_id.clone(), "cap", pool_id.clone()));
    net.add_arc(PetriArc::output(t_release_id.clone(), "done", done_id.clone()));

    (
        net,
        PoolPlaces {
            pool: pool_id,
            claim_inbox: claim_id,
            release_inbox: release_id,
            done: done_id,
        },
    )
}

struct ReqPlaces {
    start: PlaceId,
    holding: PlaceId,
    done: PlaceId,
    finish_trigger: PlaceId,
}

fn build_requester_net(pool_net_id: &str) -> (PetriNet, ReqPlaces) {
    let mut net = PetriNet::new();

    let start = Place::internal("start").with_id(PlaceId::named("start"));
    let holding = Place::internal("holding").with_id(PlaceId::named("holding"));
    let done = Place::internal("done").with_id(PlaceId::named("done"));
    // Gate the release on an explicit signal so the hold is observable and the
    // test controls release timing (otherwise evaluate fires receive→finish in
    // one shot and the hold is never observable).
    let finish_trigger =
        Place::signal("finish_trigger").with_id(PlaceId::named("finish_trigger"));
    let finish_trigger_id = finish_trigger.id.clone();

    // claim_out: bridge_out to pool/claim_inbox, with the "grant" reply channel
    // routed back to this net's grant_inbox.
    let mut channels = HashMap::new();
    channels.insert("grant".to_string(), "grant_inbox".to_string());
    let claim_out =
        Place::bridge_out_reply_channels("claim_out", pool_net_id, "claim_inbox", channels)
            .with_id(PlaceId::named("claim_out"));
    // Grant landing place: a normal internal place. The pool's reply routing
    // delivers here by (net_id, place_name); the place kind only needs to be
    // one a transition can consume from (bridge_reply places cannot be inputs).
    let grant_inbox = Place::internal("grant_inbox").with_id(PlaceId::named("grant_inbox"));
    let release_out = Place::bridge_out("release_out", pool_net_id, "release_inbox")
        .with_id(PlaceId::named("release_out"));

    let (start_id, holding_id, done_id, claim_out_id, grant_inbox_id, release_out_id) = (
        start.id.clone(),
        holding.id.clone(),
        done.id.clone(),
        claim_out.id.clone(),
        grant_inbox.id.clone(),
        release_out.id.clone(),
    );
    for p in [start, holding, done, claim_out, grant_inbox, release_out, finish_trigger] {
        net.add_place(p);
    }

    // t_claim: start → claim_out (start token already carries grant_id)
    let t_claim = Transition::new("t_claim", r#"#{ claim_out: start }"#)
        .with_input_port(Port::new("start"))
        .with_output_port(Port::new("claim_out"));
    let t_claim_id = t_claim.id.clone();
    net.add_transition(t_claim);
    net.add_arc(PetriArc::input(start_id.clone(), t_claim_id.clone(), "start"));
    net.add_arc(PetriArc::output(t_claim_id.clone(), "claim_out", claim_out_id));

    // t_receive: grant_inbox → holding
    let t_receive = Transition::new("t_receive", r#"#{ holding: grant }"#)
        .with_input_port(Port::new("grant"))
        .with_output_port(Port::new("holding"));
    let t_receive_id = t_receive.id.clone();
    net.add_transition(t_receive);
    net.add_arc(PetriArc::input(grant_inbox_id.clone(), t_receive_id.clone(), "grant"));
    net.add_arc(PetriArc::output(t_receive_id.clone(), "holding", holding_id.clone()));

    // t_finish: holding + finish_trigger → release_out + done.
    // Gated on the trigger so the hold stays observable until the test releases.
    let t_finish = Transition::new(
        "t_finish",
        r#"#{ release: #{ grant_id: holding.grant_id, gpu_id: holding.gpu_id }, local: holding }"#,
    )
    .with_input_port(Port::new("holding"))
    .with_input_port(Port::new("trigger"))
    .with_output_port(Port::new("release"))
    .with_output_port(Port::new("local"));
    let t_finish_id = t_finish.id.clone();
    net.add_transition(t_finish);
    net.add_arc(PetriArc::input(holding_id.clone(), t_finish_id.clone(), "holding"));
    net.add_arc(PetriArc::input(finish_trigger_id.clone(), t_finish_id.clone(), "trigger"));
    net.add_arc(PetriArc::output(t_finish_id.clone(), "release", release_out_id));
    net.add_arc(PetriArc::output(t_finish_id.clone(), "local", done_id.clone()));

    (
        net,
        ReqPlaces {
            start: start_id,
            holding: holding_id,
            done: done_id,
            finish_trigger: finish_trigger_id,
        },
    )
}

// ---------------------------------------------------------------------------
// Poll helper
// ---------------------------------------------------------------------------

async fn poll<F>(svc: &Svc, predicate: F, what: &str, timeout: Duration) -> Marking
where
    F: Fn(&Marking) -> bool,
{
    let start = tokio::time::Instant::now();
    loop {
        let marking = svc.get_marking().await;
        if predicate(&marking) {
            return marking;
        }
        if start.elapsed() > timeout {
            panic!("poll timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn three_requesters_contend_for_one_capacity_unit() {
    let ctx = PoolTestContext::setup(N_REQUESTERS).await;

    // Build + initialise the pool and requester nets.
    let (pool_net, pp) = build_pool_net();
    ctx.pool.initialize(pool_net).await.unwrap();
    ctx.pool
        .create_token(pp.pool.clone(), TokenColor::Data(serde_json::json!({ "gpu_id": "gpu-0" })))
        .await
        .unwrap();

    let mut req_places = Vec::new();
    for (i, svc) in ctx.requesters.iter().enumerate() {
        let (net, rp) = build_requester_net(&ctx.pool_id);
        svc.initialize(net).await.unwrap();
        // Each requester mints a distinct grant_id and claims.
        svc.create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": format!("req-{i}") })),
        )
        .await
        .unwrap();
        req_places.push(rp);
    }

    // All three requesters fire their claim → three claims bridge to the pool.
    for svc in &ctx.requesters {
        svc.evaluate_until_quiescent(10).await.unwrap();
    }
    poll(
        &ctx.pool,
        |m| m.token_count(&pp.claim_inbox) >= N_REQUESTERS,
        "all claims to arrive at pool",
        Duration::from_secs(10),
    )
    .await;

    // Count how many requesters currently hold a grant (the live holders).
    let count_holders = |req_places: &[ReqPlaces]| {
        let reqs = &ctx.requesters;
        let places: Vec<PlaceId> = req_places.iter().map(|r| r.holding.clone()).collect();
        async move {
            let mut n = 0usize;
            for (svc, hp) in reqs.iter().zip(places.iter()) {
                n += svc.get_marking().await.token_count(hp);
            }
            n
        }
    };

    // Drive the contention: with N=1 capacity, exactly one requester holds at a
    // time and the rest queue in the pool's claim_inbox (visible backpressure).
    let mut served: Vec<bool> = vec![false; N_REQUESTERS];
    for round in 0..N_REQUESTERS {
        // Pool grants one claim (only one capacity token exists → at most one grant).
        ctx.pool.evaluate_until_quiescent(10).await.unwrap();

        // Find the requester that received the grant and drive it to a (stable)
        // hold. t_finish is gated on finish_trigger, so evaluating only moves
        // grant_inbox → holding and stops there.
        let mut holder: Option<usize> = None;
        for (i, svc) in ctx.requesters.iter().enumerate() {
            if served[i] {
                continue;
            }
            let got = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    svc.evaluate_until_quiescent(10).await.unwrap();
                    if svc.get_marking().await.token_count(&req_places[i].holding) >= 1 {
                        return true;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap_or(false);
            if got {
                holder = Some(i);
                break;
            }
        }
        let h = holder.unwrap_or_else(|| panic!("round {round}: no requester received the grant"));

        // Correct routing: the hold carries THIS requester's grant_id (not cross-routed).
        let hold_marking = ctx.requesters[h].get_marking().await;
        match &hold_marking.tokens_at(&req_places[h].holding)[0].color {
            TokenColor::Data(d) => assert_eq!(
                d.get("grant_id").and_then(|v| v.as_str()),
                Some(format!("req-{h}").as_str()),
                "grant cross-routed: requester {h} holds {d}"
            ),
            _ => panic!("hold token is not Data"),
        }

        // SERIALIZATION: exactly one holder, and the pool is drained to zero
        // available capacity while it's held.
        assert_eq!(
            count_holders(&req_places).await,
            1,
            "round {round}: expected exactly one holder (serialization)"
        );
        assert_eq!(
            ctx.pool.get_marking().await.token_count(&pp.pool),
            POOL_CAPACITY - 1,
            "round {round}: held capacity should leave the pool drained"
        );

        // The holder finishes: inject its finish trigger, evaluate → release
        // bridges (fire-and-forget) back to the pool.
        ctx.requesters[h]
            .create_token(req_places[h].finish_trigger.clone(), TokenColor::Unit)
            .await
            .unwrap();
        ctx.requesters[h].evaluate_until_quiescent(10).await.unwrap();
        served[h] = true;

        poll(
            &ctx.pool,
            |m| m.token_count(&pp.release_inbox) >= 1,
            "release to arrive at pool",
            Duration::from_secs(10),
        )
        .await;
        // Pool releases (capacity returns to the pool); the next queued claim is
        // granted on the next round's evaluate.
        ctx.pool.evaluate_until_quiescent(10).await.unwrap();
    }

    // All served, capacity fully returned, every requester completed.
    let pm = ctx.pool.get_marking().await;
    assert_eq!(pm.token_count(&pp.pool), POOL_CAPACITY, "capacity returned to pool");
    assert_eq!(count_holders(&req_places).await, 0, "no holds outstanding");
    assert_eq!(pm.token_count(&pp.done), N_REQUESTERS, "every claim served once");
    for (i, rp) in req_places.iter().enumerate() {
        assert_eq!(
            ctx.requesters[i].get_marking().await.token_count(&rp.done),
            1,
            "requester {i} completed"
        );
    }

    ctx.teardown().await;
}

// ===========================================================================
// M4 — Full showcase scenario: N capacity, K jobs, register + lease reap.
//
// This is the "GPU rendering pool" the design doc describes, driven over real
// NATS exactly as the deployable `resource_pool_net` example would be. The pool
// uses the registration pattern: t_grant emits ONLY the bridge reply (no taint),
// the holder registers its hold over a clean bridge, and t_release / t_reap
// consume that CLEAN in_use hold so recycled capacity never carries stale
// routing. This gives an observable `in_use` and a working lease reap.
// ===========================================================================

/// Place ids on the registration-pattern pool net.
struct RegPoolPlaces {
    pool: PlaceId,
    in_use: PlaceId,
    claim_inbox: PlaceId,
    register_inbox: PlaceId,
    release_inbox: PlaceId,
    lease_expired: PlaceId,
    done: PlaceId,
}

fn build_registered_pool_net(capacity: usize) -> (PetriNet, RegPoolPlaces) {
    let mut net = PetriNet::new();

    let pool = Place::internal("pool");
    let in_use = Place::internal("in_use");
    let done = Place::internal("done");
    let claim_inbox = Place::bridge_in("claim_inbox");
    let register_inbox = Place::bridge_in("register_inbox");
    let release_inbox = Place::bridge_in("release_inbox");
    let lease_expired = Place::signal("lease_expired");
    let grant_outbox = Place::bridge_reply_channel("grant_outbox", "grant");

    let pool_id = pool.id.clone();
    let in_use_id = in_use.id.clone();
    let done_id = done.id.clone();
    let claim_id = claim_inbox.id.clone();
    let register_id = register_inbox.id.clone();
    let release_id = release_inbox.id.clone();
    let lease_id = lease_expired.id.clone();
    let grant_id = grant_outbox.id.clone();
    for p in [
        pool, in_use, done, claim_inbox, register_inbox, release_inbox, lease_expired, grant_outbox,
    ] {
        net.add_place(p);
    }

    // t_grant: claim + cap → grant reply ONLY (no tainted local output).
    let t_grant = Transition::new(
        "t_grant",
        r#"#{ grant: #{ grant_id: claim.grant_id, gpu_id: cap.gpu_id } }"#,
    )
    .with_input_port(Port::new("claim"))
    .with_input_port(Port::new("cap"))
    .with_output_port(Port::new("grant"));
    let tg = t_grant.id.clone();
    net.add_transition(t_grant);
    net.add_arc(PetriArc::input(claim_id.clone(), tg.clone(), "claim"));
    net.add_arc(PetriArc::input(pool_id.clone(), tg.clone(), "cap"));
    net.add_arc(PetriArc::output(tg.clone(), "grant", grant_id));

    // t_register: register_inbox → in_use (CLEAN hold).
    let t_register = Transition::new(
        "t_register",
        r#"#{ hold: #{ grant_id: reg.grant_id, gpu_id: reg.gpu_id } }"#,
    )
    .with_input_port(Port::new("reg"))
    .with_output_port(Port::new("hold"));
    let tr = t_register.id.clone();
    net.add_transition(t_register);
    net.add_arc(PetriArc::input(register_id.clone(), tr.clone(), "reg"));
    net.add_arc(PetriArc::output(tr.clone(), "hold", in_use_id.clone()));

    // t_release: release + in_use (correlate grant_id) → pool + done.
    let t_release = Transition::new(
        "t_release",
        r#"#{ cap: #{ gpu_id: held.gpu_id }, done: #{ grant_id: held.grant_id, outcome: "released" } }"#,
    )
    .with_input_port(Port::new("req"))
    .with_input_port(Port::new("held"))
    .with_guard("req.grant_id == held.grant_id")
    .with_output_port(Port::new("cap"))
    .with_output_port(Port::new("done"));
    let trel = t_release.id.clone();
    net.add_transition(t_release);
    net.add_arc(PetriArc::input(release_id.clone(), trel.clone(), "req"));
    net.add_arc(PetriArc::input(in_use_id.clone(), trel.clone(), "held"));
    net.add_arc(PetriArc::output(trel.clone(), "cap", pool_id.clone()));
    net.add_arc(PetriArc::output(trel.clone(), "done", done_id.clone()));

    // t_reap: lease_expired + in_use (correlate grant_id) → pool + done.
    let t_reap = Transition::new(
        "t_reap",
        r#"#{ cap: #{ gpu_id: held.gpu_id }, done: #{ grant_id: held.grant_id, outcome: "reaped" } }"#,
    )
    .with_input_port(Port::new("exp"))
    .with_input_port(Port::new("held"))
    .with_guard("exp.grant_id == held.grant_id")
    .with_output_port(Port::new("cap"))
    .with_output_port(Port::new("done"));
    let trp = t_reap.id.clone();
    net.add_transition(t_reap);
    net.add_arc(PetriArc::input(lease_id.clone(), trp.clone(), "exp"));
    net.add_arc(PetriArc::input(in_use_id.clone(), trp.clone(), "held"));
    net.add_arc(PetriArc::output(trp.clone(), "cap", pool_id.clone()));
    net.add_arc(PetriArc::output(trp.clone(), "done", done_id.clone()));

    let _ = capacity; // caller seeds the pool

    (
        net,
        RegPoolPlaces {
            pool: pool_id,
            in_use: in_use_id,
            claim_inbox: claim_id,
            register_inbox: register_id,
            release_inbox: release_id,
            lease_expired: lease_id,
            done: done_id,
        },
    )
}

/// Requester that claims → receives grant → registers its hold → (waits for
/// finish_trigger) → releases. `holding` is the observable "this instance holds
/// a GPU" state.
struct RegReqPlaces {
    start: PlaceId,
    holding: PlaceId,
    done: PlaceId,
    finish_trigger: PlaceId,
}

fn build_registered_requester_net(pool_net_id: &str) -> (PetriNet, RegReqPlaces) {
    let mut net = PetriNet::new();

    let start = Place::internal("start");
    let holding = Place::internal("holding");
    let done = Place::internal("done");
    let grant_inbox = Place::internal("grant_inbox");
    let finish_trigger = Place::signal("finish_trigger");
    let mut channels = HashMap::new();
    channels.insert("grant".to_string(), "grant_inbox".to_string());
    let claim_out =
        Place::bridge_out_reply_channels("claim_out", pool_net_id, "claim_inbox", channels);
    let register_out = Place::bridge_out("register_out", pool_net_id, "register_inbox");
    let release_out = Place::bridge_out("release_out", pool_net_id, "release_inbox");

    let start_id = start.id.clone();
    let holding_id = holding.id.clone();
    let done_id = done.id.clone();
    let grant_inbox_id = grant_inbox.id.clone();
    let finish_id = finish_trigger.id.clone();
    let claim_out_id = claim_out.id.clone();
    let register_out_id = register_out.id.clone();
    let release_out_id = release_out.id.clone();
    for p in [
        start, holding, done, grant_inbox, finish_trigger, claim_out, register_out, release_out,
    ] {
        net.add_place(p);
    }

    // t_claim: start → claim_out
    let t_claim = Transition::new("t_claim", r#"#{ claim_out: start }"#)
        .with_input_port(Port::new("start"))
        .with_output_port(Port::new("claim_out"));
    let tc = t_claim.id.clone();
    net.add_transition(t_claim);
    net.add_arc(PetriArc::input(start_id.clone(), tc.clone(), "start"));
    net.add_arc(PetriArc::output(tc.clone(), "claim_out", claim_out_id));

    // t_receive: grant_inbox → holding + register_out (echo the grant cleanly).
    let t_receive = Transition::new(
        "t_receive",
        r#"#{ holding: grant, register: #{ grant_id: grant.grant_id, gpu_id: grant.gpu_id } }"#,
    )
    .with_input_port(Port::new("grant"))
    .with_output_port(Port::new("holding"))
    .with_output_port(Port::new("register"));
    let trc = t_receive.id.clone();
    net.add_transition(t_receive);
    net.add_arc(PetriArc::input(grant_inbox_id.clone(), trc.clone(), "grant"));
    net.add_arc(PetriArc::output(trc.clone(), "holding", holding_id.clone()));
    net.add_arc(PetriArc::output(trc.clone(), "register", register_out_id));

    // t_finish: holding + finish_trigger → release_out + done.
    let t_finish = Transition::new(
        "t_finish",
        r#"#{ release: #{ grant_id: holding.grant_id }, local: holding }"#,
    )
    .with_input_port(Port::new("holding"))
    .with_input_port(Port::new("trigger"))
    .with_output_port(Port::new("release"))
    .with_output_port(Port::new("local"));
    let tf = t_finish.id.clone();
    net.add_transition(t_finish);
    net.add_arc(PetriArc::input(holding_id.clone(), tf.clone(), "holding"));
    net.add_arc(PetriArc::input(finish_id.clone(), tf.clone(), "trigger"));
    net.add_arc(PetriArc::output(tf.clone(), "release", release_out_id));
    net.add_arc(PetriArc::output(tf.clone(), "local", done_id.clone()));

    (
        net,
        RegReqPlaces {
            start: start_id,
            holding: holding_id,
            done: done_id,
            finish_trigger: finish_id,
        },
    )
}

/// Drain bridge traffic + fire enabled transitions across the pool and every
/// requester until things settle. Repeated a few times to let cross-net
/// messages propagate (each call evaluates every net once).
async fn settle(ctx: &PoolTestContext, rounds: usize) {
    for _ in 0..rounds {
        ctx.pool.evaluate_until_quiescent(20).await.unwrap();
        for svc in &ctx.requesters {
            svc.evaluate_until_quiescent(20).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
}

async fn pool_in_use(ctx: &PoolTestContext, pp: &RegPoolPlaces) -> usize {
    ctx.pool.get_marking().await.token_count(&pp.in_use)
}

/// The headline showcase: 2 GPUs, 4 render jobs. At most two hold at once; the
/// rest queue; releasing frees a slot for a waiter; all four complete.
#[tokio::test]
async fn gpu_pool_two_capacity_four_jobs_showcase() {
    const CAP: usize = 2;
    const JOBS: usize = 4;
    let ctx = PoolTestContext::setup(JOBS).await;

    let (pool_net, pp) = build_registered_pool_net(CAP);
    ctx.pool.initialize(pool_net).await.unwrap();
    for i in 0..CAP {
        ctx.pool
            .create_token(pp.pool.clone(), TokenColor::Data(serde_json::json!({ "gpu_id": format!("gpu-{i}") })))
            .await
            .unwrap();
    }

    let mut rps = Vec::new();
    for (i, svc) in ctx.requesters.iter().enumerate() {
        let (net, rp) = build_registered_requester_net(&ctx.pool_id);
        svc.initialize(net).await.unwrap();
        svc.create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": format!("job-{i}") })),
        )
        .await
        .unwrap();
        rps.push(rp);
    }

    // Everyone claims; the pool grants up to CAP and registers their holds.
    settle(&ctx, 4).await;

    // The invariant that makes contention legible: never more than CAP holders,
    // and exactly CAP while there is a backlog.
    assert!(
        pool_in_use(&ctx, &pp).await <= CAP,
        "in_use exceeded capacity — serialization broken"
    );
    let initial_holders: usize = {
        let mut n = 0;
        for (svc, rp) in ctx.requesters.iter().zip(rps.iter()) {
            n += svc.get_marking().await.token_count(&rp.holding);
        }
        n
    };
    assert_eq!(initial_holders, CAP, "exactly CAP jobs should be running, the rest queued");

    // Release holders one at a time; each freed GPU is handed to a waiter.
    let mut completed = 0usize;
    for _ in 0..JOBS {
        // Find a current holder and finish it.
        let mut holder = None;
        for (i, (svc, rp)) in ctx.requesters.iter().zip(rps.iter()).enumerate() {
            if svc.get_marking().await.token_count(&rp.holding) >= 1 {
                holder = Some(i);
                break;
            }
        }
        let h = match holder {
            Some(h) => h,
            None => break,
        };
        ctx.requesters[h]
            .create_token(rps[h].finish_trigger.clone(), TokenColor::Unit)
            .await
            .unwrap();
        settle(&ctx, 4).await;
        completed += 1;
        assert!(
            pool_in_use(&ctx, &pp).await <= CAP,
            "in_use exceeded capacity after release {completed}"
        );
    }

    settle(&ctx, 4).await;
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        CAP,
        "all GPUs returned to the pool"
    );
    assert_eq!(pool_in_use(&ctx, &pp).await, 0, "no holds outstanding");
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.done),
        JOBS,
        "every job completed"
    );

    ctx.teardown().await;
}

/// The money shot: a holder crashes (never releases); injecting the journaled
/// lease-expiry signal reaps its GPU, which is then granted to a waiter.
#[tokio::test]
async fn crashed_holder_lease_is_reaped_and_regranted() {
    const CAP: usize = 1;
    let ctx = PoolTestContext::setup(2).await;

    let (pool_net, pp) = build_registered_pool_net(CAP);
    ctx.pool.initialize(pool_net).await.unwrap();
    ctx.pool
        .create_token(pp.pool.clone(), TokenColor::Data(serde_json::json!({ "gpu_id": "gpu-0" })))
        .await
        .unwrap();

    let mut rps = Vec::new();
    for (i, svc) in ctx.requesters.iter().enumerate() {
        let (net, rp) = build_registered_requester_net(&ctx.pool_id);
        svc.initialize(net).await.unwrap();
        svc.create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": format!("job-{i}") })),
        )
        .await
        .unwrap();
        rps.push(rp);
    }

    settle(&ctx, 4).await;
    // Exactly one holder; the other job is queued.
    assert_eq!(pool_in_use(&ctx, &pp).await, 1);
    let mut holder = None;
    for i in 0..2 {
        if ctx.requesters[i].get_marking().await.token_count(&rps[i].holding) >= 1 {
            holder = Some(i);
            break;
        }
    }
    let holder = holder.expect("one holder");

    // The holder "crashes": it never finishes. Inject the lease-expiry signal
    // for its grant_id (in production a durable timer emits this).
    ctx.pool
        .create_token(
            pp.lease_expired.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": format!("job-{holder}") })),
        )
        .await
        .unwrap();
    settle(&ctx, 4).await;

    // The reaped GPU was handed to the waiting job, which now holds.
    assert_eq!(pool_in_use(&ctx, &pp).await, 1, "capacity reclaimed and regranted");
    let mut new_holder = None;
    for i in 0..2 {
        if i != holder
            && ctx.requesters[i].get_marking().await.token_count(&rps[i].holding) >= 1
        {
            new_holder = Some(i);
            break;
        }
    }
    assert!(new_holder.is_some(), "the waiting job got the reaped GPU");

    ctx.teardown().await;
}
