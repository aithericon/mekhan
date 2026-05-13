//! Example: Request-Reply Bridge — Client Net
//!
//! Demonstrates the request-reply pattern over Petri net bridges.
//! This is the **client** side that sends requests and receives replies.
//!
//! The client net:
//! 1. Takes a `CalcRequest` from the queue
//! 2. Forwards it to the server net via bridge-out (with reply_to)
//! 3. Receives the reply in a bridge-reply inbox
//! 4. A transition processes the reply into the results place
//!
//! ```text
//! [requests] → (send) → [outbox: bridge-out → server/inbox]
//!                                              reply_to: reply_inbox
//!
//! [reply_inbox: bridge-reply] → (handle_reply) → [results]
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

fn definition(ctx: &mut Context) {
    // Local places
    let requests = ctx.state::<CalcRequest>("requests", "Request Queue");
    let results = ctx.state::<CalcResult>("results", "Results");

    // Bridge-out with reply: sends to server's "inbox", expects reply back at "reply_inbox"
    let outbox = ctx.bridge_out_reply::<CalcRequest>(
        "outbox",
        "Request Outbox",
        "calc-server", // target net ID
        "inbox",       // target place name on server
        "reply_inbox", // local place name to receive reply
    );

    // Bridge-reply: receives reply tokens from the server
    let reply_inbox = ctx.bridge_reply::<CalcResult>("reply_inbox", "Reply Inbox");

    // Seed some requests
    ctx.seed(
        &requests,
        vec![
            CalcRequest {
                id: "req-1".into(),
                operation: "add".into(),
                a: 10,
                b: 32,
            },
            CalcRequest {
                id: "req-2".into(),
                operation: "mul".into(),
                a: 7,
                b: 6,
            },
            CalcRequest {
                id: "req-3".into(),
                operation: "sub".into(),
                a: 99,
                b: 57,
            },
        ],
    );

    // Send: move request from queue to bridge outbox
    ctx.transition("send", "Send Request")
        .auto_input("req", &requests)
        .auto_output("out", &outbox)
        .logic(r#"#{ out: req }"#);

    // Handle reply: move result from reply inbox to results
    ctx.transition("handle_reply", "Handle Reply")
        .auto_input("reply", &reply_inbox)
        .auto_output("result", &results)
        .logic(r#"#{ result: reply }"#);
}

fn main() {
    aithericon_sdk::run(
        "calc-client",
        "Request-reply bridge example: client sends CalcRequests, receives CalcResults",
        definition,
    );
}
