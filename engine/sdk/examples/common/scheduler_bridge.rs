//! Typed connector for scheduler-net bridge interface.
//!
//! Scheduler-net uses two named reply channels ("result" and "failure") to route
//! results and failures back to the originating job-net instance. This module
//! provides:
//!
//! - Shared token types (`SchedulerJobResult`, `SchedulerJobFailure`) used by
//!   both scheduler-net and job-net.
//! - A typed connector (`SchedulerReply` + `connect_to_scheduler`) that wires
//!   up the reply channels with compile-time type safety — missing or mistyped
//!   channels cause compile errors.
//!
//! # Usage in job-net
//!
//! ```ignore
//! use common::scheduler_bridge::*;
//!
//! let result_inbox = ctx.bridge_reply::<SchedulerJobResult>("result_inbox", "Result Inbox");
//! let failure_inbox = ctx.bridge_reply::<SchedulerJobFailure>("failure_inbox", "Failure Inbox");
//!
//! let to_scheduler = connect_to_scheduler(ctx, SchedulerReply {
//!     result: &result_inbox,
//!     failure: &failure_inbox,
//! });
//! ```

use aithericon_sdk::prelude::*;

/// Result relayed back from scheduler-net (success path).
#[token]
pub struct SchedulerJobResult {
    pub job_id: String,
    pub run: i64,
    pub detail: serde_json::Value,
}

/// Failure relayed back from scheduler-net (failure path).
#[token]
pub struct SchedulerJobFailure {
    pub job_id: String,
    pub run: i64,
    pub reason: String,
    pub retries: i64,
    pub max_retries: i64,
    pub spec: serde_json::Value,
    pub model_name: String,
}

/// Reply channels expected by scheduler-net.
///
/// Scheduler-net's `result_outbox` uses channel "result" and `failure_outbox`
/// uses channel "failure". This struct enforces that callers provide both.
pub struct SchedulerReply<'a> {
    /// Place where successful job results should land.
    pub result: &'a PlaceHandle<SchedulerJobResult>,
    /// Place where job failures should land.
    pub failure: &'a PlaceHandle<SchedulerJobFailure>,
}

/// Wire up a bridge_out to scheduler-net with typed reply channels.
///
/// Returns a `PlaceHandle<SchedulerSubmitInput>` — the dispatch outbox.
pub fn connect_to_scheduler(
    ctx: &mut Context,
    reply: SchedulerReply<'_>,
) -> PlaceHandle<SchedulerSubmitInput> {
    ctx.bridge_out_reply_channels::<SchedulerSubmitInput>(
        "to_scheduler",
        "To Scheduler",
        "scheduler-net",
        "job_inbox",
        &[
            ("result", reply.result.id()),
            ("failure", reply.failure.id()),
        ],
    )
}
