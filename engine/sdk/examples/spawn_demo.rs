//! Minimal dynamic spawn demo.
//!
//! Demonstrates `ctx.spawn()` end-to-end: a parent orchestrator spawns a child
//! net that doubles a number and bridges the result back.
//!
//! ## Data flow
//!
//! ```text
//! Parent:
//!   [job] → (prepare_spawn) → [worker_request] → SPAWN EFFECT → [worker_spawned]
//!
//! Child (created dynamically):
//!   [inbox] → (process) → [reply_out]  ─── bridge_reply ──►  parent
//!
//! Parent (continued):
//!   [worker_reply] → (handle_reply) → [result: terminal]
//! ```
//!
//! ## Usage
//!
//! ```bash
//! # Print AIR JSON
//! cargo run -p aithericon-sdk --example spawn_demo
//!
//! # Deploy to running engine
//! cargo run -p aithericon-sdk --example spawn_demo -- --deploy --net-id spawn-demo
//! ```

use aithericon_sdk::prelude::*;

/// Parent orchestrator: seeds a job, spawns a child to process it, collects the result.
fn definition(ctx: &mut Context) {
    // Job to process
    let job = ctx.state::<DynamicToken>("job", "Job");
    ctx.seed(
        &job,
        vec![DynamicToken(serde_json::json!({ "value": 21, "operation": "double" }))],
    );

    // Spawn child worker — io.inbox, io.reply, io.failure are auto-created
    let worker = ctx.spawn::<DynamicToken>("worker", |child, io| {
        child
            .transition("process", "Double Value")
            .auto_input("job", &io.inbox)
            .auto_output("out", &io.reply)
            .logic(r#"#{ out: #{ value: job.value * 2, original: job.value, status: "done" } }"#);
    });

    // Prepare the spawn request: wrap job as initial_token for the child's inbox
    ctx.transition("prepare_spawn", "Prepare Spawn")
        .auto_input("job", &job)
        .auto_output("spawn_request", &worker.request)
        .logic(
            r#"#{
            spawn_request: #{
                initial_token: #{ value: job.value, operation: job.operation },
                target_place: "inbox"
            }
        }"#,
        );

    // Handle success: child result arrives at worker_reply (via BridgeReply correlation)
    let result = ctx.state::<DynamicToken>("result", "Result");
    ctx.transition("handle_reply", "Handle Reply")
        .auto_input("reply", &worker.reply)
        .auto_output("out", &result)
        .logic(r#"#{ out: reply }"#);
    ctx.wire_terminal(&result, "completed");

    // Handle failure: child failure arrives at worker_failure
    let failed = ctx.state::<DynamicToken>("failed", "Failed");
    ctx.transition("handle_failure", "Handle Failure")
        .auto_input("err", &worker.failure)
        .auto_output("out", &failed)
        .logic(r#"#{ out: err }"#);
    ctx.wire_terminal(&failed, "dead_letter");
}

fn main() {
    aithericon_sdk::run(
        "spawn-demo",
        "Minimal ctx.spawn() demo: parent spawns child, child doubles a number, result bridges back",
        definition,
    );
}
