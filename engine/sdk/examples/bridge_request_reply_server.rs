//! Example: Request-Reply Bridge — Server Net
//!
//! Demonstrates the request-reply pattern over Petri net bridges.
//! This is the **server** side that receives requests, processes them,
//! and sends replies back to the client.
//!
//! The server net:
//! 1. Receives a `CalcRequest` in its bridge-in inbox
//! 2. A transition computes the result
//! 3. The result is placed in a bridge-reply outbox (auto-routed back to sender)
//! 4. A copy is kept locally for audit
//!
//! ```text
//! [inbox: bridge-in] → (compute) → [reply_outbox: bridge-reply]  (→ back to client)
//!                                 → [audit_log]                   (local record)
//! ```
//!
//! Run both nets together:
//! ```sh
//! cargo run --example bridge_request_reply_client
//! cargo run --example bridge_request_reply_server
//! ```

use aithericon_sdk::prelude::*;

#[token]
struct CalcRequest {
    id: String,
    operation: String,
    a: i64,
    b: i64,
}

#[token]
struct CalcResult {
    request_id: String,
    operation: String,
    result: i64,
}

#[token]
struct AuditEntry {
    request_id: String,
    operation: String,
    a: i64,
    b: i64,
    result: i64,
}

fn definition(ctx: &mut Context) {
    // Bridge-in: receives requests from the client net
    let inbox = ctx.bridge_in_from::<CalcRequest>("inbox", "Request Inbox", "calc-client", "outbox");

    // Bridge-reply: sends results back to the client's reply_to place
    let reply_outbox = ctx.bridge_reply::<CalcResult>("reply_outbox", "Reply Outbox");

    // Local audit log
    let audit_log = ctx.state::<AuditEntry>("audit_log", "Audit Log");

    // Compute: process the request and produce both a reply and an audit entry
    //
    // Rhai doesn't have match/switch, so we use if-else for the operation.
    // The reply goes to bridge-reply (auto-routed back to sender).
    // The audit entry stays local.
    ctx.transition("compute", "Compute Result")
        .auto_input("req", &inbox)
        .auto_output("reply", &reply_outbox)
        .auto_output("audit", &audit_log)
        .logic(
            r#"
            let result = if req.operation == "add" {
                req.a + req.b
            } else if req.operation == "sub" {
                req.a - req.b
            } else if req.operation == "mul" {
                req.a * req.b
            } else {
                0
            };
            #{
                reply: #{
                    request_id: req.id,
                    operation: req.operation,
                    result: result
                },
                audit: #{
                    request_id: req.id,
                    operation: req.operation,
                    a: req.a,
                    b: req.b,
                    result: result
                }
            }
        "#,
        );
}

fn main() {
    aithericon_sdk::run(
        "calc-server",
        "Request-reply bridge example: server receives CalcRequests, computes results, replies",
        definition,
    );
}
