//! Example: Effect retry pattern with error routing.
//!
//! Demonstrates how effect transitions route failures to an `_error` port
//! with enriched error tokens containing `{ inputs, retryable }`. A downstream
//! retry transition extracts the original inputs and resubmits them.
//!
//! Pattern:
//! ```text
//! [requests] → (call_api: effect) → [responses]     (success)
//!                                  → [errors]        (failure — has inputs + retryable)
//!                                       ↓
//!                         (retry: guard "err.retryable == true")
//!                            → extracts err.inputs.request → [requests]
//!                         (dead_letter: guard "err.retryable != true")
//!                            → [dead_letter]
//! ```

use aithericon_sdk::prelude::*;

#[token]
struct Request {
    url: String,
    payload: String,
}

#[token]
struct Response {
    status: u32,
    body: String,
}

fn definition(ctx: &mut Context) {
    // Places
    let requests = ctx.state::<Request>("requests", "Request Queue");
    let responses = ctx.state::<Response>("responses", "Responses");
    let errors = ctx.state::<DynamicToken>("errors", "Error Queue");
    let dead_letter = ctx.state::<DynamicToken>("dead_letter", "Dead Letter Queue");

    // Effect transition with error output wired up
    ctx.transition("call_api", "Call External API")
        .auto_input("request", &requests)
        .auto_output("response", &responses)
        .error_output(&errors)
        .effect("http_handler");

    // Retry retryable errors by extracting original inputs; dead-letter the rest.
    ctx.effect_error_handler(&errors, &requests, &dead_letter, "request");
}

fn main() {
    aithericon_sdk::run(
        "effect-retry-example",
        "Demonstrates effect retry pattern with error routing and dead-letter handling",
        definition,
    );
}
