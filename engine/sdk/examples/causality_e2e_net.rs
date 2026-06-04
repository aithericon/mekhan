//! Causality E2E test scenario.
//!
//! Purpose-built workflow for exercising the full ADR-18 causality pipeline:
//! - Seed token → process auto-creation
//! - Rhai transition → process tag propagation
//! - Human task → task ingest + completion signal
//! - Executor (Python backend) → executor lifecycle + catalogue artifacts
//! - Terminal → NetCompleted lifecycle
//!
//! ## Topology
//!
//! ```text
//! [start]                                        ─── SEED: creates process via causality
//!   │
//!   ▼
//! (t_prepare: rhai)                              ─── TransitionFired: tag propagation
//!   │
//!   ▼
//! [prepared]
//!   │
//!   ▼
//! (t_request_review: human_task effect)          ─── EffectCompleted: publishes to HUMAN_REQUESTS
//!   │                                                Mekhan task ingest creates hpi_tasks row
//!   ▼
//! [review_assigned]  +  [sig_review]             ─── Signal: human completes review via NATS
//!   │                     │
//!   └─────────┬───────────┘
//!             ▼
//! (t_dispatch: rhai, guard approved == "yes")    ─── TransitionFired: builds executor spec
//!   │
//!   ▼
//! [exec_queue]
//!   │
//!   ▼
//! (executor_lifecycle component)                 ─── Full executor: submit → accepted → running → completed
//!   │                                                Python script emits artifact → catalogue_register effect
//!   ▼
//! [done] (terminal)                              ─── NetCompleted: lifecycle listener updates instance
//! ```
//!
//! ## Running
//!
//! ```bash
//! # Print AIR JSON to stdout
//! cargo run --example causality_e2e_net
//!
//! # Deploy directly to a running engine
//! cargo run --example causality_e2e_net | curl -X POST http://localhost:3030/api/nets/e2e-test/scenario -d @-
//! ```

mod common;

use aithericon_sdk::prelude::*;
use common::executor_lifecycle::{executor_lifecycle, ExecutorBridges};
use serde_json::json;

const PYTHON_SCRIPT: &str = r#"
import aithericon
import json
import tempfile
import os

aithericon.init()

# Load inputs (the approval data is passed through)
inputs = aithericon.load_inputs()
aithericon.log_info("Causality E2E test script running")

# Emit a metric
aithericon.log_metric("test_metric", 1.0, step=1)

# Report progress
aithericon.update_progress(fraction=0.5, message="Processing")

# Create a test artifact
data = {"result": "success", "source": "causality-e2e"}
path = os.path.join(tempfile.gettempdir(), "e2e_result.json")
with open(path, "w") as f:
    json.dump(data, f)

aithericon.log_artifact(
    path=path,
    name="e2e_result.json",
    category="output",
    extract_metadata=True,
)

# Set output
aithericon.set_output("result", data)
aithericon.update_progress(fraction=1.0, message="Done")
# Don't call shutdown() - let process exit naturally; sidecar handles cleanup
"#;

#[token]
struct WorkflowInput {
    workflow_id: String,
    payload: serde_json::Value,
}

#[token]
struct PreparedData {
    workflow_id: String,
    payload: serde_json::Value,
    step: String,
}

fn definition(ctx: &mut Context) {
    // ── Places ────────────────────────────────────────────────────────────

    let start = ctx.state::<WorkflowInput>("start", "Start");
    let prepared = ctx.state::<PreparedData>("prepared", "Prepared");

    // Human task places
    let review_form = ctx.state::<HumanTaskRequest>("review_form", "Review Form");
    let review_assigned = ctx.state::<HumanTaskAssigned>("review_assigned", "Review Assigned");
    let sig_review = ctx.signal::<HumanTaskResponse>("sig_review", "Review Response");
    let human_errors = ctx.state::<EffectError>("human_errors", "Human Effect Errors");

    // Executor input
    let exec_queue = ctx.state::<ExecutorSubmitInput>("exec_queue", "Execution Queue");

    // ── Seed ──────────────────────────────────────────────────────────────

    ctx.seed(
        &start,
        vec![WorkflowInput {
            workflow_id: "causality-e2e".into(),
            payload: json!({ "value": 42 }),
        }],
    );

    // ── Phase 1: Prepare (pure Rhai → tests tag propagation) ─────────────

    ctx.transition("t_prepare", "Prepare Data")
        .auto_input("input", &start)
        .auto_output("prepared", &prepared)
        .auto_output("form", &review_form)
        .logic(
            r#"
            #{
                prepared: #{
                    workflow_id: input.workflow_id,
                    payload: input.payload,
                    step: "prepared"
                },
                form: #{
                    title: "Review: " + input.workflow_id,
                    instructions_mdsvex: "Please review the data and approve processing.",
                    steps: [
                        #{
                            id: "review",
                            title: "Review Data",
                            blocks: [
                                #{
                                    type: "mdsvex",
                                    content: "Value to process: **" + input.payload.value.to_string() + "**"
                                },
                                #{
                                    type: "input",
                                    field: #{
                                        name: "approved",
                                        label: "Approve processing?",
                                        kind: "select",
                                        options: ["yes", "no"],
                                        required: true
                                    }
                                }
                            ]
                        }
                    ],
                    payload: #{
                        workflow_id: input.workflow_id
                    }
                }
            }
            "#,
        );

    // ── Phase 2: Human Review ────────────────────────────────────────────

    ctx.transition("t_request_review", "Request Human Review")
        .human_task_to(HumanTaskSubmit {
            task: &review_form,
            assigned: &review_assigned,
            errors: &human_errors,
            response_signal: &sig_review,
        });

    // ── Phase 3: Dispatch to executor (after approval) ───────────────────

    // Register script content as Rhai variable to avoid JSON escaping issues
    ctx.rhai_var("E2E_SCRIPT", PYTHON_SCRIPT);

    ctx.transition("t_dispatch", "Dispatch to Executor")
        .auto_input("state", &prepared)
        .auto_input("assigned", &review_assigned)
        .auto_input("signal", &sig_review)
        .guard(r#"signal.data.approved == "yes""#)
        .auto_output("job", &exec_queue)
        .logic(
            r#"
            #{
                job: #{
                    job_id: "e2e-" + state.workflow_id,
                    run: 0,
                    retries: 0,
                    max_retries: 1,
                    spec: #{
                        backend: "python",
                        config: #{
                            script: "e2e_script.py",
                            virtualenv: true,
                            sdk: true,
                            requirements: []
                        },
                        inputs: [
                            #{
                                name: "e2e_script.py",
                                source: #{ type: "raw", content: E2E_SCRIPT }
                            }
                        ],
                        outputs: [
                            #{ name: "result" }
                        ]
                    }
                }
            }
            "#,
        );

    // ── Phase 4: Executor lifecycle → terminal ───────────────────────────

    let handles = executor_lifecycle(
        ctx,
        ExecutorBridges {
            inbox: exec_queue,
            result_out: None,
            failure_out: None,
            process_id: None,
            process_step: None,
            catalogue: true,
            process: false,
            stream_output: None,
            control_in: None,
        },
    );

    // Mark "completed" as terminal so NetCompleted fires
    ctx.wire_terminal(&handles.completed, "done");
}

fn main() {
    aithericon_sdk::run(
        "causality-e2e",
        "Causality E2E test: seed → prepare → human review → python executor → catalogue → terminal",
        definition,
    );
}
