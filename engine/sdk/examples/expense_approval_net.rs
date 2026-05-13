//! Expense Approval Workflow — combining Python, Human-in-the-loop, and Timer.
//!
//! This example demonstrates a realistic approval workflow:
//!
//! 1. **Python executor** — Analyzes expense (categorization, policy check, risk score)
//! 2. **Human-in-the-loop** — Manager reviews flagged expenses
//! 3. **Timer** — Escalation if no response within SLA
//!
//! ## Flow
//!
//! ```text
//! [expense_inbox] → (analyze: effect "executor_submit")
//!                      ↓
//!                 [analyzed]
//!                      ↓
//!        ┌─────────────┴─────────────┐
//!        │ risk_level == "low"       │ risk_level != "low"
//!        ↓                           ↓
//!   [auto_approved]            [pending_review]
//!                                    │
//!                     ┌──────────────┼──────────────┐
//!                     │              │              │
//!                     ↓              ↓              ↓
//!              (start_timer)   (request_approval)
//!                     ↓              │
//!              [timer_data]          ↓
//!                     ↓         [awaiting_human]
//!              (schedule_timer)      │
//!                     ↓              │
//!              [sla_tracking] ←──────┤
//!                     │              │
//!           ┌────────┴────────┐     │
//!           │                 │     │
//!           ↓                 ↓     ↓
//!    [sig_sla_timeout]   [sig_approval_response]
//!           │                 │
//!           ↓                 ↓
//!      (escalate)        (finalize_approval)
//!           ↓                 ↓
//!      [escalated]    ┌──────┴──────┐
//!                     │             │
//!                     ↓             ↓
//!               [approved]    [rejected]
//! ```
//!
//! ## Environment
//!
//! ```bash
//! # For Python executor
//! EXECUTOR_ENABLED=true
//! EXECUTOR_SIGNAL_ROUTES=completed:sig_analysis_complete,failed:sig_analysis_failed
//!
//! # For Clockmaster (timer)
//! CLOCKMASTER_ENABLED=true
//! ```

use aithericon_sdk::prelude::*;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// Incoming expense request.
#[token]
struct ExpenseRequest {
    expense_id: String,
    submitter: String,
    amount: f64,
    currency: String,
    category: String,
    description: String,
    receipt_url: Option<String>,
}

/// Job wrapper for the Python analysis.
#[token]
struct AnalysisJob {
    job_id: String,
    run: i64,
    retries: i64,
    max_retries: i64,
    spec: serde_json::Value,
    // Carry along original request for later
    original_request: serde_json::Value,
}

/// Submitted analysis job (awaiting executor signals).
#[token]
struct SubmittedAnalysis {
    job_id: String,
    run: i64,
    execution_id: String,
    original_request: serde_json::Value,
}

/// Result of Python analysis.
#[token]
struct AnalyzedExpense {
    expense_id: String,
    submitter: String,
    amount: f64,
    currency: String,
    category: String,
    description: String,
    // Analysis results
    risk_level: String, // "low", "medium", "high"
    policy_violations: Vec<String>,
    suggested_category: String,
    auto_approve: bool,
}

/// Expense pending human review.
/// Includes form schema for the human task handler.
#[token]
struct PendingReview {
    // Form schema (required by human_task handler)
    title: String,
    instructions: String,
    fields: serde_json::Value,

    // Business data
    expense_id: String,
    submitter: String,
    amount: f64,
    currency: String,
    category: String,
    description: String,
    risk_level: String,
    policy_violations: Vec<String>,
    suggested_category: String,
    reviewer: String,
}

/// Human task for manager approval.
#[token]
struct ApprovalTask {
    expense_id: String,
    task_id: String,
    reviewer: String,
}

/// SLA tracking token (paired with timer).
#[token]
struct SlaTracking {
    expense_id: String,
    task_id: String,
    reviewer: String,
    timeout_minutes: i64,
}

/// Final approved expense.
#[token]
struct ApprovedExpense {
    expense_id: String,
    approved_by: String,
    approval_type: String, // "auto", "manual"
    comments: Option<String>,
}

/// Final rejected expense.
#[token]
struct RejectedExpense {
    expense_id: String,
    rejected_by: String,
    reason: String,
}

/// Escalated expense (SLA breach).
#[token]
struct EscalatedExpense {
    expense_id: String,
    original_reviewer: String,
    escalation_reason: String,
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // -------------------------------------------------------------------------
    // Places
    // -------------------------------------------------------------------------

    // Intake
    let expense_inbox = ctx.state::<ExpenseRequest>("expense_inbox", "Incoming Expenses");

    // Analysis phase
    let analysis_queue = ctx.state::<AnalysisJob>("analysis_queue", "Analysis Queue");
    let analysis_submitted =
        ctx.state::<SubmittedAnalysis>("analysis_submitted", "Submitted for Analysis");
    let sig_analysis_complete =
        ctx.signal::<ExecutorStatusSignal>("sig_analysis_complete", "Analysis Complete Signal");
    let sig_analysis_failed =
        ctx.signal::<ExecutorStatusSignal>("sig_analysis_failed", "Analysis Failed Signal");
    let analyzed = ctx.state::<AnalyzedExpense>("analyzed", "Analyzed Expenses");

    // Routing
    let auto_approved = ctx.state::<ApprovedExpense>("auto_approved", "Auto-Approved");
    let pending_review = ctx.state::<HumanTaskRequest>("pending_review", "Pending Review");

    // Human approval phase
    let approval_task = ctx.state::<HumanTaskAssigned>("approval_task", "Active Approval Tasks");
    let sla_tracking = ctx.state::<SlaTracking>("sla_tracking", "SLA Tracking");
    let timer_data = ctx.state::<TimerInput>("timer_data", "Timer Data");
    let timer_scheduled = ctx.state::<TimerScheduled>("timer_scheduled", "Timer Scheduled");
    let timer_to_cancel = ctx.state::<TimerCancelInput>("timer_to_cancel", "Timers to Cancel");
    let timer_cancelled = ctx.state::<TimerCancelled>("timer_cancelled", "Cancelled Timers");
    let sig_approval_response =
        ctx.signal::<HumanTaskResponse>("sig_approval_response", "Approval Response Signal");
    let sig_sla_timeout = ctx.signal::<DynamicToken>("sig_sla_timeout", "sig_sla_timeout");

    // Outcomes
    let approved = ctx.state::<ApprovedExpense>("approved", "Approved Expenses");
    let rejected = ctx.state::<RejectedExpense>("rejected", "Rejected Expenses");
    let escalated = ctx.state::<EscalatedExpense>("escalated", "Escalated Expenses");

    // Errors
    let effect_errors = ctx.state::<EffectError>("effect_errors", "Effect Errors");

    // -------------------------------------------------------------------------
    // Seed test data
    // -------------------------------------------------------------------------

    ctx.seed(
        &expense_inbox,
        vec![
            ExpenseRequest {
                expense_id: "EXP-001".into(),
                submitter: "alice@example.com".into(),
                amount: 45.00,
                currency: "USD".into(),
                category: "meals".into(),
                description: "Team lunch".into(),
                receipt_url: Some("https://receipts.example.com/001".into()),
            },
            ExpenseRequest {
                expense_id: "EXP-002".into(),
                submitter: "bob@example.com".into(),
                amount: 2500.00,
                currency: "USD".into(),
                category: "equipment".into(),
                description: "New laptop for development".into(),
                receipt_url: Some("https://receipts.example.com/002".into()),
            },
        ],
    );

    // -------------------------------------------------------------------------
    // Phase 1: Prepare analysis job
    // -------------------------------------------------------------------------

    // Build spec with embedded Python script (like the python demo's job_net)
    let python_script = include_str!("../../demos/python/expense_analyzer.py");

    ctx.transition("prepare_analysis", "Prepare Analysis Job")
        .auto_input("expense", &expense_inbox)
        .auto_output("job", &analysis_queue)
        .logic(format!(
            r#"
            let job_id = "analysis-" + expense.expense_id;
            #{{
                job: #{{
                    job_id: job_id,
                    run: 1,
                    retries: 0,
                    max_retries: 2,
                    spec: #{{
                        type: "python",
                        config: #{{
                            script: "expense_analyzer.py",
                            virtualenv: true,
                            sdk: true
                        }},
                        inputs: [
                            #{{
                                name: "expense_analyzer.py",
                                source: #{{
                                    type: "raw",
                                    content: {script_json}
                                }},
                                required: true
                            }},
                            #{{
                                name: "input.json",
                                source: #{{
                                    type: "inline",
                                    value: #{{
                                        expense_id: expense.expense_id,
                                        amount: expense.amount,
                                        currency: expense.currency,
                                        category: expense.category,
                                        description: expense.description,
                                        submitter: expense.submitter
                                    }}
                                }},
                                required: true
                            }}
                        ],
                        outputs: [
                            #{{ name: "result", required: true }}
                        ]
                    }},
                    original_request: expense
                }}
            }}
        "#,
            script_json = serde_json::to_string(python_script).unwrap()
        ));

    // -------------------------------------------------------------------------
    // Phase 2: Submit to Python executor
    // -------------------------------------------------------------------------

    ctx.transition("submit_analysis", "Submit Analysis")
        .auto_input("job", &analysis_queue)
        .auto_output("submitted", &analysis_submitted)
        .error_output(&effect_errors)
        .causes(&sig_analysis_complete)
        .causes(&sig_analysis_failed)
        .executor_submit();

    // Handle analysis completion
    // The executor's completed status detail includes: outcome, duration_ms, outputs, etc.
    // Our Python script calls set_output("result", {...}), so the analysis result is at sig.detail.outputs.result
    ctx.transition("analysis_done", "Analysis Complete")
        .auto_input("job", &analysis_submitted)
        .auto_input("sig", &sig_analysis_complete)
        .correlate("sig", "job", "execution_id")
        .auto_output("result", &analyzed)
        .logic(
            r#"
            let analysis = sig.detail.outputs.result;
            let orig = job.original_request;
            #{
                result: #{
                    expense_id: orig.expense_id,
                    submitter: orig.submitter,
                    amount: orig.amount,
                    currency: orig.currency,
                    category: orig.category,
                    description: orig.description,
                    risk_level: analysis.risk_level,
                    policy_violations: analysis.policy_violations,
                    suggested_category: analysis.suggested_category,
                    auto_approve: analysis.auto_approve
                }
            }
        "#,
        );

    // Handle analysis failure (retry or dead letter)
    ctx.transition("analysis_failed", "Analysis Failed")
        .auto_input("job", &analysis_submitted)
        .auto_input("sig", &sig_analysis_failed)
        .correlate("sig", "job", "execution_id")
        .auto_output("err", &effect_errors)
        .logic(r#"#{ err: #{ message: "Analysis failed", detail: sig } }"#);

    // -------------------------------------------------------------------------
    // Phase 3: Route based on risk level
    // -------------------------------------------------------------------------

    // Low risk → auto-approve
    ctx.transition("route_auto_approve", "Auto-Approve Low Risk")
        .auto_input("expense", &analyzed)
        .guard(r#"expense.auto_approve == true"#)
        .auto_output("approved", &auto_approved)
        .logic(
            r#"
            #{
                approved: #{
                    expense_id: expense.expense_id,
                    approved_by: "system",
                    approval_type: "auto",
                    comments: ()
                }
            }
        "#,
        );

    // Medium/High risk → human review
    ctx.transition("route_to_review", "Route to Human Review")
        .auto_input("expense", &analyzed)
        .guard(r#"expense.auto_approve != true"#)
        .auto_output("review", &pending_review)
        .logic(r#"
            #{
                review: #{
                    // Form schema for human task (steps/blocks model)
                    title: "Expense Approval: " + expense.expense_id,
                    instructions_mdsvex: "Please review this expense request and make an approval decision.",
                    steps: [
                        #{
                            id: "review",
                            title: "Review Expense",
                            description_mdsvex: "**Submitter:** " + expense.submitter + "\n**Amount:** $" + expense.amount + " " + expense.currency + "\n**Category:** " + expense.category + "\n**Description:** " + expense.description + "\n**Risk Level:** " + expense.risk_level,
                            blocks: [
                                #{
                                    type: "mdsvex",
                                    content: "Review the expense details above and select your decision."
                                },
                                #{
                                    type: "input",
                                    field: #{
                                        name: "decision",
                                        label: "Decision",
                                        kind: "select",
                                        options: ["approve", "reject"],
                                        required: true
                                    }
                                },
                                #{
                                    type: "input",
                                    field: #{
                                        name: "comments",
                                        label: "Comments",
                                        kind: "textarea",
                                        required: false
                                    }
                                }
                            ]
                        }
                    ],
                    // Business data
                    expense_id: expense.expense_id,
                    submitter: expense.submitter,
                    amount: expense.amount,
                    currency: expense.currency,
                    category: expense.category,
                    description: expense.description,
                    risk_level: expense.risk_level,
                    policy_violations: expense.policy_violations,
                    suggested_category: expense.suggested_category,
                    reviewer: "manager@example.com"
                }
            }
        "#);

    // -------------------------------------------------------------------------
    // Phase 4: Human-in-the-loop approval
    // -------------------------------------------------------------------------

    // Create human task with form schema in token
    ctx.transition("request_approval", "Request Manager Approval")
        .human_task_to(HumanTaskSubmit {
            task: &pending_review,
            assigned: &approval_task,
            errors: &effect_errors,
            response_signal: &sig_approval_response,
        });

    // -------------------------------------------------------------------------
    // Phase 5: Timer for SLA tracking
    // -------------------------------------------------------------------------

    // Start SLA timer when task is created
    ctx.transition("start_sla_timer", "Start SLA Timer")
        .auto_input("task", &approval_task)
        .auto_output("tracking", &sla_tracking)
        .auto_output("timer", &timer_data)
        .logic(format!(
            r#"
            {{
                let tracking = #{{
                    expense_id: task.expense_id,
                    task_id: task.task_id,
                    reviewer: task.reviewer,
                    timeout_minutes: 5
                }};
                let timer = #{{
                    delay_ms: 300000,
                    target_place_id: "{}",
                    payload: #{{
                        expense_id: task.expense_id,
                        task_id: task.task_id
                    }}
                }};
                #{{ tracking: tracking, timer: timer }}
            }}
        "#,
            sig_sla_timeout.id()
        ));

    ctx.transition("schedule_sla_timer", "Schedule SLA Timer")
        .timer_schedule_to(TimerSchedule {
            timer: &timer_data,
            scheduled: &timer_scheduled,
            errors: &effect_errors,
            signal: &sig_sla_timeout,
        });

    // -------------------------------------------------------------------------
    // Phase 6: Resolution - Human response or timeout
    // -------------------------------------------------------------------------

    // Human approved - also cancels the SLA timer
    ctx.transition("human_approved", "Human Approved")
        .auto_input("tracking", &sla_tracking)
        .auto_input("timer", &timer_scheduled)
        .auto_input("response", &sig_approval_response)
        .guard(r#"response.task_id == tracking.task_id && timer.payload.task_id == tracking.task_id && response.decision == "approve""#)
        .auto_output("approved", &approved)
        .auto_output("cancel", &timer_to_cancel)
        .logic(r#"
            #{
                approved: #{
                    expense_id: tracking.expense_id,
                    approved_by: tracking.reviewer,
                    approval_type: "manual",
                    comments: response.comments
                },
                cancel: #{
                    timer_correlation_id: timer.timer_correlation_id,
                    target_place_id: timer.target_place_id
                }
            }
        "#);

    // Human rejected - also cancels the SLA timer
    ctx.transition("human_rejected", "Human Rejected")
        .auto_input("tracking", &sla_tracking)
        .auto_input("timer", &timer_scheduled)
        .auto_input("response", &sig_approval_response)
        .guard(r#"response.task_id == tracking.task_id && timer.payload.task_id == tracking.task_id && response.decision == "reject""#)
        .auto_output("rejected", &rejected)
        .auto_output("cancel", &timer_to_cancel)
        .logic(r#"
            #{
                rejected: #{
                    expense_id: tracking.expense_id,
                    rejected_by: tracking.reviewer,
                    reason: response.comments
                },
                cancel: #{
                    timer_correlation_id: timer.timer_correlation_id,
                    target_place_id: timer.target_place_id
                }
            }
        "#);

    // Cancel SLA timer when task is completed
    ctx.transition("cancel_sla_timer", "Cancel SLA Timer")
        .timer_cancel_to(TimerCancel {
            timer: &timer_to_cancel,
            cancelled: &timer_cancelled,
            errors: &effect_errors,
        });

    // SLA timeout → escalate (consumes all related tokens)
    ctx.transition("sla_breach", "SLA Breach - Escalate")
        .auto_input("tracking", &sla_tracking)
        .auto_input("timer", &timer_scheduled)
        .auto_input("timeout", &sig_sla_timeout)
        .guard(
            r#"timeout.task_id == tracking.task_id && timer.payload.task_id == tracking.task_id"#,
        )
        .auto_output("escalated", &escalated)
        .logic(
            r#"
            #{
                escalated: #{
                    expense_id: tracking.expense_id,
                    original_reviewer: tracking.reviewer,
                    escalation_reason: "SLA breach - no response within timeout"
                }
            }
        "#,
        );

    // -------------------------------------------------------------------------
    // Move auto-approved to final approved
    // -------------------------------------------------------------------------

    ctx.transition("finalize_auto", "Finalize Auto-Approval")
        .auto_input("auto", &auto_approved)
        .auto_output("final", &approved)
        .logic(r#"#{ final: auto }"#);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    aithericon_sdk::run(
        "expense-approval",
        "Expense Approval Workflow with Python analysis, Human review, and SLA timer",
        definition,
    );
}
