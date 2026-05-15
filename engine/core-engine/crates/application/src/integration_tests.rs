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

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};
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
