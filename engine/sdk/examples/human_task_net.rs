//! Human-in-the-loop task demo scenario.
//!
//! Demonstrates triggering a human task in the Petri Lab engine,
//! which publishes to NATS and waits for a completion signal from
//! the Human UI.
//!
//! The form schema is defined in the token itself, making each task
//! self-describing and allowing for dynamic forms based on runtime data.

use aithericon_sdk::prelude::*;
use serde_json::json;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// Completed verification result.
#[token]
struct CompletedVerification {
    input_ref: String,
    task_id: String,
    is_correct: bool,
    comments: String,
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // -- Places ---------------------------------------------------------------

    let start = ctx.state::<HumanTaskRequest>("start", "start");
    let task_active = ctx.state::<HumanTaskAssigned>("task_active", "task_active");
    let signal_inbox = ctx.signal::<HumanTaskResponse>("signal_inbox", "signal_inbox");
    let completed = ctx.state::<CompletedVerification>("completed", "completed");
    let effect_errors = ctx.state::<EffectError>("effect_errors", "effect_errors");

    // -- Seed data ------------------------------------------------------------
    // Form schema is now part of the token, making it self-describing.

    ctx.seed(
        &start,
        vec![serde_json::from_value(json!({
            "title": "Petri Verification Task",
            "instructions_mdsvex": "Please confirm the data received from the Petri net.",
            "steps": [
                {
                    "id": "verify",
                    "title": "Verify Data",
                    "blocks": [
                        {
                            "type": "mdsvex",
                            "content": "Review the data below and confirm it is correct."
                        },
                        {
                            "type": "input",
                            "field": {
                                "name": "is_correct",
                                "label": "Is the state correct?",
                                "kind": "checkbox",
                                "required": true
                            }
                        },
                        {
                            "type": "input",
                            "field": {
                                "name": "comments",
                                "label": "Additional comments",
                                "kind": "textarea"
                            }
                        }
                    ]
                }
            ],
            "payload": {
                "input_ref": "PETRI-DEMO-001",
                "origin": "demo-script"
            }
        })).unwrap()],
    );

    // -- Transitions ----------------------------------------------------------

    // 1. request_verification — transition that triggers the "human_task" effect.
    //    Form schema comes from the token; config only specifies routing.
    ctx.transition("request_verification", "Request Human Verification")
        .human_task_to(HumanTaskSubmit {
            task: &start,
            assigned: &task_active,
            errors: &effect_errors,
            response_signal: &signal_inbox,
        });

    // 2. finalize — transition that moves from active to done when a signal arrives.
    //    Consumes both the state (task_active) and the signal (signal_inbox).
    ctx.transition("finalize", "Finalize Task")
        .auto_input("state", &task_active)
        .auto_input("signal", &signal_inbox)
        .auto_output("done", &completed)
        .logic(
            r#"
            let result = #{
                input_ref: state.input_ref,
                task_id: state.task_id,
                is_correct: signal.is_correct,
                comments: signal.comments
            };
            return #{ done: result };
        "#,
        );
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    aithericon_sdk::run(
        "human-demo",
        "Human-in-the-loop task demo scenario",
        definition,
    );
}
