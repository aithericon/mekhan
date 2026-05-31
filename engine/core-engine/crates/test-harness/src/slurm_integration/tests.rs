//! Real Slurm E2E tests for the slurm_batch_net scenario.
//!
//! Submits batch jobs to a real Slurm cluster (Docker container) via SSH,
//! observes completion via SlurmWatcher -> NATS -> SignalListener, and verifies
//! the Petri engine fires signal-join transitions and routes tokens to the
//! completed place.
//!
//! **Scope**: Happy-path only (all jobs succeed via a trivial script).
//! Failure, retry, and dead-letter paths are covered by the mock e2e tests.
//!
//! Infrastructure:
//! - Docker Slurm container (started via `just slurm-up`, SSH on port 2222)
//! - NATS JetStream (testcontainer via `shared_nats_url`)
//! - PetriNetService with real SchedulerSubmitHandler + SlurmClient
//! - SlurmWatcher publishing signals to NATS
//! - SignalListener injecting tokens from NATS into the engine

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use petri_application::{PetriNetService, SchedulerSubmitHandler};
use petri_domain::Marking;
use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};
use petri_nats::{NatsConfig, NatsEventPublisher, SignalListener};
use petri_slurm::{SlurmClient, SlurmConfig, SlurmWatcher};

use crate::fixtures::TestScenario;
use crate::nats::{ensure_global_stream, shared_nats_url};

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

type SlurmService =
    PetriNetService<NatsEventPublisher<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>;

// ---------------------------------------------------------------------------
// SlurmTestHarness -- reusable test infrastructure
// ---------------------------------------------------------------------------

/// Shared infrastructure for Slurm integration tests.
///
/// Encapsulates the Slurm Docker container (via SSH), NATS testcontainer,
/// PetriNetService, and the slurm_batch scenario. Watcher and listener are
/// started separately per test to allow restart/kill scenarios.
struct SlurmTestHarness {
    service: Arc<SlurmService>,
    jetstream: async_nats::jetstream::Context,
    scenario: TestScenario,
    slurm_config: SlurmConfig,
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

impl SlurmTestHarness {
    /// Set up full infrastructure: Slurm SSH connectivity, NATS stream, engine, scenario.
    async fn setup() -> Self {
        let nats_url: &str = shared_nats_url().await;
        let nats_client = async_nats::connect(nats_url).await.expect("connect NATS");
        let jetstream = async_nats::jetstream::new(nats_client);

        let net_id = format!("slurm-batch-integ-{}", uuid::Uuid::new_v4().simple());

        // Ensure the PETRI_GLOBAL stream exists
        ensure_global_stream(&jetstream)
            .await
            .expect("PETRI_GLOBAL stream");

        // Slurm sandbox configuration (Docker container on localhost:2222)
        // Template already exists at /opt/petri/templates/default.sh inside container
        let slurm_config = SlurmConfig {
            ssh_host: "localhost".to_string(),
            ssh_port: 2222,
            ssh_user: "testuser".to_string(),
            ssh_key: "infra/slurm/ssh/slurm_test".to_string(),
            ssh_known_hosts: "accept".to_string(),
            poll_interval_secs: 2,
            template_dir: "/opt/petri/templates".to_string(),
            lookback_window_secs: 3600,
        };

        // Build scenario and engine
        let scenario = TestScenario::slurm_batch();

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

        // Register the real SchedulerSubmitHandler with SlurmClient
        let signal_routes = HashMap::from([
            ("running".to_string(), "sig_running".to_string()),
            ("completed".to_string(), "sig_completed".to_string()),
            ("failed".to_string(), "sig_failed".to_string()),
            ("timed_out".to_string(), "sig_timed_out".to_string()),
        ]);
        let slurm_client = SlurmClient::new(
            slurm_config.clone(),
            &net_id,
            "sig_completed",
            signal_routes,
        );
        let handler = Arc::new(SchedulerSubmitHandler::new(
            Arc::new(slurm_client),
            "default",
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
            slurm_config,
            net_id,
            eval_notify,
        }
    }

    /// Start a fresh SlurmWatcher + SignalListener pair.
    async fn start_components(&self) -> LiveComponents {
        // SlurmWatcher
        let watcher = SlurmWatcher::new(self.slurm_config.clone(), self.jetstream.clone())
            .await
            .expect("create SlurmWatcher");
        let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
        let watcher_handle = tokio::spawn(async move {
            watcher.run(shutdown_rx).await;
        });

        // SignalListener
        let signal_listener = Arc::new(SignalListener::new(
            self.net_id.clone(),
            self.jetstream.clone(),
        ));
        let listener_handle = signal_listener
            .clone()
            .start(self.service.clone(), self.eval_notify.clone());

        // Let components start up
        tokio::time::sleep(Duration::from_secs(2)).await;

        LiveComponents {
            watcher_shutdown_tx: shutdown_tx,
            watcher_handle,
            signal_listener,
            listener_handle,
        }
    }

    /// Run the full lifecycle: evaluate (submits jobs) -> poll until all reach
    /// `completed` place, evaluating between polls to consume arriving signals.
    ///
    /// Slurm poll interval is 2s, so we use a 60s timeout (vs. Nomad's 30s).
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
            if start.elapsed() > Duration::from_secs(60) {
                panic!(
                    "run_full_lifecycle timed out after 60s. Marking: {:?}",
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
            "sig_timed_out",
            "job_queue",
            "submitted_jobs",
            "running_jobs",
            "failed_jobs",
            "effect_errors",
            "dead_letter",
            "timed_out",
        ] {
            assert_eq!(
                marking.tokens_at(&self.scenario.places[*place_name]).len(),
                0,
                "{} should be empty",
                place_name
            );
        }
    }

    /// Read the SlurmWatcher KV checkpoint cursor (ISO timestamp string).
    async fn read_checkpoint_cursor(&self) -> Option<String> {
        let kv = self.jetstream.get_key_value("PETRI_WATCHER").await.ok()?;
        match kv.get("slurm.poll_cursor").await {
            Ok(Some(bytes)) => {
                let s = std::str::from_utf8(&bytes).ok()?;
                Some(s.to_string())
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
        self.service
            .initialize(self.scenario.net.clone())
            .await
            .unwrap();
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
// Test 1: Happy-path E2E
// ===========================================================================

/// Full Slurm E2E: submit 3 batch jobs -> Slurm runs trivial script ->
/// SlurmWatcher detects completion -> NATS signal -> SignalListener injects
/// token -> t_success fires -> all 3 jobs land in completed place.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_slurm_batch_net_real_dispatch() {
    let harness = SlurmTestHarness::setup().await;
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
/// 1. Run full lifecycle (3 jobs -> completed)
/// 2. Read checkpoint -- assert non-empty ISO timestamp
/// 3. Kill watcher
/// 4. Reload scenario + reset consumer + start new watcher
/// 5. Run 3 more jobs
/// 6. Assert: 3 completed (only new batch), checkpoint advanced
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_watcher_checkpoint_survives_restart() {
    let harness = SlurmTestHarness::setup().await;
    let components = harness.start_components().await;

    // ---- Phase 1: run full lifecycle ----
    harness.run_full_lifecycle().await;

    let marking = harness.service.get_marking().await;
    harness.assert_clean_completion(&marking, 3);

    // Read checkpoint -- should be a non-empty ISO timestamp
    let checkpoint_1 = harness
        .read_checkpoint_cursor()
        .await
        .expect("checkpoint should exist after first run");
    assert!(
        !checkpoint_1.is_empty(),
        "Checkpoint should be non-empty, got empty string"
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
        .read_checkpoint_cursor()
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
// Test 3: No duplicate signals after watcher restart
// ===========================================================================

/// Verifies that restarting the watcher after a completed lifecycle does NOT
/// produce duplicate signals.
///
/// 1. Run full lifecycle (3 jobs -> completed)
/// 2. Kill watcher + restart with fresh instance
/// 3. Wait several poll cycles
/// 4. Re-evaluate
/// 5. Assert: still exactly 3 in completed (not 6 from duplicate signals)
///
/// With Slurm's poll-based watcher, duplicates are prevented by:
/// - KV checkpoint cursor: new watcher only queries sacct from the saved timestamp
/// - JetStream msg_id dedup: signal publish uses `slurm-{job_id}-{status}` as msg_id
/// - Tracked jobs persistence: new watcher can infer completion for jobs that
///   disappeared from squeue during downtime
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn test_no_duplicate_signals_after_watcher_restart() {
    let harness = SlurmTestHarness::setup().await;
    let components = harness.start_components().await;

    // Phase 1: Run the full lifecycle (all 3 jobs -> completed)
    harness.run_full_lifecycle().await;

    let marking = harness.service.get_marking().await;
    harness.assert_clean_completion(&marking, 3);

    // Phase 2: Kill watcher, start a fresh one
    harness.stop_watcher(&components).await;

    let new_watcher = SlurmWatcher::new(harness.slurm_config.clone(), harness.jetstream.clone())
        .await
        .expect("create new SlurmWatcher");
    let (shutdown_tx_2, shutdown_rx_2) = tokio::sync::broadcast::channel::<()>(1);
    let watcher_handle_2 = tokio::spawn(async move {
        new_watcher.run(shutdown_rx_2).await;
    });

    // Let the new watcher run several poll cycles (it may re-discover old jobs
    // from the checkpoint window). Any duplicate signals should be deduplicated
    // by JetStream msg_id or simply fail to match (no tokens in running_jobs).
    tokio::time::sleep(Duration::from_secs(6)).await;

    // Re-evaluate to ensure any stale signals don't create extra tokens
    let _ = harness.service.evaluate_until_quiescent(20).await;

    let marking = harness.service.get_marking().await;
    // Should still be exactly 3 in completed -- no duplicates
    harness.assert_clean_completion(&marking, 3);

    // Cleanup
    let _ = shutdown_tx_2.send(());
    let _ = tokio::time::timeout(Duration::from_secs(5), watcher_handle_2).await;
    components.listener_handle.abort();
    if let Ok(stream) = harness.jetstream.get_stream("PETRI_GLOBAL").await {
        let consumer_name = format!("signal-inbound-{}", harness.net_id);
        let _ = stream.delete_consumer(&consumer_name).await;
    }
}
