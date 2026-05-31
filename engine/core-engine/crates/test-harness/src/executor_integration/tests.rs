//! Real executor E2E tests for the executor_lifecycle scenario.
//!
//! Submits execution jobs via the `executor_submit` effect handler, a real
//! executor worker (apalis + ProcessBackend) receives and executes jobs,
//! publishes status updates, the ExecutorWatcher consumes those, publishes
//! signals, and the SignalListener injects them into the engine which fires
//! signal-join transitions.
//!
//! Infrastructure:
//! - NATS JetStream (testcontainer via `shared_nats_url`)
//! - PetriNetService with real ExecutorSubmitHandler + ExecutorNatsClient
//! - Real executor worker (apalis + ProcessBackend + StatusReporter)
//! - ExecutorWatcher publishing signals from status updates
//! - SignalListener injecting tokens from NATS into the engine

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::Notify;

use aithericon_executor_backend::ProcessBackend;

/// Initialize tracing subscriber (once). Shows watcher/signal logs on stderr.
fn init_tracing() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new(
                        "warn,petri_executor=debug,petri_scheduler_bridge=debug,petri_nats=debug",
                    )
                }),
            )
            .with_test_writer()
            .try_init();
    });
}
use aithericon_executor_domain::ExecutionJob;
use aithericon_executor_worker::{
    handle_execution, BackendRegistry, CancellationRegistry, CleanupPolicy, JobExecutor,
    SidecarLogConfig, StatusReporter,
};
use apalis::prelude::*;
use apalis_nats::NatsStorage;
use petri_application::{ExecutorSubmitHandler, PetriNetService};
use petri_domain::{ExecutorClient, Marking, TokenColor};
use petri_executor::{ExecutorConfig, ExecutorNatsClient, ExecutorWatcher};
use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};
use petri_nats::{NatsConfig, NatsEventPublisher, SignalListener};

use crate::fixtures::TestScenario;
use crate::nats::{ensure_global_stream, shared_nats_url};

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

type ExecutorService =
    PetriNetService<NatsEventPublisher<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>;

// ---------------------------------------------------------------------------
// Real executor worker — spawns an apalis worker with ProcessBackend
// ---------------------------------------------------------------------------

/// Spawn a real executor worker connected to the given NATS server.
///
/// Creates `EXECUTOR_STATUS` and `EXECUTOR_EVENTS` streams (standard names),
/// pulls jobs from `{namespace}_medium` (matching `ExecutorNatsClient`), and
/// actually executes them via `ProcessBackend` (runs `echo hello` etc.).
async fn spawn_real_executor(nats_url: &str, namespace: &str) -> tokio::task::JoinHandle<()> {
    let client = async_nats::connect(nats_url)
        .await
        .expect("connect NATS for executor worker");
    let jetstream = async_nats::jetstream::new(client.clone());

    let reporter = StatusReporter::new(jetstream, "test-executor".into(), 1)
        .await
        .expect("create StatusReporter");

    let registry =
        Arc::new(BackendRegistry::new(Duration::from_secs(30)).register(ProcessBackend::new()));

    let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
    let base_dir = PathBuf::from(format!("/tmp/ex-{short_id}"));
    let pipeline = Arc::new(aithericon_executor_worker::staging::default_pipeline(
        base_dir.clone(),
        None,
        None,
        None,
    ));

    let storage = NatsStorage::<ExecutionJob>::new_with_config(
        client,
        apalis_nats::Config {
            namespace: namespace.to_string(),
            ack_wait: Duration::from_secs(5),
            max_ack_pending: 10,
            ..Default::default()
        },
    )
    .await
    .expect("create NatsStorage for executor worker");

    let executor = Arc::new(JobExecutor {
        reporter,
        registry,
        pipeline,
        base_dir,
        artifact_store: None,
        cleanup_policy: CleanupPolicy::Immediate,
        metric_sink: None,
        log_sink: None,
        cancel_registry: CancellationRegistry::new(),
        log_config: SidecarLogConfig::default(),
        completion_tracker: None,
    });

    let worker = WorkerBuilder::new("test-executor")
        .concurrency(4)
        .data(executor)
        .backend(storage)
        .build_fn(handle_execution);

    tokio::spawn(async move {
        let _ = Monitor::new().register(worker).run().await;
    })
}

// ---------------------------------------------------------------------------
// ExecutorTestHarness — reusable test infrastructure
// ---------------------------------------------------------------------------

/// Optional configuration for test harness setup.
#[derive(Default)]
struct HarnessOptions {
    /// Process context to inject into seed tokens for dual-publish testing.
    process_id: Option<String>,
    process_step: Option<String>,
}

struct ExecutorTestHarness {
    service: Arc<ExecutorService>,
    jetstream: async_nats::jetstream::Context,
    scenario: TestScenario,
    executor_config: ExecutorConfig,
    net_id: String,
    eval_notify: Arc<Notify>,
}

struct LiveComponents {
    watcher_shutdown_tx: tokio::sync::broadcast::Sender<()>,
    watcher_handle: tokio::task::JoinHandle<()>,
    executor_handle: tokio::task::JoinHandle<()>,
    signal_listener: Arc<SignalListener>,
    listener_handle: tokio::task::JoinHandle<()>,
}

impl ExecutorTestHarness {
    async fn setup() -> Self {
        Self::setup_with(HarnessOptions::default()).await
    }

    async fn setup_with(opts: HarnessOptions) -> Self {
        let nats_url: &str = shared_nats_url().await;
        let nats_client = async_nats::connect(nats_url).await.expect("connect NATS");
        let jetstream = async_nats::jetstream::new(nats_client.clone());

        let net_id = format!("exec-integ-{}", uuid::Uuid::new_v4().simple());
        let namespace = "executor_integ_jobs";

        // Ensure the PETRI_GLOBAL stream exists
        ensure_global_stream(&jetstream)
            .await
            .expect("PETRI_GLOBAL stream");

        // Build scenario and engine
        let mut scenario = TestScenario::executor_lifecycle();

        // Inject process context into seed tokens if provided.
        // The ExecutorSubmitHandler will pick these up from token data.
        if let (Some(ref pid), Some(ref step)) = (&opts.process_id, &opts.process_step) {
            for (_place_id, token) in &mut scenario.initial_tokens {
                if let TokenColor::Data(ref mut value) = token.color {
                    if let Some(obj) = value.as_object_mut() {
                        obj.insert("process_id".to_string(), serde_json::json!(pid));
                        obj.insert("process_step".to_string(), serde_json::json!(step));
                    }
                }
            }
        }

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

        // Register the real ExecutorSubmitHandler with ExecutorNatsClient.
        // Signal routes use SDK IDs which are now the PlaceIds (from from_sdk).
        let signal_routes = HashMap::from([
            ("accepted".to_string(), "sig_accepted".to_string()),
            ("running".to_string(), "sig_running".to_string()),
            ("completed".to_string(), "sig_completed".to_string()),
            ("failed".to_string(), "sig_failed".to_string()),
        ]);
        let executor_client = ExecutorNatsClient::new(
            nats_client.clone(),
            jetstream.clone(),
            &net_id,
            "sig_accepted",
            signal_routes,
            HashMap::new(),
            namespace,
        );
        let handler = Arc::new(ExecutorSubmitHandler::new(
            Arc::new(executor_client) as Arc<dyn ExecutorClient>,
            "job",
            "submitted",
        ));
        service
            .register_effect_handler("executor_submit", handler)
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

        let executor_config = ExecutorConfig {
            nats_url: nats_url.to_string(),
            namespace: namespace.to_string(),
            status_stream: "EXECUTOR_STATUS".to_string(),
            events_stream: "EXECUTOR_EVENTS".to_string(),
        };

        Self {
            service,
            jetstream,
            scenario,
            executor_config,
            net_id,
            eval_notify,
        }
    }

    /// Start a fresh real executor worker + ExecutorWatcher + SignalListener.
    async fn start_components(&self) -> LiveComponents {
        let executor_handle = spawn_real_executor(
            &self.executor_config.nats_url,
            &self.executor_config.namespace,
        )
        .await;

        // ExecutorWatcher
        let watcher = ExecutorWatcher::new(self.executor_config.clone(), self.jetstream.clone())
            .await
            .expect("create ExecutorWatcher");
        let (watcher_shutdown_tx, watcher_shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
        let watcher_handle = tokio::spawn(async move {
            watcher.run(watcher_shutdown_rx).await;
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
            watcher_shutdown_tx,
            watcher_handle,
            executor_handle,
            signal_listener,
            listener_handle,
        }
    }

    /// Run the full lifecycle: evaluate (submits jobs) → poll until all reach
    /// `completed` place, evaluating between polls to consume arriving signals.
    async fn run_full_lifecycle(&self) {
        let completed_id = self.scenario.places["completed"].clone();

        // Initial evaluate: submits jobs
        self.service.evaluate_until_quiescent(20).await.unwrap();

        // Poll until all 3 reach completed
        let start = tokio::time::Instant::now();
        loop {
            let marking = self.service.get_marking().await;
            if marking.tokens_at(&completed_id).len() >= 3 {
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                self.print_marking_debug(&marking);
                panic!("run_full_lifecycle timed out after 30s waiting for 3 completed tokens");
            }
            let _ = self.service.evaluate_until_quiescent(20).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Assert clean completion: N tokens in completed, all intermediate places empty.
    fn assert_clean_completion(&self, marking: &Marking, expected: usize) {
        let completed_id = &self.scenario.places["completed"];
        assert_eq!(
            marking.tokens_at(completed_id).len(),
            expected,
            "Expected {} tokens in completed place",
            expected
        );

        for place_name in &[
            "sig_accepted",
            "sig_running",
            "sig_completed",
            "sig_failed",
            "exec_queue",
            "submitted",
            "accepted",
            "running",
            "failed",
            "effect_errors",
            "dead_letter",
        ] {
            if let Some(pid) = self.scenario.places.get(*place_name) {
                assert_eq!(
                    marking.tokens_at(pid).len(),
                    0,
                    "{} should be empty",
                    place_name
                );
            }
        }
    }

    fn print_marking_debug(&self, marking: &Marking) {
        eprintln!("=== Marking debug ===");
        for (name, pid) in &self.scenario.places {
            let tokens = marking.tokens_at(pid);
            if !tokens.is_empty() && !name.contains(' ') {
                // Skip display names (contain spaces), show SDK IDs only
                eprintln!("  {}: {} tokens", name, tokens.len());
            }
        }
        eprintln!("====================");
    }

    /// Read the ExecutorWatcher KV checkpoint for status stream.
    async fn read_status_checkpoint(&self) -> Option<String> {
        let kv = self.jetstream.get_key_value("PETRI_WATCHER").await.ok()?;
        match kv.get("executor.status_seq").await {
            Ok(Some(bytes)) => std::str::from_utf8(&bytes).ok().map(|s| s.to_string()),
            _ => None,
        }
    }

    /// Stop all components.
    async fn stop_components(&self, components: LiveComponents) {
        components.executor_handle.abort();
        let _ = components.watcher_shutdown_tx.send(());
        components.listener_handle.abort();
        let _ = tokio::time::timeout(Duration::from_secs(5), components.executor_handle).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), components.watcher_handle).await;

        // Clean up the durable signal consumer (per-net, safe to remove between test phases)
        if let Ok(stream) = self.jetstream.get_stream("PETRI_GLOBAL").await {
            let consumer_name = format!("signal-inbound-{}", self.net_id);
            let _ = stream.delete_consumer(&consumer_name).await;
        }
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

/// Full executor E2E: submit 3 execution jobs → real executor worker runs
/// lifecycle (accepted → running → completed) → ExecutorWatcher detects
/// status → NATS signal → SignalListener injects token → signal-join
/// transitions fire → all 3 jobs land in completed place.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_executor_lifecycle_happy_path() {
    init_tracing();
    let harness = ExecutorTestHarness::setup().await;
    let components = harness.start_components().await;

    harness.run_full_lifecycle().await;

    let marking = harness.service.get_marking().await;
    harness.assert_clean_completion(&marking, 3);

    // Verify completed tokens have execution_ids
    let completed_id = &harness.scenario.places["completed"];
    for token in marking.tokens_at(completed_id) {
        let data = petri_application::token_color_to_json(&token.color);
        assert!(
            data.get("execution_id").is_some(),
            "Completed token should have execution_id"
        );
        assert!(
            data.get("job_id").is_some(),
            "Completed token should have job_id"
        );
    }

    harness.stop_components(components).await;
}

// ===========================================================================
// Test 2: Watcher checkpoint persists
// ===========================================================================

/// Verifies the KV checkpoint advances after processing status updates.
///
/// 1. Run full lifecycle (3 jobs → completed)
/// 2. Read checkpoint — assert it exists
/// 3. Stop components, reload scenario, start fresh
/// 4. Run 3 more jobs
/// 5. Assert: checkpoint advanced
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_executor_watcher_checkpoint_survives_restart() {
    let harness = ExecutorTestHarness::setup().await;
    let components = harness.start_components().await;

    // ---- Phase 1: run full lifecycle ----
    harness.run_full_lifecycle().await;

    let marking = harness.service.get_marking().await;
    harness.assert_clean_completion(&marking, 3);

    // Read checkpoint — should exist
    let checkpoint_1 = harness
        .read_status_checkpoint()
        .await
        .expect("checkpoint should exist after first run");
    assert!(!checkpoint_1.is_empty(), "Checkpoint should not be empty");

    // ---- Stop and restart ----
    harness.stop_components(components).await;
    harness.reload_scenario().await;

    let components = harness.start_components().await;

    // Advance epoch so stale signals from batch 1 are filtered
    components
        .signal_listener
        .advance_epoch()
        .await
        .expect("advance epoch");
    harness.run_full_lifecycle().await;

    let marking = harness.service.get_marking().await;
    harness.assert_clean_completion(&marking, 3);

    // Checkpoint should still exist and have advanced
    let checkpoint_2 = harness
        .read_status_checkpoint()
        .await
        .expect("checkpoint should still exist");
    let seq_1: u64 = checkpoint_1
        .parse()
        .expect("checkpoint_1 should be numeric");
    let seq_2: u64 = checkpoint_2
        .parse()
        .expect("checkpoint_2 should be numeric");
    assert!(
        seq_2 > seq_1,
        "Checkpoint should have advanced: {} -> {}",
        seq_1,
        seq_2
    );

    harness.stop_components(components).await;
}

// ===========================================================================
// Test 3: Scenario topology validation
// ===========================================================================

/// Verifies the executor_lifecycle test scenario has the expected structure.
#[tokio::test]
async fn test_executor_lifecycle_scenario_structure() {
    let scenario = TestScenario::executor_lifecycle();

    // 12 places (by SDK ID)
    let expected_places = [
        "exec_queue",
        "submitted",
        "accepted",
        "running",
        "completed",
        "failed",
        "effect_errors",
        "dead_letter",
        "sig_accepted",
        "sig_running",
        "sig_completed",
        "sig_failed",
    ];
    for name in &expected_places {
        assert!(
            scenario.places.contains_key(*name),
            "Missing place: {}",
            name
        );
    }

    // Verify effect transitions
    let submit = scenario
        .net
        .transitions
        .values()
        .find(|t| t.name == "Submit Execution")
        .expect("submit transition not found");
    assert_eq!(
        submit.effect_handler_id.as_deref(),
        Some("executor_submit"),
        "submit should use executor_submit effect"
    );

    // Verify guards
    let retry = scenario
        .net
        .transitions
        .values()
        .find(|t| t.name == "Retry Failed Execution")
        .expect("retry transition not found");
    assert_eq!(
        retry.guard.as_deref(),
        Some("err.retries < err.max_retries")
    );

    // 3 seed tokens
    assert_eq!(scenario.initial_tokens.len(), 3);
}

// ===========================================================================
// Test 4: Process events dual-publish (WIP)
// ===========================================================================
