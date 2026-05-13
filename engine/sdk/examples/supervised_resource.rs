//! Pattern 3: Supervised Resource — Health Checks & Drift Detection
//!
//! Continuous resource monitoring via timeout adapter recycling. When a resource
//! is in a "leased" state, a timeout adapter periodically injects health signals.
//! Guarded transitions evaluate the signal and either **recycle** the token
//! (healthy → re-triggers next check), **retire** it (max checks reached), or
//! route to **stale/drift** failure places that feed compensation transitions.
//!
//! ```text
//! [leased] → timeout_adapter(2s) → [sig_health]
//!     ▲                                  │
//!     │    (healthy) ◄── guard: ok ──────┤
//!     │        │                         ├── (stale) → [stale] → (compensate) → [requeued]
//!     └────────┘ recycle                 ├── (drift) → [drifted] → (compensate) → [requeued]
//!                                        └── (retire) → [retired]
//! ```
//!
//! Termination guarantees:
//! 1. **max_checks** — when `check_count >= max_checks`, `t_retire` fires
//! 2. **External consumption** — if another transition consumes the leased token
//!    before the adapter fires, `check_token_exists: true` skips silently
//! 3. **Failure routing** — stale/drift routes break the loop
//!
//! Run with: `cargo run --example supervised_resource`
//! Deploy to engine: `cargo run --example supervised_resource -- --deploy`

use aithericon_sdk::prelude::*;

// ============================================================================
// Token Types
// ============================================================================

/// Resource in leased state with supervision metadata.
#[token]
struct SupervisedWorker {
    resource_id: String,
    expected_state: String,
    check_count: i64,
    max_checks: i64,
    job_data: String,
}

/// Adapter-injected health check result.
#[token]
struct HealthSignal {
    resource_id: String,
    status: String,
    observed_state: String,
    age_ms: i64,
}

/// Resource that missed heartbeat.
#[token]
struct StaleResource {
    resource_id: String,
    job_data: String,
    last_check: i64,
}

/// Resource whose external state diverged.
#[token]
struct DriftedResource {
    resource_id: String,
    expected_state: String,
    observed_state: String,
    job_data: String,
}

/// Resource that completed its supervision lifetime.
#[token]
struct RetiredResource {
    resource_id: String,
    total_checks: i64,
    job_data: String,
}

/// Compensation output (job data + failure reason).
#[token]
struct RequeuedJob {
    resource_id: String,
    job_data: String,
    reason: String,
}

// ============================================================================
// Workflow Definition
// ============================================================================

fn definition(ctx: &mut Context) {
    // ========================================================================
    // Places
    // ========================================================================

    let leased = ctx.state::<SupervisedWorker>("leased", "Leased (Supervised)");
    let sig_health = ctx.signal::<HealthSignal>("sig_health", "Health Signals");
    let stale = ctx.state::<StaleResource>("stale", "Stale Resources");
    let drifted = ctx.state::<DriftedResource>("drifted", "Drifted Resources");
    let retired = ctx.state::<RetiredResource>("retired", "Retired Resources");
    let requeued = ctx.state::<RequeuedJob>("requeued", "Requeued Jobs");

    // ========================================================================
    // Seed Data — 3 GPU workers with max_checks: 5
    // ========================================================================

    ctx.seed(
        &leased,
        vec![
            SupervisedWorker {
                resource_id: "gpu-0".into(),
                expected_state: "running".into(),
                check_count: 0,
                max_checks: 5,
                job_data: "train-model-alpha".into(),
            },
            SupervisedWorker {
                resource_id: "gpu-1".into(),
                expected_state: "running".into(),
                check_count: 0,
                max_checks: 5,
                job_data: "train-model-beta".into(),
            },
            SupervisedWorker {
                resource_id: "gpu-2".into(),
                expected_state: "running".into(),
                check_count: 0,
                max_checks: 5,
                job_data: "train-model-gamma".into(),
            },
        ],
    );

    // ========================================================================
    // Timeout Adapter — periodic health checks on leased resources
    // ========================================================================
    // Fires every 2s while a token remains in [leased].
    // Rhai logic: 70% ok, 15% stale, 15% drift.

    ctx.timeout_adapter(
        &leased,
        "Health Check Probe",
        2000,
        format!(
            r#"
            let r = random();
            let age = timestamp() - token_created_at;
            if r < 0.70 {{
                #{{ target_place: "{sig}", data: #{{
                    resource_id: token.resource_id,
                    status: "ok",
                    observed_state: token.expected_state,
                    age_ms: age
                }} }}
            }} else if r < 0.85 {{
                #{{ target_place: "{sig}", data: #{{
                    resource_id: token.resource_id,
                    status: "stale",
                    observed_state: "unreachable",
                    age_ms: age
                }} }}
            }} else {{
                #{{ target_place: "{sig}", data: #{{
                    resource_id: token.resource_id,
                    status: "drift",
                    observed_state: "stopped",
                    age_ms: age
                }} }}
            }}
            "#,
            sig = sig_health.id()
        ),
    );

    // ========================================================================
    // Transitions
    // ========================================================================

    // t_healthy — recycle loop: healthy resource gets check_count incremented
    ctx.transition("t_healthy", "Healthy (Recycle)")
        .auto_input("res", &leased)
        .auto_input("sig", &sig_health)
        .guard(
            r#"sig.status == "ok"
            && res.resource_id == sig.resource_id
            && (res.max_checks == 0 || res.check_count < res.max_checks)"#,
        )
        .auto_output("recycled", &leased)
        .logic(
            r#"#{
                recycled: #{
                    resource_id: res.resource_id,
                    expected_state: res.expected_state,
                    check_count: res.check_count + 1,
                    max_checks: res.max_checks,
                    job_data: res.job_data
                }
            }"#,
        );

    // t_retire — max checks reached, supervision complete
    ctx.transition("t_retire", "Retire (Max Checks)")
        .auto_input("res", &leased)
        .auto_input("sig", &sig_health)
        .guard(
            r#"sig.status == "ok"
            && res.resource_id == sig.resource_id
            && res.max_checks > 0
            && res.check_count >= res.max_checks"#,
        )
        .auto_output("done", &retired)
        .logic(
            r#"#{
                done: #{
                    resource_id: res.resource_id,
                    total_checks: res.check_count,
                    job_data: res.job_data
                }
            }"#,
        );

    // t_stale — no heartbeat detected
    ctx.transition("t_stale", "Stale Detected")
        .auto_input("res", &leased)
        .auto_input("sig", &sig_health)
        .guard(r#"sig.status == "stale" && res.resource_id == sig.resource_id"#)
        .auto_output("failed", &stale)
        .logic(
            r#"#{
                failed: #{
                    resource_id: res.resource_id,
                    job_data: res.job_data,
                    last_check: res.check_count
                }
            }"#,
        );

    // t_drift — state divergence detected
    ctx.transition("t_drift", "Drift Detected")
        .auto_input("res", &leased)
        .auto_input("sig", &sig_health)
        .guard(r#"sig.status == "drift" && res.resource_id == sig.resource_id"#)
        .auto_output("failed", &drifted)
        .logic(
            r#"#{
                failed: #{
                    resource_id: res.resource_id,
                    expected_state: res.expected_state,
                    observed_state: sig.observed_state,
                    job_data: res.job_data
                }
            }"#,
        );

    // t_compensate_stale — re-queue stale job
    ctx.transition("t_compensate_stale", "Compensate Stale")
        .auto_input("res", &stale)
        .auto_output("job", &requeued)
        .logic(
            r#"#{
                job: #{
                    resource_id: res.resource_id,
                    job_data: res.job_data,
                    reason: "stale: missed heartbeat at check " + res.last_check
                }
            }"#,
        );

    // t_compensate_drift — re-queue drifted job
    ctx.transition("t_compensate_drift", "Compensate Drift")
        .auto_input("res", &drifted)
        .auto_output("job", &requeued)
        .logic(
            r#"#{
                job: #{
                    resource_id: res.resource_id,
                    job_data: res.job_data,
                    reason: "drift: expected " + res.expected_state + " but observed " + res.observed_state
                }
            }"#,
        );
}

fn main() {
    aithericon_sdk::run(
        "supervised-resource",
        "Supervised resource pattern: continuous health checks via timeout adapter recycling \
         with guard-based routing to stale/drift compensation and retirement on max checks.",
        definition,
    );
}
