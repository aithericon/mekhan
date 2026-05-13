//! Claim Pattern Component — Lightweight Resource Coordination
//!
//! This component replaces the old `ReservationPattern` with a claim-based model.
//! Instead of workers flowing physically through the net, the adapter manages resource
//! ownership and the workflow holds lightweight `ClaimHandle` references.
//!
//! # Architecture
//!
//! ```text
//!  [Job Queue] ──┐
//!                │   [claim_handles] (adapter injects ClaimHandle tokens)
//!                │        │
//!                ▼        ▼
//!            ┌───────────────────────────────────────────────┐
//!            │  (1. start)  job + claim_handle → processing  │
//!            │                       │                       │
//!            │        ┌──────────────┼──────────────┐        │
//!            │        │              │              │        │
//!            │        ▼              ▼              ▼        │
//!            │  [sig_completed] [sig_exec_error] [sig_cancelled]
//!            │        │          │       │          │        │
//!            │        ▼          ▼       ▼          ▼        │
//!            │  (2. complete) (3/4/5)  (6. cancel)           │
//!            │        │          │       │          │        │
//!            └────────│──────────│───────│──────────│────────┘
//!                     │          │       │          │
//!                     ▼          ▼       ▼          ▼
//!              [done] + [pending_releases]    [failed] + [pending_releases]
//!                                │
//!                  [sig_invalidation] ──► (7. handle_invalidation)
//!                                               │
//!                                               ▼
//!                                          [Job Queue]  (NO release)
//! ```
//!
//! # Key Difference from ReservationPattern
//!
//! - Workers stay in the adapter. Only `ClaimHandle` references flow through the net.
//! - Release is tracked via `pending_releases` — the auto-claim loop watches this place.
//! - Invalidation (resource died) re-queues the job WITHOUT releasing (claim already invalid).
//!
//! # Example
//!
//! ```ignore
//! use aithericon_sdk::prelude::*;
//!
//! #[token]
//! struct Job { id: String, data: String }
//!
//! fn definition(ctx: &mut Context) {
//!     let jobs = ctx.state::<Job>("jobs", "Job Queue");
//!
//!     let claim = ctx.use_component(
//!         ClaimPattern::new("gpu")
//!             .with_max_retries(2)
//!             .with_mock_adapters(),
//!         ClaimInput {
//!             job_queue_id: jobs.id().to_string(),
//!         },
//!     );
//!
//!     // Wire terminal outputs
//!     ctx.transition("archive", "Archive")
//!         .auto_input("result", &claim.done)
//!         .auto_output("archived", &done)
//!         .logic(r#"#{ archived: result }"#);
//! }
//! ```

use schemars::JsonSchema;
use serde::Serialize;

use crate::component::Component;
use crate::context::Context;
use crate::place::PlaceHandle;
use crate::scenario::AdapterLogic;

// ============================================================================
// Token Types
// ============================================================================

/// Adapter-injected reference to a claimed resource.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ClaimHandleToken {
    /// Unique handle ID (matches ClaimHandle.id in the adapter)
    pub handle_id: String,
    /// Resource ID in the adapter
    pub resource_id: String,
    /// Snapshot of resource data at claim time
    pub resource_data: serde_json::Value,
}

/// Job in-flight with a claimed resource.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ClaimProcessing {
    /// Correlation ID for matching signals
    pub correlation_id: String,
    /// Original job data
    pub job: serde_json::Value,
    /// Handle ID of the claimed resource
    pub claim_handle_id: String,
    /// Resource ID in the adapter
    pub resource_id: String,
    /// Current retry count
    pub retries: i64,
    /// Maximum retries allowed
    pub max_retries: i64,
}

/// Signal: work completed successfully.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct CompletedSignal {
    /// Correlation ID to match with processing job
    pub correlation_id: String,
    /// Result data
    pub data: serde_json::Value,
}

/// Signal: work execution failed.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ExecErrorSignal {
    /// Correlation ID to match with processing job
    pub correlation_id: String,
    /// Error message
    pub error: String,
    /// If true, don't retry — fail immediately
    pub fatal: bool,
}

/// Signal: external cancellation request.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct CancelledSignal {
    /// Correlation ID to match with processing job
    pub correlation_id: String,
}

/// Signal: resource invalidation (resource died while claimed).
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct InvalidationSignal {
    /// Handle ID of the invalidated claim
    pub handle_id: String,
    /// Resource ID that was invalidated
    pub resource_id: String,
    /// Reason for invalidation
    pub reason: String,
}

/// Successful job result.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ClaimJobResult {
    /// Original job ID
    pub job_id: String,
    /// Result data
    pub output: serde_json::Value,
    /// Resource ID that processed it
    pub resource_id: String,
    /// Handle ID (for release tracking)
    pub handle_id: String,
}

/// Failed job.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ClaimFailedJob {
    /// Original job ID
    pub job_id: String,
    /// Error reason
    pub error: String,
    /// Handle ID (for release tracking)
    pub handle_id: String,
}

/// Release tracking token — auto-claim loop watches this place.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ClaimRelease {
    /// Handle ID to release
    pub handle_id: String,
}

// ============================================================================
// Component Input/Output
// ============================================================================

/// Input for the ClaimPattern component.
pub struct ClaimInput {
    /// ID of the job queue place (triggers claim acquisition)
    pub job_queue_id: String,
}

/// Output places exposed by the ClaimPattern component.
pub struct ClaimOutput {
    // Internal states
    /// Claim handles injected by the adapter
    pub claim_handles: PlaceHandle<ClaimHandleToken>,
    /// Job being processed with claimed resource
    pub processing: PlaceHandle<ClaimProcessing>,

    // Signal places (for external injection)
    /// Signal: work completed
    pub sig_completed: PlaceHandle<CompletedSignal>,
    /// Signal: work failed
    pub sig_exec_error: PlaceHandle<ExecErrorSignal>,
    /// Signal: cancellation
    pub sig_cancelled: PlaceHandle<CancelledSignal>,
    /// Signal: resource invalidation
    pub sig_invalidation: PlaceHandle<InvalidationSignal>,

    // Terminal-like outputs (user wires these to their terminal places)
    /// Successful completions
    pub done: PlaceHandle<ClaimJobResult>,
    /// Failed jobs
    pub failed: PlaceHandle<ClaimFailedJob>,
    /// Pending releases (auto-claim loop watches this)
    pub pending_releases: PlaceHandle<ClaimRelease>,

    // External reference
    /// Reference back to job queue (for retries and invalidation re-queue)
    pub job_queue: PlaceHandle<crate::DynamicToken>,
}

// ============================================================================
// Component Implementation
// ============================================================================

/// Claim pattern component for lightweight resource coordination.
///
/// Creates the Petri net structure with:
/// - ClaimHandle references flowing through the net (not physical workers)
/// - Release tracked via `pending_releases` place
/// - Invalidation path for resource death (no release needed)
/// - Retry logic with exhaustion handling
pub struct ClaimPattern {
    pool_name: String,
    max_retries: i64,
    include_mock_adapters: bool,
}

impl ClaimPattern {
    /// Create a new claim pattern for the given resource pool.
    ///
    /// By default, NO adapters are included — call `.with_mock_adapters()`
    /// for testing/demo purposes.
    pub fn new(pool: impl Into<String>) -> Self {
        Self {
            pool_name: pool.into(),
            max_retries: 3,
            include_mock_adapters: false,
        }
    }

    /// Set the maximum retry count.
    pub fn with_max_retries(mut self, retries: i64) -> Self {
        self.max_retries = retries;
        self
    }

    /// Include mock adapters for testing/demo purposes.
    ///
    /// When enabled, the component adds a mock adapter on the `processing` place:
    /// - 70% → sig_completed (success)
    /// - 20% → sig_exec_error (transient)
    /// - 10% → sig_exec_error (fatal)
    ///
    /// No scheduler mock is needed — the engine's auto-claim loop handles granting.
    pub fn with_mock_adapters(mut self) -> Self {
        self.include_mock_adapters = true;
        self
    }
}

impl Component for ClaimPattern {
    type Input = ClaimInput;
    type Output = ClaimOutput;

    fn name(&self) -> String {
        format!("claim_{}", self.pool_name)
    }

    fn instantiate(self, ctx: &mut Context, input: Self::Input) -> Self::Output {
        let pool = &self.pool_name;
        let max_retries = self.max_retries;

        // Register token schemas
        ctx.register_schema::<ClaimHandleToken>();
        ctx.register_schema::<ClaimProcessing>();
        ctx.register_schema::<CompletedSignal>();
        ctx.register_schema::<ExecErrorSignal>();
        ctx.register_schema::<CancelledSignal>();
        ctx.register_schema::<InvalidationSignal>();
        ctx.register_schema::<ClaimJobResult>();
        ctx.register_schema::<ClaimFailedJob>();
        ctx.register_schema::<ClaimRelease>();

        // External place reference
        let job_queue: PlaceHandle<crate::DynamicToken> =
            PlaceHandle::external(input.job_queue_id.clone());

        // =====================================================================
        // Claim Handle Resource (shared state for adapter injection)
        // =====================================================================
        let claim_handles_resource = ctx
            .resource_def::<ClaimHandleToken>("claim_handles")
            .state("available", |s| s.signal())
            .build();
        let claim_handles = claim_handles_resource.state("available").clone();

        // =====================================================================
        // Internal State Places
        // =====================================================================
        let processing = ctx.state::<ClaimProcessing>("processing", "Processing");

        // =====================================================================
        // Signal Places
        // =====================================================================
        let sig_completed = ctx.signal::<CompletedSignal>("sig_completed", "Sig: Completed");
        let sig_exec_error = ctx.signal::<ExecErrorSignal>("sig_exec_error", "Sig: Exec Error");
        let sig_cancelled = ctx.signal::<CancelledSignal>("sig_cancelled", "Sig: Cancelled");
        let sig_invalidation =
            ctx.signal::<InvalidationSignal>("sig_invalidation", "Sig: Invalidation");

        // =====================================================================
        // Output Places
        // =====================================================================
        let done = ctx.state::<ClaimJobResult>("done", "Done");
        let failed = ctx.state::<ClaimFailedJob>("failed", "Failed");
        let pending_releases = ctx.state::<ClaimRelease>("pending_releases", "Pending Releases");

        // =====================================================================
        // Transition 1: Start — job + claim_handle → processing
        // =====================================================================
        ctx.transition("start", "1. Start")
            .auto_input("job", &job_queue)
            .auto_input("claim", &claim_handles)
            .auto_output("processing", &processing)
            .logic(format!(
                r#"#{{
                    processing: #{{
                        correlation_id: job.id,
                        job: job,
                        claim_handle_id: claim.handle_id,
                        resource_id: claim.resource_id,
                        retries: if job.retries != () {{ job.retries }} else {{ 0 }},
                        max_retries: if job.max_retries != () {{ job.max_retries }} else {{ {} }}
                    }}
                }}"#,
                max_retries
            ));

        // =====================================================================
        // Transition 2: Complete — processing + sig_completed → done + pending_releases
        // =====================================================================
        ctx.transition("complete", "2. Complete")
            .auto_input("proc", &processing)
            .auto_input("sig", &sig_completed)
            .auto_output("result", &done)
            .auto_output("release", &pending_releases)
            .guard("proc.correlation_id == sig.correlation_id")
            .logic(
                r#"#{
                result: #{
                    job_id: proc.correlation_id,
                    output: sig.data,
                    resource_id: proc.resource_id,
                    handle_id: proc.claim_handle_id
                },
                release: #{ handle_id: proc.claim_handle_id }
            }"#,
            );

        // =====================================================================
        // Transition 3: Error Retry — processing + sig_exec_error → job_queue + pending_releases
        //   guard: !fatal && retries < max_retries
        // =====================================================================
        ctx.transition("error_retry", "3. Error (Retry)")
            .auto_input("proc", &processing)
            .auto_input("sig", &sig_exec_error)
            .auto_output("job", &job_queue)
            .auto_output("release", &pending_releases)
            .guard("proc.correlation_id == sig.correlation_id && !sig.fatal && proc.retries < proc.max_retries")
            .logic(r#"#{
                job: #{
                    id: proc.job.id,
                    retries: proc.retries + 1,
                    max_retries: proc.max_retries
                },
                release: #{ handle_id: proc.claim_handle_id }
            }"#);

        // =====================================================================
        // Transition 4: Error Exhausted — processing + sig_exec_error → failed + pending_releases
        //   guard: !fatal && retries >= max_retries
        // =====================================================================
        ctx.transition("error_exhausted", "4. Error (Exhausted)")
            .auto_input("proc", &processing)
            .auto_input("sig", &sig_exec_error)
            .auto_output("fail", &failed)
            .auto_output("release", &pending_releases)
            .guard("proc.correlation_id == sig.correlation_id && !sig.fatal && proc.retries >= proc.max_retries")
            .logic(r#"#{
                fail: #{
                    job_id: proc.correlation_id,
                    error: "Execution retries exhausted: " + sig.error,
                    handle_id: proc.claim_handle_id
                },
                release: #{ handle_id: proc.claim_handle_id }
            }"#);

        // =====================================================================
        // Transition 5: Fatal Error — processing + sig_exec_error → failed + pending_releases
        //   guard: fatal
        // =====================================================================
        ctx.transition("fatal_error", "5. Fatal Error")
            .auto_input("proc", &processing)
            .auto_input("sig", &sig_exec_error)
            .auto_output("fail", &failed)
            .auto_output("release", &pending_releases)
            .guard("proc.correlation_id == sig.correlation_id && sig.fatal")
            .logic(
                r#"#{
                fail: #{
                    job_id: proc.correlation_id,
                    error: "Fatal: " + sig.error,
                    handle_id: proc.claim_handle_id
                },
                release: #{ handle_id: proc.claim_handle_id }
            }"#,
            );

        // =====================================================================
        // Transition 6: Cancel — processing + sig_cancelled → failed + pending_releases
        // =====================================================================
        ctx.transition("cancel", "6. Cancel")
            .auto_input("proc", &processing)
            .auto_input("sig", &sig_cancelled)
            .auto_output("fail", &failed)
            .auto_output("release", &pending_releases)
            .guard("proc.correlation_id == sig.correlation_id")
            .logic(
                r#"#{
                fail: #{
                    job_id: proc.correlation_id,
                    error: "Cancelled",
                    handle_id: proc.claim_handle_id
                },
                release: #{ handle_id: proc.claim_handle_id }
            }"#,
            );

        // =====================================================================
        // Transition 7: Handle Invalidation — processing + sig_invalidation → job_queue
        //   guard: signal.handle_id == processing.claim_handle_id
        //   NO release — claim already invalid
        // =====================================================================
        ctx.transition("handle_invalidation", "7. Handle Invalidation")
            .auto_input("proc", &processing)
            .auto_input("sig", &sig_invalidation)
            .auto_output("job", &job_queue)
            .guard("sig.handle_id == proc.claim_handle_id")
            .logic(
                r#"#{
                job: #{
                    id: proc.job.id,
                    retries: proc.retries,
                    max_retries: proc.max_retries
                }
            }"#,
            );

        // =====================================================================
        // Mock Adapters (optional — for testing/demo only)
        // =====================================================================
        if self.include_mock_adapters {
            // Worker Executor mock — responds to processing jobs
            // 70% success, 20% transient error, 10% fatal error
            ctx.mock_adapters.push(crate::scenario::MockAdapterConfig {
                name: format!("{} Executor (Mock)", pool),
                trigger_place_id: processing.id().to_string(),
                latency_ms: 2000,
                logic: AdapterLogic::rhai(format!(
                    r#"
                    let r = random();
                    if r < 0.7 {{
                        #{{ target_place: "{}", data: #{{ correlation_id: token.correlation_id, data: #{{ result: "processed_" + timestamp() }} }} }}
                    }} else if r < 0.9 {{
                        #{{ target_place: "{}", data: #{{ correlation_id: token.correlation_id, error: "Transient failure", fatal: false }} }}
                    }} else {{
                        #{{ target_place: "{}", data: #{{ correlation_id: token.correlation_id, error: "Hardware failure", fatal: true }} }}
                    }}
                    "#,
                    sig_completed.id(),
                    sig_exec_error.id(),
                    sig_exec_error.id()
                )),
                check_token_exists: false,
            });
        }

        ClaimOutput {
            claim_handles,
            processing,
            sig_completed,
            sig_exec_error,
            sig_cancelled,
            sig_invalidation,
            done,
            failed,
            pending_releases,
            job_queue,
        }
    }
}

// PlaceHandle::external is already defined in reservation.rs (which we keep for the impl)
// We need to ensure the external constructor is available here too.
// It's defined in reservation.rs with pub(crate), so it's accessible.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claim_pattern_new() {
        let pattern = ClaimPattern::new("workers");
        assert_eq!(pattern.pool_name, "workers");
        assert_eq!(pattern.max_retries, 3);
        assert!(!pattern.include_mock_adapters);
    }

    #[test]
    fn test_claim_pattern_with_max_retries() {
        let pattern = ClaimPattern::new("workers").with_max_retries(5);
        assert_eq!(pattern.max_retries, 5);
    }

    #[test]
    fn test_claim_pattern_with_mock_adapters() {
        let pattern = ClaimPattern::new("gpu").with_mock_adapters();
        assert!(pattern.include_mock_adapters);
    }

    #[test]
    fn test_component_name() {
        let pattern = ClaimPattern::new("gpu-instances");
        assert_eq!(pattern.name(), "claim_gpu-instances");
    }
}
