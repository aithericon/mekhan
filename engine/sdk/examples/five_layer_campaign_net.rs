//! Layer 0: Campaign Net — Hyperparameter sweep orchestration with fan-out/fan-in.
//!
//! Part of the five-layer bridged net composition (campaign → workflow → job → scheduler → executor).
//! This net dispatches the same ML pipeline workflow with different hyperparameter
//! configurations (lr=0.01 vs lr=0.001), collects results from both, and selects the best —
//! demonstrating **multi-instance orchestration** of the full four-layer stack.
//!
//! ## Demo Scenario: Hyperparameter Sweep
//!
//! ```text
//!                     ┌── workflow (lr=0.01)  ──┐
//! [campaign_start] ──►│   (full 4-step pipeline)│──► [select_best] ──► [campaign_completed]
//!                     └── workflow (lr=0.001) ──┘
//!                          (parallel configs)       (fan-in + select)
//! ```
//!
//! ## Data flow
//!
//! ```text
//! [campaign_start] → (init_campaign) → [config_A_ready] + [config_B_ready]   ← fan-out
//!
//! [config_A_ready] → (dispatch_A) → [to_workflows: bridge_out] + [config_A_pending]
//! [config_B_ready] → (dispatch_B) → [to_workflows: bridge_out] + [config_B_pending]
//!
//! [result_inbox: bridge_in] + [config_A_pending] → (join_A) → [config_A_done]
//! [result_inbox: bridge_in] + [config_B_pending] → (join_B) → [config_B_done]
//!   guard: result.workflow_id == pending.workflow_id
//!
//! [config_A_done] + [config_B_done] → (select_best) → [campaign_completed]  ← fan-in!
//!   guard: a.campaign_id == b.campaign_id
//!
//! [failure_inbox: bridge_in] + [config_X_pending] → (fail_X) → [campaign_failed]
//! ```
//!
//! ## Deploy
//!
//! ```bash
//! # As part of the five-layer campaign demo:
//! just campaign-demo
//!
//! # Or manually (deploy all downstream nets first):
//! cargo run -p aithericon-sdk --example five_layer_campaign_net -- --deploy --net-id campaign-net
//! ```
//!
//! ## Net ID: `campaign-net`

use aithericon_sdk::prelude::*;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// Campaign definition — seeded in campaign_start.
#[token]
struct Campaign {
    campaign_id: String,
    campaign_name: String,
}

/// A workflow config ready to be dispatched.
#[token]
struct ConfigReady {
    campaign_id: String,
    workflow_id: String,
    pipeline_name: String,
    config: serde_json::Value,
}

/// Pending config — held while waiting for workflow result.
#[token]
struct ConfigPending {
    campaign_id: String,
    workflow_id: String,
    config_label: String,
}

/// Workflow request dispatched to workflow-net via bridge.
/// Must match the Workflow token shape expected by five_layer_workflow_net.
#[token]
struct WorkflowRequest {
    workflow_id: String,
    pipeline_name: String,
    config: serde_json::Value,
}

/// Workflow result received from workflow-net via bridge.
#[token]
struct WorkflowResult {
    workflow_id: String,
    pipeline_name: String,
    final_detail: serde_json::Value,
}

/// Workflow failure received from workflow-net via bridge.
#[token]
struct WorkflowFailure {
    workflow_id: String,
    failed_step: String,
    reason: String,
}

/// Completed config — holds workflow result for this config.
#[token]
struct ConfigDone {
    campaign_id: String,
    workflow_id: String,
    config_label: String,
    final_detail: serde_json::Value,
}

/// Terminal campaign completion — both configs finished.
#[token]
struct CampaignCompleted {
    campaign_id: String,
    campaign_name: String,
    config_a_workflow_id: String,
    config_a_detail: serde_json::Value,
    config_b_workflow_id: String,
    config_b_detail: serde_json::Value,
}

/// Terminal campaign failure.
#[token]
struct CampaignFailed {
    campaign_id: String,
    failed_workflow_id: String,
    config_label: String,
    reason: String,
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // -- Places ---------------------------------------------------------------

    // Seeded — campaign definition
    let campaign_start = ctx.state::<Campaign>("campaign_start", "Campaign Start");

    // Config readiness (produced by init_campaign)
    let config_a_ready = ctx.state::<ConfigReady>("config_A_ready", "Config A Ready");
    let config_b_ready = ctx.state::<ConfigReady>("config_B_ready", "Config B Ready");

    // Bridge out — single place for ALL workflow dispatches to workflow-net
    let to_workflows = ctx.bridge_out::<WorkflowRequest>(
        "to_workflows",
        "To Workflows",
        "workflow-net",
        "workflow_start",
    );

    // Pending places — hold metadata while waiting for workflow results
    let config_a_pending = ctx.state::<ConfigPending>("config_A_pending", "Config A Pending");
    let config_b_pending = ctx.state::<ConfigPending>("config_B_pending", "Config B Pending");

    // Bridge in — receive results and failures from workflow-net
    let result_inbox = ctx.bridge_in_from::<WorkflowResult>("result_inbox", "Result Inbox", "workflow-net", "result_outbox");
    let failure_inbox = ctx.bridge_in_from::<WorkflowFailure>("failure_inbox", "Failure Inbox", "workflow-net", "failure_outbox");

    // Done places — track completed configs for fan-in gating
    let config_a_done = ctx.state::<ConfigDone>("config_A_done", "Config A Done");
    let config_b_done = ctx.state::<ConfigDone>("config_B_done", "Config B Done");

    // Terminal places
    let campaign_completed =
        ctx.state::<CampaignCompleted>("campaign_completed", "Campaign Completed");
    let campaign_failed = ctx.state::<CampaignFailed>("campaign_failed", "Campaign Failed");

    // -- Seed data ------------------------------------------------------------

    ctx.seed(
        &campaign_start,
        vec![Campaign {
            campaign_id: "sweep-001".into(),
            campaign_name: "ResNet Hyperparameter Sweep".into(),
        }],
    );

    // -- Transitions ----------------------------------------------------------

    // 1. init_campaign — fan-out: split campaign into two parallel configs.
    ctx.transition("init_campaign", "Initialize Campaign")
        .auto_input("campaign", &campaign_start)
        .auto_output("config_a", &config_a_ready)
        .auto_output("config_b", &config_b_ready)
        .logic(
            r#"#{
                config_a: #{
                    campaign_id: campaign.campaign_id,
                    workflow_id: campaign.campaign_id + ":lr-high",
                    pipeline_name: "ResNet ML Pipeline",
                    config: #{
                        lr: "0.01",
                        batch_size: "32"
                    }
                },
                config_b: #{
                    campaign_id: campaign.campaign_id,
                    workflow_id: campaign.campaign_id + ":lr-low",
                    pipeline_name: "ResNet ML Pipeline",
                    config: #{
                        lr: "0.001",
                        batch_size: "64"
                    }
                }
            }"#,
        );

    // 2. dispatch_A — bridge config A workflow to workflow-net, hold pending.
    ctx.transition("dispatch_A", "Dispatch Config A")
        .auto_input("cfg", &config_a_ready)
        .auto_output("req", &to_workflows)
        .auto_output("pending", &config_a_pending)
        .logic(
            r#"#{
                req: #{
                    workflow_id: cfg.workflow_id,
                    pipeline_name: cfg.pipeline_name,
                    config: cfg.config
                },
                pending: #{
                    campaign_id: cfg.campaign_id,
                    workflow_id: cfg.workflow_id,
                    config_label: "lr-high"
                }
            }"#,
        );

    // 3. dispatch_B — bridge config B workflow to workflow-net, hold pending.
    ctx.transition("dispatch_B", "Dispatch Config B")
        .auto_input("cfg", &config_b_ready)
        .auto_output("req", &to_workflows)
        .auto_output("pending", &config_b_pending)
        .logic(
            r#"#{
                req: #{
                    workflow_id: cfg.workflow_id,
                    pipeline_name: cfg.pipeline_name,
                    config: cfg.config
                },
                pending: #{
                    campaign_id: cfg.campaign_id,
                    workflow_id: cfg.workflow_id,
                    config_label: "lr-low"
                }
            }"#,
        );

    // 4+5. join/fail config A
    ctx.join_pair(
        "A", "Config A",
        &config_a_pending,
        &result_inbox, &config_a_done,
        r#"#{
                out: #{
                    campaign_id: pending.campaign_id,
                    workflow_id: pending.workflow_id,
                    config_label: pending.config_label,
                    final_detail: result.final_detail
                }
            }"#,
        &failure_inbox, &campaign_failed,
        r#"#{
                out: #{
                    campaign_id: pending.campaign_id,
                    failed_workflow_id: pending.workflow_id,
                    config_label: pending.config_label,
                    reason: fail.reason
                }
            }"#,
        &["workflow_id"],
    );

    // 6+7. join/fail config B
    ctx.join_pair(
        "B", "Config B",
        &config_b_pending,
        &result_inbox, &config_b_done,
        r#"#{
                out: #{
                    campaign_id: pending.campaign_id,
                    workflow_id: pending.workflow_id,
                    config_label: pending.config_label,
                    final_detail: result.final_detail
                }
            }"#,
        &failure_inbox, &campaign_failed,
        r#"#{
                out: #{
                    campaign_id: pending.campaign_id,
                    failed_workflow_id: pending.workflow_id,
                    config_label: pending.config_label,
                    reason: fail.reason
                }
            }"#,
        &["workflow_id"],
    );

    // 6. select_best — FAN-IN: fires ONLY when both config_A_done AND config_B_done exist.
    //    This is the key Petri net synchronization primitive — collects results from
    //    both hyperparameter configs and reports both for comparison.
    ctx.transition("select_best", "Select Best (fan-in)")
        .auto_input("a", &config_a_done)
        .auto_input("b", &config_b_done)
        .correlate("a", "b", "campaign_id")
        .auto_output("done", &campaign_completed)
        .logic(
            r#"#{
                done: #{
                    campaign_id: a.campaign_id,
                    campaign_name: "ResNet Hyperparameter Sweep",
                    config_a_workflow_id: a.workflow_id,
                    config_a_detail: a.final_detail,
                    config_b_workflow_id: b.workflow_id,
                    config_b_detail: b.final_detail
                }
            }"#,
        );

    // (Failure paths are now included in join_pair calls above.)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    aithericon_sdk::run(
        "five-layer-campaign",
        "Layer 0: Campaign orchestration net — hyperparameter sweep dispatching parallel workflow configs",
        definition,
    );
}
