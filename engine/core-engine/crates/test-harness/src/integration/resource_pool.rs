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
//!     trip (not the single request/reply the scheduler relay net does), correlated back
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
    Arc as PetriArc, DomainEvent, Marking, PetriNet, Place, PlaceId, Port, TokenColor, Transition,
};
use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};
use petri_nats::{CrossNetBridge, NatsConfig, NatsEventPublisher};

use crate::nats::{ensure_global_stream, shared_nats_url};

type Svc =
    PetriNetService<NatsEventPublisher<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>;

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
    /// Join handle of the pool net's inbound cross-net bridge listener. Retained
    /// so a test can `abort()` it to model the pool net being hibernated
    /// (removed from the active set / no longer draining its bridge), then
    /// re-establish a fresh listener to model waking the pool. The bridge
    /// consumer is DURABLE (`durable_name`, `AckPolicy::Explicit`), so messages
    /// published while the listener task is gone stay buffered/unacked on the
    /// consumer and are delivered when a new listener re-binds — exactly the
    /// real wake-on-release path. See `restart_pool_listener`.
    pool_listener: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
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
        let requester_ids: Vec<String> = (0..n_requesters)
            .map(|i| format!("req-{i}-{suffix}"))
            .collect();

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

        // Start inbound bridge listeners for every net. The pool's handle is
        // retained (see `pool_listener`) so a test can hibernate/wake it.
        let mut pool_listener: Option<tokio::task::JoinHandle<()>> = None;
        let mut all: Vec<(&String, &Arc<Svc>)> = vec![(&pool_id, &pool)];
        for (id, svc) in requester_ids.iter().zip(requesters.iter()) {
            all.push((id, svc));
        }
        for (net_id, svc) in &all {
            let bridge = Arc::new(CrossNetBridge::new((*net_id).clone(), jetstream.clone()));
            let handle = bridge.start_inbound_listener((*svc).clone(), Arc::new(Notify::new()));
            if *net_id == &pool_id {
                pool_listener = Some(handle);
            }
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
            pool_listener: parking_lot::Mutex::new(pool_listener),
        }
    }

    /// Model the pool net hibernating: abort its inbound bridge listener task so
    /// the pool stops draining its `claim_inbox`/`release_inbox`. The DURABLE
    /// bridge consumer is left in place, so any token bridged to the pool while
    /// it is "hibernated" stays buffered/unacked on the consumer rather than
    /// being lost. This stands in for `NetRegistry::hibernate` removing the net
    /// from the active set + cancelling its eval loop (which the test-harness
    /// cannot drive directly: `petri-api`/`NetRegistry` is not a dependency of
    /// `petri-test-harness`). The pool service + its event log stay intact, the
    /// same way a hibernated net's durable state survives in production.
    fn hibernate_pool(&self) {
        if let Some(handle) = self.pool_listener.lock().take() {
            handle.abort();
        }
    }

    /// Model the pool net waking: re-establish a fresh inbound bridge listener
    /// bound to the SAME durable consumer (`bridge-inbound-{pool_id}`). On
    /// re-bind, JetStream redelivers the messages buffered while the pool was
    /// hibernated — the release published by the cancelled holder is consumed
    /// and applied, exactly the production wake-on-release path.
    fn wake_pool(&self) {
        let bridge = Arc::new(CrossNetBridge::new(
            self.pool_id.clone(),
            self.jetstream.clone(),
        ));
        let handle = bridge.start_inbound_listener(self.pool.clone(), Arc::new(Notify::new()));
        *self.pool_listener.lock() = Some(handle);
    }

    async fn teardown(&self) {
        if let Some(handle) = self.pool_listener.lock().take() {
            handle.abort();
        }
        let stream = match self.jetstream.get_stream("PETRI_GLOBAL").await {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut ids = vec![self.pool_id.clone()];
        ids.extend(self.requester_ids.iter().cloned());
        for net_id in &ids {
            let _ = stream
                .delete_consumer(&format!("bridge-inbound-{net_id}"))
                .await;
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
    let grant_outbox = Place::bridge_reply_channel("grant_outbox", "grant")
        .with_id(PlaceId::named("grant_outbox"));

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
    net.add_arc(PetriArc::input(
        claim_id.clone(),
        t_grant_id.clone(),
        "claim",
    ));
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
    net.add_arc(PetriArc::input(
        release_id.clone(),
        t_release_id.clone(),
        "req",
    ));
    net.add_arc(PetriArc::output(
        t_release_id.clone(),
        "cap",
        pool_id.clone(),
    ));
    net.add_arc(PetriArc::output(
        t_release_id.clone(),
        "done",
        done_id.clone(),
    ));

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
    let finish_trigger = Place::signal("finish_trigger").with_id(PlaceId::named("finish_trigger"));
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
    for p in [
        start,
        holding,
        done,
        claim_out,
        grant_inbox,
        release_out,
        finish_trigger,
    ] {
        net.add_place(p);
    }

    // t_claim: start → claim_out (start token already carries grant_id)
    let t_claim = Transition::new("t_claim", r#"#{ claim_out: start }"#)
        .with_input_port(Port::new("start"))
        .with_output_port(Port::new("claim_out"));
    let t_claim_id = t_claim.id.clone();
    net.add_transition(t_claim);
    net.add_arc(PetriArc::input(
        start_id.clone(),
        t_claim_id.clone(),
        "start",
    ));
    net.add_arc(PetriArc::output(
        t_claim_id.clone(),
        "claim_out",
        claim_out_id,
    ));

    // t_receive: grant_inbox → holding
    let t_receive = Transition::new("t_receive", r#"#{ holding: grant }"#)
        .with_input_port(Port::new("grant"))
        .with_output_port(Port::new("holding"));
    let t_receive_id = t_receive.id.clone();
    net.add_transition(t_receive);
    net.add_arc(PetriArc::input(
        grant_inbox_id.clone(),
        t_receive_id.clone(),
        "grant",
    ));
    net.add_arc(PetriArc::output(
        t_receive_id.clone(),
        "holding",
        holding_id.clone(),
    ));

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
    net.add_arc(PetriArc::input(
        holding_id.clone(),
        t_finish_id.clone(),
        "holding",
    ));
    net.add_arc(PetriArc::input(
        finish_trigger_id.clone(),
        t_finish_id.clone(),
        "trigger",
    ));
    net.add_arc(PetriArc::output(
        t_finish_id.clone(),
        "release",
        release_out_id,
    ));
    net.add_arc(PetriArc::output(
        t_finish_id.clone(),
        "local",
        done_id.clone(),
    ));

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
        .create_token(
            pp.pool.clone(),
            TokenColor::Data(serde_json::json!({ "gpu_id": "gpu-0" })),
        )
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
        ctx.requesters[h]
            .evaluate_until_quiescent(10)
            .await
            .unwrap();
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
    assert_eq!(
        pm.token_count(&pp.pool),
        POOL_CAPACITY,
        "capacity returned to pool"
    );
    assert_eq!(count_holders(&req_places).await, 0, "no holds outstanding");
    assert_eq!(
        pm.token_count(&pp.done),
        N_REQUESTERS,
        "every claim served once"
    );
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
        pool,
        in_use,
        done,
        claim_inbox,
        register_inbox,
        release_inbox,
        lease_expired,
        grant_outbox,
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
        start,
        holding,
        done,
        grant_inbox,
        finish_trigger,
        claim_out,
        register_out,
        release_out,
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
    net.add_arc(PetriArc::input(
        grant_inbox_id.clone(),
        trc.clone(),
        "grant",
    ));
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

/// Requester that claims → receives grant → registers its hold → holds, and
/// carries a `t_finally` FINALIZER (consume `holding` → emit release) exactly
/// like the mekhan compiler's lease-bridge `t_{id}_finally`. There is NO
/// `finish_trigger` / normal `t_exit` here: this models a leased scope whose
/// body never completes (the runner is mid-job when the user hits Cancel), so
/// the only way the held lease can ever return to the pool is the engine's
/// forced finalizer drain on `terminate`. This is the load-bearing shape the
/// holder-side `tests/finalizer_drain.rs` proves in isolation; here we wire its
/// release across the REAL cross-net bridge to a REAL pool net.
fn build_finalizer_requester_net(pool_net_id: &str) -> (PetriNet, RegReqPlaces) {
    let mut net = PetriNet::new();

    let start = Place::internal("start");
    let holding = Place::internal("holding");
    let done = Place::internal("done");
    let grant_inbox = Place::internal("grant_inbox");
    // Kept only so RegReqPlaces stays uniform; never fed in this scenario.
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
        start,
        holding,
        done,
        grant_inbox,
        finish_trigger,
        claim_out,
        register_out,
        release_out,
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
    net.add_arc(PetriArc::input(
        grant_inbox_id.clone(),
        trc.clone(),
        "grant",
    ));
    net.add_arc(PetriArc::output(trc.clone(), "holding", holding_id.clone()));
    net.add_arc(PetriArc::output(trc.clone(), "register", register_out_id));

    // t_finally: holding → release_out. FINALIZER — invisible to Normal
    // selection (otherwise it would release the lease the instant the hold is
    // acquired); fires ONLY during the engine's forced `drain_finalizers` on
    // teardown. Mirrors lease_bridge.rs `t_{id}_finally` exactly.
    let t_finally = Transition::new("t_finally", r#"#{ release: #{ grant_id: holding.grant_id } }"#)
        .with_input_port(Port::new("holding"))
        .with_output_port(Port::new("release"))
        .with_finalizer(true);
    let tfin = t_finally.id.clone();
    net.add_transition(t_finally);
    net.add_arc(PetriArc::input(holding_id.clone(), tfin.clone(), "holding"));
    net.add_arc(PetriArc::output(tfin.clone(), "release", release_out_id));

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
            .create_token(
                pp.pool.clone(),
                TokenColor::Data(serde_json::json!({ "gpu_id": format!("gpu-{i}") })),
            )
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
    assert_eq!(
        initial_holders, CAP,
        "exactly CAP jobs should be running, the rest queued"
    );

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
        .create_token(
            pp.pool.clone(),
            TokenColor::Data(serde_json::json!({ "gpu_id": "gpu-0" })),
        )
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
        if ctx.requesters[i]
            .get_marking()
            .await
            .token_count(&rps[i].holding)
            >= 1
        {
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
    assert_eq!(
        pool_in_use(&ctx, &pp).await,
        1,
        "capacity reclaimed and regranted"
    );
    let mut new_holder = None;
    for i in 0..2 {
        if i != holder
            && ctx.requesters[i]
                .get_marking()
                .await
                .token_count(&rps[i].holding)
                >= 1
        {
            new_holder = Some(i);
            break;
        }
    }
    assert!(new_holder.is_some(), "the waiting job got the reaped GPU");

    ctx.teardown().await;
}

// ===========================================================================
// Manual cancel (runner stays ONLINE) frees the held pool lease end-to-end.
//
// Reproduces the live incident: a user cancels a running instance from the UI.
// The remote worker is cancelled fine, BUT the runner stays online — so the
// presence/lease never EXPIRES and the pool's `t_reap` (lease_expired → in_use)
// never fires. The ONLY thing that can return the held unit to the pool is the
// holder's forced finalizer drain (what the engine's `terminate` runs before
// NetCancelled) emitting a release that crosses the bridge to the pool's
// `release_inbox`, where `t_release` returns the capacity.
//
// `tests/finalizer_drain.rs` proves the HOLDER half (the drain fires the
// finalizer and emits a release into a LOCAL stand-in place). These two tests
// close the gap it leaves: the release must actually traverse the REAL cross-net
// bridge and free a REAL pool net's `in_use` hold — the half the live failure
// was in ("worker cancel worked, pool-net lease stayed stuck").
// ===========================================================================

/// THE FIX, end to end: a holder whose body never completes is cancelled while
/// holding. A forced finalizer drain emits the release; it crosses the bridge;
/// the pool's `t_release` returns the unit. `in_use` goes back to 0 and the
/// capacity token is restored — WITHOUT any lease-expiry signal (the runner is
/// still online, so `t_reap` must NOT be the thing that saves us).
#[tokio::test]
async fn manual_cancel_while_holding_frees_pool_lease_via_finalizer_drain() {
    const CAP: usize = 1;
    let ctx = PoolTestContext::setup(1).await;

    let (pool_net, pp) = build_registered_pool_net(CAP);
    ctx.pool.initialize(pool_net).await.unwrap();
    ctx.pool
        .create_token(
            pp.pool.clone(),
            TokenColor::Data(serde_json::json!({ "gpu_id": "gpu-0" })),
        )
        .await
        .unwrap();

    let (req_net, rp) = build_finalizer_requester_net(&ctx.pool_id);
    ctx.requesters[0].initialize(req_net).await.unwrap();
    ctx.requesters[0]
        .create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": "job-0" })),
        )
        .await
        .unwrap();

    // The instance acquires + registers its hold; the body is now "running".
    settle(&ctx, 4).await;
    assert_eq!(pool_in_use(&ctx, &pp).await, 1, "lease held while running");
    assert_eq!(
        ctx.requesters[0]
            .get_marking()
            .await
            .token_count(&rp.holding),
        1,
        "the requester observably holds the unit"
    );

    // Manual cancel from the UI. The runner is STILL ONLINE — no lease_expired
    // signal is ever injected, so the pool's `t_reap` cannot fire. Normal
    // evaluation cannot release either (the finalizer is invisible to Normal
    // selection, and the body never completes). The forced drain is the engine's
    // `terminate` pre-teardown hook.
    ctx.requesters[0]
        .evaluate_until_quiescent(50)
        .await
        .unwrap();
    assert_eq!(
        pool_in_use(&ctx, &pp).await,
        1,
        "normal evaluation alone must NOT free the lease — only the forced drain can"
    );

    let fired = ctx.requesters[0].drain_finalizers().await;
    assert!(
        !fired.is_empty(),
        "the forced finalizer drain must fire the release on cancel"
    );

    // The released unit must cross the bridge and the pool must reclaim it.
    settle(&ctx, 4).await;
    assert_eq!(
        pool_in_use(&ctx, &pp).await,
        0,
        "cancel must free the pool lease (in_use back to 0), not strand it"
    );
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        CAP,
        "capacity must be restored to the pool so the next job can be granted"
    );

    ctx.teardown().await;
}

/// THE FAILURE SHAPE (regression guard): a holder with NO release finalizer —
/// the pre-fix lease-bridge, or any leased scope whose teardown does not drain a
/// finalizer — leaks the unit forever when cancelled with the runner online.
/// Neither normal evaluation nor a finalizer drain frees it, and because the
/// runner never expires, `t_reap` never fires. The unit is stranded in `in_use`,
/// which is exactly the stuck pool-net lease observed live. This test PINS that
/// failure mode so the only escape remains the finalizer wired above.
#[tokio::test]
async fn manual_cancel_without_finalizer_strands_pool_lease() {
    const CAP: usize = 1;
    let ctx = PoolTestContext::setup(1).await;

    let (pool_net, pp) = build_registered_pool_net(CAP);
    ctx.pool.initialize(pool_net).await.unwrap();
    ctx.pool
        .create_token(
            pp.pool.clone(),
            TokenColor::Data(serde_json::json!({ "gpu_id": "gpu-0" })),
        )
        .await
        .unwrap();

    // The stock requester releases ONLY via `t_finish` (gated on finish_trigger)
    // — it has no finalizer. We never fire the trigger (cancel, not completion).
    let (req_net, rp) = build_registered_requester_net(&ctx.pool_id);
    ctx.requesters[0].initialize(req_net).await.unwrap();
    ctx.requesters[0]
        .create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": "job-0" })),
        )
        .await
        .unwrap();

    settle(&ctx, 4).await;
    assert_eq!(pool_in_use(&ctx, &pp).await, 1, "lease held while running");

    // Cancel: drain finalizers (there are none) and settle. Runner stays online,
    // so no lease_expired is ever injected.
    let fired = ctx.requesters[0].drain_finalizers().await;
    assert!(
        fired.is_empty(),
        "no finalizer exists on this requester — nothing to drain"
    );
    settle(&ctx, 4).await;

    assert_eq!(
        pool_in_use(&ctx, &pp).await,
        1,
        "BUG SHAPE: with no finalizer and the runner online, the cancelled hold is \
         stranded in the pool's in_use forever — capacity never returns"
    );
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        CAP - 1,
        "the pool's free capacity stays depleted — the next job can never be granted"
    );

    ctx.teardown().await;
}

/// The REAL cancel entrypoint, end to end. The two tests above drive the *inner*
/// operation (`drain_finalizers`) directly; this one drives the engine's actual
/// `terminate` SEQUENCE — the production path a UI cancel takes — and proves
/// that sequence (not just a hand-call of the drain) frees the held pool lease.
///
/// `NetRegistry::terminate` (petri-api, `net_registry.rs`) does, in order:
///   1. cancel in-flight executor jobs,
///   2. `instance.service.drain_finalizers().await`   ← frees held leases,
///   3. `instance.service.append_event(NetCancelled)`  ← journals the cancel,
///   4. hibernate + delete snapshot.
///
/// RESIDUAL GAP (documented, unavoidable here): `terminate` lives on
/// `NetRegistry<E,T,S>` in the `petri-api` crate, which is NOT a dependency of
/// `petri-test-harness` (adding it would pull the whole Axum/registry stack into
/// the harness and is out of scope for this file). So we cannot call the literal
/// `registry.terminate(...)`. Instead `terminate_holder` below replays its
/// load-bearing core — steps 2 and 3 — against the holder service exactly as the
/// registry would. Steps 1 (executor cancel) and 4 (hibernate/snapshot) are
/// orthogonal to freeing the pool lease (the lease is freed by the step-2 drain
/// emitting a release across the bridge), so omitting them does not weaken what
/// this test proves: that running terminate's sequence — drain THEN NetCancelled
/// — returns the unit to the pool. Driving the literal registry `terminate`
/// against a deployed engine is deferred to live dogfood.
async fn terminate_holder(holder: &Svc, net_id: &str) -> Vec<petri_domain::PersistedEvent> {
    // Step 2: forced finalizer drain (the strand that releases held leases).
    let finalizer_events = holder.drain_finalizers().await;
    // Step 3: journal the cancel, exactly as `terminate` does after the drain.
    holder
        .append_event(DomainEvent::NetCancelled {
            net_id: net_id.to_string(),
            reason: Some("cancel".to_string()),
            cancelled_by: Some("test".to_string()),
        })
        .await
        .expect("append NetCancelled");
    finalizer_events
}

/// THE FIX via the REAL terminate sequence: a holder whose body never completes
/// is cancelled while holding by running terminate's drain→NetCancelled path
/// (`terminate_holder`). The drained finalizer's release crosses the bridge and
/// the pool's `t_release` returns the unit. `in_use` goes back to 0 and capacity
/// is restored — closing the seam the two tests above leave (they prove
/// drain→bridge→pool, but never that the terminate ENTRYPOINT actually invokes
/// the drain + journals the cancel).
#[tokio::test]
async fn terminate_while_holding_frees_pool_lease_end_to_end() {
    const CAP: usize = 1;
    let ctx = PoolTestContext::setup(1).await;

    let (pool_net, pp) = build_registered_pool_net(CAP);
    ctx.pool.initialize(pool_net).await.unwrap();
    ctx.pool
        .create_token(
            pp.pool.clone(),
            TokenColor::Data(serde_json::json!({ "gpu_id": "gpu-0" })),
        )
        .await
        .unwrap();

    let (req_net, rp) = build_finalizer_requester_net(&ctx.pool_id);
    ctx.requesters[0].initialize(req_net).await.unwrap();
    ctx.requesters[0]
        .create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": "job-0" })),
        )
        .await
        .unwrap();

    // The instance acquires + registers its hold; the body is now "running".
    settle(&ctx, 4).await;
    assert_eq!(pool_in_use(&ctx, &pp).await, 1, "lease held while running");
    assert_eq!(
        ctx.requesters[0]
            .get_marking()
            .await
            .token_count(&rp.holding),
        1,
        "the requester observably holds the unit"
    );

    // Drive the REAL terminate sequence (drain finalizers → NetCancelled). The
    // runner stays online, so no lease_expired is injected — only the drain run
    // by terminate can free the lease.
    let fired = terminate_holder(&ctx.requesters[0], &ctx.requester_ids[0]).await;
    assert!(
        !fired.is_empty(),
        "terminate must drain the release finalizer on cancel"
    );
    // The cancel was journaled as part of the terminate sequence.
    assert!(
        ctx.requesters[0]
            .get_events()
            .await
            .iter()
            .any(|e| matches!(&e.event, DomainEvent::NetCancelled { .. })),
        "terminate must journal NetCancelled after the drain"
    );

    // The released unit must cross the bridge and the pool must reclaim it.
    settle(&ctx, 4).await;
    assert_eq!(
        pool_in_use(&ctx, &pp).await,
        0,
        "terminate must free the pool lease (in_use back to 0), not strand it"
    );
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        CAP,
        "capacity must be restored to the pool so the next job can be granted"
    );

    ctx.teardown().await;
}

/// HIBERNATED POOL wakes on the cancel-release. Reproduces the residual concern
/// from the live incident: the holder is cancelled (its finalizer drains a
/// release) WHILE THE POOL NET IS HIBERNATED — removed from the active set, not
/// draining its bridge. The release must not be lost: it buffers on the pool's
/// durable bridge consumer, and when the pool WAKES, JetStream redelivers it,
/// the pool applies it, `t_release` fires, and the lease is freed.
///
/// `hibernate_pool` aborts the pool's inbound bridge listener (the durable
/// consumer survives); `wake_pool` re-binds a fresh listener to the same durable
/// consumer. This is the harness-reachable stand-in for
/// `NetRegistry::hibernate` → `get_or_create` (wake/rehydrate): the engine's
/// registry is not a harness dependency (see `terminate_while_holding...`), so
/// we approximate hibernation at the bridge-consumer layer, which is precisely
/// the layer the buffered release must survive across.
#[tokio::test]
async fn hibernated_pool_wakes_and_frees_lease_on_cancel_release() {
    const CAP: usize = 1;
    let ctx = PoolTestContext::setup(1).await;

    let (pool_net, pp) = build_registered_pool_net(CAP);
    ctx.pool.initialize(pool_net).await.unwrap();
    ctx.pool
        .create_token(
            pp.pool.clone(),
            TokenColor::Data(serde_json::json!({ "gpu_id": "gpu-0" })),
        )
        .await
        .unwrap();

    let (req_net, rp) = build_finalizer_requester_net(&ctx.pool_id);
    ctx.requesters[0].initialize(req_net).await.unwrap();
    ctx.requesters[0]
        .create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": "job-0" })),
        )
        .await
        .unwrap();

    // Acquire + register the hold while the pool is awake.
    settle(&ctx, 4).await;
    assert_eq!(pool_in_use(&ctx, &pp).await, 1, "lease held while running");

    // The pool HIBERNATES (stops draining its bridge). Its durable consumer
    // survives, so anything bridged to it from here is buffered, not lost.
    ctx.hibernate_pool();

    // Cancel the holder via the terminate sequence. The drained finalizer's
    // release is published to the pool's bridge subject — but the pool is
    // hibernated, so it lands in the durable consumer's backlog, unconsumed.
    let fired = terminate_holder(&ctx.requesters[0], &ctx.requester_ids[0]).await;
    assert!(!fired.is_empty(), "cancel must drain the release finalizer");
    // Let the requester actually publish the release across the bridge.
    for _ in 0..4 {
        ctx.requesters[0].evaluate_until_quiescent(20).await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
    }

    // While hibernated, the pool cannot have processed the release: the lease is
    // still held (this is the stuck-pool shape if it never woke).
    assert_eq!(
        pool_in_use(&ctx, &pp).await,
        1,
        "while hibernated the pool can't apply the release — lease still held"
    );

    // The pool WAKES: re-bind to the durable consumer. JetStream redelivers the
    // buffered release; the pool applies it and `t_release` frees the unit.
    ctx.wake_pool();
    settle(&ctx, 6).await;

    assert_eq!(
        pool_in_use(&ctx, &pp).await,
        0,
        "waking the pool must apply the buffered release and free the lease"
    );
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        CAP,
        "capacity restored after the woken pool processes the release"
    );

    ctx.teardown().await;
}

// ===========================================================================
// Presence / runner-pool variant: cancel frees the lease while the runner is
// ONLINE. Models the mekhan presence-pool topology (a runner's live presence is
// the capacity) rather than a seeded token pool: `presence_acquire` admits a
// unit carrying `{ unit_id, runner_id }`; a holder claims → registers → holds;
// a `presence_expired { runner_id }` signal reaps a HELD unit by routing a
// failure to the holder. The load-bearing case the test name pins is the
// COMPLEMENT of reap: the runner stays ONLINE (no presence_expired is ever
// injected, so `t_reap_held` must NOT be what saves us), the holder is
// cancelled, and the finalizer-drained release returns the presence unit to the
// pool — the exact runner-pool analogue of the seeded-pool fix above.
//
// FEASIBILITY (per the presence brief): a self-contained COMPILING + PASSING
// presence-style test is feasible by injecting `presence_acquire` over the
// bridge in place of mekhan's heartbeat (the live mekhan injector/sweep is NOT
// available to the harness). What is NOT modelled here — heartbeat parsing,
// per-runner TTL sweep, concurrency C>1, runner DB lookup — is exactly what the
// brief lists as live-only; this test covers the load-bearing topology
// (presence admission, hold, and cancel-release-frees) at C=1.
// ===========================================================================

/// Place ids on the presence-pool net the test observes. The bridge inboxes
/// (`claim_inbox`/`register_inbox`/`release_inbox`) and the `fail_outbox`/`done`
/// places are wired by name inside the builder and never need their ids out
/// here, so they are intentionally not surfaced.
struct PresencePoolPlaces {
    pool: PlaceId,
    in_use: PlaceId,
    presence_acquire: PlaceId,
    presence_expired: PlaceId,
}

/// Presence-pool net mirroring the mekhan presence branch (`pool_net.rs`
/// `CapacitySource::Presence`, Auto admission): ZERO seeded capacity;
/// `t_presence_acquire` admits a unit from a bridged `presence_acquire`;
/// `t_grant` hands a claim the unit; `t_register` parks the hold in `in_use`;
/// `t_release` returns the unit (resetting reply routing, the production rule);
/// `t_reap_held` routes a `presence_expired { runner_id }` failure to the held
/// unit's "fail" channel. `runner_id` threads through for reap correlation.
fn build_presence_pool_net() -> (PetriNet, PresencePoolPlaces) {
    let mut net = PetriNet::new();

    let pool = Place::internal("pool");
    let in_use = Place::internal("in_use");
    let done = Place::internal("done");
    let presence_acquire = Place::bridge_in("presence_acquire");
    let claim_inbox = Place::bridge_in("claim_inbox");
    let register_inbox = Place::bridge_in("register_inbox");
    let release_inbox = Place::bridge_in("release_inbox");
    let presence_expired = Place::signal("presence_expired");
    let grant_outbox = Place::bridge_reply_channel("grant_outbox", "grant");
    let fail_outbox = Place::bridge_reply_channel("fail_outbox", "fail");

    let pool_id = pool.id.clone();
    let in_use_id = in_use.id.clone();
    let done_id = done.id.clone();
    let acquire_id = presence_acquire.id.clone();
    let claim_id = claim_inbox.id.clone();
    let register_id = register_inbox.id.clone();
    let release_id = release_inbox.id.clone();
    let expired_id = presence_expired.id.clone();
    let grant_id = grant_outbox.id.clone();
    let fail_id = fail_outbox.id.clone();
    for p in [
        pool,
        in_use,
        done,
        presence_acquire,
        claim_inbox,
        register_inbox,
        release_inbox,
        presence_expired,
        grant_outbox,
        fail_outbox,
    ] {
        net.add_place(p);
    }

    // t_presence_acquire: presence_acquire → pool. Mints a free unit carrying
    // the runner identity (production admits one such unit per heartbeat slot).
    let t_acquire = Transition::new(
        "t_presence_acquire",
        r#"#{ unit: #{ unit_id: adm.unit_id, runner_id: adm.runner_id } }"#,
    )
    .with_input_port(Port::new("adm"))
    .with_output_port(Port::new("unit"));
    let ta = t_acquire.id.clone();
    net.add_transition(t_acquire);
    net.add_arc(PetriArc::input(acquire_id.clone(), ta.clone(), "adm"));
    net.add_arc(PetriArc::output(ta.clone(), "unit", pool_id.clone()));

    // t_grant: claim + pool → grant reply ONLY (carries unit_id + runner_id).
    let t_grant = Transition::new(
        "t_grant",
        r#"#{ grant: #{ grant_id: claim.grant_id, unit_id: unit.unit_id, runner_id: unit.runner_id } }"#,
    )
    .with_input_port(Port::new("claim"))
    .with_input_port(Port::new("unit"))
    .with_output_port(Port::new("grant"));
    let tg = t_grant.id.clone();
    net.add_transition(t_grant);
    net.add_arc(PetriArc::input(claim_id.clone(), tg.clone(), "claim"));
    net.add_arc(PetriArc::input(pool_id.clone(), tg.clone(), "unit"));
    net.add_arc(PetriArc::output(tg.clone(), "grant", grant_id));

    // t_register: register_inbox → in_use (CLEAN hold carrying runner_id).
    let t_register = Transition::new(
        "t_register",
        r#"#{ hold: #{ grant_id: reg.grant_id, unit_id: reg.unit_id, runner_id: reg.runner_id } }"#,
    )
    .with_input_port(Port::new("reg"))
    .with_output_port(Port::new("hold"));
    let tr = t_register.id.clone();
    net.add_transition(t_register);
    net.add_arc(PetriArc::input(register_id.clone(), tr.clone(), "reg"));
    net.add_arc(PetriArc::output(tr.clone(), "hold", in_use_id.clone()));

    // t_release: release + in_use (correlate grant_id) → pool + done. Returns
    // the presence unit (runner_id preserved so it can be reaped/reclaimed).
    let t_release = Transition::new(
        "t_release",
        r#"#{ unit: #{ unit_id: held.unit_id, runner_id: held.runner_id }, done: #{ grant_id: held.grant_id, outcome: "released" } }"#,
    )
    .with_input_port(Port::new("req"))
    .with_input_port(Port::new("held"))
    .with_guard("req.grant_id == held.grant_id")
    .with_output_port(Port::new("unit"))
    .with_output_port(Port::new("done"));
    let trel = t_release.id.clone();
    net.add_transition(t_release);
    net.add_arc(PetriArc::input(release_id.clone(), trel.clone(), "req"));
    net.add_arc(PetriArc::input(in_use_id.clone(), trel.clone(), "held"));
    net.add_arc(PetriArc::output(trel.clone(), "unit", pool_id.clone()));
    net.add_arc(PetriArc::output(trel.clone(), "done", done_id.clone()));

    // t_reap_held: presence_expired + in_use (correlate runner_id) → fail reply
    // + done. Production's held-unit reap: routes a failure to the holder over
    // the hold's "fail" channel. Present so the topology is faithful; this test
    // deliberately never injects presence_expired (runner stays ONLINE).
    let t_reap_held = Transition::new(
        "t_reap_held",
        r#"#{ fail: #{ runner_id: held.runner_id, unit_id: held.unit_id }, done: #{ grant_id: held.grant_id, outcome: "reaped" } }"#,
    )
    .with_input_port(Port::new("exp"))
    .with_input_port(Port::new("held"))
    .with_guard("exp.runner_id == held.runner_id")
    .with_output_port(Port::new("fail"))
    .with_output_port(Port::new("done"));
    let trh = t_reap_held.id.clone();
    net.add_transition(t_reap_held);
    net.add_arc(PetriArc::input(expired_id.clone(), trh.clone(), "exp"));
    net.add_arc(PetriArc::input(in_use_id.clone(), trh.clone(), "held"));
    net.add_arc(PetriArc::output(trh.clone(), "fail", fail_id.clone()));
    net.add_arc(PetriArc::output(trh.clone(), "done", done_id.clone()));

    (
        net,
        PresencePoolPlaces {
            pool: pool_id,
            in_use: in_use_id,
            presence_acquire: acquire_id,
            presence_expired: expired_id,
        },
    )
}

/// Presence holder: claims → receives the presence grant (holds + echoes the
/// register) → carries a `t_finally` FINALIZER that releases on cancel (the same
/// leased-scope-with-no-normal-exit shape as `build_finalizer_requester_net`,
/// but echoing the presence unit_id + runner_id to register).
fn build_presence_holder_net(pool_net_id: &str) -> (PetriNet, RegReqPlaces) {
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
        start,
        holding,
        done,
        grant_inbox,
        finish_trigger,
        claim_out,
        register_out,
        release_out,
    ] {
        net.add_place(p);
    }

    let t_claim = Transition::new("t_claim", r#"#{ claim_out: start }"#)
        .with_input_port(Port::new("start"))
        .with_output_port(Port::new("claim_out"));
    let tc = t_claim.id.clone();
    net.add_transition(t_claim);
    net.add_arc(PetriArc::input(start_id.clone(), tc.clone(), "start"));
    net.add_arc(PetriArc::output(tc.clone(), "claim_out", claim_out_id));

    let t_receive = Transition::new(
        "t_receive",
        r#"#{ holding: grant, register: #{ grant_id: grant.grant_id, unit_id: grant.unit_id, runner_id: grant.runner_id } }"#,
    )
    .with_input_port(Port::new("grant"))
    .with_output_port(Port::new("holding"))
    .with_output_port(Port::new("register"));
    let trc = t_receive.id.clone();
    net.add_transition(t_receive);
    net.add_arc(PetriArc::input(
        grant_inbox_id.clone(),
        trc.clone(),
        "grant",
    ));
    net.add_arc(PetriArc::output(trc.clone(), "holding", holding_id.clone()));
    net.add_arc(PetriArc::output(trc.clone(), "register", register_out_id));

    // t_finally: holding → release_out. FINALIZER — fires ONLY on the forced
    // drain (cancel), never during normal evaluation. Mirrors the lease-bridge.
    let t_finally =
        Transition::new("t_finally", r#"#{ release: #{ grant_id: holding.grant_id } }"#)
            .with_input_port(Port::new("holding"))
            .with_output_port(Port::new("release"))
            .with_finalizer(true);
    let tfin = t_finally.id.clone();
    net.add_transition(t_finally);
    net.add_arc(PetriArc::input(holding_id.clone(), tfin.clone(), "holding"));
    net.add_arc(PetriArc::output(tfin.clone(), "release", release_out_id));

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

/// Runner-pool analogue of the seeded-pool fix: a runner is ONLINE (its presence
/// admitted a unit), a holder leases it, and the holder is cancelled mid-job.
/// No `presence_expired` is ever injected (the runner never goes away), so the
/// pool's `t_reap_held` cannot be what frees the unit — only the holder's
/// finalizer-drained release can. This proves cancel returns the live runner's
/// presence unit to the pool so the next claim can be granted.
#[tokio::test]
async fn presence_runner_pool_frees_lease_on_cancel_while_online() {
    let ctx = PoolTestContext::setup(1).await;

    let (pool_net, pp) = build_presence_pool_net();
    ctx.pool.initialize(pool_net).await.unwrap();

    // Initialize the holder net up front (no start token yet) so `settle` — which
    // evaluates every requester — has a topology to drive. The holder stays inert
    // until its start token is seeded below.
    let (req_net, rp) = build_presence_holder_net(&ctx.pool_id);
    ctx.requesters[0].initialize(req_net).await.unwrap();

    // Admit one presence unit over the bridge — the harness stand-in for a
    // mekhan heartbeat `presence_acquire` injection for an ONLINE runner.
    ctx.pool
        .create_token(
            pp.presence_acquire.clone(),
            TokenColor::Data(serde_json::json!({
                "unit_id": "runner-a#0",
                "runner_id": "runner-a"
            })),
        )
        .await
        .unwrap();
    settle(&ctx, 2).await;
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        1,
        "the online runner's presence admitted one free unit"
    );

    // The holder now claims and leases the presence unit.
    ctx.requesters[0]
        .create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": "job-0" })),
        )
        .await
        .unwrap();

    settle(&ctx, 4).await;
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.in_use),
        1,
        "the runner's presence unit is held while running"
    );
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        0,
        "no free units while the only one is held"
    );

    // Cancel the holder via the terminate sequence. The runner is STILL ONLINE,
    // so no presence_expired is injected — `t_reap_held` cannot fire. Only the
    // drained finalizer's release can return the presence unit.
    let fired = terminate_holder(&ctx.requesters[0], &ctx.requester_ids[0]).await;
    assert!(!fired.is_empty(), "cancel must drain the release finalizer");
    settle(&ctx, 6).await;

    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.in_use),
        0,
        "cancel must free the runner-pool lease while the runner stays online"
    );
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        1,
        "the presence unit returns to the pool so the next claim can be granted"
    );
    // The fail channel must NOT have fired — this was a clean release, not a reap.
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.presence_expired),
        0,
        "no presence_expired was injected: the unit returned by release, not reap"
    );

    ctx.teardown().await;
}

// ===========================================================================
// DRIFT RECOVERY — the EXACT live incident the happy-path tests above miss, and
// the contract the reconciler fix relies on.
//
// `presence_runner_pool_frees_lease_on_cancel_while_online` cancels a CLEAN held
// lease and proves the finalizer drain returns it. That path was always green —
// and the live incident never took it. By cancel time the pool was already
// `pool=0, in_use=0`: the held unit had VANISHED (a pool-net hibernate/rehydrate
// or OOM restart that didn't restore the in-memory token, or an engine-side reap
// that emitted no compensating release). So `leases_reclaimed: 0`, the drain was
// a no-op with nothing to release, and recovery depended on RE-ESTABLISHING
// capacity from the still-online runner — not on releasing an in_use token.
//
// The engine pool net is deliberately dumb token-accounting: it has NO notion of
// "this runner is still online", so it cannot and must not self-heal lost
// capacity. Recovery is mekhan's job — its presence heartbeat is a level-triggered
// reconciler (`presence/runners.rs::handle_presence` → `slots_to_reconcile`): it
// observes the runner has fewer than C units in the engine and re-mints the
// deficit via a fresh `presence_acquire`. This test pins the ENGINE half of that
// contract: a fresh acquire after a loss restores the pool to EXACTLY one free
// unit, and the cancel's stranded release never spuriously inflates it. The mekhan
// half (the heartbeat computing the right deficit) is pinned by
// `presence::runners::tests::reconcile_tops_up_lost_units_and_stays_lazy_on_shrink`.
// ===========================================================================

/// Capacity LOST (not held), runner ONLINE: cancel alone cannot restore it (the
/// engine has no presence knowledge), and the reconciler's re-mint converges the
/// pool back to one free unit. Distinct from the happy-path presence test, which
/// cancels a clean held lease.
#[tokio::test]
async fn reconcile_restores_pool_after_capacity_lost_then_cancelled_online() {
    let ctx = PoolTestContext::setup(1).await;

    let (pool_net, pp) = build_presence_pool_net();
    ctx.pool.initialize(pool_net).await.unwrap();

    // Initialize the holder up front so `settle` has a topology to drive.
    let (req_net, rp) = build_presence_holder_net(&ctx.pool_id);
    ctx.requesters[0].initialize(req_net).await.unwrap();

    // The online runner's heartbeat admits one presence unit (harness stand-in
    // for a mekhan `presence_acquire` injection).
    ctx.pool
        .create_token(
            pp.presence_acquire.clone(),
            TokenColor::Data(serde_json::json!({
                "unit_id": "runner-a#0",
                "runner_id": "runner-a"
            })),
        )
        .await
        .unwrap();
    settle(&ctx, 2).await;
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        1,
        "the online runner admitted one free unit"
    );

    // The holder claims and leases the unit (in_use=1, pool=0).
    ctx.requesters[0]
        .create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": "job-0" })),
        )
        .await
        .unwrap();
    settle(&ctx, 4).await;
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.in_use),
        1,
        "the presence unit is held while running"
    );
    assert_eq!(ctx.pool.get_marking().await.token_count(&pp.pool), 0);

    // --- CAPACITY IS LOST ---------------------------------------------------
    // The held unit VANISHES with no release: model the pool-net lifecycle drop
    // (hibernate/rehydrate or OOM restart that didn't restore the in-memory
    // token, or an engine-side held-unit reap that emitted no compensating
    // release). The pool now matches the live incident's degraded shape.
    let held_id = ctx
        .pool
        .get_marking()
        .await
        .tokens_at(&pp.in_use)
        .iter()
        .map(|t| t.id.clone())
        .next()
        .expect("the held unit token");
    ctx.pool
        .remove_token(
            pp.in_use.clone(),
            Some(held_id),
            None,
            Some("lifecycle drop".to_string()),
        )
        .await
        .unwrap();
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.in_use),
        0,
        "held unit lost"
    );
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        0,
        "DEGRADED: pool=0, in_use=0 — the exact live incident state"
    );

    // --- CANCEL, runner still ONLINE ---------------------------------------
    // The UI cancel runs the terminate sequence: drain finalizers, then journal
    // NetCancelled. The drained release bridges to the pool's `release_inbox`,
    // but there is no in_use token to correlate with, so `t_release` cannot fire
    // — the release strands. No presence_expired is injected (runner is online),
    // so `t_reap_held` cannot fire either.
    let fired = terminate_holder(&ctx.requesters[0], &ctx.requester_ids[0]).await;
    assert!(
        !fired.is_empty(),
        "cancel still drains the release finalizer — but it has nothing to free"
    );
    settle(&ctx, 6).await;

    // Cancel ALONE cannot restore lost capacity: the engine pool net has no notion
    // that the runner is still online, and the drained release strands in
    // `release_inbox` with no in_use token to correlate against. This is correct
    // engine behaviour — and exactly why a reconciler is required.
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        0,
        "cancel alone does not restore lost capacity — recovery needs the reconciler"
    );

    // --- RECONCILER RECOVERY -----------------------------------------------
    // mekhan's presence reconciler observes (next heartbeat, ~10s) that the online
    // runner has 0 units in the engine — below its target C — and re-mints the
    // deficit. Modelled here by a fresh `presence_acquire` for a NEW episode (the
    // `@2` epoch), the harness stand-in for `slots_to_reconcile`'s injection.
    ctx.pool
        .create_token(
            pp.presence_acquire.clone(),
            TokenColor::Data(serde_json::json!({
                "unit_id": "runner-a#0@2",
                "runner_id": "runner-a"
            })),
        )
        .await
        .unwrap();
    settle(&ctx, 6).await;

    // CONVERGENCE INVARIANT: one online runner ⇒ exactly one free unit available
    // for the next claim. The stranded release (grant_id `job-0`) must NOT
    // spuriously fire — `t_release`'s grant_id correlation keeps it inert, so the
    // pool lands at 1, never 2.
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        1,
        "the reconciler's re-mint restores exactly one free unit for the online runner"
    );
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.in_use),
        0,
        "no held unit — the stranded release never spuriously fired"
    );

    ctx.teardown().await;
}

// ===========================================================================
// R3 — tokens backend: the mekhan `build_token_pool_net` contract end to end.
//
// This proves the chain the R3 milestone delivers: mekhan's parameterized
// token-pool net (capacity N, `pool-<resource_id>`) contended for by K
// instances whose claim/grant/register/release follow the **R2 compiled
// contract** exactly:
//
//   * the grant reply is the TYPED LEASE `{ grant_id, unit_id }` — R1's
//     `TokenPoolLease { unit_id }` + R2's `Lease__token_pool` schema. `unit_id`
//     (NOT `gpu_id`) is the body-visible lease field; `grant_id` rides for
//     correlation. (Field-name alignment is pinned on the mekhan side by
//     `mekhan_service::petri::pool_net::tests::grant_reply_is_typed_lease_unit_id`.)
//   * the claim carries `{ grant_id, request }` (R2's `t_claim`). v1 grants one
//     unit per claim and ignores `request` — asserted here by sending a
//     non-trivial `request` and confirming the grant still flows.
//   * register echoes `{ grant_id, unit_id }` over a PLAIN bridge; release is
//     `{ grant_id }`.
//
// FIDELITY: the pool net + requesters here are hand-built domain `PetriNet`s
// that MIRROR mekhan's builder + R2 lowering (a literal cross-workspace call
// is impossible — `petri-test-harness` is in the engine workspace and cannot
// depend on `mekhan-service`). The mekhan-side unit tests pin the real builder
// AIR to this same contract, so the two cannot drift silently. Loading the
// actual mekhan-compiled instance AIR is deferred to live dogfood on the dev
// stack (R5).
// ===========================================================================

/// Pool net mirroring `mekhan_service::petri::pool_net::build_token_pool_net`:
/// `unit_id`-typed lease, request-tolerant `t_grant`, registration pattern.
fn build_token_pool_net_mirror() -> (PetriNet, RegPoolPlaces) {
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
        pool,
        in_use,
        done,
        claim_inbox,
        register_inbox,
        release_inbox,
        lease_expired,
        grant_outbox,
    ] {
        net.add_place(p);
    }

    // t_grant: claim + cap → grant reply ONLY. The grant IS the typed lease
    // `{ grant_id, unit_id }`. `claim.request` is intentionally not read (v1
    // grants one unit/claim); a present `request` field is ignored, never a
    // fault.
    let t_grant = Transition::new(
        "t_grant",
        r#"#{ grant: #{ grant_id: claim.grant_id, unit_id: cap.unit_id } }"#,
    )
    .with_input_port(Port::new("claim"))
    .with_input_port(Port::new("cap"))
    .with_output_port(Port::new("grant"));
    let tg = t_grant.id.clone();
    net.add_transition(t_grant);
    net.add_arc(PetriArc::input(claim_id.clone(), tg.clone(), "claim"));
    net.add_arc(PetriArc::input(pool_id.clone(), tg.clone(), "cap"));
    net.add_arc(PetriArc::output(tg.clone(), "grant", grant_id));

    // t_register: register_inbox → in_use (CLEAN hold, carries unit_id).
    let t_register = Transition::new(
        "t_register",
        r#"#{ hold: #{ grant_id: reg.grant_id, unit_id: reg.unit_id } }"#,
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
        r#"#{ cap: #{ unit_id: held.unit_id }, done: #{ grant_id: held.grant_id, outcome: "released" } }"#,
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
        r#"#{ cap: #{ unit_id: held.unit_id }, done: #{ grant_id: held.grant_id, outcome: "reaped" } }"#,
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

/// Requester mirroring R2's `lower_automated_step_pooled` (alias branch):
/// claim carries `{ grant_id, request }`; on grant, holds + registers the lease
/// echo `{ grant_id, unit_id }`; on finish, releases `{ grant_id }`. The
/// `holding` token IS the typed lease (so the test can assert `unit_id`).
fn build_r2_contract_requester_net(pool_net_id: &str) -> (PetriNet, RegReqPlaces) {
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
        start,
        holding,
        done,
        grant_inbox,
        finish_trigger,
        claim_out,
        register_out,
        release_out,
    ] {
        net.add_place(p);
    }

    // t_claim: start → claim_out. The claim carries `{ grant_id, request }`
    // exactly like R2's `t_claim` (`#{ grant_id: gid, request: <...> }`). The
    // start token already holds both.
    let t_claim = Transition::new("t_claim", r#"#{ claim_out: start }"#)
        .with_input_port(Port::new("start"))
        .with_output_port(Port::new("claim_out"));
    let tc = t_claim.id.clone();
    net.add_transition(t_claim);
    net.add_arc(PetriArc::input(start_id.clone(), tc.clone(), "start"));
    net.add_arc(PetriArc::output(tc.clone(), "claim_out", claim_out_id));

    // t_receive: grant_inbox → holding + register_out. R2's `t_acquire` parks
    // the WHOLE grant on `p_held` (`held: grant`) and echoes `reg: grant`, so
    // `holding` is the full typed lease `{ grant_id, unit_id }`.
    let t_receive = Transition::new(
        "t_receive",
        r#"#{ holding: grant, register: #{ grant_id: grant.grant_id, unit_id: grant.unit_id } }"#,
    )
    .with_input_port(Port::new("grant"))
    .with_output_port(Port::new("holding"))
    .with_output_port(Port::new("register"));
    let trc = t_receive.id.clone();
    net.add_transition(t_receive);
    net.add_arc(PetriArc::input(
        grant_inbox_id.clone(),
        trc.clone(),
        "grant",
    ));
    net.add_arc(PetriArc::output(trc.clone(), "holding", holding_id.clone()));
    net.add_arc(PetriArc::output(trc.clone(), "register", register_out_id));

    // t_finish: holding + finish_trigger → release_out + done. R2's
    // `t_to_output`/`t_to_error`: release is `{ grant_id: held.grant_id }`.
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

/// R3 headline: mekhan's token-pool contract, capacity=2, K=4 contending
/// instances. Proves contention (≤2 hold, 2 queue), the typed `{ unit_id }`
/// lease routes back to each requester, releases free waiters, all 4 complete,
/// and capacity is conserved.
#[tokio::test]
async fn tokens_backend_r2_contract_two_capacity_four_jobs() {
    const CAP: usize = 2;
    const JOBS: usize = 4;
    let ctx = PoolTestContext::setup(JOBS).await;

    let (pool_net, pp) = build_token_pool_net_mirror();
    ctx.pool.initialize(pool_net).await.unwrap();
    // Seed CAP clean `{ unit_id }` capacity tokens — exactly how the mekhan
    // builder's `seed_one(&pool, DynamicToken({ unit_id: "unit-i" }))` does.
    for i in 0..CAP {
        ctx.pool
            .create_token(
                pp.pool.clone(),
                TokenColor::Data(serde_json::json!({ "unit_id": format!("unit-{i}") })),
            )
            .await
            .unwrap();
    }

    let mut rps = Vec::new();
    for (i, svc) in ctx.requesters.iter().enumerate() {
        let (net, rp) = build_r2_contract_requester_net(&ctx.pool_id);
        svc.initialize(net).await.unwrap();
        // Start token carries grant_id AND a non-trivial `request` (the R2
        // claim shape). v1 ignores `request`; we include it to prove the pool
        // doesn't choke on its presence.
        svc.create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({
                "grant_id": format!("job-{i}"),
                "request": { "units": 1 }
            })),
        )
        .await
        .unwrap();
        rps.push(rp);
    }

    // Everyone claims; the pool grants up to CAP and registers their holds.
    settle(&ctx, 4).await;

    // CONTENTION: never more than CAP holders; exactly CAP while backlogged.
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
    assert_eq!(
        initial_holders, CAP,
        "exactly CAP jobs running, the rest queued"
    );

    // TYPED LEASE: every current holder's `holding` token carries a `unit_id`
    // (the R1/R2 lease field) drawn from the seeded pool — never `gpu_id`.
    let mut seen_units = std::collections::HashSet::new();
    for (svc, rp) in ctx.requesters.iter().zip(rps.iter()) {
        let m = svc.get_marking().await;
        for tok in m.tokens_at(&rp.holding) {
            if let TokenColor::Data(d) = &tok.color {
                let unit = d
                    .get("unit_id")
                    .and_then(|v| v.as_str())
                    .expect("holding token must carry the typed lease field `unit_id`");
                assert!(
                    unit.starts_with("unit-"),
                    "lease unit_id must come from the seeded pool, got {unit}"
                );
                assert!(
                    d.get("gpu_id").is_none(),
                    "lease must be unit_id-typed, not gpu_id"
                );
                assert!(
                    seen_units.insert(unit.to_string()),
                    "two holders share a unit — mutex broken"
                );
            }
        }
    }
    assert_eq!(seen_units.len(), CAP, "CAP distinct units leased");

    // Release holders one at a time; each freed unit is handed to a waiter.
    let mut completed = 0usize;
    for _ in 0..JOBS {
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

    // CONSERVATION: all units back in the pool, no holds, every job done once.
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.pool),
        CAP,
        "all units returned to the pool"
    );
    assert_eq!(pool_in_use(&ctx, &pp).await, 0, "no holds outstanding");
    assert_eq!(
        ctx.pool.get_marking().await.token_count(&pp.done),
        JOBS,
        "every job completed exactly once"
    );
    for (i, rp) in rps.iter().enumerate() {
        assert_eq!(
            ctx.requesters[i].get_marking().await.token_count(&rp.done),
            1,
            "requester {i} completed"
        );
    }

    ctx.teardown().await;
}

// ===========================================================================
// R4c — scheduler backend: the datacenter lease-adapter end to end.
//
// Proves the R4a `resource_lease` effects + the R4b adapter topology against a
// MOCK HTTP allocator over real NATS. The adapter net here is a hand-built
// PetriNet mirroring `mekhan_service::petri::pool_net::build_datacenter_lease_adapter_net`
// (claim → resource_lease_acquire effect → grant; register → in_use carrying
// alloc_id; release prep-join → resource_lease_release effect; reap drops the
// hold) — its AIR is pinned to the mekhan builder by R4b's unit tests, closing
// drift. The R4a effect HANDLERS are the real `petri_application` ones, driven
// by a real `HttpAllocatorClient` pointed at a mock allocator.
//
// FIDELITY: hand-built adapter mirror + hand-built requesters (test-harness
// can't depend on mekhan-service — workspace cycle, same as R3). The literal
// mekhan-compiled instance + mekhan-built+auto-deployed adapter against a real
// allocator is deferred to live dogfood (R5).
// ===========================================================================

use petri_application::resource_lease_handlers::{
    HttpAllocatorClient, ResourceLeaseAcquireHandler, ResourceLeaseReleaseHandler,
};

/// A mock HTTP allocator. POST grants a fresh `{alloc_id,node,expiry,scheduler}`
/// (alloc-N, node-N); DELETE `/leases/<alloc_id>` records the release. Runs
/// for the test's lifetime on a `tokio` accept loop (no `wiremock`/`hyper`
/// dev-dep — same hand-rolled TCP approach as R4a's unit test).
struct MockAllocator {
    addr: std::net::SocketAddr,
    granted: Arc<std::sync::Mutex<Vec<String>>>,
    released: Arc<std::sync::Mutex<Vec<String>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl MockAllocator {
    async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let granted = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let released = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let granted_srv = granted.clone();
        let released_srv = released.clone();
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let task = tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let granted_c = granted_srv.clone();
                let released_c = released_srv.clone();
                let counter_c = counter.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                    let first_line = raw.lines().next().unwrap_or("");
                    let body = if first_line.starts_with("POST") {
                        let i = counter_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let alloc_id = format!("alloc-{i}");
                        granted_c.lock().unwrap().push(alloc_id.clone());
                        format!(
                            r#"{{"alloc_id":"{alloc_id}","node":"node-{i}","expiry":"2026-12-31T00:00:00Z","scheduler":{{"flavor":"http"}}}}"#
                        )
                    } else if first_line.starts_with("DELETE") {
                        // DELETE /leases/<alloc_id>
                        let path = first_line.split_whitespace().nth(1).unwrap_or("");
                        let alloc_id = path.rsplit('/').next().unwrap_or("").to_string();
                        released_c.lock().unwrap().push(alloc_id);
                        "{}".to_string()
                    } else {
                        "{}".to_string()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });

        Self {
            addr,
            granted,
            released,
            _task: task,
        }
    }

    fn url(&self) -> String {
        format!("http://{}/leases", self.addr)
    }
    fn granted(&self) -> Vec<String> {
        self.granted.lock().unwrap().clone()
    }
    fn released(&self) -> Vec<String> {
        self.released.lock().unwrap().clone()
    }
}

/// Datacenter adapter net mirroring `build_datacenter_lease_adapter_net`:
/// claim → `resource_lease_acquire` effect (effect_config = allocator url+token)
/// → grant_outbox; register → in_use (carries alloc_id); release prep-join
/// (release_inbox + in_use → {grant_id, alloc_id}) → `resource_lease_release`
/// effect; reap drops the hold. `effect_config` points at the mock allocator.
fn build_datacenter_adapter_net_mirror(allocator_url: &str) -> (PetriNet, RegPoolPlaces) {
    let mut net = PetriNet::new();

    let in_use = Place::internal("in_use");
    let done = Place::internal("done");
    let release_prep = Place::internal("release_prep");
    let claim_inbox = Place::bridge_in("claim_inbox");
    let register_inbox = Place::bridge_in("register_inbox");
    let release_inbox = Place::bridge_in("release_inbox");
    let lease_expired = Place::signal("lease_expired");
    let grant_outbox = Place::bridge_reply_channel("grant_outbox", "grant");

    let in_use_id = in_use.id.clone();
    let done_id = done.id.clone();
    let release_prep_id = release_prep.id.clone();
    let claim_id = claim_inbox.id.clone();
    let register_id = register_inbox.id.clone();
    let release_id = release_inbox.id.clone();
    let lease_id = lease_expired.id.clone();
    let grant_id = grant_outbox.id.clone();
    for p in [
        in_use,
        done,
        release_prep,
        claim_inbox,
        register_inbox,
        release_inbox,
        lease_expired,
        grant_outbox,
    ] {
        net.add_place(p);
    }

    let effect_config = serde_json::json!({ "allocator_url": allocator_url, "token": "" });

    // t_request: claim → resource_lease_acquire effect → grant (lease reply).
    let t_request = Transition::new("t_request", "#{}")
        .with_effect_handler("resource_lease_acquire")
        .with_effect_config(effect_config.clone())
        .with_input_port(Port::new("request"))
        .with_output_port(Port::new("lease"));
    let trq = t_request.id.clone();
    net.add_transition(t_request);
    net.add_arc(PetriArc::input(claim_id.clone(), trq.clone(), "request"));
    net.add_arc(PetriArc::output(trq.clone(), "lease", grant_id));

    // t_register: register_inbox → in_use (CLEAN hold carrying alloc_id + lease).
    let t_register = Transition::new(
        "t_register",
        r#"#{ hold: #{ grant_id: reg.grant_id, alloc_id: reg.alloc_id, node: reg.node, expiry: reg.expiry, scheduler: reg.scheduler } }"#,
    )
    .with_input_port(Port::new("reg"))
    .with_output_port(Port::new("hold"));
    let trg = t_register.id.clone();
    net.add_transition(t_register);
    net.add_arc(PetriArc::input(register_id.clone(), trg.clone(), "reg"));
    net.add_arc(PetriArc::output(trg.clone(), "hold", in_use_id.clone()));

    // t_release_prep: release_inbox + in_use (correlate grant_id) →
    // {grant_id, alloc_id} on release_prep + done record.
    let t_release_prep = Transition::new(
        "t_release_prep",
        r#"#{ release: #{ grant_id: held.grant_id, alloc_id: held.alloc_id }, done: #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "released" } }"#,
    )
    .with_input_port(Port::new("req"))
    .with_input_port(Port::new("held"))
    .with_guard("req.grant_id == held.grant_id")
    .with_output_port(Port::new("release"))
    .with_output_port(Port::new("done"));
    let trp = t_release_prep.id.clone();
    net.add_transition(t_release_prep);
    net.add_arc(PetriArc::input(release_id.clone(), trp.clone(), "req"));
    net.add_arc(PetriArc::input(in_use_id.clone(), trp.clone(), "held"));
    net.add_arc(PetriArc::output(
        trp.clone(),
        "release",
        release_prep_id.clone(),
    ));
    net.add_arc(PetriArc::output(trp.clone(), "done", done_id.clone()));

    // t_release: release_prep → resource_lease_release effect (DELETE alloc).
    let t_release = Transition::new("t_release", "#{}")
        .with_effect_handler("resource_lease_release")
        .with_effect_config(effect_config.clone())
        .with_input_port(Port::new("release"))
        .with_output_port(Port::new("released"));
    let trl = t_release.id.clone();
    net.add_transition(t_release);
    net.add_arc(PetriArc::input(
        release_prep_id.clone(),
        trl.clone(),
        "release",
    ));
    net.add_arc(PetriArc::output(trl.clone(), "released", done_id.clone()));

    // t_reap: lease_expired + in_use (correlate grant_id) → drop hold.
    let t_reap = Transition::new(
        "t_reap",
        r#"#{ done: #{ grant_id: held.grant_id, alloc_id: held.alloc_id, outcome: "reaped" } }"#,
    )
    .with_input_port(Port::new("exp"))
    .with_input_port(Port::new("held"))
    .with_guard("exp.grant_id == held.grant_id")
    .with_output_port(Port::new("done"));
    let trp2 = t_reap.id.clone();
    net.add_transition(t_reap);
    net.add_arc(PetriArc::input(lease_id.clone(), trp2.clone(), "exp"));
    net.add_arc(PetriArc::input(in_use_id.clone(), trp2.clone(), "held"));
    net.add_arc(PetriArc::output(trp2.clone(), "done", done_id.clone()));

    (
        net,
        RegPoolPlaces {
            pool: in_use_id.clone(), // no capacity pool for a lease adapter; reuse field for in_use
            in_use: in_use_id,
            claim_inbox: claim_id,
            register_inbox: register_id,
            release_inbox: release_id,
            lease_expired: lease_id,
            done: done_id,
        },
    )
}

/// Datacenter requester mirroring R2's instance claim contract for the lease
/// kind: claim `{grant_id, request}`; on grant, holds + echoes the WHOLE
/// datacenter lease (incl. alloc_id) to register; on finish, releases
/// `{grant_id}`.
fn build_datacenter_requester_net(adapter_net_id: &str) -> (PetriNet, RegReqPlaces) {
    let mut net = PetriNet::new();

    let start = Place::internal("start");
    let holding = Place::internal("holding");
    let done = Place::internal("done");
    let grant_inbox = Place::internal("grant_inbox");
    let finish_trigger = Place::signal("finish_trigger");
    let mut channels = HashMap::new();
    channels.insert("grant".to_string(), "grant_inbox".to_string());
    let claim_out =
        Place::bridge_out_reply_channels("claim_out", adapter_net_id, "claim_inbox", channels);
    let register_out = Place::bridge_out("register_out", adapter_net_id, "register_inbox");
    let release_out = Place::bridge_out("release_out", adapter_net_id, "release_inbox");

    let start_id = start.id.clone();
    let holding_id = holding.id.clone();
    let done_id = done.id.clone();
    let grant_inbox_id = grant_inbox.id.clone();
    let finish_id = finish_trigger.id.clone();
    let claim_out_id = claim_out.id.clone();
    let register_out_id = register_out.id.clone();
    let release_out_id = release_out.id.clone();
    for p in [
        start,
        holding,
        done,
        grant_inbox,
        finish_trigger,
        claim_out,
        register_out,
        release_out,
    ] {
        net.add_place(p);
    }

    let t_claim = Transition::new("t_claim", r#"#{ claim_out: start }"#)
        .with_input_port(Port::new("start"))
        .with_output_port(Port::new("claim_out"));
    let tc = t_claim.id.clone();
    net.add_transition(t_claim);
    net.add_arc(PetriArc::input(start_id.clone(), tc.clone(), "start"));
    net.add_arc(PetriArc::output(tc.clone(), "claim_out", claim_out_id));

    // On grant, the WHOLE datacenter lease is held + echoed to register (so the
    // adapter's in_use hold carries alloc_id for release).
    let t_receive = Transition::new(
        "t_receive",
        r#"#{ holding: grant, register: #{ grant_id: grant.grant_id, alloc_id: grant.alloc_id, node: grant.node, expiry: grant.expiry, scheduler: grant.scheduler } }"#,
    )
    .with_input_port(Port::new("grant"))
    .with_output_port(Port::new("holding"))
    .with_output_port(Port::new("register"));
    let trc = t_receive.id.clone();
    net.add_transition(t_receive);
    net.add_arc(PetriArc::input(
        grant_inbox_id.clone(),
        trc.clone(),
        "grant",
    ));
    net.add_arc(PetriArc::output(trc.clone(), "holding", holding_id.clone()));
    net.add_arc(PetriArc::output(trc.clone(), "register", register_out_id));

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

/// R4c headline: K=2 instances lease from a mock datacenter allocator through
/// the R4a effect + R4b adapter topology. Asserts each gets a real
/// `{alloc_id, scheduler}` lease from the allocator, the lease routes back
/// body-visible, release fires a DELETE with the right alloc_id, and a
/// `lease_expired` inject reaps the hold.
#[tokio::test]
async fn datacenter_lease_adapter_grants_and_releases_via_mock_allocator() {
    const JOBS: usize = 2;
    let allocator = MockAllocator::start().await;
    let ctx = PoolTestContext::setup(JOBS).await;

    // Build + initialise the adapter net, and register the REAL R4a handlers
    // (driven by a real HttpAllocatorClient) on the adapter service.
    let (adapter_net, pp) = build_datacenter_adapter_net_mirror(&allocator.url());
    ctx.pool.initialize(adapter_net).await.unwrap();
    let alloc_client = Arc::new(HttpAllocatorClient::new());
    ctx.pool
        .register_effect_handler(
            "resource_lease_acquire",
            Arc::new(ResourceLeaseAcquireHandler::new(
                alloc_client.clone(),
                "request",
                "lease",
            )),
        )
        .unwrap();
    ctx.pool
        .register_effect_handler(
            "resource_lease_release",
            Arc::new(ResourceLeaseReleaseHandler::new(
                alloc_client,
                "release",
                "released",
            )),
        )
        .unwrap();

    // K requesters claim leases.
    let mut rps = Vec::new();
    for (i, svc) in ctx.requesters.iter().enumerate() {
        let (net, rp) = build_datacenter_requester_net(&ctx.pool_id);
        svc.initialize(net).await.unwrap();
        svc.create_token(
            rp.start.clone(),
            TokenColor::Data(serde_json::json!({
                "grant_id": format!("job-{i}"),
                "request": { "gpu_count": 1, "gpu_type": "a100" }
            })),
        )
        .await
        .unwrap();
        rps.push(rp);
    }

    settle(&ctx, 5).await;

    // TYPED LEASE: every requester holds a lease with a REAL alloc_id + the typed
    // per-flavor `scheduler` detail from the allocator (no in-net capacity — the
    // allocator is the source of truth).
    let mut held_allocs = std::collections::HashSet::new();
    for (svc, rp) in ctx.requesters.iter().zip(rps.iter()) {
        let m = svc.get_marking().await;
        let toks = m.tokens_at(&rp.holding);
        assert_eq!(toks.len(), 1, "each requester holds exactly one lease");
        if let TokenColor::Data(d) = &toks[0].color {
            let flavor = d
                .get("scheduler")
                .and_then(|s| s.get("flavor"))
                .and_then(|v| v.as_str())
                .expect("lease.scheduler.flavor");
            assert_eq!(flavor, "http", "mock allocator emits the http flavor");
            assert!(
                d.get("gpu_uuid").is_none(),
                "retired gpu_uuid must not appear on the lease"
            );
            let alloc = d
                .get("alloc_id")
                .and_then(|v| v.as_str())
                .expect("lease.alloc_id");
            assert!(
                alloc.starts_with("alloc-"),
                "real allocator alloc_id, got {alloc}"
            );
            assert!(
                held_allocs.insert(alloc.to_string()),
                "alloc_ids must be distinct per lease"
            );
        }
    }
    assert_eq!(held_allocs.len(), JOBS, "K distinct leases granted");
    assert_eq!(
        allocator.granted().len(),
        JOBS,
        "allocator granted K leases"
    );

    // Finish requester 0 → release fires a DELETE to the allocator for its alloc_id.
    let alloc0 = {
        let m = ctx.requesters[0].get_marking().await;
        match &m.tokens_at(&rps[0].holding)[0].color {
            TokenColor::Data(d) => d
                .get("alloc_id")
                .and_then(|v| v.as_str())
                .unwrap()
                .to_string(),
            _ => panic!("hold not data"),
        }
    };
    ctx.requesters[0]
        .create_token(rps[0].finish_trigger.clone(), TokenColor::Unit)
        .await
        .unwrap();
    settle(&ctx, 5).await;

    let released = allocator.released();
    assert!(
        released.contains(&alloc0),
        "release must DELETE the held alloc_id {alloc0} at the allocator; released={released:?}"
    );

    // REAP: requester 1 "crashes" (never finishes). Inject lease_expired for its
    // grant_id → the adapter drops the hold (the allocator TTL already reclaimed it).
    let in_use_before = ctx.pool.get_marking().await.token_count(&pp.in_use);
    ctx.pool
        .create_token(
            pp.lease_expired.clone(),
            TokenColor::Data(serde_json::json!({ "grant_id": "job-1" })),
        )
        .await
        .unwrap();
    settle(&ctx, 5).await;
    let in_use_after = ctx.pool.get_marking().await.token_count(&pp.in_use);
    assert!(
        in_use_after < in_use_before,
        "lease_expired inject must reap the hold: in_use {in_use_before} → {in_use_after}"
    );

    ctx.teardown().await;
}
