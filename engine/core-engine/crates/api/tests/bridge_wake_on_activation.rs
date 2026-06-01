//! Regression test for the activation-gate bridge-target wake.
//!
//! Guards the fix in `handlers::net_scoped::net_set_run_mode`: before the
//! Strict `validate_bridges` gate runs (on a transition to `Running`), the
//! handler must WAKE any hibernated bridge-target nets. A target net's
//! topology is durable (its `NetInitialized` event survives in the log), but
//! after hibernation / an engine restart the target is absent from the hot
//! in-memory registry. The strict resolver (`resolve_topology`) only sees
//! *live* nets, so without the pre-wake loop activation would wrongly 422 with
//! `BRIDGE_TARGET_NET_MISSING` for a target that merely needs rehydrating.
//!
//! This was the recurring live symptom on the datacenter lease-adapter pools
//! (`pool-<resource_id>`): a workflow instance bridging to a pool that had
//! hibernated since the last engine start could not activate.
//!
//! The store factory below pre-loads each net's topology by id — this is the
//! deterministic stand-in for the production path where `get_or_create`'s
//! factory blocks on NATS event-log replay before returning, so a woken net
//! comes back with its topology already hydrated.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use petri_api::net_registry::StoreFactory;
use petri_api::{create_router_with_registry, NetRegistry};
use petri_domain::{Arc as PetriArc, PetriNet, Place, Port, Transition};
use petri_test_harness::doubles::{
    MockEventRepository, MockStateProjection, MockTopologyRepository,
};
use serde_json::{json, Value};
use tower::ServiceExt;

type Reg = NetRegistry<MockEventRepository, MockTopologyRepository, MockStateProjection>;

/// Parent net: `source` (internal) → `produce` → `outbox` (bridge_out → target/inbox).
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

/// Target net: a single `inbox` bridge_in place (the bridge_out's target).
fn target_net() -> PetriNet {
    let mut net = PetriNet::new();
    net.add_place(Place::bridge_in("inbox"));
    net
}

/// A registry whose store factory rehydrates topology by net id — simulating
/// the production wake-on-demand path (event-log replay) deterministically.
fn registry_with_topologies(topos: Vec<(String, PetriNet)>) -> (Router, Arc<Reg>) {
    let factory: StoreFactory<MockEventRepository, MockTopologyRepository, MockStateProjection> =
        Arc::new(move |net_id: &str| {
            let (_tx, rx) = tokio::sync::watch::channel(0u64);
            let topo = topos
                .iter()
                .find(|(id, _)| id == net_id)
                .map(|(_, net)| MockTopologyRepository::with_topology(net.clone()))
                .unwrap_or_default();
            (
                Arc::new(MockEventRepository::new()),
                Arc::new(topo),
                Arc::new(MockStateProjection::new()),
                rx,
            )
        });
    let registry = Arc::new(NetRegistry::new(factory));
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

/// The fix: activating a net whose bridge target has hibernated must WAKE the
/// target and pass strict validation — not 422 with BRIDGE_TARGET_NET_MISSING.
///
/// Without the pre-wake loop in `net_set_run_mode`, the hibernated target is
/// invisible to `resolve_topology` (hot-only) and this returns 422.
#[tokio::test]
async fn activation_wakes_hibernated_bridge_target() {
    let parent_id = "parent-net";
    let target_id = "pool-target-net";

    let (router, registry) = registry_with_topologies(vec![
        (parent_id.to_string(), parent_net(target_id)),
        (target_id.to_string(), target_net()),
    ]);

    // Bring both nets live, then hibernate the target so it is absent from the
    // hot registry but still rehydratable (the post-restart / idle-eviction
    // condition the gate must tolerate).
    registry.get_or_create(parent_id);
    registry.get_or_create(target_id);
    registry
        .hibernate(target_id)
        .expect("hibernate target should succeed");
    assert!(
        registry.get(target_id).is_none(),
        "precondition: target must be hibernated (cold) before activation"
    );

    // Activate the parent.
    let (status, body) = put_run_mode(router, parent_id, "running").await;

    assert_eq!(
        status,
        StatusCode::OK,
        "activation must succeed by waking the hibernated bridge target; got body: {body}"
    );
    assert!(
        registry.get(target_id).is_some(),
        "the activation gate must have woken the hibernated bridge target"
    );
}

/// Negative control: the gate must STILL reject a genuinely-missing bridge
/// target (one that was never deployed and cannot be rehydrated). This proves
/// the wake loop doesn't blunt the validation — it only rescues rehydratable
/// targets.
#[tokio::test]
async fn activation_rejects_truly_missing_bridge_target() {
    let parent_id = "parent-net";
    let ghost_id = "ghost-target-net";

    // Only the parent has topology; the ghost target is unknown to the factory
    // (empty topology) and was never created → not hot, not known, no metadata.
    let (router, registry) =
        registry_with_topologies(vec![(parent_id.to_string(), parent_net(ghost_id))]);
    registry.get_or_create(parent_id);

    let (status, body) = put_run_mode(router, parent_id, "running").await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "activation must reject an undeployable bridge target; got body: {body}"
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
