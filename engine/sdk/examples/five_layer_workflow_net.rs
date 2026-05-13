//! Layer 1: Workflow Net — Parameterized ML pipeline with bridge-based input/output.
//!
//! Part of the five-layer bridged net composition (campaign → workflow → job → scheduler → executor).
//! Based on the four-layer `four_layer_workflow_net.rs` but modified for bridge composition:
//!
//! - `workflow_start` is now `bridge_in` (receives from campaign-net) instead of seeded
//! - `Workflow` token carries a `config: serde_json::Value` for hyperparameters
//! - Step specs are parameterized with `wf.config.lr` / `wf.config.batch_size`
//! - Results flow back to campaign-net via `bridge_out` (result_outbox)
//! - Failures flow back to campaign-net via `bridge_out` (failure_outbox)
//! - No seed data (campaign-net provides workflow definitions)
//!
//! ## ML Pipeline Demo (parameterized by config)
//!
//! ```text
//!                     ┌── preprocess-A ──┐
//! [workflow_start] ──►│                  │──► [train] ──► [evaluate] ──► [workflow_completed]
//!   (bridge_in)       └── preprocess-B ──┘                               │
//!                          (parallel)       (fan-in)    (sequential)      ▼
//!                                                                 [result_outbox → campaign-net]
//! ```
//!
//! ## Data flow
//!
//! ```text
//! [workflow_start: bridge_in] → (init_workflow) → [step_A_ready] + [step_B_ready]   ← fan-out
//!
//! [step_A_ready] → (dispatch_A) → [to_jobs: bridge_out] + [A_pending]
//! [step_B_ready] → (dispatch_B) → [to_jobs: bridge_out] + [B_pending]
//!
//! [result_inbox: bridge_in] + [A_pending] → (join_A) → [A_done]
//! [result_inbox: bridge_in] + [B_pending] → (join_B) → [B_done]
//!   guard: result.job_id == pending.job_id
//!
//! [A_done] + [B_done] → (gate_train) → [train_ready]                      ← fan-in!
//!   guard: a.workflow_id == b.workflow_id
//!
//! [train_ready] → (dispatch_train) → [to_jobs] + [train_pending]
//! [result_inbox] + [train_pending] → (join_train) → [train_done]
//!
//! [train_done] → (dispatch_eval) → [to_jobs] + [eval_pending]
//! [result_inbox] + [eval_pending] → (join_eval) → [workflow_completed]
//!
//! [workflow_completed] → (forward_result) → [result_outbox → campaign-net/result_inbox]
//! [workflow_failed] → (forward_failure) → [failure_outbox → campaign-net/failure_inbox]
//! ```
//!
//! ## Deploy
//!
//! ```bash
//! # As part of the five-layer campaign demo:
//! just campaign-demo
//!
//! # Or manually (deploy executor-net, scheduler-net, and job-net first):
//! cargo run -p aithericon-sdk --example five_layer_workflow_net -- --deploy --net-id workflow-net
//! ```
//!
//! ## Net ID: `workflow-net`

use aithericon_sdk::prelude::*;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// Pipeline definition — received from campaign-net via bridge.
/// Carries hyperparameter config for parameterized execution.
#[token]
struct Workflow {
    workflow_id: String,
    pipeline_name: String,
    config: serde_json::Value,
}

/// A step ready to be dispatched to the job layer.
#[token]
struct StepReady {
    workflow_id: String,
    job_id: String,
    model_name: String,
    run: i64,
    retries: i64,
    max_retries: i64,
    spec: serde_json::Value,
}

/// Pending step — held while waiting for result from job-net.
#[token]
struct StepPending {
    workflow_id: String,
    job_id: String,
    step_name: String,
}

/// Job dispatched to job-net via bridge (matches job-net's expected Job shape).
#[token]
struct JobRequest {
    job_id: String,
    model_name: String,
    run: i64,
    retries: i64,
    max_retries: i64,
    spec: serde_json::Value,
}

/// Result received from job-net via bridge.
#[token]
struct StepResult {
    job_id: String,
    model_name: String,
    detail: serde_json::Value,
}

/// Failure received from job-net via bridge.
#[token]
struct StepFailure {
    job_id: String,
    model_name: String,
    reason: String,
    retries_exhausted: i64,
}

/// Completed step — intermediate done state for dependency tracking.
#[token]
struct StepDone {
    workflow_id: String,
    job_id: String,
    step_name: String,
    detail: serde_json::Value,
}

/// Training step ready — produced by the fan-in gate.
#[token]
struct TrainReady {
    workflow_id: String,
    job_id: String,
    model_name: String,
    run: i64,
    retries: i64,
    max_retries: i64,
    spec: serde_json::Value,
    preprocess_a_detail: serde_json::Value,
    preprocess_b_detail: serde_json::Value,
}

/// Terminal workflow completion.
#[token]
struct WorkflowCompleted {
    workflow_id: String,
    pipeline_name: String,
    final_detail: serde_json::Value,
}

/// Terminal workflow failure.
#[token]
struct WorkflowFailed {
    workflow_id: String,
    failed_step: String,
    reason: String,
}

/// Result forwarded back to campaign-net.
#[token]
struct CampaignResult {
    workflow_id: String,
    pipeline_name: String,
    final_detail: serde_json::Value,
}

/// Failure forwarded back to campaign-net.
#[token]
struct CampaignFailure {
    workflow_id: String,
    failed_step: String,
    reason: String,
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // -- Places ---------------------------------------------------------------

    // Bridge in — receive workflow definitions from campaign-net (replaces seeded workflow_start)
    let workflow_start = ctx.bridge_in_from::<Workflow>("workflow_start", "Workflow Start", "campaign-net", "to_workflows");

    // Parallel step readiness (produced by init_workflow)
    let step_a_ready = ctx.state::<StepReady>("step_A_ready", "Step A Ready");
    let step_b_ready = ctx.state::<StepReady>("step_B_ready", "Step B Ready");

    // Bridge out — single place for ALL step dispatches to job-net
    let to_jobs = ctx.bridge_out::<JobRequest>("to_jobs", "To Jobs", "job-net", "job_queue");

    // Pending places — hold metadata while waiting for step results
    let a_pending = ctx.state::<StepPending>("A_pending", "A Pending");
    let b_pending = ctx.state::<StepPending>("B_pending", "B Pending");
    let train_pending = ctx.state::<StepPending>("train_pending", "Train Pending");
    let eval_pending = ctx.state::<StepPending>("eval_pending", "Eval Pending");

    // Bridge in — receive results and failures from job-net
    let result_inbox = ctx.bridge_in_from::<StepResult>("result_inbox", "Result Inbox", "job-net", "result_outbox");
    let failure_inbox = ctx.bridge_in_from::<StepFailure>("failure_inbox", "Failure Inbox", "job-net", "failure_outbox");

    // Done places — track completed steps for dependency gating
    let a_done = ctx.state::<StepDone>("A_done", "A Done");
    let b_done = ctx.state::<StepDone>("B_done", "B Done");

    // Train step readiness (produced by fan-in gate)
    let train_ready = ctx.state::<TrainReady>("train_ready", "Train Ready");

    // Train done — triggers evaluation dispatch
    let train_done = ctx.state::<StepDone>("train_done", "Train Done");

    // Terminal places
    let workflow_completed =
        ctx.state::<WorkflowCompleted>("workflow_completed", "Workflow Completed");
    let workflow_failed = ctx.state::<WorkflowFailed>("workflow_failed", "Workflow Failed");

    // Bridge out — forward results/failures back to campaign-net
    let result_outbox = ctx.bridge_out::<CampaignResult>(
        "result_outbox",
        "Result Outbox",
        "campaign-net",
        "result_inbox",
    );
    let failure_outbox = ctx.bridge_out::<CampaignFailure>(
        "failure_outbox",
        "Failure Outbox",
        "campaign-net",
        "failure_inbox",
    );

    // -- No seed data (campaign-net provides workflow definitions via bridge) --

    // -- Transitions ----------------------------------------------------------

    // 1. init_workflow — fan-out: split pipeline into parallel preprocessing steps.
    //    Uses wf.config.lr and wf.config.batch_size from campaign hyperparams.
    ctx.transition("init_workflow", "Initialize Workflow")
        .auto_input("wf", &workflow_start)
        .auto_output("step_a", &step_a_ready)
        .auto_output("step_b", &step_b_ready)
        .logic(
            r#"#{
                step_a: #{
                    workflow_id: wf.workflow_id,
                    job_id: wf.workflow_id + ":preprocess-A",
                    model_name: "preprocess-A",
                    run: 0,
                    retries: 0,
                    max_retries: 2,
                    spec: #{
                        type: "process",
                        config: #{
                            command: "echo",
                            args: ["preprocess-A complete (lr=" + wf.config.lr + " batch=" + wf.config.batch_size + ")"]
                        },
                        inputs: [],
                        outputs: []
                    }
                },
                step_b: #{
                    workflow_id: wf.workflow_id,
                    job_id: wf.workflow_id + ":preprocess-B",
                    model_name: "preprocess-B",
                    run: 0,
                    retries: 0,
                    max_retries: 2,
                    spec: #{
                        type: "process",
                        config: #{
                            command: "echo",
                            args: ["preprocess-B complete (lr=" + wf.config.lr + " batch=" + wf.config.batch_size + ")"]
                        },
                        inputs: [],
                        outputs: []
                    }
                }
            }"#,
        );

    // 2. dispatch_A — bridge preprocess-A to job-net, hold pending.
    ctx.transition("dispatch_A", "Dispatch Preprocess-A")
        .auto_input("step", &step_a_ready)
        .auto_output("req", &to_jobs)
        .auto_output("pending", &a_pending)
        .logic(
            r#"#{
                req: #{
                    job_id: step.job_id,
                    model_name: step.model_name,
                    run: step.run,
                    retries: step.retries,
                    max_retries: step.max_retries,
                    spec: step.spec
                },
                pending: #{
                    workflow_id: step.workflow_id,
                    job_id: step.job_id,
                    step_name: "preprocess-A"
                }
            }"#,
        );

    // 3. dispatch_B — bridge preprocess-B to job-net, hold pending.
    ctx.transition("dispatch_B", "Dispatch Preprocess-B")
        .auto_input("step", &step_b_ready)
        .auto_output("req", &to_jobs)
        .auto_output("pending", &b_pending)
        .logic(
            r#"#{
                req: #{
                    job_id: step.job_id,
                    model_name: step.model_name,
                    run: step.run,
                    retries: step.retries,
                    max_retries: step.max_retries,
                    spec: step.spec
                },
                pending: #{
                    workflow_id: step.workflow_id,
                    job_id: step.job_id,
                    step_name: "preprocess-B"
                }
            }"#,
        );

    // 4+5. join/fail preprocess-A
    ctx.join_pair(
        "A", "Preprocess-A",
        &a_pending,
        &result_inbox, &a_done,
        r#"#{
                out: #{
                    workflow_id: pending.workflow_id,
                    job_id: pending.job_id,
                    step_name: pending.step_name,
                    detail: result.detail
                }
            }"#,
        &failure_inbox, &workflow_failed,
        r#"#{
                out: #{
                    workflow_id: pending.workflow_id,
                    failed_step: pending.step_name,
                    reason: fail.reason
                }
            }"#,
        &["job_id"],
    );

    // 6+7. join/fail preprocess-B
    ctx.join_pair(
        "B", "Preprocess-B",
        &b_pending,
        &result_inbox, &b_done,
        r#"#{
                out: #{
                    workflow_id: pending.workflow_id,
                    job_id: pending.job_id,
                    step_name: pending.step_name,
                    detail: result.detail
                }
            }"#,
        &failure_inbox, &workflow_failed,
        r#"#{
                out: #{
                    workflow_id: pending.workflow_id,
                    failed_step: pending.step_name,
                    reason: fail.reason
                }
            }"#,
        &["job_id"],
    );

    // 6. gate_train — FAN-IN: fires ONLY when both A_done AND B_done tokens exist.
    //    This is the key Petri net synchronization primitive — multi-input transition
    //    that naturally waits for all prerequisites without custom coordination code.
    ctx.transition("gate_train", "Gate: Train (fan-in)")
        .auto_input("a", &a_done)
        .auto_input("b", &b_done)
        .correlate("a", "b", "workflow_id")
        .auto_output("ready", &train_ready)
        .logic(
            r#"#{
                ready: #{
                    workflow_id: a.workflow_id,
                    job_id: a.workflow_id + ":train",
                    model_name: "train",
                    run: 0,
                    retries: 0,
                    max_retries: 2,
                    spec: #{
                        type: "process",
                        config: #{
                            command: "echo",
                            args: ["training complete"]
                        },
                        inputs: [],
                        outputs: []
                    },
                    preprocess_a_detail: a.detail,
                    preprocess_b_detail: b.detail
                }
            }"#,
        );

    // 7. dispatch_train — bridge training step to job-net, hold pending.
    ctx.transition("dispatch_train", "Dispatch Train")
        .auto_input("step", &train_ready)
        .auto_output("req", &to_jobs)
        .auto_output("pending", &train_pending)
        .logic(
            r#"#{
                req: #{
                    job_id: step.job_id,
                    model_name: step.model_name,
                    run: step.run,
                    retries: step.retries,
                    max_retries: step.max_retries,
                    spec: step.spec
                },
                pending: #{
                    workflow_id: step.workflow_id,
                    job_id: step.job_id,
                    step_name: "train"
                }
            }"#,
        );

    // 8+9. join/fail train
    ctx.join_pair(
        "train", "Train",
        &train_pending,
        &result_inbox, &train_done,
        r#"#{
                out: #{
                    workflow_id: pending.workflow_id,
                    job_id: pending.job_id,
                    step_name: pending.step_name,
                    detail: result.detail
                }
            }"#,
        &failure_inbox, &workflow_failed,
        r#"#{
                out: #{
                    workflow_id: pending.workflow_id,
                    failed_step: pending.step_name,
                    reason: fail.reason
                }
            }"#,
        &["job_id"],
    );

    // 9. dispatch_eval — bridge evaluation step to job-net, hold pending.
    ctx.transition("dispatch_eval", "Dispatch Evaluate")
        .auto_input("step", &train_done)
        .auto_output("req", &to_jobs)
        .auto_output("pending", &eval_pending)
        .logic(
            r#"#{
                req: #{
                    job_id: step.workflow_id + ":evaluate",
                    model_name: "evaluate",
                    run: 0,
                    retries: 0,
                    max_retries: 2,
                    spec: #{
                        type: "process",
                        config: #{
                            command: "echo",
                            args: ["evaluation complete"]
                        },
                        inputs: [],
                        outputs: []
                    }
                },
                pending: #{
                    workflow_id: step.workflow_id,
                    job_id: step.workflow_id + ":evaluate",
                    step_name: "evaluate"
                }
            }"#,
        );

    // 10+11. join/fail evaluate
    ctx.join_pair(
        "eval", "Evaluate",
        &eval_pending,
        &result_inbox, &workflow_completed,
        r#"#{
                out: #{
                    workflow_id: pending.workflow_id,
                    pipeline_name: "ResNet ML Pipeline",
                    final_detail: result.detail
                }
            }"#,
        &failure_inbox, &workflow_failed,
        r#"#{
                out: #{
                    workflow_id: pending.workflow_id,
                    failed_step: pending.step_name,
                    reason: fail.reason
                }
            }"#,
        &["job_id"],
    );

    // -- Bridge-out transitions (results/failures back to campaign-net) -------

    // 11. forward_result — bridge completed workflow result to campaign-net.
    ctx.transition("forward_result", "Forward Result to Campaign")
        .auto_input("done", &workflow_completed)
        .auto_output("out", &result_outbox)
        .logic(
            r#"#{
                out: #{
                    workflow_id: done.workflow_id,
                    pipeline_name: done.pipeline_name,
                    final_detail: done.final_detail
                }
            }"#,
        );

    // 12. forward_failure — bridge workflow failure to campaign-net.
    ctx.transition("forward_failure", "Forward Failure to Campaign")
        .auto_input("fail", &workflow_failed)
        .auto_output("out", &failure_outbox)
        .logic(
            r#"#{
                out: #{
                    workflow_id: fail.workflow_id,
                    failed_step: fail.failed_step,
                    reason: fail.reason
                }
            }"#,
        );

    // (Failure paths are now included in join_pair calls above.)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    aithericon_sdk::run(
        "five-layer-workflow",
        "Layer 1: Parameterized workflow net — receives config from campaign-net, dispatches ML pipeline, relays results back",
        definition,
    );
}
