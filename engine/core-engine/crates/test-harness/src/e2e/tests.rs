//! End-to-end scenario tests.
//!
//! Tests are organized in tiers from simple to complex:
//! - Tier 1: Simple pass-through (2 places, 1 transition)
//! - Tier 2: Resource allocation (4 places, 2 transitions)
//! - Tier 3: Producer-consumer with bounded buffer
//! - Tier 4: Guard expressions (conditional routing)
//! - Tier 5: Effect transitions with signal-based completion (nomad batch net)
//! - Tier 6: User-driven job cancellation
//! - Tier 7: Engine crash recovery from event log
//! - Tier 8: Terminal place completion detection
//! - Tier 9: Error surfacing (script failures → ErrorOccurred events)

use std::collections::HashMap;
use std::sync::Arc;

use petri_application::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};
use petri_application::PetriNetService;
use petri_domain::{DomainEvent, Token, TokenColor};

use crate::doubles::{MockEventRepository, MockStateProjection, MockTopologyRepository};
use crate::e2e::{MarkingAssertions, ScenarioTest};
use crate::fixtures::{TestContext, TestScenario};

// =============================================================================
// Tier 1: Simple Pass-Through
// =============================================================================

/// Basic token flow: A → T → B
#[tokio::test]
async fn test_simple_pass_through() {
    ScenarioTest::new(TestScenario::simple_pass_through())
        .expect_quiescent()
        .expect_empty("A")
        .expect_tokens("B", 1)
        .run()
        .await;
}

/// Multiple tokens flow through in sequence
#[tokio::test]
async fn test_simple_pass_through_multiple_tokens() {
    let mut scenario = TestScenario::simple_pass_through();
    let place_a = scenario.places["A"].clone();

    // Add more tokens
    scenario.initial_tokens = vec![
        (place_a.clone(), Token::new_unit()),
        (place_a.clone(), Token::new_unit()),
        (place_a.clone(), Token::new_unit()),
    ];

    ScenarioTest::new(scenario)
        .expect_quiescent()
        .expect_empty("A")
        .expect_tokens("B", 3)
        .run()
        .await;
}

/// Empty scenario (no tokens) should be quiescent immediately
#[tokio::test]
async fn test_simple_pass_through_no_tokens() {
    let mut scenario = TestScenario::simple_pass_through();
    scenario.initial_tokens.clear();

    ScenarioTest::new(scenario)
        .expect_quiescent()
        .expect_empty("A")
        .expect_empty("B")
        .run()
        .await;
}

// =============================================================================
// Tier 2: Resource Allocation
// =============================================================================

/// 2 workers, 3 tasks → all tasks complete, workers return
#[tokio::test]
async fn test_resource_allocation_completes_all_tasks() {
    ScenarioTest::new(TestScenario::resource_allocation())
        .expect_quiescent()
        .expect_empty("Tasks")
        .expect_empty("InProgress")
        .expect_tokens("Completed", 3)
        .expect_tokens("Workers", 2) // Workers returned to pool
        .run()
        .await;
}

/// Resource allocation with no workers should not make progress
#[tokio::test]
async fn test_resource_allocation_no_workers() {
    let mut scenario = TestScenario::resource_allocation();
    let workers_id = scenario.places["Workers"].clone();

    // Remove workers from initial tokens
    scenario
        .initial_tokens
        .retain(|(pid, _)| pid != &workers_id);

    ScenarioTest::new(scenario)
        .expect_quiescent()
        .expect_tokens("Tasks", 3) // Tasks remain
        .expect_empty("InProgress")
        .expect_empty("Completed")
        .expect_empty("Workers")
        .run()
        .await;
}

/// Single worker processes all tasks sequentially
#[tokio::test]
async fn test_resource_allocation_single_worker() {
    let mut scenario = TestScenario::resource_allocation();
    let workers_id = scenario.places["Workers"].clone();

    // Keep only one worker
    let mut found_worker = false;
    scenario.initial_tokens.retain(|(pid, _)| {
        if pid == &workers_id {
            if !found_worker {
                found_worker = true;
                true // Keep first worker
            } else {
                false // Remove other workers
            }
        } else {
            true // Keep non-worker tokens
        }
    });

    ScenarioTest::new(scenario)
        .expect_quiescent()
        .expect_empty("Tasks")
        .expect_empty("InProgress")
        .expect_tokens("Completed", 3)
        .expect_tokens("Workers", 1)
        .run()
        .await;
}

// =============================================================================
// Tier 3: Producer-Consumer with Bounded Buffer
// =============================================================================
//
// Note: The producer-consumer scenario is cyclic by design - consumed items
// return a Ready signal, which enables more production. This means it never
// reaches quiescence naturally; it runs until the step limit.

/// Bounded buffer with capacity 2, cyclic processing
#[tokio::test]
async fn test_producer_consumer_bounded() {
    // With a cyclic scenario, we verify that:
    // 1. The system hits the step limit (as expected for cycles)
    // 2. Items are being consumed (system is making progress)
    ScenarioTest::new(TestScenario::producer_consumer(2))
        .max_steps(20)
        .expect_limit_reached()
        .expect_at_least("Consumed", 5) // With cycling, many items get consumed
        .run()
        .await;
}

/// Large buffer doesn't restrict throughput
#[tokio::test]
async fn test_producer_consumer_large_buffer() {
    ScenarioTest::new(TestScenario::producer_consumer(10))
        .max_steps(20)
        .expect_limit_reached()
        .expect_at_least("Consumed", 5)
        .run()
        .await;
}

/// Buffer capacity 1 - serial processing (still cyclic)
#[tokio::test]
async fn test_producer_consumer_serial() {
    ScenarioTest::new(TestScenario::producer_consumer(1))
        .max_steps(20)
        .expect_limit_reached()
        .expect_at_least("Consumed", 5)
        .run()
        .await;
}

// =============================================================================
// Tier 4: Guard Expressions
// =============================================================================

/// Two requests: high-value approved, low-value rejected
#[tokio::test]
async fn test_guard_approve_and_reject() {
    ScenarioTest::new(TestScenario::with_guard())
        .expect_quiescent()
        .expect_empty("Input")
        .expect_tokens("Approved", 1) // amount >= 100
        .expect_tokens("Rejected", 1) // amount < 100
        .run()
        .await;
}

/// All high-value requests should be approved
#[tokio::test]
async fn test_guard_all_approved() {
    let mut scenario = TestScenario::with_guard();
    let input_id = scenario.places["Input"].clone();

    // Replace with all high-value requests
    scenario.initial_tokens = vec![
        (
            input_id.clone(),
            Token::new_data(serde_json::json!({"id": "R1", "amount": 150})),
        ),
        (
            input_id.clone(),
            Token::new_data(serde_json::json!({"id": "R2", "amount": 200})),
        ),
        (
            input_id.clone(),
            Token::new_data(serde_json::json!({"id": "R3", "amount": 100})),
        ), // Boundary
    ];

    ScenarioTest::new(scenario)
        .expect_quiescent()
        .expect_empty("Input")
        .expect_tokens("Approved", 3)
        .expect_empty("Rejected")
        .run()
        .await;
}

/// All low-value requests should be rejected
#[tokio::test]
async fn test_guard_all_rejected() {
    let mut scenario = TestScenario::with_guard();
    let input_id = scenario.places["Input"].clone();

    // Replace with all low-value requests
    scenario.initial_tokens = vec![
        (
            input_id.clone(),
            Token::new_data(serde_json::json!({"id": "R1", "amount": 50})),
        ),
        (
            input_id.clone(),
            Token::new_data(serde_json::json!({"id": "R2", "amount": 99})),
        ), // Just under boundary
    ];

    ScenarioTest::new(scenario)
        .expect_quiescent()
        .expect_empty("Input")
        .expect_empty("Approved")
        .expect_tokens("Rejected", 2)
        .run()
        .await;
}

/// Boundary test: amount exactly 100 should be approved
#[tokio::test]
async fn test_guard_boundary_exact() {
    let mut scenario = TestScenario::with_guard();
    let input_id = scenario.places["Input"].clone();

    scenario.initial_tokens = vec![(
        input_id.clone(),
        Token::new_data(serde_json::json!({"id": "R1", "amount": 100})),
    )];

    ScenarioTest::new(scenario)
        .expect_quiescent()
        .expect_empty("Input")
        .expect_tokens("Approved", 1)
        .expect_empty("Rejected")
        .run()
        .await;
}

// =============================================================================
// Edge Cases and Error Handling
// =============================================================================

/// Empty scenario is immediately quiescent
#[tokio::test]
async fn test_empty_scenario() {
    ScenarioTest::new(TestScenario::empty())
        .expect_quiescent()
        .run()
        .await;
}

/// Step limit prevents infinite loops (though our scenarios don't have them)
#[tokio::test]
async fn test_max_steps_limit() {
    // Producer-consumer with cyclic ready signal could run forever if we kept adding signals
    // With limit of 5, it will stop mid-execution
    ScenarioTest::new(TestScenario::producer_consumer(2))
        .max_steps(5)
        .expect_limit_reached()
        .expect_at_least("Consumed", 1) // At least some were consumed
        .run()
        .await;
}

// =============================================================================
// Tier 5: Effect Transitions with Signal-Based Completion (Nomad Batch Net)
// =============================================================================
//
// These tests exercise the nomad_batch_net scenario end-to-end:
// - Effect transition (scheduler_submit) with mock handler
// - Signal injection simulating NomadWatcher completion events
// - Guard-based retry and dead-letter routing
//
// Because effect transitions require a registered handler and signals must be
// injected between evaluation phases, these tests use the lower-level
// TestContext + service API instead of the ScenarioTest fluent builder.

/// Mock scheduler_submit handler that simulates Nomad job dispatch.
///
/// Reads `job_id` and `run` from the input token on port "job",
/// produces a `SubmittedJob` token on port "submitted" with a
/// deterministic `scheduler_job_id` of `"nomad-{job_id}-{run}"`.
struct MockSchedulerSubmit;

/// Mock scheduler_cancel handler that simulates Nomad job cancellation.
///
/// Reads the job token from port "job", produces it on port "cancelled"
/// with `cancelled: true` added — mirroring the real `SchedulerCancelHandler`.
struct MockSchedulerCancel;

#[async_trait::async_trait]
impl EffectHandler for MockSchedulerSubmit {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let job = input
            .inputs
            .get("job")
            .expect("scheduler_submit requires 'job' input port");
        let job_id = job["job_id"].as_str().unwrap();
        let run = job["run"].as_i64().unwrap();

        let scheduler_job_id = format!("nomad-{}-{}", job_id, run);

        let mut submitted = job.clone();
        submitted["scheduler_job_id"] = serde_json::Value::String(scheduler_job_id);

        let mut tokens = HashMap::new();
        tokens.insert("submitted".to_string(), submitted);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({"dispatched": true}),
        })
    }

    fn name(&self) -> &str {
        "mock_scheduler_submit"
    }
}

#[async_trait::async_trait]
impl EffectHandler for MockSchedulerCancel {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let job = input
            .inputs
            .get("job")
            .expect("scheduler_cancel requires 'job' input port");

        let mut cancelled = job.clone();
        cancelled["cancelled"] = serde_json::Value::Bool(true);

        let mut tokens = HashMap::new();
        tokens.insert("cancelled".to_string(), cancelled);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({"cancelled": job["scheduler_job_id"]}),
        })
    }

    fn name(&self) -> &str {
        "mock_scheduler_cancel"
    }
}

/// Simulate an engine crash by dumping events from the current context
/// and creating a new PetriNetService that replays from the event log.
///
/// Returns `(service, event_repo)` so callers can register effect handlers
/// and continue evaluation on the recovered service.
async fn simulate_crash(
    source_ctx: &TestContext<MockEventRepository, MockTopologyRepository, MockStateProjection>,
    scenario: &TestScenario,
) -> (
    Arc<PetriNetService<MockEventRepository, MockTopologyRepository, MockStateProjection>>,
    Arc<MockEventRepository>,
) {
    let events = source_ctx.event_repo.recorded_events();

    let event_repo = Arc::new(MockEventRepository::with_events(events));
    let topology_repo = Arc::new(MockTopologyRepository::with_topology(scenario.net.clone()));
    let projection = Arc::new(MockStateProjection::new());

    let service = Arc::new(PetriNetService::new(
        event_repo.clone(),
        topology_repo,
        projection,
    ));

    (service, event_repo)
}

/// Phase 1: All three batch jobs are submitted via the effect handler.
///
/// Verifies that the mock scheduler_submit handler is called for each job,
/// producing SubmittedJob tokens with scheduler_job_id values.
#[tokio::test]
async fn test_nomad_batch_phase1_submit() {
    let scenario = TestScenario::nomad_batch();

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    test_ctx
        .service
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    let result = test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // 3 jobs submitted → 3 effect firings
    assert_eq!(result.steps_executed, 3, "All 3 jobs should be submitted");

    let marking = test_ctx.service.get_marking().await;
    marking.assert_empty(&scenario.places["job_queue"]);
    marking.assert_token_count(&scenario.places["submitted_jobs"], 3);
    marking.assert_empty(&scenario.places["effect_errors"]);
}

/// Phase 2: Signal-based completion and failure routing via per-status signal places.
///
/// After submitting all 3 jobs, injects "running" signals for all 3, then:
/// - batch-001: "completed" signal → completed place
/// - batch-002: "failed" signal → retry (retries:0 < max_retries:2) → resubmitted
/// - batch-003: no completion signal injected, remains in running_jobs
#[tokio::test]
async fn test_nomad_batch_phase2_signal_completion() {
    let scenario = TestScenario::nomad_batch();

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    test_ctx
        .service
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    // Phase 1: submit all jobs
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    let sig_running_place = &scenario.places["sig_running"];
    let sig_completed_place = &scenario.places["sig_completed"];
    let sig_failed_place = &scenario.places["sig_failed"];

    // Inject "running" signals for all 3 jobs
    for job_id in &[
        "nomad-batch-001-0",
        "nomad-batch-002-0",
        "nomad-batch-003-0",
    ] {
        test_ctx
            .service
            .create_token(
                sig_running_place.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }

    // Evaluate: t_running fires for all 3
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // Inject "completed" signal for batch-001
    test_ctx
        .service
        .create_token(
            sig_completed_place.clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-001-0",
                "exit_code": 0,
                "node_name": "node-1",
                "message": "",
                "allocation_id": "alloc-001",
                "node_id": "node-id-1"
            })),
        )
        .await
        .unwrap();

    // Inject "failed" signal for batch-002
    test_ctx
        .service
        .create_token(
            sig_failed_place.clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-002-0",
                "exit_code": 1,
                "node_name": "node-2",
                "message": "OOM killed",
                "allocation_id": "alloc-002",
                "node_id": "node-id-2"
            })),
        )
        .await
        .unwrap();

    // Phase 2: evaluate signal joins + retry transitions
    let result = test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // t_success(batch-001) + t_failed(batch-002) + retry(batch-002) + resubmit(batch-002) = 4
    assert_eq!(
        result.steps_executed, 4,
        "Expected 4 steps: success + failure + retry + resubmit"
    );

    let marking = test_ctx.service.get_marking().await;

    // batch-001 completed successfully
    marking.assert_token_count(&scenario.places["completed"], 1);

    // batch-002 retried+resubmitted → 1 in submitted_jobs, batch-003 in running_jobs → 1
    marking.assert_token_count(&scenario.places["submitted_jobs"], 1);
    marking.assert_token_count(&scenario.places["running_jobs"], 1);

    // No failures lingering
    marking.assert_empty(&scenario.places["failed_jobs"]);
}

/// Full lifecycle: submit → running → complete/fail → retry → dead-letter.
///
/// batch-001: completed on first try
/// batch-002: fails, retries once, then completes
/// batch-003: fails, retries once (max_retries=1), fails again → dead-letter
#[tokio::test]
async fn test_nomad_batch_full_lifecycle() {
    let scenario = TestScenario::nomad_batch();

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    test_ctx
        .service
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    let sig_running_place = &scenario.places["sig_running"];
    let sig_completed_place = &scenario.places["sig_completed"];
    let sig_failed_place = &scenario.places["sig_failed"];

    // Phase 1: submit all 3 jobs
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // Phase 1b: inject "running" signals for all 3 jobs
    for job_id in &[
        "nomad-batch-001-0",
        "nomad-batch-002-0",
        "nomad-batch-003-0",
    ] {
        test_ctx
            .service
            .create_token(
                sig_running_place.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }

    // Evaluate: t_running fires for all 3
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // Phase 2: batch-001 completes, batch-002 and batch-003 fail
    test_ctx
        .service
        .create_token(
            sig_completed_place.clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-001-0",
                "exit_code": 0,
                "node_name": "node-1",
                "message": "",
                "allocation_id": "alloc-001",
                "node_id": "node-id-1"
            })),
        )
        .await
        .unwrap();

    test_ctx
        .service
        .create_token(
            sig_failed_place.clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-002-0",
                "exit_code": 1,
                "node_name": "node-2",
                "message": "OOM killed",
                "allocation_id": "alloc-002",
                "node_id": "node-id-2"
            })),
        )
        .await
        .unwrap();

    test_ctx
        .service
        .create_token(
            sig_failed_place.clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-003-0",
                "exit_code": 137,
                "node_name": "node-3",
                "message": "timeout",
                "allocation_id": "alloc-003",
                "node_id": "node-id-3"
            })),
        )
        .await
        .unwrap();

    // Evaluate: signal joins + retries + resubmits
    test_ctx.service.evaluate_until_quiescent(30).await.unwrap();

    let marking = test_ctx.service.get_marking().await;
    // batch-001 → completed
    marking.assert_token_count(&scenario.places["completed"], 1);
    // batch-002 retried (retries:0 < max:2), now in submitted_jobs at run=1
    // batch-003 retried (retries:0 < max:1), now in submitted_jobs at run=1
    marking.assert_empty(&scenario.places["failed_jobs"]);

    // Phase 2b: inject "running" signals for retried jobs
    for job_id in &["nomad-batch-002-1", "nomad-batch-003-1"] {
        test_ctx
            .service
            .create_token(
                sig_running_place.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }

    // Evaluate: t_running fires for both retried jobs
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // Phase 3: batch-002 retry succeeds, batch-003 retry fails again
    test_ctx
        .service
        .create_token(
            sig_completed_place.clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-002-1",
                "exit_code": 0,
                "node_name": "node-2",
                "message": "",
                "allocation_id": "alloc-004",
                "node_id": "node-id-2"
            })),
        )
        .await
        .unwrap();

    test_ctx
        .service
        .create_token(
            sig_failed_place.clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-003-1",
                "exit_code": 137,
                "node_name": "node-3",
                "message": "timeout again",
                "allocation_id": "alloc-005",
                "node_id": "node-id-3"
            })),
        )
        .await
        .unwrap();

    // Evaluate: batch-002 completes, batch-003 hits dead-letter
    test_ctx.service.evaluate_until_quiescent(30).await.unwrap();

    let marking = test_ctx.service.get_marking().await;

    // batch-001 + batch-002 completed
    marking.assert_token_count(&scenario.places["completed"], 2);

    // batch-003: retries:1 >= max_retries:1 → dead-letter
    marking.assert_token_count(&scenario.places["dead_letter"], 1);

    // All terminal — nothing left in intermediate places
    marking.assert_empty(&scenario.places["job_queue"]);
    marking.assert_empty(&scenario.places["submitted_jobs"]);
    marking.assert_empty(&scenario.places["running_jobs"]);
    marking.assert_empty(&scenario.places["failed_jobs"]);
    marking.assert_empty(&scenario.places["effect_errors"]);
}

// =============================================================================
// Tier 6: User-Driven Job Cancellation
// =============================================================================
//
// Tests that a user can cancel jobs by injecting a cancel_request token with
// the target scheduler_job_id. Cancellation works on both submitted and
// running jobs via the scheduler_cancel effect handler.

/// Cancel a submitted job before it starts running.
///
/// submit all 3 → inject cancel_request for batch-001 → evaluate →
/// batch-001 in cancelled, batch-002/003 still in submitted_jobs
#[tokio::test]
async fn test_nomad_batch_cancel_submitted_job() {
    let scenario = TestScenario::nomad_batch();

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    test_ctx
        .service
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();
    test_ctx
        .service
        .register_effect_handler("scheduler_cancel", Arc::new(MockSchedulerCancel))
        .unwrap();

    // Phase 1: submit all 3 jobs
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    let marking = test_ctx.service.get_marking().await;
    marking.assert_token_count(&scenario.places["submitted_jobs"], 3);

    // Inject cancel_request for batch-001 (scheduler_job_id = "nomad-batch-001-0")
    test_ctx
        .service
        .create_token(
            scenario.places["cancel_request"].clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-001-0"
            })),
        )
        .await
        .unwrap();

    // Evaluate: cancel_submitted fires for batch-001
    let result = test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    assert!(result.steps_executed >= 1, "cancel_submitted should fire");

    let marking = test_ctx.service.get_marking().await;

    // batch-001 cancelled
    marking.assert_token_count(&scenario.places["cancelled"], 1);

    // batch-002 and batch-003 still submitted
    marking.assert_token_count(&scenario.places["submitted_jobs"], 2);

    // cancel_request consumed
    marking.assert_empty(&scenario.places["cancel_request"]);

    // Verify the cancelled token has the right data
    let cancelled_tokens = marking.tokens_at(&scenario.places["cancelled"]);
    match &cancelled_tokens[0].color {
        TokenColor::Data(data) => {
            assert_eq!(data["scheduler_job_id"], "nomad-batch-001-0");
            assert_eq!(data["cancelled"], true);
        }
        other => panic!("Expected Data token, got {:?}", other),
    }
}

/// Cancel a running job.
///
/// submit all 3 → running signals → inject cancel_request for batch-002 →
/// evaluate → batch-002 in cancelled, others still in running_jobs
#[tokio::test]
async fn test_nomad_batch_cancel_running_job() {
    let scenario = TestScenario::nomad_batch();

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    test_ctx
        .service
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();
    test_ctx
        .service
        .register_effect_handler("scheduler_cancel", Arc::new(MockSchedulerCancel))
        .unwrap();

    // Phase 1: submit all 3 jobs
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // Phase 2: inject running signals for all 3
    let sig_running_place = &scenario.places["sig_running"];
    for job_id in &[
        "nomad-batch-001-0",
        "nomad-batch-002-0",
        "nomad-batch-003-0",
    ] {
        test_ctx
            .service
            .create_token(
                sig_running_place.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }

    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    let marking = test_ctx.service.get_marking().await;
    marking.assert_token_count(&scenario.places["running_jobs"], 3);

    // Inject cancel_request for batch-002
    test_ctx
        .service
        .create_token(
            scenario.places["cancel_request"].clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-002-0"
            })),
        )
        .await
        .unwrap();

    // Evaluate: cancel_running fires for batch-002
    let result = test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    assert!(result.steps_executed >= 1, "cancel_running should fire");

    let marking = test_ctx.service.get_marking().await;

    // batch-002 cancelled
    marking.assert_token_count(&scenario.places["cancelled"], 1);

    // batch-001 and batch-003 still running
    marking.assert_token_count(&scenario.places["running_jobs"], 2);

    // Now complete the remaining two normally
    let sig_completed_place = &scenario.places["sig_completed"];
    for job_id in &["nomad-batch-001-0", "nomad-batch-003-0"] {
        test_ctx
            .service
            .create_token(
                sig_completed_place.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "exit_code": 0,
                    "node_name": "node-1",
                    "message": "",
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }

    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    let marking = test_ctx.service.get_marking().await;

    // 2 completed + 1 cancelled = all 3 jobs terminal
    marking.assert_token_count(&scenario.places["completed"], 2);
    marking.assert_token_count(&scenario.places["cancelled"], 1);
    marking.assert_empty(&scenario.places["running_jobs"]);
    marking.assert_empty(&scenario.places["submitted_jobs"]);
    marking.assert_empty(&scenario.places["job_queue"]);
}

// =============================================================================
// Tier 7: Engine Crash Recovery from Event Log
// =============================================================================
//
// These tests simulate engine crashes at various lifecycle stages by dumping
// the event log and creating a new PetriNetService that replays from it.
// The recovered service should have an identical marking and be able to
// continue evaluation normally.

/// Crash after all 3 jobs submitted. Recovery should reconstruct the marking
/// and allow signal injection to continue the lifecycle.
#[tokio::test]
async fn test_crash_recovery_after_submit() {
    let scenario = TestScenario::nomad_batch();

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    test_ctx
        .service
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    // Submit all 3 jobs
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    let marking = test_ctx.service.get_marking().await;
    marking.assert_empty(&scenario.places["job_queue"]);
    marking.assert_token_count(&scenario.places["submitted_jobs"], 3);

    // === Crash and recover ===
    let (recovered, _) = simulate_crash(&test_ctx, &scenario).await;
    recovered
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    // Verify recovered marking matches pre-crash state
    let recovered_marking = recovered.get_marking().await;
    recovered_marking.assert_empty(&scenario.places["job_queue"]);
    recovered_marking.assert_token_count(&scenario.places["submitted_jobs"], 3);
    recovered_marking.assert_empty(&scenario.places["running_jobs"]);
    recovered_marking.assert_empty(&scenario.places["completed"]);

    // Continue: inject running signals on recovered service
    let sig_running = &scenario.places["sig_running"];
    for job_id in &[
        "nomad-batch-001-0",
        "nomad-batch-002-0",
        "nomad-batch-003-0",
    ] {
        recovered
            .create_token(
                sig_running.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }

    recovered.evaluate_until_quiescent(20).await.unwrap();

    let marking = recovered.get_marking().await;
    marking.assert_token_count(&scenario.places["running_jobs"], 3);
    marking.assert_empty(&scenario.places["submitted_jobs"]);
}

/// Crash while 3 jobs are running. Recovery should let them complete normally.
#[tokio::test]
async fn test_crash_recovery_while_running() {
    let scenario = TestScenario::nomad_batch();

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    test_ctx
        .service
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    // Submit all 3
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // Move all to running
    let sig_running = &scenario.places["sig_running"];
    for job_id in &[
        "nomad-batch-001-0",
        "nomad-batch-002-0",
        "nomad-batch-003-0",
    ] {
        test_ctx
            .service
            .create_token(
                sig_running.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }

    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    let marking = test_ctx.service.get_marking().await;
    marking.assert_token_count(&scenario.places["running_jobs"], 3);

    // === Crash and recover ===
    let (recovered, _) = simulate_crash(&test_ctx, &scenario).await;
    recovered
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    let recovered_marking = recovered.get_marking().await;
    recovered_marking.assert_token_count(&scenario.places["running_jobs"], 3);
    recovered_marking.assert_empty(&scenario.places["submitted_jobs"]);
    recovered_marking.assert_empty(&scenario.places["completed"]);

    // Complete all jobs from recovered state
    let sig_completed = &scenario.places["sig_completed"];
    for job_id in &[
        "nomad-batch-001-0",
        "nomad-batch-002-0",
        "nomad-batch-003-0",
    ] {
        recovered
            .create_token(
                sig_completed.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "exit_code": 0,
                    "node_name": "node-1",
                    "message": "",
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }

    recovered.evaluate_until_quiescent(20).await.unwrap();

    let marking = recovered.get_marking().await;
    marking.assert_token_count(&scenario.places["completed"], 3);
    marking.assert_empty(&scenario.places["running_jobs"]);
    marking.assert_empty(&scenario.places["job_queue"]);
}

/// Crash mid-evaluation: 1 job completed, 2 still running. Recovery should
/// preserve partial state and allow the remaining jobs to finish.
#[tokio::test]
async fn test_crash_recovery_partial_progress() {
    let scenario = TestScenario::nomad_batch();

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    test_ctx
        .service
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    // Submit all 3
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // All running
    let sig_running = &scenario.places["sig_running"];
    for job_id in &[
        "nomad-batch-001-0",
        "nomad-batch-002-0",
        "nomad-batch-003-0",
    ] {
        test_ctx
            .service
            .create_token(
                sig_running.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // Complete only batch-001
    test_ctx
        .service
        .create_token(
            scenario.places["sig_completed"].clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-001-0",
                "exit_code": 0,
                "node_name": "node-1",
                "message": "",
                "allocation_id": "alloc-001",
                "node_id": "node-id-1"
            })),
        )
        .await
        .unwrap();

    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // Verify partial state: 1 completed, 2 still running
    let marking = test_ctx.service.get_marking().await;
    marking.assert_token_count(&scenario.places["completed"], 1);
    marking.assert_token_count(&scenario.places["running_jobs"], 2);

    // === Crash and recover ===
    let (recovered, _) = simulate_crash(&test_ctx, &scenario).await;
    recovered
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    // Verify partial state is preserved
    let recovered_marking = recovered.get_marking().await;
    recovered_marking.assert_token_count(&scenario.places["completed"], 1);
    recovered_marking.assert_token_count(&scenario.places["running_jobs"], 2);
    recovered_marking.assert_empty(&scenario.places["submitted_jobs"]);
    recovered_marking.assert_empty(&scenario.places["job_queue"]);

    // Complete remaining jobs
    let sig_completed = &scenario.places["sig_completed"];
    for job_id in &["nomad-batch-002-0", "nomad-batch-003-0"] {
        recovered
            .create_token(
                sig_completed.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "exit_code": 0,
                    "node_name": "node-1",
                    "message": "",
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }

    recovered.evaluate_until_quiescent(20).await.unwrap();

    let marking = recovered.get_marking().await;
    marking.assert_token_count(&scenario.places["completed"], 3);
    marking.assert_empty(&scenario.places["running_jobs"]);
}

/// A cancel_request token exists at crash time but hasn't been evaluated.
/// After recovery, evaluating should fire the cancel transition.
#[tokio::test]
async fn test_crash_recovery_pending_cancel() {
    let scenario = TestScenario::nomad_batch();

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    test_ctx
        .service
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();
    test_ctx
        .service
        .register_effect_handler("scheduler_cancel", Arc::new(MockSchedulerCancel))
        .unwrap();

    // Submit all 3
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // Move all to running
    let sig_running = &scenario.places["sig_running"];
    for job_id in &[
        "nomad-batch-001-0",
        "nomad-batch-002-0",
        "nomad-batch-003-0",
    ] {
        test_ctx
            .service
            .create_token(
                sig_running.clone(),
                TokenColor::Data(serde_json::json!({
                    "scheduler_job_id": job_id,
                    "allocation_id": format!("alloc-{}", job_id),
                    "node_id": "node-id-1"
                })),
            )
            .await
            .unwrap();
    }
    test_ctx.service.evaluate_until_quiescent(20).await.unwrap();

    // Inject cancel_request for batch-002 but DO NOT evaluate
    test_ctx
        .service
        .create_token(
            scenario.places["cancel_request"].clone(),
            TokenColor::Data(serde_json::json!({
                "scheduler_job_id": "nomad-batch-002-0"
            })),
        )
        .await
        .unwrap();

    // Verify: cancel pending, all 3 running
    let marking = test_ctx.service.get_marking().await;
    marking.assert_token_count(&scenario.places["running_jobs"], 3);
    marking.assert_token_count(&scenario.places["cancel_request"], 1);

    // === Crash and recover ===
    let (recovered, _) = simulate_crash(&test_ctx, &scenario).await;
    recovered
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();
    recovered
        .register_effect_handler("scheduler_cancel", Arc::new(MockSchedulerCancel))
        .unwrap();

    // Verify cancel_request survived the crash
    let recovered_marking = recovered.get_marking().await;
    recovered_marking.assert_token_count(&scenario.places["running_jobs"], 3);
    recovered_marking.assert_token_count(&scenario.places["cancel_request"], 1);

    // Evaluate: cancel should fire
    let result = recovered.evaluate_until_quiescent(20).await.unwrap();
    assert!(
        result.steps_executed >= 1,
        "Cancel should fire after recovery"
    );

    let marking = recovered.get_marking().await;
    marking.assert_token_count(&scenario.places["cancelled"], 1);
    marking.assert_token_count(&scenario.places["running_jobs"], 2);
    marking.assert_empty(&scenario.places["cancel_request"]);

    // Verify the correct job was cancelled
    let cancelled_tokens = marking.tokens_at(&scenario.places["cancelled"]);
    match &cancelled_tokens[0].color {
        TokenColor::Data(data) => {
            assert_eq!(data["scheduler_job_id"], "nomad-batch-002-0");
            assert_eq!(data["cancelled"], true);
        }
        other => panic!("Expected Data token, got {:?}", other),
    }
}

/// After crash recovery, effects that completed before the crash should NOT
/// re-execute. The event log records token consumption, so the consumed tokens
/// no longer enable the transition.
#[tokio::test]
async fn test_crash_recovery_effects_not_reexecuted() {
    let scenario = TestScenario::nomad_batch();

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    test_ctx
        .service
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    // Submit all 3 jobs
    let result = test_ctx.service.evaluate_until_quiescent(20).await.unwrap();
    assert_eq!(result.steps_executed, 3, "All 3 jobs should be submitted");

    // Count events and EffectCompleted events
    let events_before = test_ctx.event_repo.recorded_events();
    let event_count_before = events_before.len();
    let effect_count = events_before
        .iter()
        .filter(|e| matches!(&e.event, DomainEvent::EffectCompleted { .. }))
        .count();
    assert_eq!(effect_count, 3, "Should have 3 EffectCompleted events");

    // === Crash and recover ===
    let (recovered, recovered_repo) = simulate_crash(&test_ctx, &scenario).await;
    recovered
        .register_effect_handler("scheduler_submit", Arc::new(MockSchedulerSubmit))
        .unwrap();

    // Evaluate on recovered service — should find no enabled transitions
    // because job_queue is empty (tokens consumed by original effects)
    let result = recovered.evaluate_until_quiescent(20).await.unwrap();
    assert_eq!(
        result.steps_executed, 0,
        "No transitions should fire — effects already consumed the tokens"
    );

    // Event count should not have increased
    assert_eq!(
        recovered_repo.recorded_events().len(),
        event_count_before,
        "No new events should be appended during recovery evaluation"
    );

    // Marking should match pre-crash state
    let marking = recovered.get_marking().await;
    marking.assert_empty(&scenario.places["job_queue"]);
    marking.assert_token_count(&scenario.places["submitted_jobs"], 3);
}

// =============================================================================
// Tier 8: Terminal Place Completion Detection
// =============================================================================

/// Token moves to terminal place → terminal_reached is Some with exit_code
#[tokio::test]
async fn test_terminal_scenario_completes_with_exit_code() {
    let scenario = TestScenario::with_terminal(Some(serde_json::json!(0)));

    let ctx: crate::fixtures::TestContext<
        MockEventRepository,
        MockTopologyRepository,
        MockStateProjection,
    > = crate::fixtures::TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    let result = ctx
        .service
        .evaluate_until_quiescent(100)
        .await
        .expect("evaluation should succeed");

    assert!(
        matches!(
            result.final_state,
            petri_application::EvaluateFinalState::Quiescent
        ),
        "Should reach quiescent state"
    );

    let terminal = result
        .terminal_reached
        .expect("terminal_reached should be Some when token is at terminal place");
    assert_eq!(
        terminal.place_id,
        scenario.places["Done"].to_string(),
        "Should report the Done place as terminal"
    );
    assert_eq!(
        terminal.exit_code,
        Some(serde_json::json!(0)),
        "Should extract exit_code from token data"
    );
}

/// Terminal place with Unit token → terminal_reached is Some, exit_code is None
#[tokio::test]
async fn test_terminal_scenario_completes_unit_token() {
    let scenario = TestScenario::with_terminal(None);

    let ctx: crate::fixtures::TestContext<
        MockEventRepository,
        MockTopologyRepository,
        MockStateProjection,
    > = crate::fixtures::TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    let result = ctx
        .service
        .evaluate_until_quiescent(100)
        .await
        .expect("evaluation should succeed");

    let terminal = result
        .terminal_reached
        .expect("terminal_reached should be Some");
    assert_eq!(terminal.place_id, scenario.places["Done"].to_string());
    // Unit token has no exit_code field
    assert_eq!(terminal.exit_code, None);
}

/// No initial tokens → quiescent immediately, terminal_reached is None
#[tokio::test]
async fn test_terminal_scenario_no_tokens() {
    let mut scenario = TestScenario::with_terminal(Some(serde_json::json!(0)));
    scenario.initial_tokens.clear();

    let ctx: crate::fixtures::TestContext<
        MockEventRepository,
        MockTopologyRepository,
        MockStateProjection,
    > = crate::fixtures::TestContext::builder()
        .with_scenario(scenario)
        .build()
        .await;

    let result = ctx
        .service
        .evaluate_until_quiescent(100)
        .await
        .expect("evaluation should succeed");

    assert!(
        result.terminal_reached.is_none(),
        "No tokens → terminal should not be reached"
    );
}

// =============================================================================
// Tier 9: Spawn initial token injection
// =============================================================================
//
// These tests verify that tokens injected via CreateNetRequest.initial_tokens
// (the same code path used by create_and_load after scenario loading) land in
// the correct place and enable transitions to fire.

/// Simulates the create_and_load flow: load a scenario with a bridge_in "inbox",
/// then inject an initial token into inbox (as create_and_load does for spawn).
/// The token should flow through the net normally.
#[tokio::test]
async fn test_spawn_initial_token_arrives_at_inbox() {
    use aithericon_sdk::prelude::*;

    // Build a minimal child net: inbox → pass_through → done
    let mut ctx = Context::new("spawn_child");
    let inbox = ctx.bridge_in::<DynamicToken>("inbox", "Inbox");
    let done = ctx.state::<DynamicToken>("done", "Done");

    ctx.transition("pass_through", "Pass Through")
        .auto_input("inp", &inbox)
        .auto_output("out", &done)
        .logic("#{ out: inp }");

    let scenario = TestScenario::from_sdk(ctx.build());

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    // Simulate what create_and_load does: inject initial_tokens AFTER scenario loading
    let inbox_place = &scenario.places["inbox"];
    test_ctx
        .service
        .create_token(
            inbox_place.clone(),
            TokenColor::Data(serde_json::json!({
                "job_id": "test-job-1",
                "spec": {"backend": "mock"}
            })),
        )
        .await
        .expect("initial token injection should succeed");

    // Evaluate — pass_through should fire
    test_ctx.service.evaluate_until_quiescent(10).await.unwrap();

    let marking = test_ctx.service.get_marking().await;
    marking.assert_empty(inbox_place);
    marking.assert_token_count(&scenario.places["done"], 1);

    // Verify token data survived
    let tokens = marking.tokens_at(&scenario.places["done"]);
    match &tokens[0].color {
        TokenColor::Data(data) => assert_eq!(data["job_id"], "test-job-1"),
        other => panic!("Expected Data token, got {:?}", other),
    }
}

/// Verify backward compatibility: a net with NO initial_tokens works normally.
#[tokio::test]
async fn test_spawn_child_without_initial_token() {
    use aithericon_sdk::prelude::*;

    let mut ctx = Context::new("spawn_child_empty");
    let inbox = ctx.bridge_in::<DynamicToken>("inbox", "Inbox");
    let _done = ctx.state::<DynamicToken>("done", "Done");

    ctx.transition("pass_through", "Pass Through")
        .auto_input("inp", &inbox)
        .auto_output("out", &_done)
        .logic("#{ out: inp }");

    let scenario = TestScenario::from_sdk(ctx.build());

    let test_ctx = TestContext::builder()
        .with_scenario(scenario.clone())
        .build()
        .await;

    // No initial tokens injected — evaluation should be a no-op
    test_ctx.service.evaluate_until_quiescent(10).await.unwrap();

    let marking = test_ctx.service.get_marking().await;
    marking.assert_empty(&scenario.places["inbox"]);
    marking.assert_empty(&scenario.places["done"]);
}

// =============================================================================
// Tier 9: Error Surfacing
// =============================================================================

/// A Rhai script error emits an ErrorOccurred event instead of failing silently.
///
/// Before this fix, script errors propagated as Err and killed the eval loop
/// with no event trace. Now the engine emits ErrorOccurred so errors are
/// visible via `aithericon errors` and in the event log.
#[tokio::test]
async fn test_script_error_emits_error_occurred_event() {
    use aithericon_sdk::{Context, UnitToken};

    let mut ctx = Context::new("broken_script");
    let a = ctx.state::<UnitToken>("a", "A");
    let b = ctx.state::<UnitToken>("b", "B");

    // This script references an undefined variable — will fail at runtime
    ctx.transition("broken", "Broken Transition")
        .auto_input("inp", &a)
        .auto_output("out", &b)
        .logic("#{ out: undefined_variable }");

    ctx.seed(&a, vec![UnitToken]);

    let scenario = TestScenario::from_sdk(ctx.build());
    let a_id = scenario.places["A"].clone();

    let test_ctx: TestContext<MockEventRepository, MockTopologyRepository, MockStateProjection> =
        TestContext::builder().with_scenario(scenario).build().await;

    // Should succeed (not Err) — a permanent failure stops the pass cleanly
    // with the failure recorded, instead of propagating an Err the driver
    // would log-and-retry forever.
    let result = test_ctx
        .service
        .evaluate_until_quiescent(10)
        .await
        .expect("evaluate should not return Err on script failures");

    assert!(
        result.failure_reached.is_some(),
        "a permanent script failure should set failure_reached"
    );

    // Token is CONSUMED (marking advances) so the broken transition is no
    // longer enabled — this is what breaks the infinite loop.
    let marking = test_ctx.service.get_marking().await;
    assert_eq!(
        marking.token_count(&a_id),
        0,
        "Token should be consumed after a permanent script error"
    );

    // An ErrorOccurred event is still emitted (human-readable visibility for
    // the event log and `aithericon errors`).
    let has_error_event = result.events.iter().any(|e| {
        matches!(
            &e.event,
            DomainEvent::ErrorOccurred { message } if message.contains("undefined_variable")
        )
    });
    assert!(
        has_error_event,
        "Expected an ErrorOccurred event mentioning the script error, got events: {:?}",
        result.events.iter().map(|e| &e.event).collect::<Vec<_>>()
    );
}

/// A script error does not prevent earlier successful transitions from persisting.
///
/// Scenario: A → T1 (succeeds) → B → T2 (broken) → C
/// T1 should fire and produce an event, then T2 fails and produces ErrorOccurred.
#[tokio::test]
async fn test_script_error_preserves_prior_transitions() {
    use aithericon_sdk::{Context, UnitToken};

    let mut ctx = Context::new("partial_progress");
    let a = ctx.state::<UnitToken>("a", "A");
    let b = ctx.state::<UnitToken>("b", "B");
    let c = ctx.state::<UnitToken>("c", "C");

    ctx.transition("good", "Good Transition")
        .auto_input("inp", &a)
        .auto_output("out", &b)
        .logic("#{ out: inp }");

    ctx.transition("broken", "Broken Transition")
        .auto_input("inp", &b)
        .auto_output("out", &c)
        .logic("#{ out: nonexistent }");

    ctx.seed(&a, vec![UnitToken]);

    let scenario = TestScenario::from_sdk(ctx.build());
    let b_id = scenario.places["B"].clone();

    let test_ctx: TestContext<MockEventRepository, MockTopologyRepository, MockStateProjection> =
        TestContext::builder().with_scenario(scenario).build().await;

    let result = test_ctx
        .service
        .evaluate_until_quiescent(10)
        .await
        .expect("evaluate should not return Err");

    // T1 should have fired (1 step); T2's permanent failure does not count.
    assert_eq!(
        result.steps_executed, 1,
        "Good transition should have fired"
    );

    // T1's success persists, but T2's permanent failure consumes B's token so
    // the broken transition cannot be re-selected (loop-breaking behavior).
    let marking = test_ctx.service.get_marking().await;
    assert_eq!(
        marking.token_count(&b_id),
        0,
        "B's token is consumed by the permanent failure of T2"
    );

    // Should have TransitionFired + ErrorOccurred
    let has_transition = result
        .events
        .iter()
        .any(|e| matches!(&e.event, DomainEvent::TransitionFired { .. }));
    let has_error = result
        .events
        .iter()
        .any(|e| matches!(&e.event, DomainEvent::ErrorOccurred { .. }));
    assert!(has_transition, "Should have a TransitionFired event");
    assert!(has_error, "Should have an ErrorOccurred event");
}
