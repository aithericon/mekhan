//! Reproduction for the hibernated capacity-pool `BRIDGE_TARGET_NET_MISSING`
//! incident (workspace-owned `pool-<resource_id>` net).
//!
//! Chain of failure (all in this repo):
//!   1. A workflow instance bridges to its capacity pool `pool-<id>`. After the
//!      pool idles it HIBERNATES (cancel token + removed from the hot registry),
//!      but its topology is durable in the event log.
//!   2. On the instance's next activation, the to-Running gate
//!      (`net_set_run_mode`) WAKES the pool via `get_or_create` before the
//!      strict bridge gate runs.
//!   3. `get_or_create`'s WOKEN-net branch only starts (and blocks on) the
//!      event consumer — the thing that replays the log to hydrate topology —
//!      when the woken-workspace resolver returns `Some(ws)`. If it returns
//!      `None`, the consumer is DEFERRED (the genuinely-fresh-net contract) and
//!      never started for a wake, so topology never hydrates.
//!   4. The production resolver (`KvWokenWorkspaceResolver`) used to fold the
//!      DEFAULT workspace into `None`. Every `default`-recorded pool therefore
//!      woke with no consumer → `resolve_topology` returned `None` → the gate
//!      422'd with `BRIDGE_TARGET_NET_MISSING` for a pool that merely needed
//!      rehydrating.
//!
//! These tests pin the registry CONTRACT that drives the fix: the resolver's
//! `Some`/`None` return decides whether a woken net hydrates. The factory below
//! hydrates the pool's topology ONLY from the consumer starter (the production
//! truth — a woken net's topology comes from event-log replay), so the
//! consumer-start dependency is exercised rather than masked by a pre-loaded
//! mock. The companion unit test in `core-engine/src/main.rs`
//! (`woken_workspace_resolver_tests`) pins the resolver fix itself: a
//! default-recorded entry must resolve to `Some("default")`, not `None`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use petri_api::net_registry::{ConsumerStarter, StoreFactory, WokenWorkspaceResolver};
use petri_api::{create_router_with_registry, NetRegistry};
use petri_application::TopologyRepository;
use petri_domain::{Arc as PetriArc, PetriNet, Place, Port, Transition};
use petri_test_harness::doubles::{
    MockEventRepository, MockStateProjection, MockTopologyRepository,
};
use serde_json::{json, Value};
use tower::ServiceExt;

type Reg = NetRegistry<MockEventRepository, MockTopologyRepository, MockStateProjection>;

/// Parent net: `source` → `produce` → `outbox` (bridge_out → pool/inbox).
fn parent_net(target_net_id: &str) -> PetriNet {
    let mut net = PetriNet::new();
    let source = Place::internal("source");
    let source_id = source.id.clone();
    net.add_place(source);
    let outbox = Place::bridge_out("outbox", target_net_id, "inbox");
    let outbox_id = outbox.id.clone();
    net.add_place(outbox);
    let produce = Transition::new("produce", "#{ outbox: source }")
        .with_input_port(Port::new("source"))
        .with_output_port(Port::new("outbox"));
    let produce_id = produce.id.clone();
    net.add_transition(produce);
    net.add_arc(PetriArc::input(source_id, produce_id.clone(), "source"));
    net.add_arc(PetriArc::output(produce_id, "outbox", outbox_id));
    net
}

/// Pool target net: a single `inbox` bridge_in place.
fn pool_net() -> PetriNet {
    let mut net = PetriNet::new();
    net.add_place(Place::bridge_in("inbox"));
    net
}

/// Stub woken-workspace resolver: returns `target_ws` for `target_id`, `None`
/// for everything else. `target_ws = Some(..)` models the FIXED resolver
/// (default included); `None` models the BUG (default collapsed to None).
struct StubResolver {
    target_id: String,
    target_ws: Option<String>,
}

#[async_trait::async_trait]
impl WokenWorkspaceResolver for StubResolver {
    async fn workspace_for(&self, net_id: &str) -> Option<String> {
        if net_id == self.target_id {
            self.target_ws.clone()
        } else {
            None
        }
    }
}

/// Registry whose factory hydrates the POOL target's topology ONLY when the
/// deferred consumer starter runs (the production truth: a woken net's topology
/// arrives via event-log replay). The parent's topology is pre-loaded so it is
/// always activatable regardless of the resolver.
fn build(target_ws: Option<&str>) -> (Router, Arc<Reg>) {
    let target_id = "pool-target-net";

    let parent_topo = parent_net(target_id);
    let pool_topo = pool_net();

    let factory: StoreFactory<MockEventRepository, MockTopologyRepository, MockStateProjection> =
        Arc::new(move |net_id: &str| {
            let (_tx, rx) = tokio::sync::watch::channel(0u64);
            if net_id == target_id {
                // Pool target: EMPTY topology + a consumer starter that
                // populates it (simulated replay). No starter run → no topology.
                let topo = Arc::new(MockTopologyRepository::new());
                let topo_for_starter = topo.clone();
                let net = pool_topo.clone();
                let starter: ConsumerStarter = Arc::new(move |_ws: String| {
                    let topo = topo_for_starter.clone();
                    let net = net.clone();
                    Box::pin(async move {
                        // Stand-in for the NATS event consumer replaying
                        // `NetInitialized` to hydrate topology on wake.
                        topo.set_topology(net);
                    })
                });
                (
                    Arc::new(MockEventRepository::new()),
                    topo,
                    Arc::new(MockStateProjection::new()),
                    rx,
                    Arc::new(std::sync::RwLock::new(None)),
                    starter,
                    Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    Arc::new(parking_lot::RwLock::new(None)),
                )
            } else {
                // Parent (and anything else): topology pre-loaded; no-op starter.
                let topo = MockTopologyRepository::with_topology(parent_topo.clone());
                (
                    Arc::new(MockEventRepository::new()),
                    Arc::new(topo),
                    Arc::new(MockStateProjection::new()),
                    rx,
                    Arc::new(std::sync::RwLock::new(None)),
                    Arc::new(|_ws: String| {
                        Box::pin(async {})
                            as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                    }),
                    Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    Arc::new(parking_lot::RwLock::new(None)),
                )
            }
        });

    let registry = Arc::new(NetRegistry::new(factory));
    registry.set_woken_workspace_resolver(Arc::new(StubResolver {
        target_id: target_id.to_string(),
        target_ws: target_ws.map(|s| s.to_string()),
    }));
    let router = create_router_with_registry(registry.clone());
    (router, registry)
}

async fn put_run_mode(router: Router, net_id: &str, mode: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("PUT")
        .uri(format!("/api/nets/{net_id}/run-mode"))
        .header("content-type", "application/json")
        .body(Body::from(json!({ "mode": mode }).to_string()))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// Bring the pool up then hibernate it: known-but-cold, the post-idle / restart
/// condition the activation gate must tolerate.
async fn hibernate_pool(registry: &Arc<Reg>) {
    let target_id = "pool-target-net";
    registry.get_or_create(target_id);
    registry
        .hibernate(target_id)
        .await
        .expect("hibernate pool should succeed");
    assert!(
        registry.get(target_id).is_none(),
        "precondition: pool must be hibernated (cold) before activation"
    );
}

/// FIX: when the resolver returns the recorded workspace — INCLUDING the default
/// workspace — waking the pool starts its consumer, topology hydrates, and the
/// instance activates past the strict bridge gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_recorded_pool_hydrates_and_activation_succeeds() {
    let (router, registry) = build(Some("default"));
    registry.get_or_create("parent-net");
    hibernate_pool(&registry).await;

    let (status, body) = put_run_mode(router, "parent-net", "running").await;

    assert_eq!(
        status,
        StatusCode::OK,
        "default-recorded pool must wake + hydrate so activation passes; body: {body}"
    );
    let topo = registry
        .get("pool-target-net")
        .and_then(|i| i.service.get_topology());
    assert!(
        topo.is_some(),
        "the woken pool must have hydrated its topology via the consumer start"
    );
}

/// BUG REPRODUCTION: when the resolver collapses the default workspace to `None`
/// (the original behaviour), waking the pool DOES NOT start its consumer, so
/// topology never hydrates and the strict gate 422s with
/// `BRIDGE_TARGET_NET_MISSING` for a pool that merely needed rehydrating.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pool_stays_cold_when_resolver_collapses_default_to_none() {
    let (router, registry) = build(None);
    registry.get_or_create("parent-net");
    hibernate_pool(&registry).await;

    let (status, body) = put_run_mode(router, "parent-net", "running").await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "resolver→None must reproduce the cold-pool failure; body: {body}"
    );
    let codes = body["issues"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i["code"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        codes.contains(&"BRIDGE_TARGET_NET_MISSING"),
        "expected BRIDGE_TARGET_NET_MISSING, got issues: {body}"
    );
}
