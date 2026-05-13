//! Cross-net bridge integration tests.
//!
//! Verifies end-to-end token bridging between two separate Petri net instances
//! communicating over real NATS JetStream via a shared testcontainer.

use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use tokio::sync::Notify;

use petri_application::PetriNetService;
use petri_domain::{
    Arc as PetriArc, Marking, PetriNet, Place, PlaceId, Port, TokenColor, Transition, TransitionId,
};
use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};
use petri_nats::{CrossNetBridge, NatsConfig, NatsEventPublisher};

use crate::nats::{ensure_global_stream, shared_nats_url};

// ---------------------------------------------------------------------------
// CrossNetTestContext (per-test, shares the container)
// ---------------------------------------------------------------------------

struct CrossNetTestContext {
    net_a_id: String,
    net_b_id: String,
    service_a: Arc<
        PetriNetService<
            NatsEventPublisher<MemoryEventStore>,
            MemoryTopologyStore,
            MarkingProjection,
        >,
    >,
    service_b: Arc<
        PetriNetService<
            NatsEventPublisher<MemoryEventStore>,
            MemoryTopologyStore,
            MarkingProjection,
        >,
    >,
    #[allow(dead_code)]
    eval_notify_a: Arc<Notify>,
    #[allow(dead_code)]
    eval_notify_b: Arc<Notify>,
    jetstream: jetstream::Context,
}

impl CrossNetTestContext {
    async fn setup() -> Self {
        // Each test gets a fresh NATS connection from the shared testcontainer.
        // Sharing a single JetStream context across tests can degrade when
        // bridge listener tasks encounter errors after teardown.
        let nats_url = shared_nats_url().await;
        let client = async_nats::connect(nats_url)
            .await
            .expect("connect to shared NATS testcontainer");
        let jetstream = jetstream::new(client);
        ensure_global_stream(&jetstream)
            .await
            .expect("PETRI_GLOBAL stream");

        let uuid_suffix = uuid::Uuid::new_v4().simple().to_string();
        let net_a_id = format!("net-a-xnet-{uuid_suffix}");
        let net_b_id = format!("net-b-xnet-{uuid_suffix}");

        let build_service = |net_id: &str, js: jetstream::Context, url: &str| {
            let store = Arc::new(MemoryEventStore::new());
            let config = NatsConfig {
                url: url.to_string(),
                net_id: Some(net_id.to_string()),
                ..NatsConfig::default()
            };
            let publisher = NatsEventPublisher::new(store, js, config);
            let events = Arc::new(publisher);
            let topology = Arc::new(MemoryTopologyStore::new());
            let projection = Arc::new(MarkingProjection::new());
            Arc::new(PetriNetService::new(events, topology, projection))
        };

        let service_a = build_service(&net_a_id, jetstream.clone(), nats_url);
        let service_b = build_service(&net_b_id, jetstream.clone(), nats_url);

        let eval_notify_a = Arc::new(Notify::new());
        let eval_notify_b = Arc::new(Notify::new());

        // Start inbound bridge listeners
        let bridge_a = Arc::new(CrossNetBridge::new(net_a_id.clone(), jetstream.clone()));
        bridge_a.start_inbound_listener(
            service_a.clone(),
            eval_notify_a.clone(),
        );

        let bridge_b = Arc::new(CrossNetBridge::new(net_b_id.clone(), jetstream.clone()));
        bridge_b.start_inbound_listener(
            service_b.clone(),
            eval_notify_b.clone(),
        );

        // Wait for bridge consumers to be created (DeliverPolicy::New means
        // messages published before the consumer exists are missed).
        let stream = jetstream
            .get_stream("PETRI_GLOBAL")
            .await
            .expect("get PETRI_GLOBAL stream");
        for net_id in [&net_a_id, &net_b_id] {
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
            net_a_id,
            net_b_id,
            service_a,
            service_b,
            eval_notify_a,
            eval_notify_b,
            jetstream,
        }
    }

    /// Clean up durable consumers created by the bridge listeners.
    async fn teardown(&self) {
        let stream = match self.jetstream.get_stream("PETRI_GLOBAL").await {
            Ok(s) => s,
            Err(_) => return,
        };
        for net_id in [&self.net_a_id, &self.net_b_id] {
            let consumer_name = format!("bridge-inbound-{net_id}");
            let _ = stream.delete_consumer(&consumer_name).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Poll helper
// ---------------------------------------------------------------------------

async fn poll_marking<F>(
    service: &PetriNetService<
        NatsEventPublisher<MemoryEventStore>,
        MemoryTopologyStore,
        MarkingProjection,
    >,
    predicate: F,
    timeout: Duration,
) -> Marking
where
    F: Fn(&Marking) -> bool,
{
    let start = tokio::time::Instant::now();
    loop {
        let marking = service.get_marking().await;
        if predicate(&marking) {
            return marking;
        }
        if start.elapsed() > timeout {
            panic!(
                "poll_marking timed out after {:?}. Last marking: {:?}",
                timeout, marking
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// Scenario builders
// ---------------------------------------------------------------------------

fn one_way_bridge_scenario(
    target_net_id: &str,
) -> (PetriNet, PetriNet, TransitionId, PlaceId, PlaceId, PlaceId) {
    let mut sender_net = PetriNet::new();

    let source = Place::internal("source");
    let source_id = source.id.clone();
    sender_net.add_place(source);

    let outbox = Place::bridge_out("outbox", target_net_id, "inbox");
    let outbox_id = outbox.id.clone();
    sender_net.add_place(outbox);

    let produce = Transition::new("produce", "#{ outbox: source }")
        .with_input_port(Port::new("source"))
        .with_output_port(Port::new("outbox"));
    let produce_id = produce.id.clone();
    sender_net.add_transition(produce);

    sender_net.add_arc(PetriArc::input(
        source_id.clone(),
        produce_id.clone(),
        "source",
    ));
    sender_net.add_arc(PetriArc::output(
        produce_id.clone(),
        "outbox",
        outbox_id.clone(),
    ));

    let mut receiver_net = PetriNet::new();
    let inbox = Place::internal("inbox");
    let inbox_id = inbox.id.clone();
    receiver_net.add_place(inbox);

    (
        sender_net,
        receiver_net,
        produce_id,
        source_id,
        outbox_id,
        inbox_id,
    )
}

#[allow(clippy::type_complexity)]
fn request_reply_scenario(
    target_net_id: &str,
) -> (
    PetriNet,
    PetriNet,
    TransitionId,
    PlaceId,
    PlaceId,
    PlaceId,
    PlaceId,
    PlaceId,
    TransitionId,
) {
    let mut sender_net = PetriNet::new();

    let source = Place::internal("source");
    let source_id = source.id.clone();
    sender_net.add_place(source);

    let outbox = Place::bridge_out_reply("outbox", target_net_id, "inbox", "reply_inbox");
    let outbox_id = outbox.id.clone();
    sender_net.add_place(outbox);

    let reply_inbox = Place::internal("reply_inbox");
    let reply_inbox_id = reply_inbox.id.clone();
    sender_net.add_place(reply_inbox);

    let send_request = Transition::new("send_request", "#{ outbox: source }")
        .with_input_port(Port::new("source"))
        .with_output_port(Port::new("outbox"));
    let send_request_id = send_request.id.clone();
    sender_net.add_transition(send_request);

    sender_net.add_arc(PetriArc::input(
        source_id.clone(),
        send_request_id.clone(),
        "source",
    ));
    sender_net.add_arc(PetriArc::output(
        send_request_id.clone(),
        "outbox",
        outbox_id.clone(),
    ));

    let mut receiver_net = PetriNet::new();

    let inbox = Place::internal("inbox");
    let inbox_id = inbox.id.clone();
    receiver_net.add_place(inbox);

    let processed = Place::bridge_reply("processed");
    let processed_id = processed.id.clone();
    receiver_net.add_place(processed);

    let process = Transition::new("process", "#{ processed: inbox }")
        .with_input_port(Port::new("inbox"))
        .with_output_port(Port::new("processed"));
    let process_id = process.id.clone();
    receiver_net.add_transition(process);

    receiver_net.add_arc(PetriArc::input(
        inbox_id.clone(),
        process_id.clone(),
        "inbox",
    ));
    receiver_net.add_arc(PetriArc::output(
        process_id.clone(),
        "processed",
        processed_id.clone(),
    ));

    (
        sender_net,
        receiver_net,
        send_request_id,
        source_id,
        outbox_id,
        reply_inbox_id,
        inbox_id,
        processed_id,
        process_id,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_one_way_bridge() {
    let ctx = CrossNetTestContext::setup().await;

    let (sender_net, receiver_net, produce_id, source_id, _outbox_id, inbox_id) =
        one_way_bridge_scenario(&ctx.net_b_id);

    ctx.service_a.initialize(sender_net.clone()).await.unwrap();
    ctx.service_b.initialize(receiver_net.clone()).await.unwrap();
    ctx.service_a
        .create_token(source_id.clone(), TokenColor::Unit)
        .await
        .expect("create token");

    ctx.service_a
        .fire_transition(produce_id)
        .await
        .expect("fire produce");

    let marking_b = poll_marking(
        &ctx.service_b,
        |m| m.token_count(&inbox_id) >= 1,
        Duration::from_secs(10),
    )
    .await;

    assert_eq!(marking_b.token_count(&inbox_id), 1);

    let marking_a = ctx.service_a.get_marking().await;
    assert_eq!(marking_a.token_count(&source_id), 0);

    ctx.teardown().await;
}

#[tokio::test]
async fn test_request_reply_bridge() {
    let ctx = CrossNetTestContext::setup().await;

    let (
        sender_net,
        receiver_net,
        send_request_id,
        source_id,
        _outbox_id,
        reply_inbox_id,
        inbox_id,
        _processed_id,
        process_id,
    ) = request_reply_scenario(&ctx.net_b_id);

    ctx.service_a.initialize(sender_net.clone()).await.unwrap();
    ctx.service_b.initialize(receiver_net.clone()).await.unwrap();
    ctx.service_a
        .create_token(
            source_id.clone(),
            TokenColor::Data(serde_json::json!({"msg": "hello"})),
        )
        .await
        .expect("create token");

    ctx.service_a
        .fire_transition(send_request_id)
        .await
        .expect("fire send_request");

    poll_marking(
        &ctx.service_b,
        |m| m.token_count(&inbox_id) >= 1,
        Duration::from_secs(10),
    )
    .await;

    ctx.service_b
        .fire_transition(process_id)
        .await
        .expect("fire process");

    let marking_a = poll_marking(
        &ctx.service_a,
        |m| m.token_count(&reply_inbox_id) >= 1,
        Duration::from_secs(10),
    )
    .await;

    assert_eq!(marking_a.token_count(&reply_inbox_id), 1);
    assert_eq!(marking_a.token_count(&source_id), 0);

    ctx.teardown().await;
}

#[tokio::test]
async fn test_bridge_multiple_tokens() {
    let ctx = CrossNetTestContext::setup().await;

    let (sender_net, receiver_net, produce_id, source_id, _outbox_id, inbox_id) =
        one_way_bridge_scenario(&ctx.net_b_id);

    ctx.service_a.initialize(sender_net.clone()).await.unwrap();
    ctx.service_b.initialize(receiver_net.clone()).await.unwrap();

    for _ in 0..3 {
        ctx.service_a
            .create_token(source_id.clone(), TokenColor::Unit)
            .await
            .expect("create token");
    }

    for _ in 0..3 {
        ctx.service_a
            .fire_transition(produce_id.clone())
            .await
            .expect("fire produce");
    }

    let marking_b = poll_marking(
        &ctx.service_b,
        |m| m.token_count(&inbox_id) >= 3,
        Duration::from_secs(15),
    )
    .await;

    assert_eq!(marking_b.token_count(&inbox_id), 3);

    let marking_a = ctx.service_a.get_marking().await;
    assert_eq!(marking_a.token_count(&source_id), 0);

    ctx.teardown().await;
}

#[tokio::test]
async fn test_bridge_with_data_token() {
    let ctx = CrossNetTestContext::setup().await;

    let (sender_net, receiver_net, produce_id, source_id, _outbox_id, inbox_id) =
        one_way_bridge_scenario(&ctx.net_b_id);

    ctx.service_a.initialize(sender_net.clone()).await.unwrap();
    ctx.service_b.initialize(receiver_net.clone()).await.unwrap();
    ctx.service_a
        .create_token(
            source_id.clone(),
            TokenColor::Data(serde_json::json!({"key": "value"})),
        )
        .await
        .expect("create token");

    ctx.service_a
        .fire_transition(produce_id)
        .await
        .expect("fire produce");

    let marking_b = poll_marking(
        &ctx.service_b,
        |m| m.token_count(&inbox_id) >= 1,
        Duration::from_secs(10),
    )
    .await;

    assert_eq!(marking_b.token_count(&inbox_id), 1);

    let tokens = marking_b.tokens_at(&inbox_id);
    assert_eq!(tokens.len(), 1);
    assert_eq!(
        tokens[0].color,
        TokenColor::Data(serde_json::json!({"key": "value"}))
    );

    ctx.teardown().await;
}

// ---------------------------------------------------------------------------
// Multi-instance reply-to routing test
// ---------------------------------------------------------------------------
//
// Verifies that when two sender nets (alpha, beta) dispatch to a shared relay
// net via bridge_out_reply, the relay's bridge_reply places route results back
// to the correct originating sender — not cross-routed.
//
// Topology:
//   sender-alpha ──bridge_out_reply──► relay/inbox ──bridge_reply──► sender-alpha/reply_inbox
//   sender-beta  ──bridge_out_reply──► relay/inbox ──bridge_reply──► sender-beta/reply_inbox

struct MultiInstanceTestContext {
    alpha_id: String,
    beta_id: String,
    relay_id: String,
    service_alpha: Arc<
        PetriNetService<
            NatsEventPublisher<MemoryEventStore>,
            MemoryTopologyStore,
            MarkingProjection,
        >,
    >,
    service_beta: Arc<
        PetriNetService<
            NatsEventPublisher<MemoryEventStore>,
            MemoryTopologyStore,
            MarkingProjection,
        >,
    >,
    service_relay: Arc<
        PetriNetService<
            NatsEventPublisher<MemoryEventStore>,
            MemoryTopologyStore,
            MarkingProjection,
        >,
    >,
    jetstream: jetstream::Context,
}

impl MultiInstanceTestContext {
    async fn setup() -> Self {
        let nats_url = shared_nats_url().await;
        let client = async_nats::connect(nats_url)
            .await
            .expect("connect to shared NATS testcontainer");
        let jetstream = jetstream::new(client);
        ensure_global_stream(&jetstream)
            .await
            .expect("PETRI_GLOBAL stream");

        let uuid_suffix = uuid::Uuid::new_v4().simple().to_string();
        let alpha_id = format!("alpha-{uuid_suffix}");
        let beta_id = format!("beta-{uuid_suffix}");
        let relay_id = format!("relay-{uuid_suffix}");

        let build_service = |net_id: &str, js: jetstream::Context, url: &str| {
            let store = Arc::new(MemoryEventStore::new());
            let config = NatsConfig {
                url: url.to_string(),
                net_id: Some(net_id.to_string()),
                ..NatsConfig::default()
            };
            let publisher = NatsEventPublisher::new(store, js, config);
            let events = Arc::new(publisher);
            let topology = Arc::new(MemoryTopologyStore::new());
            let projection = Arc::new(MarkingProjection::new());
            Arc::new(PetriNetService::new(events, topology, projection))
        };

        let service_alpha = build_service(&alpha_id, jetstream.clone(), nats_url);
        let service_beta = build_service(&beta_id, jetstream.clone(), nats_url);
        let service_relay = build_service(&relay_id, jetstream.clone(), nats_url);

        // Start inbound bridge listeners for all three nets
        for (net_id, service) in [
            (&alpha_id, &service_alpha),
            (&beta_id, &service_beta),
            (&relay_id, &service_relay),
        ] {
            let bridge = Arc::new(CrossNetBridge::new(net_id.clone(), jetstream.clone()));
            bridge.start_inbound_listener(service.clone(), Arc::new(Notify::new()));
        }

        // Wait for all bridge consumers to be ready
        let stream = jetstream
            .get_stream("PETRI_GLOBAL")
            .await
            .expect("get PETRI_GLOBAL stream");
        for net_id in [&alpha_id, &beta_id, &relay_id] {
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
            alpha_id,
            beta_id,
            relay_id,
            service_alpha,
            service_beta,
            service_relay,
            jetstream,
        }
    }

    async fn teardown(&self) {
        let stream = match self.jetstream.get_stream("PETRI_GLOBAL").await {
            Ok(s) => s,
            Err(_) => return,
        };
        for net_id in [&self.alpha_id, &self.beta_id, &self.relay_id] {
            let consumer_name = format!("bridge-inbound-{net_id}");
            let _ = stream.delete_consumer(&consumer_name).await;
        }
    }
}

/// Build a sender net that dispatches to a relay via bridge_out_reply.
///
/// Places: source, outbox (bridge_out_reply → relay/inbox, reply_to: reply_inbox), reply_inbox
/// Transitions: send_request (source → outbox)
fn multi_instance_sender(relay_net_id: &str) -> (PetriNet, TransitionId, PlaceId, PlaceId) {
    let mut net = PetriNet::new();

    let source = Place::internal("source");
    let source_id = source.id.clone();
    net.add_place(source);

    let outbox = Place::bridge_out_reply("outbox", relay_net_id, "inbox", "reply_inbox");
    let outbox_id = outbox.id.clone();
    net.add_place(outbox);

    let reply_inbox = Place::bridge_reply("reply_inbox");
    let reply_inbox_id = reply_inbox.id.clone();
    net.add_place(reply_inbox);

    let send = Transition::new("send_request", "#{ outbox: source }")
        .with_input_port(Port::new("source"))
        .with_output_port(Port::new("outbox"));
    let send_id = send.id.clone();
    net.add_transition(send);

    net.add_arc(PetriArc::input(source_id.clone(), send_id.clone(), "source"));
    net.add_arc(PetriArc::output(send_id.clone(), "outbox", outbox_id));

    (net, send_id, source_id, reply_inbox_id)
}

/// Build a relay net that receives, processes, and replies via bridge_reply.
///
/// Places: inbox (bridge_in), processed (bridge_reply)
/// Transitions: process (inbox → processed)
fn multi_instance_relay() -> (PetriNet, TransitionId, PlaceId, PlaceId) {
    let mut net = PetriNet::new();

    let inbox = Place::internal("inbox");
    let inbox_id = inbox.id.clone();
    net.add_place(inbox);

    let processed = Place::bridge_reply("processed");
    let processed_id = processed.id.clone();
    net.add_place(processed);

    let process = Transition::new("process", "#{ processed: inbox }")
        .with_input_port(Port::new("inbox"))
        .with_output_port(Port::new("processed"));
    let process_id = process.id.clone();
    net.add_transition(process);

    net.add_arc(PetriArc::input(inbox_id.clone(), process_id.clone(), "inbox"));
    net.add_arc(PetriArc::output(
        process_id.clone(),
        "processed",
        processed_id.clone(),
    ));

    (net, process_id, inbox_id, processed_id)
}

/// Two sender nets dispatch to a shared relay; verify results route back to
/// the correct originator via bridge_reply (not cross-routed).
#[tokio::test]
async fn test_multi_instance_reply_routing() {
    let ctx = MultiInstanceTestContext::setup().await;

    // Build nets
    let (alpha_net, alpha_send_id, alpha_source_id, alpha_reply_id) =
        multi_instance_sender(&ctx.relay_id);
    let (beta_net, beta_send_id, beta_source_id, beta_reply_id) =
        multi_instance_sender(&ctx.relay_id);
    let (relay_net, relay_process_id, relay_inbox_id, _relay_processed_id) =
        multi_instance_relay();

    // Initialize all nets
    ctx.service_alpha
        .initialize(alpha_net.clone())
        .await
        .unwrap();
    ctx.service_beta
        .initialize(beta_net.clone())
        .await
        .unwrap();
    ctx.service_relay
        .initialize(relay_net.clone())
        .await
        .unwrap();

    // Inject distinguishable tokens into each sender
    ctx.service_alpha
        .create_token(
            alpha_source_id.clone(),
            TokenColor::Data(serde_json::json!({"origin": "alpha", "value": 1})),
        )
        .await
        .expect("create alpha token");

    ctx.service_beta
        .create_token(
            beta_source_id.clone(),
            TokenColor::Data(serde_json::json!({"origin": "beta", "value": 2})),
        )
        .await
        .expect("create beta token");

    // Fire send on both senders
    ctx.service_alpha
        .fire_transition(alpha_send_id.clone())
        .await
        .expect("fire alpha send");

    ctx.service_beta
        .fire_transition(beta_send_id.clone())
        .await
        .expect("fire beta send");

    // Wait for both tokens to arrive at the relay's inbox
    poll_marking(
        &ctx.service_relay,
        |m| m.token_count(&relay_inbox_id) >= 2,
        Duration::from_secs(10),
    )
    .await;

    // Fire process on the relay twice (once per token)
    ctx.service_relay
        .fire_transition(relay_process_id.clone())
        .await
        .expect("fire relay process (1)");

    ctx.service_relay
        .fire_transition(relay_process_id.clone())
        .await
        .expect("fire relay process (2)");

    // Wait for replies to arrive at each sender's reply_inbox
    let marking_alpha = poll_marking(
        &ctx.service_alpha,
        |m| m.token_count(&alpha_reply_id) >= 1,
        Duration::from_secs(10),
    )
    .await;

    let marking_beta = poll_marking(
        &ctx.service_beta,
        |m| m.token_count(&beta_reply_id) >= 1,
        Duration::from_secs(10),
    )
    .await;

    // Verify: each sender got exactly 1 reply
    assert_eq!(
        marking_alpha.token_count(&alpha_reply_id),
        1,
        "alpha should receive exactly 1 reply"
    );
    assert_eq!(
        marking_beta.token_count(&beta_reply_id),
        1,
        "beta should receive exactly 1 reply"
    );

    // Verify: alpha's reply has alpha's data, beta's has beta's data
    let alpha_tokens = marking_alpha.tokens_at(&alpha_reply_id);
    assert_eq!(alpha_tokens.len(), 1);
    if let TokenColor::Data(ref data) = alpha_tokens[0].color {
        assert_eq!(
            data.get("origin").and_then(|v| v.as_str()),
            Some("alpha"),
            "alpha's reply should contain alpha's data, got: {data}"
        );
    } else {
        panic!("expected Data token in alpha reply_inbox");
    }

    let beta_tokens = marking_beta.tokens_at(&beta_reply_id);
    assert_eq!(beta_tokens.len(), 1);
    if let TokenColor::Data(ref data) = beta_tokens[0].color {
        assert_eq!(
            data.get("origin").and_then(|v| v.as_str()),
            Some("beta"),
            "beta's reply should contain beta's data, got: {data}"
        );
    } else {
        panic!("expected Data token in beta reply_inbox");
    }

    // Verify: sources are empty (tokens were consumed)
    assert_eq!(marking_alpha.token_count(&alpha_source_id), 0);
    assert_eq!(marking_beta.token_count(&beta_source_id), 0);

    ctx.teardown().await;
}

// ---------------------------------------------------------------------------
// Multi-channel reply routing test
// ---------------------------------------------------------------------------
//
// Verifies named reply channels: a sender embeds two channels ("alpha", "beta"),
// and the relay has two bridge_reply_channel places that route to different
// places on the sender.

/// Sender with bridge_out_reply_channels and two separate reply inboxes.
fn multi_channel_sender(
    relay_net_id: &str,
) -> (PetriNet, TransitionId, PlaceId, PlaceId, PlaceId) {
    let mut net = PetriNet::new();

    let source = Place::internal("source");
    let source_id = source.id.clone();
    net.add_place(source);

    let mut channels = std::collections::HashMap::new();
    channels.insert("alpha".to_string(), "alpha_inbox".to_string());
    channels.insert("beta".to_string(), "beta_inbox".to_string());
    let outbox = Place::bridge_out_reply_channels("outbox", relay_net_id, "inbox", channels);
    let outbox_id = outbox.id.clone();
    net.add_place(outbox);

    let alpha_inbox = Place::bridge_reply("alpha_inbox");
    let alpha_inbox_id = alpha_inbox.id.clone();
    net.add_place(alpha_inbox);

    let beta_inbox = Place::bridge_reply("beta_inbox");
    let beta_inbox_id = beta_inbox.id.clone();
    net.add_place(beta_inbox);

    let send = Transition::new("send", "#{ outbox: source }")
        .with_input_port(Port::new("source"))
        .with_output_port(Port::new("outbox"));
    let send_id = send.id.clone();
    net.add_transition(send);

    net.add_arc(PetriArc::input(source_id.clone(), send_id.clone(), "source"));
    net.add_arc(PetriArc::output(send_id.clone(), "outbox", outbox_id));

    (net, send_id, source_id, alpha_inbox_id, beta_inbox_id)
}

/// Relay with two bridge_reply_channel places for "alpha" and "beta" channels.
fn multi_channel_relay() -> (PetriNet, TransitionId, PlaceId) {
    let mut net = PetriNet::new();

    let inbox = Place::internal("inbox");
    let inbox_id = inbox.id.clone();
    net.add_place(inbox);

    let alpha_out = Place::bridge_reply_channel("alpha_out", "alpha");
    let alpha_out_id = alpha_out.id.clone();
    net.add_place(alpha_out);

    let beta_out = Place::bridge_reply_channel("beta_out", "beta");
    let beta_out_id = beta_out.id.clone();
    net.add_place(beta_out);

    // Process transition: produces to both channel outputs
    let process = Transition::new(
        "process",
        r#"#{ alpha_out: #{ msg: inbox.msg, channel: "alpha" }, beta_out: #{ msg: inbox.msg, channel: "beta" } }"#,
    )
    .with_input_port(Port::new("inbox"))
    .with_output_port(Port::new("alpha_out"))
    .with_output_port(Port::new("beta_out"));
    let process_id = process.id.clone();
    net.add_transition(process);

    net.add_arc(PetriArc::input(
        inbox_id.clone(),
        process_id.clone(),
        "inbox",
    ));
    net.add_arc(PetriArc::output(
        process_id.clone(),
        "alpha_out",
        alpha_out_id,
    ));
    net.add_arc(PetriArc::output(
        process_id.clone(),
        "beta_out",
        beta_out_id,
    ));

    (net, process_id, inbox_id)
}

/// Named reply channels route to separate places on the sender.
#[tokio::test]
async fn test_multi_channel_reply_routing() {
    let ctx = CrossNetTestContext::setup().await;

    let (sender_net, send_id, source_id, alpha_inbox_id, beta_inbox_id) =
        multi_channel_sender(&ctx.net_b_id);
    let (relay_net, process_id, relay_inbox_id) = multi_channel_relay();

    ctx.service_a.initialize(sender_net).await.unwrap();
    ctx.service_b.initialize(relay_net).await.unwrap();

    // Inject a token into the sender
    ctx.service_a
        .create_token(
            source_id.clone(),
            TokenColor::Data(serde_json::json!({"msg": "hello"})),
        )
        .await
        .expect("create token");

    // Send to relay
    ctx.service_a
        .fire_transition(send_id)
        .await
        .expect("fire send");

    // Wait for token at relay inbox
    poll_marking(
        &ctx.service_b,
        |m| m.token_count(&relay_inbox_id) >= 1,
        Duration::from_secs(10),
    )
    .await;

    // Process — produces to both alpha_out and beta_out channels
    ctx.service_b
        .fire_transition(process_id)
        .await
        .expect("fire process");

    // Wait for replies at sender's alpha_inbox and beta_inbox
    let marking = poll_marking(
        &ctx.service_a,
        |m| m.token_count(&alpha_inbox_id) >= 1 && m.token_count(&beta_inbox_id) >= 1,
        Duration::from_secs(10),
    )
    .await;

    // Verify: each inbox got exactly 1 token
    assert_eq!(marking.token_count(&alpha_inbox_id), 1, "alpha_inbox should have 1 token");
    assert_eq!(marking.token_count(&beta_inbox_id), 1, "beta_inbox should have 1 token");

    // Verify: alpha_inbox has the alpha channel token
    let alpha_tokens = marking.tokens_at(&alpha_inbox_id);
    if let TokenColor::Data(ref data) = alpha_tokens[0].color {
        assert_eq!(data.get("channel").and_then(|v| v.as_str()), Some("alpha"));
    } else {
        panic!("expected Data token in alpha_inbox");
    }

    // Verify: beta_inbox has the beta channel token
    let beta_tokens = marking.tokens_at(&beta_inbox_id);
    if let TokenColor::Data(ref data) = beta_tokens[0].color {
        assert_eq!(data.get("channel").and_then(|v| v.as_str()), Some("beta"));
    } else {
        panic!("expected Data token in beta_inbox");
    }

    ctx.teardown().await;
}
