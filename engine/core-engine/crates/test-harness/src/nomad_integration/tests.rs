//! Real Nomad E2E tests for the nomad_batch_net scenario.
//!
//! Submits batch jobs to a real Nomad dev agent, observes completion via
//! NomadWatcher → NATS → SignalListener, and verifies the Petri engine fires
//! signal-join transitions and routes tokens to the completed place.
//!
//! **Scope**: Happy-path only (all jobs succeed via `/bin/echo`). Failure,
//! retry, and dead-letter paths are covered by the mock e2e tests in
//! `e2e/tests.rs::test_nomad_batch_full_lifecycle`.
//!
//! Infrastructure:
//! - Nomad dev agent (started automatically via `ensure_nomad_dev`)
//! - NATS JetStream (testcontainer via `shared_nats_url`)
//! - PetriNetService with real SchedulerSubmitHandler + NomadClient
//! - NomadWatcher publishing signals to NATS
//! - SignalListener injecting tokens from NATS into the engine

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use petri_application::{PetriNetService, SchedulerSubmitHandler};
use petri_domain::Marking;
use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};
use petri_nats::{NatsConfig, NatsEventPublisher, SignalListener};
use petri_nomad::{NomadClient, NomadConfig, NomadWatcher};

use crate::fixtures::TestScenario;
use crate::nats::{ensure_global_stream, shared_nats_url};
use crate::nomad::{ensure_nomad_dev, register_test_job_template};

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

type NomadService =
    PetriNetService<NatsEventPublisher<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>;

// ---------------------------------------------------------------------------
// NomadTestHarness — reusable test infrastructure
// ---------------------------------------------------------------------------

/// Shared infrastructure for Nomad integration tests.
///
/// Encapsulates the Nomad dev agent, NATS testcontainer, PetriNetService,
/// and the nomad_batch scenario. Watcher and listener are started
/// separately per test to allow restart/kill scenarios.
struct NomadTestHarness {
    service: Arc<NomadService>,
    jetstream: async_nats::jetstream::Context,
    scenario: TestScenario,
    nomad_config: NomadConfig,
    net_id: String,
    eval_notify: Arc<Notify>,
}

/// Handle to a running watcher + listener pair.
struct LiveComponents {
    watcher_shutdown_tx: tokio::sync::broadcast::Sender<()>,
    watcher_handle: tokio::task::JoinHandle<()>,
    signal_listener: Arc<SignalListener>,
    listener_handle: tokio::task::JoinHandle<()>,
}

impl NomadTestHarness {
    /// Set up full infrastructure: Nomad dev agent, NATS stream, engine, scenario.
    async fn setup() -> Self {
        let nomad_addr: &str = ensure_nomad_dev().await;
        let nats_url: &str = shared_nats_url().await;
        let nats_client = async_nats::connect(nats_url).await.expect("connect NATS");
        let jetstream = async_nats::jetstream::new(nats_client);

        let net_id = format!("nomad-batch-integ-{}", uuid::Uuid::new_v4().simple());
        let job_template = "petri-batch-integ";

        // Ensure the PETRI_GLOBAL stream exists
        ensure_global_stream(&jetstream)
            .await
            .expect("PETRI_GLOBAL stream");

        // Register a job template that just runs `/bin/echo done`
        register_test_job_template(nomad_addr, job_template, "/bin/echo", &["done"])
            .await
            .expect("register job template");
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Build scenario and engine
        let scenario = TestScenario::nomad_batch();

        let store = Arc::new(MemoryEventStore::new());
        let nats_config = NatsConfig {
            url: nats_url.to_string(),
            net_id: Some(net_id.clone()),
            ..NatsConfig::default()
        };
        let publisher = NatsEventPublisher::new(store, jetstream.clone(), nats_config);
        let events = Arc::new(publisher);
        let topology = Arc::new(MemoryTopologyStore::new());
        let projection = Arc::new(MarkingProjection::new());
        let service = Arc::new(PetriNetService::new(events, topology, projection));

        // Register the real SchedulerSubmitHandler with NomadClient
        let nomad_config = NomadConfig {
            addr: nomad_addr.to_string(),
            token: None,
            region: "global".to_string(),
            task_name: "petri-worker".to_string(),
            ca_cert: None,
        };
        let signal_routes = HashMap::from([
            ("running".to_string(), "sig_running".to_string()),
            ("completed".to_string(), "sig_completed".to_string()),
            ("failed".to_string(), "sig_failed".to_string()),
        ]);
        let nomad_client = NomadClient::new(
            nomad_config.clone(),
            &net_id,
            "sig_completed",
            signal_routes,
        )
        .expect("create NomadClient");
        let handler = Arc::new(SchedulerSubmitHandler::new(
            Arc::new(nomad_client),
            job_template,
            "job",
            "submitted",
        ));
        service
            .register_effect_handler("scheduler_submit", handler)
            .unwrap();

        // Initialize the net and seed tokens
        service.initialize(scenario.net.clone()).await.unwrap();
        for (place_id, token) in &scenario.initial_tokens {
            service
                .create_token(place_id.clone(), token.color.clone())
                .await
                .expect("seed token");
        }

        let eval_notify = Arc::new(Notify::new());

        Self {
            service,
            jetstream,
            scenario,
            nomad_config,
            net_id,
            eval_notify,
        }
    }

    /// Start a fresh NomadWatcher + SignalListener pair.
    async fn start_components(&self) -> LiveComponents {
        // NomadWatcher
        let watcher = NomadWatcher::new(self.nomad_config.clone(), self.jetstream.clone())
            .await
            .expect("create NomadWatcher");
        let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
        let watcher_handle = tokio::spawn(async move {
            watcher.run(shutdown_rx).await;
        });

        // SignalListener
        let signal_listener = Arc::new(SignalListener::new(
            self.net_id.clone(),
            self.jetstream.clone(),
        ));
        let listener_handle = signal_listener.clone().start(
            self.service.clone(),
            self.eval_notify.clone(),
        );

        // Let components start up
        tokio::time::sleep(Duration::from_secs(2)).await;

        LiveComponents {
            watcher_shutdown_tx: shutdown_tx,
            watcher_handle,
            signal_listener,
            listener_handle,
        }
    }

    /// Run the full lifecycle: evaluate (submits jobs) → poll until all reach
    /// `completed` place, evaluating between polls to consume arriving signals.
    ///
    /// The evaluate and signal injection are concurrent: fast Nomad jobs
    /// (like `/bin/echo`) can complete during the same evaluate call that
    /// submitted them, so we cannot assume intermediate signal places accumulate.
    async fn run_full_lifecycle(&self) {
        let completed_id = self.scenario.places["completed"].clone();

        // Initial evaluate: submits jobs (and may also fire t_running/t_success
        // if signals arrive during the async effect handler execution).
        self.service.evaluate_until_quiescent(20).await.unwrap();

        // Poll until all 3 reach completed, evaluating on each iteration
        // to consume any newly-arrived signals.
        let start = tokio::time::Instant::now();
        loop {
            let marking = self.service.get_marking().await;
            if marking.tokens_at(&completed_id).len() >= 3 {
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                panic!(
                    "run_full_lifecycle timed out after 30s. Marking: {:?}",
                    marking
                );
            }
            let _ = self.service.evaluate_until_quiescent(20).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Assert clean completion: N tokens in completed, all signal/intermediate places empty.
    fn assert_clean_completion(&self, marking: &Marking, expected: usize) {
        let completed_id = &self.scenario.places["completed"];
        assert_eq!(
            marking.tokens_at(completed_id).len(),
            expected,
            "Expected {} tokens in completed place",
            expected
        );

        for place_name in &[
            "sig_running",
            "sig_completed",
            "sig_failed",
            "job_queue",
            "submitted_jobs",
            "running_jobs",
            "failed_jobs",
            "effect_errors",
            "dead_letter",
        ] {
            assert_eq!(
                marking.tokens_at(&self.scenario.places[*place_name]).len(),
                0,
                "{} should be empty",
                place_name
            );
        }
    }

    /// Read the NomadWatcher KV checkpoint index.
    async fn read_checkpoint_index(&self) -> Option<u64> {
        let kv = self.jetstream.get_key_value("PETRI_WATCHER").await.ok()?;
        match kv.get("nomad.event_index").await {
            Ok(Some(bytes)) => {
                let s = std::str::from_utf8(&bytes).ok()?;
                s.parse().ok()
            }
            _ => None,
        }
    }

    /// Stop watcher and listener, clean up consumers.
    async fn stop_components(&self, components: LiveComponents) {
        let _ = components.watcher_shutdown_tx.send(());
        components.listener_handle.abort();
        let _ = tokio::time::timeout(Duration::from_secs(5), components.watcher_handle).await;

        // Clean up the durable signal consumer
        if let Ok(stream) = self.jetstream.get_stream("PETRI_GLOBAL").await {
            let consumer_name = format!("signal-inbound-{}", self.net_id);
            let _ = stream.delete_consumer(&consumer_name).await;
        }
    }

    /// Stop only the watcher, keep listener running.
    async fn stop_watcher(&self, components: &LiveComponents) {
        let _ = components.watcher_shutdown_tx.send(());
        let _ = tokio::time::sleep(Duration::from_secs(1)).await;
    }

    /// Reload scenario: reset service, re-initialize net, re-seed tokens.
    async fn reload_scenario(&self) {
        self.service.clear().await;
        self.service.initialize(self.scenario.net.clone()).await.unwrap();
        self.service.set_initial_tokens(
            self.scenario
                .initial_tokens
                .iter()
                .map(|(pid, tok)| (pid.clone(), tok.color.clone()))
                .collect(),
        );
        for (place_id, token) in &self.scenario.initial_tokens {
            self.service
                .create_token(place_id.clone(), token.color.clone())
                .await
                .expect("seed token");
        }
    }
}

// ===========================================================================
// Test 1: Happy-path E2E (existing test, now using harness)
// ===========================================================================

/// Full Nomad E2E: submit 3 batch jobs → Nomad runs `/bin/echo` → NomadWatcher
/// detects completion → NATS signal → SignalListener injects token → t_success
/// fires → all 3 jobs land in completed place.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_nomad_batch_net_real_dispatch() {
    let harness = NomadTestHarness::setup().await;
    let components = harness.start_components().await;

    harness.run_full_lifecycle().await;

    let marking = harness.service.get_marking().await;
    harness.assert_clean_completion(&marking, 3);

    harness.stop_components(components).await;
}

// ===========================================================================
// Test 2: Watcher checkpoint survives restart
// ===========================================================================

/// Verifies the KV checkpoint persists across watcher restarts.
///
/// 1. Run full lifecycle (3 jobs → completed)
/// 2. Read checkpoint — assert > 0
/// 3. Kill watcher
/// 4. Reload scenario + reset consumer + start new watcher
/// 5. Run 3 more jobs
/// 6. Assert: 3 completed (only new batch), checkpoint advanced
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_watcher_checkpoint_survives_restart() {
    let harness = NomadTestHarness::setup().await;
    let components = harness.start_components().await;

    // ---- Phase 1: run full lifecycle ----
    harness.run_full_lifecycle().await;

    let marking = harness.service.get_marking().await;
    harness.assert_clean_completion(&marking, 3);

    // Read checkpoint — should be > 0
    let checkpoint_1 = harness
        .read_checkpoint_index()
        .await
        .expect("checkpoint should exist after first run");
    assert!(
        checkpoint_1 > 0,
        "Checkpoint should be > 0, got {}",
        checkpoint_1
    );

    // ---- Kill watcher + listener ----
    harness.stop_components(components).await;

    // ---- Reload scenario (simulates scenario re-deployment) ----
    harness.reload_scenario().await;

    // ---- Phase 2: start fresh components, run again ----
    let components = harness.start_components().await;

    // Advance epoch so stale signals from batch 1 are filtered out
    components
        .signal_listener
        .advance_epoch()
        .await
        .expect("advance epoch");
    harness.run_full_lifecycle().await;

    let marking = harness.service.get_marking().await;
    // Only 3 from the new batch (old marking was cleared by reload_scenario)
    harness.assert_clean_completion(&marking, 3);

    // Checkpoint should have advanced
    let checkpoint_2 = harness
        .read_checkpoint_index()
        .await
        .expect("checkpoint should still exist");
    assert!(
        checkpoint_2 >= checkpoint_1,
        "Checkpoint should have advanced: {} -> {}",
        checkpoint_1,
        checkpoint_2
    );

    harness.stop_components(components).await;
}

// ===========================================================================
// Test 3: No duplicate signals after watcher restart mid-lifecycle
// ===========================================================================

/// Verifies cursor dedup + checkpoint prevent duplicate signals when the
/// watcher restarts mid-lifecycle.
///
/// 1. Submit 3 jobs via evaluate
/// 2. Kill watcher shortly after (it may or may not have processed all events)
/// 3. Start new watcher (resumes from KV checkpoint)
/// 4. Poll until all 3 reach completed
/// 5. Assert: exactly 3 in completed (not 6 from duplicate signals)
///
/// With fast jobs (`/bin/echo`), signals may arrive during the initial
/// evaluate, so we cannot rely on intermediate places accumulating.
/// The no-duplicates guarantee is verified by `assert_clean_completion`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_no_duplicate_signals_after_watcher_restart() {
    let harness = NomadTestHarness::setup().await;
    let components = harness.start_components().await;
    let completed_id = harness.scenario.places["completed"].clone();

    // Phase 1: Submit all 3 jobs (and possibly process some signals concurrently)
    harness.service.evaluate_until_quiescent(20).await.unwrap();

    // Give the watcher a moment to process some events, then kill it
    tokio::time::sleep(Duration::from_secs(2)).await;
    harness.stop_watcher(&components).await;

    // Start a NEW watcher — it should resume from checkpoint, not re-signal
    let new_watcher = NomadWatcher::new(harness.nomad_config.clone(), harness.jetstream.clone())
        .await
        .expect("create new NomadWatcher");
    let (_shutdown_tx_2, shutdown_rx_2) = tokio::sync::broadcast::channel::<()>(1);
    let _watcher_handle_2 = tokio::spawn(async move {
        new_watcher.run(shutdown_rx_2).await;
    });

    tokio::time::sleep(Duration::from_secs(2)).await;

    // Poll until all 3 reach completed, evaluating on each iteration
    let start = tokio::time::Instant::now();
    loop {
        let marking = harness.service.get_marking().await;
        if marking.tokens_at(&completed_id).len() >= 3 {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!(
                "test_no_duplicate_signals timed out after 30s. Marking: {:?}",
                marking
            );
        }
        let _ = harness.service.evaluate_until_quiescent(20).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let marking = harness.service.get_marking().await;
    harness.assert_clean_completion(&marking, 3);

    // Cleanup
    let _ = _shutdown_tx_2.send(());
    components.listener_handle.abort();
    if let Ok(stream) = harness.jetstream.get_stream("PETRI_GLOBAL").await {
        let consumer_name = format!("signal-inbound-{}", harness.net_id);
        let _ = stream.delete_consumer(&consumer_name).await;
    }
}

// ===========================================================================
// Test 4: No stale signals after scenario reload
// ===========================================================================

/// Verifies stale signals from a previous scenario don't produce stranded tokens.
///
/// 1. Run full lifecycle (3 jobs → completed)
/// 2. Reload scenario + reset signal consumer
/// 3. Run 3 new jobs
/// 4. Assert: only 3 completed (not 6), no stranded signal tokens
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_no_stale_signals_after_scenario_reload() {
    let harness = NomadTestHarness::setup().await;
    let components = harness.start_components().await;

    // ---- Phase 1: complete first batch ----
    harness.run_full_lifecycle().await;

    let marking = harness.service.get_marking().await;
    harness.assert_clean_completion(&marking, 3);

    // ---- Reload scenario ----
    // Advance signal epoch BEFORE reloading — THIS IS THE FIX BEING TESTED.
    // The existing listener keeps running; messages at or below the epoch
    // are ACK'd without processing, filtering out batch 1's stale signals.
    components
        .signal_listener
        .advance_epoch()
        .await
        .expect("advance epoch for scenario reload");

    harness.reload_scenario().await;

    // ---- Phase 2: run second batch ----
    // The existing listener continues with the updated epoch — no restart needed.
    harness.run_full_lifecycle().await;

    let marking = harness.service.get_marking().await;
    // Should be 3 (only new batch), NOT 6 or stranded tokens
    harness.assert_clean_completion(&marking, 3);

    // Cleanup
    let _ = components.watcher_shutdown_tx.send(());
    components.listener_handle.abort();
    let _ = tokio::time::timeout(Duration::from_secs(5), components.watcher_handle).await;
    if let Ok(stream) = harness.jetstream.get_stream("PETRI_GLOBAL").await {
        let consumer_name = format!("signal-inbound-{}", harness.net_id);
        let _ = stream.delete_consumer(&consumer_name).await;
    }
}
