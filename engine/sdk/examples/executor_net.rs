//! Executor lifecycle net with status signals, mid-execution events, retry, and cancel.
//!
//! Defines a Petri net that submits execution jobs via the `executor_submit` effect handler,
//! then uses ExecutorWatcher-delivered signals to track the full lifecycle. Failed jobs
//! are retried up to `max_retries` before dead-lettering.
//!
//! ## Modes
//!
//! - **Standalone** (default): `exec_queue` is a seeded state place with a sample job.
//!   Results and failures land in local terminal places.
//! - **Bridged** (`--bridged`): `exec_queue` is a `bridge_in` receiving from the scheduler relay net.
//!   Results and failures are forwarded back to the scheduler relay net via `bridge_out`.
//!
//! ## Status lifecycle
//!
//! The executor reports these lifecycle statuses: accepted, running, completed, failed,
//! cancelled, timed_out. Each status is delivered to a dedicated signal place via
//! `EXECUTOR_SIGNAL_ROUTES` (or falls back to `EXECUTOR_SIGNAL_PLACE`).
//!
//! ## Mid-execution events
//!
//! The executor IPC protocol emits 6 event categories during execution: progress,
//! artifact, metric, phase, output, log. Each is delivered to a signal place via
//! `EXECUTOR_EVENT_ROUTES`. Events with no configured route are silently dropped.
//!
//! ## Net topology
//!
//! ```text
//! [exec_queue] ──(submit: effect "executor_submit")──► [submitted]
//!                                                  \──► [effect_errors]
//!
//! [submitted]  + [sig_accepted]  → (t_accepted) → [accepted]
//! [accepted]   + [sig_running]   → (t_running)  → [running]
//!
//! [running]    + [sig_completed] → (t_success)  → [completed]
//!   guard: sig.execution_id == job.execution_id
//!
//! [running]    + [sig_failed]    → (t_failed)   → [failed]
//! [running]    + [sig_timed_out] → (t_timeout)  → [timed_out]
//!
//! [failed]
//!   ├── (retry)       guard: err.retries < err.max_retries → [exec_queue]
//!   └── (dead_letter) guard: err.retries >= err.max_retries → [dead_letter]
//!
//! [timed_out] → (retry_timeout) guard: retries < max → [exec_queue]
//! [timed_out] → (dlq_timeout)   guard: retries >= max → [dead_letter]
//!
//! [cancel_request] + [running] → (cancel: effect "executor_cancel") → [cancelling]
//! [cancelling] + [sig_cancelled] → (t_cancelled) → [cancelled]
//!
//! Mid-execution events (independent, no lifecycle coupling):
//! [sig_progress] → (log_progress) → [progress_log]
//! [sig_artifact] → (log_artifact) → [artifact_log]
//! [sig_metric]   → (log_metric)   → [metric_log]
//! [sig_phase]    → (log_phase)    → [phase_log]
//! [sig_output]   → (log_output)   → [output_log]
//! [sig_log]      → (log_message)  → [message_log]
//! ```
//!
//! In `--bridged` mode, two additional forwarding transitions exist:
//! ```text
//! [completed]   → (forward_result)  → [result_outbox: bridge_out → scheduler relay net]
//! [dead_letter] → (forward_failure) → [failure_outbox: bridge_out → scheduler relay net]
//! ```
//!
//! ## Scoped groups (Lab UI visualization)
//!
//! The net is organized into visual groups:
//! - **Submission** — submit effect + submitted state
//! - **Status Tracking** — accepted/running signal correlation + success path
//! - **Failure & Retry** — failed/timed_out handling with retry guards
//! - **Cancellation** — cancel request → cancel effect → cancelled confirmation
//! - **Events** — independent mid-execution event logging (all 6 categories)
//! - **Effect Error Recovery** — retryable/non-retryable effect error handling
//! - **Result Forwarding** — (bridged mode only) bridge results/failures upstream
//!
//! ## Environment variables
//!
//! ```bash
//! EXECUTOR_ENABLED=true
//! EXECUTOR_SIGNAL_PLACE=sig_executor
//! EXECUTOR_SIGNAL_ROUTES=accepted:sig_accepted,running:sig_running,completed:sig_completed,failed:sig_failed,cancelled:sig_cancelled,timed_out:sig_timed_out
//! EXECUTOR_EVENT_ROUTES=progress:sig_progress,artifact:sig_artifact,metric:sig_metric,phase:sig_phase,output:sig_output,log:sig_log
//! EXECUTOR_NAMESPACE=executor_jobs
//! NATS_URL=nats://localhost:4333
//! ```
//!
//! ## Process tracking (metrics/logs as breadcrumbs)
//!
//! By default, mid-execution `metric` and `log` events from the executor IPC
//! sidecar are merely logged into local places. To route them through the
//! `process_log_metric` / `process_log_message` effect handlers — whose
//! `EffectCompleted` events are projected by Mekhan's causality consumer
//! into `hpi_metrics` / `hpi_logs` — pass `--process` (or set
//! `EXECUTOR_NET_PROCESS=true`).
//!
//! ## Three-layer composition
//!
//! In a real deployment, the executor net sits at the third layer:
//!
//! ```text
//! LAYER 1: Job/Workflow Net (user-defined)
//!   [job_queue] → bridge_out → scheduler relay net/job_inbox
//!   Waits for allocation signal bridged back
//!
//! LAYER 2: Scheduler Net (nomad/slurm)
//!   [job_inbox] (bridge_in) → "scheduler_submit" effect → [submitted]
//!   [sig_running] → [allocated]
//!   [allocated] → bridge_out → executor-net/exec_queue
//!   (carries: allocation_id, node_name, execution_spec)
//!
//! LAYER 3: Executor Net (this example with --bridged)
//!   [exec_queue] (bridge_in) → "executor_submit" effect → [submitted]
//!   ...lifecycle...
//!   [completed] → bridge_out → job-net (result with outputs/artifacts)
//! ```
//!
//! Each net runs on its own `net_id`. Cross-net communication uses
//! `petri.bridge.{target_net_id}.{place}` NATS subjects, handled by
//! the existing `CrossNetBridge` infrastructure.

mod common;

use aithericon_sdk::prelude::*;
use common::executor_lifecycle::{executor_lifecycle, ExecutorBridges};

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context, bridged: bool, process: bool) {
    let exec_queue = if bridged {
        ctx.bridge_in_from::<ExecutorSubmitInput>(
            "exec_queue",
            "Execution Queue",
            "scheduler-net",
            "to_executor",
        )
    } else {
        ctx.state::<ExecutorSubmitInput>("exec_queue", "Execution Queue")
    };

    // Seed data (standalone mode only)
    if !bridged {
        ctx.seed(
            &exec_queue,
            vec![
                serde_json::from_value::<ExecutorSubmitInput>(serde_json::json!({
                    "job_id": "train-alpha",
                    "run": 0,
                    "retries": 0,
                    "max_retries": 3,
                    "spec": {
                        "backend": "process",
                        "inputs": [],
                        "outputs": [],
                        "config": {
                            "command": "echo",
                            "args": ["training complete"]
                        }
                    }
                }))
                .unwrap(),
            ],
        );
    }

    // Bridge outputs for bridged mode
    let result_out = if bridged {
        Some(ctx.bridge_out::<DynamicToken>(
            "result_outbox",
            "Result Outbox",
            "scheduler-net",
            "exec_result_inbox",
        ))
    } else {
        None
    };

    let failure_out = if bridged {
        Some(ctx.bridge_out::<DynamicToken>(
            "failure_outbox",
            "Failure Outbox",
            "scheduler-net",
            "exec_failure_inbox",
        ))
    } else {
        None
    };

    executor_lifecycle(
        ctx,
        ExecutorBridges {
            inbox: exec_queue,
            result_out,
            failure_out,
            process_id: None,
            process_step: None,
            catalogue: true,
            process,
            stream_output: None,
            control_in: None,
        },
    );
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bridged = args.iter().any(|a| a == "--bridged");
    let process = args.iter().any(|a| a == "--process")
        || std::env::var("EXECUTOR_NET_PROCESS")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
    let (name, desc) = if bridged {
        (
            "executor-lifecycle",
            "Executor lifecycle net (bridged) — receives from scheduler relay net, returns results/failures",
        )
    } else {
        (
            "executor-lifecycle",
            "Executor lifecycle net with status signals, mid-execution events, retry, and cancel",
        )
    };
    aithericon_sdk::run(name, desc, |ctx| definition(ctx, bridged, process));
}
