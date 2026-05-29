//! In-source integration tests for the pre-dispatch hook (spec § 10).
//!
//! Per sub-reconciliation #3, these tests live in-source (NOT in
//! cargo-standard `tests/`) so they can reach the same `pub(crate)` helpers
//! as the rest of the crate's unit tests. They drive the engine's firing
//! pipeline end-to-end against a mocked `EffectHandler` to verify that the
//! hook chain's outcomes (Continue / Continue+enrich / Reject / Defer +
//! budget escalation) flow through `fire_effect_transition` correctly and
//! emit the right `DomainEvent` variants.
//!
//! The cert-plan integration tests covered here:
//!
//! * `test_reject_path_blocks_dispatch_and_emits_events`
//! * `test_continue_enrich_path_dispatches_with_enriched_config`
//! * `test_defer_budget_escalates_to_reject_after_threshold`
//! * `test_http_transport_consumes_canned_continue_outcome`
//!
//! The fifth cert-plan test ("Registration race after net-hot →
//! `RegistryFrozen`") lives in `petri-api::net_registry::tests` since it
//! exercises the registry surface, not the firing pipeline.
//!
//! Note: the spec's "no executor JobSpec was published on NATS" honest-
//! absence assertion lives in `core-engine/crates/nats/tests/` and is
//! outside this crate's owned-files scope; surfaced in the B1 report.

#![cfg(test)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use petri_domain::{
    apply_event_to_marking, Arc as PetriArc, DomainEvent, Marking, PersistedEvent, PetriNet, Place,
    PlaceId, Port, PreDispatchOutcomeKind, TokenColor, Transition, TransitionId,
};

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput, ExecutionMode};
use crate::pre_dispatch::{
    HttpPreDispatchHook, PreDispatchChain, PreDispatchChainEntry, PreDispatchContext,
    PreDispatchError, PreDispatchHook, PreDispatchOutcome, PreDispatchRuntime,
};
use crate::{
    EventRepository, EventStoreError, PetriNetService, StateProjection, TopologyRepository,
};

// ============================================================================
// Test fixtures — mirror the helpers in `service::tests` to keep this module
// self-contained. Local helpers, not exported.
// ============================================================================

struct TestEvents {
    events: RwLock<Vec<PersistedEvent>>,
}

impl TestEvents {
    fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl EventRepository for TestEvents {
    async fn append(&self, event: DomainEvent) -> Result<PersistedEvent, EventStoreError> {
        let mut events = self.events.write().unwrap();
        let sequence = events.len() as u64;
        let previous_hash = events.last().map(|e| e.hash.clone());
        let persisted = PersistedEvent::new(sequence, event, previous_hash);
        events.push(persisted.clone());
        Ok(persisted)
    }
    async fn all_events(&self) -> Vec<PersistedEvent> {
        self.events.read().unwrap().clone()
    }
    async fn events_since(&self, sequence: u64) -> Vec<PersistedEvent> {
        self.events
            .read()
            .unwrap()
            .iter()
            .filter(|e| e.sequence >= sequence)
            .cloned()
            .collect()
    }
    async fn reset(&self) {
        self.events.write().unwrap().clear();
    }
    async fn current_sequence(&self) -> u64 {
        self.events.read().unwrap().len() as u64
    }
}

struct TestTopology {
    topology: RwLock<Option<PetriNet>>,
}

impl TestTopology {
    fn new() -> Self {
        Self {
            topology: RwLock::new(None),
        }
    }
}

impl TopologyRepository for TestTopology {
    fn get_topology(&self) -> Option<PetriNet> {
        self.topology.read().unwrap().clone()
    }
    fn set_topology(&self, net: PetriNet) {
        *self.topology.write().unwrap() = Some(net);
    }
    fn clear(&self) {
        *self.topology.write().unwrap() = None;
    }
    fn update_transition_script(
        &self,
        _transition_id: &TransitionId,
        _script: String,
        _guard: Option<String>,
    ) -> bool {
        false
    }
}

struct TestProjection;
impl StateProjection for TestProjection {
    fn project(&self, events: &[PersistedEvent]) -> Marking {
        let mut marking = Marking::new();
        for persisted in events {
            apply_event_to_marking(&mut marking, &persisted.event);
        }
        marking
    }
}

fn new_service() -> PetriNetService<TestEvents, TestTopology, TestProjection> {
    PetriNetService::new(
        Arc::new(TestEvents::new()),
        Arc::new(TestTopology::new()),
        Arc::new(TestProjection),
    )
}

/// Build a minimal Petri net with one effect transition wired
/// `input -> transition -> output`. The transition references
/// `effect_handler_id` and optionally carries an `effect_config`.
fn build_effect_net(
    handler_id: &str,
    effect_config: Option<serde_json::Value>,
) -> (PetriNet, PlaceId, PlaceId, TransitionId) {
    let mut net = PetriNet::new();
    let input = Place::internal("input");
    let output = Place::internal("output");
    let mut transition = Transition::new("effect_transition", "")
        .with_input_ports(vec![Port::new("inp")])
        .with_output_ports(vec![Port::new("out")])
        .with_effect_handler(handler_id);
    if let Some(cfg) = effect_config {
        transition = transition.with_effect_config(cfg);
    }
    let input_id = input.id.clone();
    let output_id = output.id.clone();
    let transition_id = transition.id.clone();
    net.add_place(input);
    net.add_place(output);
    net.add_transition(transition);
    net.add_arc(PetriArc::input(
        input_id.clone(),
        transition_id.clone(),
        "inp",
    ));
    net.add_arc(PetriArc::output(
        transition_id.clone(),
        "out",
        output_id.clone(),
    ));
    (net, input_id, output_id, transition_id)
}

/// `EffectHandler` that records every config it sees so the test can
/// assert enrichment was actually plumbed through.
struct RecordingHandler {
    name: String,
    execute_count: AtomicUsize,
    last_config: RwLock<Option<serde_json::Value>>,
}

impl RecordingHandler {
    fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            execute_count: AtomicUsize::new(0),
            last_config: RwLock::new(None),
        })
    }
}

#[async_trait::async_trait]
impl EffectHandler for RecordingHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        self.execute_count.fetch_add(1, Ordering::SeqCst);
        *self.last_config.write().unwrap() = input.config.clone();
        let mut tokens = HashMap::new();
        tokens.insert("out".to_string(), serde_json::json!({"ok": true}));
        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({"executed": true}),
        })
    }
    fn replay(&self, _input: &EffectInput, _stored_result: &serde_json::Value) {}
    fn name(&self) -> &str {
        &self.name
    }
}

/// Hook whose outcome is dictated by construction (re-used across the
/// integration tests).
struct ScriptedHook {
    name: String,
    outcome: ScriptedOutcome,
    calls: Arc<parking_lot::Mutex<u32>>,
}

#[derive(Clone)]
enum ScriptedOutcome {
    Continue(Option<serde_json::Value>),
    Reject(String),
    Defer(Duration),
}

#[async_trait::async_trait]
impl PreDispatchHook for ScriptedHook {
    async fn pre_dispatch(
        &self,
        _ctx: &PreDispatchContext<'_>,
    ) -> Result<PreDispatchOutcome, PreDispatchError> {
        *self.calls.lock() += 1;
        match &self.outcome {
            ScriptedOutcome::Continue(c) => Ok(PreDispatchOutcome::Continue {
                enriched_effect_config: c.clone(),
            }),
            ScriptedOutcome::Reject(r) => Ok(PreDispatchOutcome::Reject { reason: r.clone() }),
            ScriptedOutcome::Defer(d) => Ok(PreDispatchOutcome::Defer { retry_after: *d }),
        }
    }
    fn name(&self) -> &str {
        &self.name
    }
}

fn scripted_hook(
    name: &str,
    outcome: ScriptedOutcome,
) -> (Arc<dyn PreDispatchHook>, Arc<parking_lot::Mutex<u32>>) {
    let calls = Arc::new(parking_lot::Mutex::new(0u32));
    let hook = Arc::new(ScriptedHook {
        name: name.to_string(),
        outcome,
        calls: calls.clone(),
    });
    (hook as Arc<dyn PreDispatchHook>, calls)
}

fn build_runtime(net_id: &str, hooks: Vec<Arc<dyn PreDispatchHook>>) -> Arc<PreDispatchRuntime> {
    let entries: Vec<PreDispatchChainEntry> = hooks
        .into_iter()
        .map(|h| PreDispatchChainEntry {
            hook: h,
            fail_open: false,
            timeout: Duration::from_secs(2),
            match_effect_handlers: vec![],
        })
        .collect();
    let chain = Arc::new(PreDispatchChain { entries });
    Arc::new(PreDispatchRuntime::new(net_id, chain))
}

// ============================================================================
// Integration tests
// ============================================================================

#[tokio::test]
async fn test_reject_path_blocks_dispatch_and_emits_events() {
    // ┌──────────────────────────────────────────────────────────────────┐
    // │ Spec § 10 cert: hook returns Reject → no EffectCompleted emitted,│
    // │ marking unchanged (honest-absence), PreDispatchEvaluated +       │
    // │ PreDispatchRejected events ARE present.                          │
    // └──────────────────────────────────────────────────────────────────┘
    let service = new_service();
    let handler = RecordingHandler::new("test_handler");
    service
        .register_effect_handler("test_handler", handler.clone())
        .unwrap();

    let (net, input_id, output_id, transition_id) = build_effect_net("test_handler", None);
    service.initialize(net).await.unwrap();

    let (hook, _) = scripted_hook(
        "rejecter",
        ScriptedOutcome::Reject("no capability for sae-tier-2".into()),
    );
    service.set_pre_dispatch_runtime(build_runtime("net-reject", vec![hook]));

    service
        .create_token(input_id.clone(), TokenColor::Unit)
        .await
        .unwrap();

    let result = service.evaluate_until_quiescent(10).await.unwrap();

    // Honest-presence: PreDispatchEvaluated + PreDispatchRejected events
    // were appended.
    let events = service.get_events().await;
    let evaluated = events
        .iter()
        .filter(|e| matches!(e.event, DomainEvent::PreDispatchEvaluated { .. }))
        .count();
    let rejected = events
        .iter()
        .filter(|e| matches!(e.event, DomainEvent::PreDispatchRejected { .. }))
        .count();
    assert_eq!(evaluated, 1, "exactly one PreDispatchEvaluated event");
    assert_eq!(rejected, 1, "exactly one PreDispatchRejected event");

    // Honest-absence: no EffectCompleted, handler.execute NEVER called.
    let completed = events
        .iter()
        .filter(|e| matches!(e.event, DomainEvent::EffectCompleted { .. }))
        .count();
    assert_eq!(completed, 0, "no EffectCompleted on reject");
    assert_eq!(handler.execute_count.load(Ordering::SeqCst), 0);

    // Honest-absence: marking unchanged — input still has the token, output is empty.
    let marking = TestProjection.project(&events);
    assert_eq!(marking.token_count(&input_id), 1);
    assert_eq!(marking.token_count(&output_id), 0);

    // Eval loop stopped on the soft outcome (no rhai transitions to fire).
    assert_eq!(result.steps_executed, 0);
    let _ = transition_id;
}

#[tokio::test]
async fn test_continue_enrich_path_dispatches_with_enriched_config() {
    // ┌──────────────────────────────────────────────────────────────────┐
    // │ Spec § 10 cert: hook returns Continue { enriched }. The          │
    // │ subsequent EffectHandler::execute call MUST see the enriched     │
    // │ config in place of the transition's static `effect_config`.     │
    // └──────────────────────────────────────────────────────────────────┘
    let service = new_service();
    let handler = RecordingHandler::new("test_handler");
    service
        .register_effect_handler("test_handler", handler.clone())
        .unwrap();

    let original_config = serde_json::json!({"queue": "default"});
    let (net, input_id, output_id, _tid) =
        build_effect_net("test_handler", Some(original_config.clone()));
    service.initialize(net).await.unwrap();

    let enrichment = serde_json::json!({"queue": "gpu-h100", "model": "test-model-a"});
    let (hook, _) = scripted_hook(
        "enricher",
        ScriptedOutcome::Continue(Some(enrichment.clone())),
    );
    service.set_pre_dispatch_runtime(build_runtime("net-enrich", vec![hook]));

    service
        .create_token(input_id.clone(), TokenColor::Unit)
        .await
        .unwrap();

    let result = service.evaluate_until_quiescent(10).await.unwrap();
    assert_eq!(result.steps_executed, 1);
    assert_eq!(handler.execute_count.load(Ordering::SeqCst), 1);

    // The handler saw the enriched config, NOT the original transition.effect_config.
    let last_config = handler.last_config.read().unwrap().clone();
    assert_eq!(
        last_config,
        Some(enrichment),
        "handler must observe the enriched config"
    );
    assert_ne!(last_config, Some(original_config));

    // EffectCompleted was emitted + marking advanced.
    let events = service.get_events().await;
    let marking = TestProjection.project(&events);
    assert_eq!(marking.token_count(&input_id), 0);
    assert_eq!(marking.token_count(&output_id), 1);
}

#[tokio::test]
async fn test_defer_budget_escalates_to_reject_after_threshold() {
    // ┌──────────────────────────────────────────────────────────────────┐
    // │ Spec § 4 + § 11 trip-wire 4: hook defers DEFAULT_MAX_DEFERS+1    │
    // │ times → final outcome must be Reject with reason                 │
    // │ "defer-budget-exceeded". Each defer emits PreDispatchDeferred,   │
    // │ the escalation emits PreDispatchRejected.                        │
    // └──────────────────────────────────────────────────────────────────┘
    let service = new_service();
    let handler = RecordingHandler::new("test_handler");
    service
        .register_effect_handler("test_handler", handler.clone())
        .unwrap();

    let (net, input_id, _output_id, _tid) = build_effect_net("test_handler", None);
    service.initialize(net).await.unwrap();

    let (hook, _) = scripted_hook(
        "deferrer",
        ScriptedOutcome::Defer(Duration::from_millis(50)),
    );
    service.set_pre_dispatch_runtime(build_runtime("net-defer-budget", vec![hook]));

    service
        .create_token(input_id.clone(), TokenColor::Unit)
        .await
        .unwrap();

    // Drive evaluate enough times to exhaust the budget.
    // DEFAULT_MAX_DEFERS = 10. Bump 11 times → escalate on the 11th.
    for _ in 0..(crate::pre_dispatch::DEFAULT_MAX_DEFERS as usize + 1) {
        let _ = service.evaluate_until_quiescent(10).await;
    }

    let events = service.get_events().await;
    let deferred_events: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let DomainEvent::PreDispatchDeferred { defer_count, .. } = &e.event {
                Some(*defer_count)
            } else {
                None
            }
        })
        .collect();
    let rejected_with_budget = events.iter().any(|e| {
        matches!(
            &e.event,
            DomainEvent::PreDispatchRejected { reason, .. } if reason == "defer-budget-exceeded"
        )
    });

    assert!(
        deferred_events.len() >= crate::pre_dispatch::DEFAULT_MAX_DEFERS as usize,
        "expected at least DEFAULT_MAX_DEFERS Deferred events, got {}",
        deferred_events.len()
    );
    assert!(
        rejected_with_budget,
        "expected PreDispatchRejected with 'defer-budget-exceeded' reason"
    );

    // Honest-absence: handler.execute was NEVER called.
    assert_eq!(handler.execute_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_http_transport_consumes_canned_continue_outcome() {
    // ┌──────────────────────────────────────────────────────────────────┐
    // │ Spec § 7 cert: instantiate `HttpPreDispatchHook` against an      │
    // │ in-test HTTP server returning a canned Continue outcome.         │
    // │ Validates that the wire format round-trips correctly and the    │
    // │ engine consumes the response. Uses a TCP listener and tiny       │
    // │ hand-rolled HTTP echo so the test has no extra dep on `hyper`    │
    // │ as a test fixture.                                               │
    // └──────────────────────────────────────────────────────────────────┘
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().unwrap();

    // Spawn a one-shot server that returns a canned Continue response.
    std::thread::spawn(move || {
        if let Ok((mut sock, _peer)) = listener.accept() {
            // Drain the request (best-effort, not a full HTTP parser).
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf);
            let body = r#"{"outcome":"continue","enriched_effect_config":{"queue":"gpu"}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes());
            let _ = sock.flush();
        }
    });

    let hook = HttpPreDispatchHook::new(
        "http-test",
        format!("http://{}/", addr),
        Duration::from_secs(2),
        0,
    );

    let tid = TransitionId::named("t-http");
    let inputs: HashMap<String, serde_json::Value> = HashMap::new();
    let read_inputs: HashMap<String, serde_json::Value> = HashMap::new();
    let ctx = PreDispatchContext {
        net_id: "test-net",
        transition_id: &tid,
        transition_name: "t",
        effect_handler_id: Some("test_handler"),
        inputs: &inputs,
        read_inputs: &read_inputs,
        effect_config: None,
        net_parameters: None,
        metadata: crate::pre_dispatch::PreDispatchMetadata::default(),
    };
    let outcome = hook.pre_dispatch(&ctx).await.expect("HTTP hook ok");
    match outcome {
        PreDispatchOutcome::Continue {
            enriched_effect_config: Some(v),
        } => {
            assert_eq!(v, serde_json::json!({"queue": "gpu"}));
        }
        other => panic!("expected Continue+enrich, got {:?}", other),
    }
    // Outcome kind projection lines up with PreDispatchOutcomeKind::Continue.
    assert_eq!(
        PreDispatchOutcomeKind::from(&PreDispatchOutcome::Continue {
            enriched_effect_config: None
        }),
        PreDispatchOutcomeKind::Continue
    );
}

// ============================================================================
// Scatter / Gather (dynamic map-reduce) — end-to-end + replay determinism.
//
// These exercise the whole Layers 0-3 stack through the real firing pipeline:
//
//   * SCATTER  — a Batch-cardinality OUTPUT port whose Rhai value is a JSON
//                array emits ONE token per element (Layer 1, firing.rs).
//   * GATHER   — a Batch INPUT arc carrying `count_from` + `correlate_on`
//                fires only when K matching result tokens are present and
//                consumes exactly those K (Layer 2, binding.rs).
//   * REPLAY   — re-running the recorded events in ExecutionMode::Replay must
//                yield a byte-identical final marking AND an identical event
//                sequence. Replay is a deterministic function of (rebuilt
//                marking + token data), never of wall-clock / insertion timing.
//
// The net models one BO-style loop iteration:
//
//   [spec] ─(propose)─▶ [coordinator]            (Single: carries k + iter id)
//                    └─▶ [raw_items]   ◀ Batch scatter: K item tokens
//   [raw_items] ─(map)─▶ [mapped_items]          (per-item Rhai transform, ×K)
//   [coordinator:read] + [mapped_items:gather K, correlate iteration_id]
//                    ─(gather)─▶ [done]           (terminal: reduce to 1 token)
// ============================================================================

/// A compact, comparable digest of the marking: for each watched place, the
/// JSON-serialized token data in marking-vector order. Used to assert the
/// live run and the replay run land on a byte-identical final marking.
fn marking_digest(marking: &Marking, places: &[(&str, &PlaceId)]) -> Vec<(String, Vec<String>)> {
    places
        .iter()
        .map(|(label, pid)| {
            let datas: Vec<String> = marking
                .tokens_at(pid)
                .iter()
                .map(|t| serde_json::to_string(&token_color_json(&t.color)).unwrap())
                .collect();
            (label.to_string(), datas)
        })
        .collect()
}

/// Project a `TokenColor` to plain JSON (Data passthrough; Unit → null;
/// Integer → number) for digest comparison.
fn token_color_json(color: &TokenColor) -> serde_json::Value {
    match color {
        TokenColor::Data(v) => v.clone(),
        TokenColor::Integer(n) => serde_json::json!(n),
        TokenColor::Unit => serde_json::Value::Null,
    }
}

/// A comparable digest of the event sequence: the variant tag plus the salient
/// payload of each event, in log order. Two runs that fire the same transitions
/// in the same order with the same token data produce identical digests.
///
/// NB: the *cross-port* ordering of produced tokens within a single firing is
/// driven by `HashMap` iteration over the script result and is NOT a
/// deterministic part of the engine contract (it predates scatter/gather). We
/// therefore sort the per-firing produced-token payloads so the digest compares
/// the produced *multiset* — which IS deterministic — rather than an incidental
/// hash order. Token DATA and firing ORDER (the load-bearing replay invariant)
/// are still asserted exactly.
fn sorted_produced(produced: &[(PlaceId, petri_domain::Token)]) -> String {
    let mut payloads: Vec<String> = produced
        .iter()
        .map(|(_, t)| token_color_json(&t.color).to_string())
        .collect();
    payloads.sort();
    payloads.join("|")
}

fn event_digest(events: &[PersistedEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match &e.event {
            DomainEvent::NetInitialized { .. } => "NetInitialized".to_string(),
            DomainEvent::TokenCreated { place_id, token, .. } => {
                format!("TokenCreated({},{})", place_id, token_color_json(&token.color))
            }
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
            DomainEvent::EffectCompleted {
                transition_id,
                produced_tokens,
                effect_result,
                ..
            } => format!(
                "EffectCompleted({},result={},produced=[{}])",
                transition_id,
                effect_result,
                sorted_produced(produced_tokens)
            ),
            DomainEvent::TokenConsumed { token_id, place_id, .. } => {
                format!("TokenConsumed({},{})", place_id, token_id)
            }
            DomainEvent::NetCompleted { .. } => "NetCompleted".to_string(),
            other => format!("{:?}", std::mem::discriminant(other)),
        })
        .collect()
}

/// Build the Rhai scatter → gather net described above. Returns the net plus
/// the PlaceIds we watch in the digest, and the seed place id.
#[allow(clippy::type_complexity)]
fn build_scatter_gather_net(
    iteration_id: &str,
    k: usize,
) -> (PetriNet, PlaceId, PlaceId, PlaceId, PlaceId, PlaceId) {
    let mut net = PetriNet::new();

    let spec = Place::internal("spec");
    let coordinator = Place::internal("coordinator");
    let raw_items = Place::internal("raw_items");
    let mapped_items = Place::internal("mapped_items");
    let done = Place::terminal("done");

    let spec_id = spec.id.clone();
    let coordinator_id = coordinator.id.clone();
    let raw_items_id = raw_items.id.clone();
    let mapped_items_id = mapped_items.id.clone();
    let done_id = done.id.clone();

    net.add_place(spec);
    net.add_place(coordinator);
    net.add_place(raw_items);
    net.add_place(mapped_items);
    net.add_place(done);

    // ── t_propose: Single `spec` in → Single `coord` out + Batch `items` out ──
    // The scatter array is built in Rhai from spec.k, stamping iteration_id +
    // __map_idx on each item so overlapping iterations never mix at the gather.
    let propose_script = r#"
        let items = [];
        let i = 0;
        while i < spec.k {
            items.push(#{ "iteration_id": spec.iteration_id, "__map_idx": i, "v": i + 1 });
            i += 1;
        }
        #{
            coord: #{ iteration_id: spec.iteration_id, k: spec.k },
            items: items
        }
    "#;
    let t_propose = Transition::new("t_propose", propose_script)
        .with_input_ports(vec![Port::new("spec")])
        .with_output_ports(vec![Port::new("coord"), Port::batch("items")]);
    let t_propose_id = t_propose.id.clone();
    net.add_transition(t_propose);
    net.add_arc(PetriArc::input(
        spec_id.clone(),
        t_propose_id.clone(),
        "spec",
    ));
    net.add_arc(PetriArc::output(
        t_propose_id.clone(),
        "coord",
        coordinator_id.clone(),
    ));
    net.add_arc(PetriArc::output(
        t_propose_id.clone(),
        "items",
        raw_items_id.clone(),
    ));

    // ── t_map: per-item transform (Single in, Single out). Fires K times. ──
    let map_script = r#"#{
        mapped: #{
            iteration_id: item.iteration_id,
            "__map_idx": item.__map_idx,
            v2: item.v * 10
        }
    }"#;
    let t_map = Transition::new("t_map", map_script)
        .with_input_ports(vec![Port::new("item")])
        .with_output_ports(vec![Port::new("mapped")]);
    let t_map_id = t_map.id.clone();
    net.add_transition(t_map);
    net.add_arc(PetriArc::input(
        raw_items_id.clone(),
        t_map_id.clone(),
        "item",
    ));
    net.add_arc(PetriArc::output(
        t_map_id.clone(),
        "mapped",
        mapped_items_id.clone(),
    ));

    // ── t_gather: counted, correlated gather barrier (Layer 2). ──
    // `expected` is a Single READ arc (coordinator stays put, supplies K +
    // iteration_id). `results` is the Batch gather arc: count_from=expected.k,
    // correlate_on=iteration_id → consumes exactly K matching mapped items.
    let gather_script = r#"#{
        reduced: #{
            iteration_id: expected.iteration_id,
            n: results.len(),
            sum: results.reduce(|acc, r| acc + r.v2, 0)
        }
    }"#;
    let t_gather = Transition::new("t_gather", gather_script)
        .with_input_ports(vec![Port::new("expected"), Port::batch("results")])
        .with_output_ports(vec![Port::new("reduced")]);
    let t_gather_id = t_gather.id.clone();
    net.add_transition(t_gather);
    net.add_arc(PetriArc::input(coordinator_id.clone(), t_gather_id.clone(), "expected").with_read(true));
    net.add_arc(
        PetriArc::input(mapped_items_id.clone(), t_gather_id.clone(), "results")
            .with_count_from("expected.k")
            .with_correlate_on("iteration_id"),
    );
    net.add_arc(PetriArc::output(
        t_gather_id.clone(),
        "reduced",
        done_id.clone(),
    ));

    let _ = (iteration_id, k);
    (
        net,
        spec_id,
        coordinator_id,
        raw_items_id,
        mapped_items_id,
        done_id,
    )
}

#[tokio::test]
async fn scatter_gather_live_run_reduces_k_items_to_one_collection_token() {
    let k = 3usize;
    let iteration_id = "iter-1";
    let (net, spec_id, coordinator_id, raw_items_id, mapped_items_id, done_id) =
        build_scatter_gather_net(iteration_id, k);

    let service = new_service();
    service.initialize(net).await.unwrap();
    service
        .create_token(
            spec_id.clone(),
            TokenColor::Data(serde_json::json!({ "iteration_id": iteration_id, "k": k })),
        )
        .await
        .unwrap();

    // propose (1) + map (K) + gather (1) = K + 2 firings.
    let result = service.evaluate_until_quiescent(k + 4).await.unwrap();
    assert_eq!(result.steps_executed, k + 2, "propose + K maps + gather");

    let events = service.get_events().await;
    let marking = TestProjection.project(&events);

    // SCATTER fanned out into exactly K raw items; all consumed by t_map.
    assert_eq!(marking.token_count(&raw_items_id), 0, "all raw items mapped");
    // GATHER consumed exactly K mapped items.
    assert_eq!(marking.token_count(&mapped_items_id), 0, "all mapped items gathered");
    // Coordinator was a READ arc → it survives.
    assert_eq!(marking.token_count(&coordinator_id), 1, "coordinator read, not consumed");
    // Exactly one reduced collection token lands in the terminal place.
    assert_eq!(marking.token_count(&done_id), 1);

    let reduced = &marking.tokens_at(&done_id)[0];
    let data = token_color_json(&reduced.color);
    assert_eq!(data["iteration_id"], iteration_id);
    assert_eq!(data["n"], k as i64);
    // v = 1..=K mapped to v2 = 10*v → sum = 10*(1+2+3) = 60.
    assert_eq!(data["sum"], 60);

    let _ = spec_id;
}

#[tokio::test]
async fn scatter_gather_replay_is_marking_and_event_identical() {
    let k = 3usize;
    let iteration_id = "iter-1";
    let watched = |spec: &PlaceId,
                   coordinator: &PlaceId,
                   raw: &PlaceId,
                   mapped: &PlaceId,
                   done: &PlaceId|
     -> Vec<(&'static str, PlaceId)> {
        vec![
            ("spec", spec.clone()),
            ("coordinator", coordinator.clone()),
            ("raw_items", raw.clone()),
            ("mapped_items", mapped.clone()),
            ("done", done.clone()),
        ]
    };

    // ── LIVE RUN ──────────────────────────────────────────────────────────
    let (net, spec_id, coordinator_id, raw_items_id, mapped_items_id, done_id) =
        build_scatter_gather_net(iteration_id, k);
    let live = new_service();
    live.initialize(net).await.unwrap();
    live.create_token(
        spec_id.clone(),
        TokenColor::Data(serde_json::json!({ "iteration_id": iteration_id, "k": k })),
    )
    .await
    .unwrap();
    live.evaluate_until_quiescent(k + 4).await.unwrap();

    let live_events = live.get_events().await;
    let live_marking = TestProjection.project(&live_events);
    let live_places = watched(
        &spec_id,
        &coordinator_id,
        &raw_items_id,
        &mapped_items_id,
        &done_id,
    );
    let live_places_ref: Vec<(&str, &PlaceId)> =
        live_places.iter().map(|(l, p)| (*l, p)).collect();
    let live_marking_digest = marking_digest(&live_marking, &live_places_ref);
    let live_event_digest = event_digest(&live_events);

    // ── REPLAY RUN ─────────────────────────────────────────────────────────
    // A fresh service replaying the same net. The scatter/gather net is pure
    // Rhai (no effect handlers), so Replay mode re-derives every TransitionFired
    // deterministically from the rebuilt marking. The replay cursor only governs
    // Effect* events — there are none here — so the marking + event sequence must
    // come out byte-identical to the live run.
    let (net2, spec_id2, coordinator_id2, raw_items_id2, mapped_items_id2, done_id2) =
        build_scatter_gather_net(iteration_id, k);
    let replay = new_service();
    replay.initialize(net2).await.unwrap();
    replay.set_execution_mode(ExecutionMode::Replay);
    replay
        .create_token(
            spec_id2.clone(),
            TokenColor::Data(serde_json::json!({ "iteration_id": iteration_id, "k": k })),
        )
        .await
        .unwrap();
    replay.evaluate_until_quiescent(k + 4).await.unwrap();

    let replay_events = replay.get_events().await;
    let replay_marking = TestProjection.project(&replay_events);
    let replay_places = watched(
        &spec_id2,
        &coordinator_id2,
        &raw_items_id2,
        &mapped_items_id2,
        &done_id2,
    );
    let replay_places_ref: Vec<(&str, &PlaceId)> =
        replay_places.iter().map(|(l, p)| (*l, p)).collect();
    let replay_marking_digest = marking_digest(&replay_marking, &replay_places_ref);
    let replay_event_digest = event_digest(&replay_events);

    // Identical final marking structure (token counts + data per place).
    assert_eq!(
        live_marking_digest, replay_marking_digest,
        "live and replay must reach a byte-identical final marking"
    );
    // Identical event sequence.
    assert_eq!(
        live_event_digest, replay_event_digest,
        "live and replay must emit an identical event sequence"
    );
}

// ── EFFECT-path scatter: exercises the stored-produced_tokens replay path ──
//
// A Batch OUTPUT port on an EFFECT transition scatters the handler's array
// result into N produced tokens, which are persisted verbatim in the
// `EffectCompleted.produced_tokens`. On Replay the engine re-emits those stored
// tokens (NOT re-running the handler), so this is the N>1 stored-token replay
// path the spec calls out.

/// Effect handler that emits a Batch array on the `items` output port. The
/// array width is fixed so the live `produced_tokens` carry N>1 elements.
struct ScatterEffectHandler {
    n: usize,
    execute_count: AtomicUsize,
    replay_count: AtomicUsize,
}

#[async_trait::async_trait]
impl EffectHandler for ScatterEffectHandler {
    async fn execute(&self, _input: EffectInput) -> Result<EffectOutput, EffectError> {
        self.execute_count.fetch_add(1, Ordering::SeqCst);
        let arr: Vec<serde_json::Value> = (0..self.n)
            .map(|i| serde_json::json!({ "__map_idx": i, "v": i * 7 }))
            .collect();
        let mut tokens = HashMap::new();
        tokens.insert("items".to_string(), serde_json::Value::Array(arr));
        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({ "scattered": self.n }),
        })
    }
    fn replay(&self, _input: &EffectInput, _stored_result: &serde_json::Value) {
        self.replay_count.fetch_add(1, Ordering::SeqCst);
    }
    fn name(&self) -> &str {
        "scatter_effect"
    }
}

/// Build a one-effect-transition net: `trigger ─(effect, Batch out)─▶ items`.
fn build_effect_scatter_net() -> (PetriNet, PlaceId, PlaceId, TransitionId) {
    let mut net = PetriNet::new();
    let trigger = Place::internal("trigger");
    let items = Place::internal("items");
    let trigger_id = trigger.id.clone();
    let items_id = items.id.clone();

    let t = Transition::new("scatter_effect_t", "")
        .with_input_ports(vec![Port::new("inp")])
        .with_output_ports(vec![Port::batch("items")])
        .with_effect_handler("scatter_effect");
    let tid = t.id.clone();

    net.add_place(trigger);
    net.add_place(items);
    net.add_transition(t);
    net.add_arc(PetriArc::input(trigger_id.clone(), tid.clone(), "inp"));
    net.add_arc(PetriArc::output(tid.clone(), "items", items_id.clone()));

    (net, trigger_id, items_id, tid)
}

#[tokio::test]
async fn effect_batch_output_scatters_and_replays_stored_tokens() {
    let n = 4usize;

    // ── LIVE: handler executes, Batch array scatters into N produced tokens ──
    let (net, trigger_id, items_id, _tid) = build_effect_scatter_net();
    let handler = Arc::new(ScatterEffectHandler {
        n,
        execute_count: AtomicUsize::new(0),
        replay_count: AtomicUsize::new(0),
    });
    let service = new_service();
    service
        .register_effect_handler("scatter_effect", handler.clone())
        .unwrap();
    service.initialize(net).await.unwrap();
    service
        .create_token(trigger_id.clone(), TokenColor::Unit)
        .await
        .unwrap();

    let result = service.evaluate_until_quiescent(4).await.unwrap();
    assert_eq!(result.steps_executed, 1, "one effect firing");
    assert_eq!(handler.execute_count.load(Ordering::SeqCst), 1);

    let events = service.get_events().await;
    let marking = TestProjection.project(&events);
    assert_eq!(marking.token_count(&trigger_id), 0, "trigger consumed");
    assert_eq!(
        marking.token_count(&items_id),
        n,
        "Batch effect output scattered into N tokens"
    );

    // The single live EffectCompleted carries N produced tokens — the stored
    // set that Replay will re-emit verbatim.
    let live_produced: Vec<(PlaceId, petri_domain::Token)> = events
        .iter()
        .find_map(|e| match &e.event {
            DomainEvent::EffectCompleted { produced_tokens, .. } => Some(produced_tokens.clone()),
            _ => None,
        })
        .expect("one EffectCompleted in live run");
    assert_eq!(live_produced.len(), n, "N>1 produced tokens stored for replay");

    // ── REPLAY: same service + log, re-seed the trigger and replay. The engine
    // re-emits the STORED produced tokens (not re-running execute) — the
    // stored-produced_tokens replay path with N>1 tokens.
    service.set_execution_mode(ExecutionMode::Replay);
    service
        .create_token(trigger_id.clone(), TokenColor::Unit)
        .await
        .unwrap();

    // Cap at one step: replay re-emits the STORED consumed/produced tokens
    // (referencing the original prior token ids), so it does not consume the
    // freshly re-seeded trigger — leaving the transition perpetually enabled.
    // One step is exactly the recorded firing we want to replay.
    let replay_result = service.evaluate_until_quiescent(1).await.unwrap();
    assert_eq!(replay_result.steps_executed, 1, "one replayed effect firing");
    assert_eq!(
        handler.execute_count.load(Ordering::SeqCst),
        1,
        "execute must NOT run again in replay"
    );
    assert_eq!(
        handler.replay_count.load(Ordering::SeqCst),
        1,
        "replay hook invoked once"
    );

    // The replayed EffectCompleted re-emits the stored produced tokens (same
    // token IDs + data) — a deterministic function of the recorded event, never
    // a fresh handler call.
    let replay_event = &replay_result.events[0];
    match &replay_event.event {
        DomainEvent::EffectCompleted { produced_tokens, .. } => {
            assert_eq!(produced_tokens.len(), n, "replay re-emits all N tokens");
            for (live, replayed) in live_produced.iter().zip(produced_tokens.iter()) {
                assert_eq!(live.1.id, replayed.1.id, "same token id");
                assert_eq!(
                    token_color_json(&live.1.color),
                    token_color_json(&replayed.1.color),
                    "same token data"
                );
            }
        }
        other => panic!("expected EffectCompleted on replay, got {:?}", other),
    }
}
