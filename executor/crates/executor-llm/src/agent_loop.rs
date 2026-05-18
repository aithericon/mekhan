//! Mekhan-side agent-stage tool-use loop driver (sub-phase 2.5d-tools).
//!
//! Sub-phase 2.5d-tools STUB. Subagent M replaces this stub with the real
//! `run_agent_loop` implementation that:
//!
//! 1. Calls `port.complete(request)` with the assembled tools list.
//! 2. If response.tool_calls is empty → returns response (terminal).
//! 3. For each tool_call:
//!    a. Emits SSE `tool_call` event upstream.
//!    b. Awaits `tool_dispatcher.dispatch(call)` (blocks on oneshot
//!       fulfilled by the pool listener's POST /v1/runs/{run_id}/tool_results
//!       handler, which is itself fulfilled by cloud-layer-workflow's
//!       POST /v1/pipelines/{run_id}/tool_results forward).
//!    c. Appends tool_result to messages; emits SSE `tool_resolved`.
//! 4. Iterates (capped at `max_iterations`, default 16, prevents runaway).
//!
//! Replaces the rig-driven `services/pipeline/step_kinds/agent.rs:50-538`
//! tool-loop in clinic. The submitter-holds-loop pattern means clinic
//! executes its OWN tools with its OWN tenant context + RLS + auth.
//!
//! Per Item 0 H.1 risk: rig's parallel-tool-dispatch + retry semantics
//! must be either preserved here OR explicitly diverged-from in the ADR
//! (mekhan/engine/docs/adr/0NNN-tool-use-first-class.md). Subagent M
//! documents the divergence.
//!
//! Wire contracts: see `plan/cloud-layer-phase-2-2.5d-tools-wire-contracts.md`
//! §5 + §6 for CompletionRequest.tools / CompletionResponse.tool_calls + the
//! agent_loop park/resume contract.

// Subagent M: replace this entire file body. Module declared in
// `executor/crates/executor-llm/src/lib.rs` already (per scaffold commit);
// run_agent_loop entry point + ToolDispatcher trait + ToolDefinition /
// ToolCall types live here.

/// Placeholder marker for the scaffold commit. Subagent M deletes this.
#[allow(dead_code)]
pub(crate) const SCAFFOLD_MARKER: &str = "2.5d-tools-agent-loop-stub";
