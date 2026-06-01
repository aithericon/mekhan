//! Job orchestration net with retry, dead-letter routing, and scheduler dispatch.
//!
//! Dispatches execution jobs downstream to a scheduler net via named reply
//! channels, so results/failures route back to this specific instance's typed
//! inboxes. Joins results/failures with correlation on `{job_id, run}`. Failed
//! jobs are retried up to `max_retries` before dead-lettering.
//!
//! ## Modes
//!
//! - **Standalone** (default): `job_queue` is a seeded state place with sample jobs.
//!   Results and failures land in local terminal places.
//! - **Bridged** (`--bridged`): `job_queue` is a `bridge_in` receiving from an upstream
//!   net. Results and failures are forwarded back upstream via `bridge_out`.
//!   Use `--upstream <net-id>` to set the upstream net name (default: `workflow-net`).
//!
//! ## Data flow
//!
//! ```text
//! [job_queue] ──(dispatch)──┬──► [to_scheduler: bridge_out_reply_channels → scheduler relay net/job_inbox]
//!                           └──► [pending_result]     (channels: result→result_inbox, failure→failure_inbox)
//!
//! [result_inbox: bridge_reply] + [pending_result] → (join_result) → [completed]
//!   guard: result.job_id == pending.job_id && result.run == pending.run
//!
//! [failure_inbox: bridge_reply] + [pending_result] → (join_failure)
//!   ├── (retry)       guard: retries < max_retries → [job_queue] (run+1, retries+1)
//!   └── (dead_letter) guard: retries >= max_retries → [dead_letter]
//! ```
//!
//! In `--bridged` mode, two additional forwarding transitions exist:
//! ```text
//! [completed]   → (forward_result)  → [result_outbox: bridge_out → upstream]
//! [dead_letter] → (forward_failure) → [failure_outbox: bridge_out → upstream]
//! ```
//!
//! ## Scoped groups (Lab UI visualization)
//!
//! - **Dispatch** — job dispatch to scheduler + pending result
//! - **Result Join** — correlate results with pending, mark completed
//! - **Failure & Retry** — retry or dead-letter failed jobs
//! - **Result Forwarding** — (bridged mode only) relay results upstream
//!
//! ## Environment variables
//!
//! ```bash
//! NATS_URL=nats://localhost:4333
//! ```

mod common;

use aithericon_sdk::prelude::*;
use common::scheduler_bridge::{
    connect_to_scheduler, SchedulerJobFailure, SchedulerJobResult, SchedulerReply,
};

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

// Job, SchedulerRequest → SchedulerSubmitInput (from effect_tokens)
// Fields: job_id, model_name, run, retries, max_retries, spec

// JobResult and JobFailure are imported from common::scheduler_bridge
// as SchedulerJobResult and SchedulerJobFailure (shared with scheduler_net).

/// Completed job.
#[token]
struct CompletedJob {
    job_id: String,
    model_name: String,
    detail: serde_json::Value,
}

/// Dead-lettered job (retries exhausted).
#[token]
struct DeadLetter {
    job_id: String,
    model_name: String,
    reason: String,
    retries_exhausted: i64,
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context, bridged: bool, upstream: &str) {
    // ── Shared cross-cutting places ───────────────────────────────────────

    let job_queue = if bridged {
        ctx.bridge_in_from::<SchedulerSubmitInput>("job_queue", "Job Queue", upstream, "to_jobs")
    } else {
        ctx.state::<SchedulerSubmitInput>("job_queue", "Job Queue")
    };

    let completed = ctx.state::<CompletedJob>("completed", "Completed");
    let dead_letter = ctx.state::<DeadLetter>("dead_letter", "Dead Letter");

    // Typed reply inboxes — separate, strongly-typed places for results and failures.
    let result_inbox = ctx.bridge_reply::<SchedulerJobResult>("result_inbox", "Result Inbox");
    let failure_inbox = ctx.bridge_reply::<SchedulerJobFailure>("failure_inbox", "Failure Inbox");

    // Typed connector — wires up bridge_out to the scheduler relay net with named reply channels.
    // Compile error if you forget a channel or use the wrong type.
    let to_scheduler = connect_to_scheduler(
        ctx,
        SchedulerReply {
            result: &result_inbox,
            failure: &failure_inbox,
        },
    );

    // ── Seed data (standalone mode only) ──────────────────────────────────

    if !bridged {
        ctx.seed(
            &job_queue,
            vec![
                SchedulerSubmitInput {
                    job_id: "train-alpha".into(),
                    model_name: "ResNet-50".into(),
                    run: 0,
                    retries: 0,
                    max_retries: 2,
                    job_template_id: None,
                    spec: serde_json::json!({
                        "backend": "process",
                        "config": {
                            "command": "echo",
                            "args": ["training complete"]
                        },
                        "inputs": [],
                        "outputs": []
                    }),
                },
                SchedulerSubmitInput {
                    job_id: "eval-beta".into(),
                    model_name: "BERT-large".into(),
                    run: 0,
                    retries: 0,
                    max_retries: 3,
                    job_template_id: None,
                    spec: serde_json::json!({
                        "backend": "process",
                        "config": {
                            "command": "echo",
                            "args": ["evaluation complete"]
                        },
                        "inputs": [],
                        "outputs": []
                    }),
                },
            ],
        );
    }

    // ── Dispatch ──────────────────────────────────────────────────────────

    ctx.scope("Dispatch", |ctx| {
        let pending_result = ctx.state::<SchedulerSubmitInput>("pending_result", "Pending Result");

        // dispatch — take job from queue, bridge to scheduler, hold pending copy.
        ctx.transition("dispatch", "Dispatch to Scheduler")
            .auto_input("job", &job_queue)
            .auto_output("req", &to_scheduler)
            .auto_output("pending", &pending_result)
            .logic(r#"#{ req: job, pending: job }"#);

        // ── Result Join ──────────────────────────────────────────────────

        // join_result — correlate result_inbox + pending on {job_id} → completed.
        // Correlate on job_id only: the result's `run` reflects the final executor
        // attempt and may differ from the pending's `run` after internal retries.
        ctx.transition("join_result", "Join Result")
            .auto_input("result", &result_inbox)
            .auto_input("pending", &pending_result)
            .correlate_on("result", "pending", &["job_id"])
            .auto_output("done", &completed)
            .logic(
                r#"#{
                    done: #{
                        job_id: pending.job_id,
                        model_name: pending.model_name,
                        detail: result.detail
                    }
                }"#,
            );

        // ── Failure & Retry ──────────────────────────────────────────────

        // retry — failure + pending on {job_id}, retries < max.
        ctx.transition("retry", "Retry Failed Job")
            .auto_input("fail", &failure_inbox)
            .auto_input("pending", &pending_result)
            .guard(r#"fail.job_id == pending.job_id && pending.retries < pending.max_retries"#)
            .auto_output("job", &job_queue)
            .logic(
                r#"#{
                    job: #{
                        job_id: pending.job_id,
                        model_name: pending.model_name,
                        run: pending.run + 1,
                        retries: pending.retries + 1,
                        max_retries: pending.max_retries,
                        spec: pending.spec
                    }
                }"#,
            );

        // dead_letter — failure + pending, retries exhausted.
        ctx.transition("dead_letter", "Dead Letter")
            .auto_input("fail", &failure_inbox)
            .auto_input("pending", &pending_result)
            .guard(r#"fail.job_id == pending.job_id && pending.retries >= pending.max_retries"#)
            .auto_output("dead", &dead_letter)
            .logic(
                r#"#{
                    dead: #{
                        job_id: pending.job_id,
                        model_name: pending.model_name,
                        reason: fail.reason,
                        retries_exhausted: pending.retries
                    }
                }"#,
            );
    });

    // ── Result Forwarding (bridged mode only) ─────────────────────────────

    if bridged {
        let result_outbox = ctx.bridge_out::<CompletedJob>(
            "result_outbox",
            "Result Outbox",
            upstream,
            "result_inbox",
        );
        let failure_outbox = ctx.bridge_out::<DeadLetter>(
            "failure_outbox",
            "Failure Outbox",
            upstream,
            "failure_inbox",
        );

        ctx.scope("Result Forwarding", |ctx| {
            // forward_result — relay completed result to upstream net.
            ctx.transition("forward_result", "Forward Result Upstream")
                .auto_input("done", &completed)
                .auto_output("out", &result_outbox)
                .logic(r#"#{ out: done }"#);

            // forward_failure — relay dead-lettered failure to upstream net.
            ctx.transition("forward_failure", "Forward Failure Upstream")
                .auto_input("dead", &dead_letter)
                .auto_output("out", &failure_outbox)
                .logic(r#"#{ out: dead }"#);
        });
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bridged = args.iter().any(|a| a == "--bridged");
    let upstream = args
        .iter()
        .position(|a| a == "--upstream")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("workflow-net");

    let desc = if bridged {
        "Job orchestration net (bridged) — receives from upstream, dispatches to scheduler relay net, relays results back"
    } else {
        "Job orchestration net — dispatches to scheduler relay net, receives results/failures"
    };
    aithericon_sdk::run("job-lifecycle", desc, |ctx| {
        definition(ctx, bridged, upstream)
    });
}
