# Agent Node — Design

Status: **Design (pre-implementation)**
Author: design record from architectural discussion 2026-05-26.
Related code (read first): [`docs/10-control-data-token-model.md`](./10-control-data-token-model.md), `service/src/compiler/lower.rs` (`lower_automated_step`, `lower_loop`, `lower_subworkflow`, `split_outputs`), `service/src/models/template.rs` (`WorkflowNodeData`, `ExecutionBackendType`), `executor/crates/executor-llm/src/adapters/anthropic.rs` (tool_use schema), `engine/core-engine/crates/application/src/firing.rs:770` (effect replay), `executor/crates/executor-worker/src/executor.rs:54` (execute lifecycle).

## 1. What problem this solves

We can already wire a single-shot LLM call as an `AutomatedStep` with `ExecutionBackendType::Llm`. What we cannot do is express **an LLM call that may decide to invoke a tool, read the tool result, then call itself again, repeat until terminal** — the basic agent loop. Today users hand-build that loop with `Decision` + `Loop` + ad-hoc Python steps. The result is verbose, doesn't surface "tool calls" as a first-class concept to the picker / variable resolver, doesn't compose with `SubWorkflow` cleanly, and gives no place to attach context-window management or per-turn cost telemetry.

LangChain, n8n, Flowise, Dify all answer this with an **Agent node** + a soup of socketed sub-nodes (Memory, Tools, Output Parser, …). The socket pattern is the wrong shape for this platform — our `Resources`, parked-data places, and `SubWorkflow` already span the same surface and force a typed-port discipline the socket model bypasses. This doc specifies the platform-native version.

## 2. The model

### 2.1 One node, two lowering paths

The Agent **subsumes** today's single-shot Llm AutomatedStep. There is one node kind, `WorkflowNodeData::Agent`. Properties scale up:

```
Agent {
    label, slug,
    model: ModelRef,              // same shape as today's LlmConfig
    system_prompt: TemplatedStr,
    user_prompt: TemplatedStr,    // initial human message (interpolated)
    response_format: ResponseFormat,
    max_turns: u32,               // default 1
    stop_when: Option<RhaiExpr>,  // optional terminal guard
    context_strategy: ContextStrategy,
    on_tool_error: ToolErrorPolicy,  // default Feedback
    // (tools are discovered structurally — children of this container)
}
```

The compiler chooses lowering by structure:

- **Degenerate path** — when there are zero child tool nodes **and** `max_turns == 1` **and** `stop_when.is_none()`, lower byte-identically to today's `lower_automated_step` with `ExecutionBackendType::Llm`. No parked messages place, no route transition, no agent loop. This is the contract pinned by the equivalence test (§ 7).
- **Agent path** — otherwise, lower to the agent loop (§ 3).

This collapses the "Llm AutomatedStep vs Agent" taxonomy users would otherwise face. Adding the first tool to a single-shot LLM node is *not* a destructive recreate — properties grow, lowering changes silently.

### 2.2 Tools are tagged children, not a node kind

A "tool" is **any child node** of the Agent container, tagged with:

```
ToolMeta {
    tool_name: String,           // Rhai-identifier-safe, unique within the agent
    tool_description: String,    // shown to the LLM
}
```

The tool's **input schema** is derived from the child node's existing input `Port` shape — same source of truth as the variable picker, validation, and read-arc synthesis. No second schema definition. The tool's implementation is whatever the child is: Python AutomatedStep, HTTP AutomatedStep, Docker, `SubWorkflow`, `HumanTask`, or *another Agent*. The Agent compiler treats them uniformly.

This is the distinctive thing this design unlocks: **tools can be full workflows**. A "research_topic" tool can be a `SubWorkflow` that does web search → 5x extract → summarize → human approval → return. The LLM sees one tool call; the platform sees a fully observable, cancellable, retry-able workflow.

`tool_name` collision within an agent is a hard `CompileError::ToolNameConflict` — same pattern as `SlugConflict` ([`docs/10`](./10-control-data-token-model.md) § 4).

### 2.3 Memory / RAG / sessions are NOT new primitives

LangChain conflates three things under "memory":

| What | Where it lives here |
|---|---|
| (a) within-run conversation state | the parked `p_{id}_state` place (§ 3) |
| (b) cross-run session memory | convention: `session_id` input + Postgres resource via existing `Resources` |
| (c) knowledge retrieval (RAG) | a *tool* whose implementation is a pgvector AutomatedStep |

No `Memory` socket node type, no `VectorStore` primitive. The Agent stays narrow.

## 3. Compiled Petri-net IR

The agent-path lowering mirrors `lower_loop` (`service/src/compiler/lower.rs:1835`) for the loop scaffold, and `lower_automated_step:1059` for the LLM-call lifecycle inside it.

```
p_{id}_input ── t_{id}_enter ──► p_{id}_state   (parked, slim envelope)
                                       │  (read-arc)
                                       ▼
                            ┌─── t_{id}_call_llm   (executor LLM job, tools=children schemas)
                            │              │
                            │              ▼
                            │       p_{id}_response
                            │              │
                            │     ┌────────┴────── t_{id}_route ─────┐
                            │     │ tool_calls non-empty             │ final / stop_when
                            │     ▼                                  ▼
                            │   p_{id}_dispatch_<toolN>          p_{id}_final
                            │     │  (one per declared tool)         │
                            │     ▼                                  ▼
                            │   <child node subnet>             t_{id}_exit
                            │     │  (any backend; parks output)     │
                            │     ▼                                  ▼
                            │   p_{child}_data  ◄── read-arc ───  p_{id}_output
                            │     │
                            │     ▼
                            │   t_{id}_collect_<toolN>  (append role:tool msg, turn+1, append JSONL)
                            │     │
                            └─────┘  guard: turn < max_turns && !stop_when
```

### 3.1 The parked state place

`p_{id}_state` is the agent's persistent state across turns. It is **slim** — never the message log itself:

```
AgentState {
    turn: u32,
    message_count: u32,
    total_tokens_in: u64,
    total_tokens_out: u64,
    final_response: Option<String>,     // set on terminal turn
    history_ref: StorageRef,            // S3 URI of latest cumulative JSONL
    workspace_uri: StorageRef,          // run-scoped artifact prefix for tools
}
```

Borrowable via `<agent_slug>.turn`, `<agent_slug>.final_response`, `<agent_slug>.history_ref`, etc. — standard read-arc synthesis. The actual message log is **never** in the parked envelope, **never** on the workflow token. Same side-channel pattern AutomatedStep already uses for `config_ref` (`lower.rs:1118-1130`).

### 3.2 The message log

A JSONL object in S3 keyed by `agent-{execution_id}/turn-{N}.jsonl` (or equivalent), cumulative per turn (S3 lacks true append; new version per turn, latest URI baked into `p_{id}_state.history_ref`). Writes happen as an executor-side effect on `t_call_llm` (before dispatch: append the user/system turn) and `t_collect_*` (after tool: append the tool result message). The engine never sees the bulk.

`<agent_slug>.messages` is **deliberately not** a Rhai-borrowable field — only `<agent_slug>.history_ref` is. Python tools and downstream AutomatedSteps that want the log read it as a staged file (`<slug>.jsonl`, same way [`docs/10`](./10-control-data-token-model.md) § "direct slug access" already stages `<slug>.json`). Rhai stays for control flow, not for slinging KBs of conversation.

### 3.3 The tool workspace

Tools that need to share files with each other across turns (Tool A writes `report.pdf` at turn 3, Tool B reads it at turn 7) use the **run-scoped artifact prefix** `workspace_uri`. This is an S3 prefix per agent `execution_id`, exposed to tools as the borrowed `<agent_slug>.workspace_uri` field; tools write/read under it via the existing `aithericon-file-metadata` plumbing. No shared local filesystem, hermeticity of individual tool jobs preserved.

### 3.4 LLM request assembly

`t_call_llm` is an executor lifecycle (`executor_lifecycle`) with `ExecutionBackendType::Llm`. The compiler emits a job spec whose `config_ref` (side-channeled) carries:

- The current `history_ref`
- The list of declared `tool_schemas` from children
- The `response_format`
- Context-strategy parameters (max tokens, compaction mode)

The executor's `prepare` hook materializes the JSONL + any referenced tool result blobs from S3, applies `context_strategy` (drop_oldest / summarize_oldest / none), builds the provider-specific request, dispatches. Tool schemas (potentially 50KB for tool-heavy agents) ride the same `config_ref` path — no Rhai-expression-complexity panic risk.

### 3.5 Routing

`t_route` consumes the response envelope. The branch decision is data-driven:

- If `response.tool_calls` is non-empty: deposit `tool_calls[0]` (serial-only — see § 6.1) on `p_{id}_dispatch_<toolN>` where `<toolN>` matches `tool_calls[0].name`. One dispatch place + one dispatch transition per declared tool, each guarded by `response.tool_calls[0].name == "<toolN>"`.
- Else (text response, or `stop_reason == "end_turn"`, or `stop_when` guard true): deposit `final_response` on `p_{id}_final`.
- **Unknown-tool fallback**: a catch-all transition guarded by `!known_tool_names.contains(response.tool_calls[0].name)` deposits a synthetic tool-error message onto `p_{id}_state` and routes back to `t_call_llm`. No new mechanism — same path as `ToolErrorPolicy::Feedback`. The LLM sees `role: tool, content: "Unknown tool 'X'. Available: …"` and tries again.

### 3.6 Tool error handling

Each tool's own retry topology fires first (existing `retry_policy` on the child AutomatedStep, untouched). If the tool ultimately fails:

- `ToolErrorPolicy::Feedback` (default): `t_collect_<toolN>` reads the error envelope (read-arc on `p_{child}_error`), appends `role: tool, content: "Tool '<name>' failed: <message>"` to the JSONL, increments turn, routes back to `t_call_llm`. The LLM decides whether to retry, switch tools, or give up.
- `ToolErrorPolicy::Bubble`: tool failure becomes agent failure — token routes to `p_{id}_error`, normal node-error handlers apply.

Default is `Feedback` because models routinely recover; `Bubble` is for tools where retrying isn't safe (e.g. side-effecting tools that may have partially succeeded).

## 4. Replay & cancellation

### 4.1 Replay

**Verified against `engine/core-engine/crates/application/src/firing.rs:770`**: during `ExecutionMode::Replay`, the engine scans `EffectCompleted` / `EffectFailed` events by cursor, calls `handler.replay()` (state-rehydration only — does *not* call `execute()`), and reuses stored `produced_tokens` and `effect_result` verbatim. The side-effecting executor dispatch (which is what makes the actual LLM API call and runs the tool) is **never re-invoked on replay**.

Implication: N agent turns produce N journaled `EffectCompleted` events; replay reproduces them positionally in log order. **Executor-side dedup is irrelevant for replay safety** — the `.lock` file (`executor.rs:103`) is in-flight protection only, and `CleanupPolicy::Immediate` wipes it post-run, but that doesn't matter because the engine never asks the executor to re-run the job on replay.

Deterministic per-turn executor_ids (e.g. derived from `(agent_id, turn_number)`) are still nice-to-have for log grep-ability across runs, but they are **not load-bearing for correctness**. A normal UUID per turn is fine.

### 4.2 Cancellation

Cancel propagates to the currently-active executor job via the existing `executor.cancel.*` channel. Parked state stays at the **last completed turn boundary** — a partially-applied turn is not committed to the JSONL log until `t_collect_*` fires. No half-applied-turn recovery is attempted; restart resumes from the last completed turn.

If a `SubWorkflow` tool is mid-run when the agent is cancelled, the cancellation propagates down the sub-workflow tree via the existing SubWorkflow cancellation path (`project_subworkflow_keystone`, already e2e-proven).

## 5. Cost telemetry

`t_call_llm` emits a per-turn metric event via the existing `stream_events: ["metric", …]` plumbing (`lower.rs:1177`):

```
{
    metric: "llm_turn",
    tags: { agent_id, model, turn },
    values: { tokens_in, tokens_out, latency_ms, tool_calls_count },
}
```

Projects into `hpi_metrics` via the existing metric consumer. Cost-to-dollars is a downstream lookup table, not a v1 concern — we just emit raw token counts. The reason to bake this in from v1: retroactive wiring across old runs is a migration headache, and "how much did that cost" is the first ops question once agents ship.

## 6. v1 scope

### 6.1 Serial tool calls only

The LLM may emit multiple `tool_calls` in one response. v1 takes only `tool_calls[0]` and ignores the rest, OR sets `parallel_tool_use=false` on the provider request where supported. The dispatch/collect Petri-net pattern is one-tool-per-turn.

True parallel tool calls require data-driven multi-instance subnets (fan out N tokens, join when all N return) — the engine's current `ParallelSplit`/`ParallelJoin` are statically shaped, so this is a new engine primitive, not a service-compiler change. **Deferred.**

### 6.2 Per-turn events, no token streaming

v1 emits per-turn `EventCategory::AgentTurn` events (added alongside the existing Metric/Progress/Phase/Log categories) on `executor.events.{exec_id}.agent_turn`. Token-level streaming (model decoding live into the UI) doubles the project scope (executor adapter changes for streaming, NATS subject conventions, mekhan pass-through, SvelteKit renderer). **Deferred to Phase 2.**

### 6.3 max_agent_depth safety knob

Template-level (or global) `max_agent_depth: u32` to prevent runaway recursion when Agents call SubWorkflows that contain Agents that … The check fires at template publish, walking the static template graph. Cheap to add, expensive to debug without.

## 7. Equivalence test contract

**This is the contract the implementation must satisfy.** Test name: `agent_degenerate_lowers_byte_identical_to_llm_automated_step`. Location: `service/src/compiler/tests/agent_lowering.rs` (new file).

Construction:

```rust
let agent_template = template_with_node(WorkflowNodeData::Agent {
    label: "X".into(),
    model: anthropic_haiku(),
    system_prompt: "You are helpful.".into(),
    user_prompt: "Do the thing.".into(),
    response_format: ResponseFormat::Text,
    max_turns: 1,
    stop_when: None,
    context_strategy: ContextStrategy::None,
    on_tool_error: ToolErrorPolicy::Feedback,
    // no children
});

let llm_template = template_with_node(WorkflowNodeData::AutomatedStep {
    label: "X".into(),
    execution_spec: ExecutionSpecConfig {
        backend_type: ExecutionBackendType::Llm,
        config: same_llm_config_json(),
    },
    retry_policy: RetryPolicy::default(),
    output: default_output_port(ExecutionBackendType::Llm),
    deployment_model: DeploymentModel::default(),
    // ...
});

let agent_ir = compile(agent_template)?.air;
let llm_ir = compile(llm_template)?.air;

assert_eq!(
    canonicalize(agent_ir.places),
    canonicalize(llm_ir.places),
);
assert_eq!(
    canonicalize(agent_ir.transitions),
    canonicalize(llm_ir.transitions),
);
assert_eq!(canonicalize(agent_ir.arcs), canonicalize(llm_ir.arcs));
assert_eq!(agent_ir.definitions, llm_ir.definitions);
```

`canonicalize` strips node-id-derived names if they would differ (the Agent's node id vs the AutomatedStep's node id) by replacing them with a stable placeholder, then sorts. Place/transition kinds, arc bindings (consume/produce/read + multiplicity), guards, and Rhai logic must match exactly.

This test must be committed in the **first implementation PR**, alongside the `WorkflowNodeData::Agent` variant and a stub `lower_agent` that delegates to `lower_automated_step` when degenerate. The full agent-path lowering then lands incrementally without ever regressing the test.

## 8. New types (executor-domain)

To normalize across Anthropic / OpenAI / Ollama tool-use shapes, `executor-domain` grows:

```rust
pub struct LlmToolCall {
    pub id: String,              // provider-assigned, opaque
    pub name: String,
    pub arguments: serde_json::Value,
}

pub enum LlmStopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Refusal,
    Other(String),
}

pub struct LlmTurnResult {
    pub content: Option<String>,         // text response, if any
    pub tool_calls: Vec<LlmToolCall>,    // possibly empty
    pub stop_reason: LlmStopReason,
    pub usage: LlmUsage,                 // tokens in/out
}
```

Each adapter (`executor-llm/src/adapters/{anthropic,openai,ollama}.rs`) translates its provider response into `LlmTurnResult`. The service compiler and the `t_route` transition operate on this normalized shape only.

We mirror **OpenAI's** tool_call JSON shape because Anthropic has converged on roughly the same structure and it's the de facto interchange format.

## 9. Out of scope (Phase 2+)

- Parallel tool calls in a single turn (requires engine-side data-driven Join primitive).
- Token-level streaming to the UI.
- Cost-to-dollars projection (lookup table; raw tokens emitted from v1).
- Agent-specific live UI renderer (a `process-live/renderers/AgentConversation.svelte` showing turn-by-turn messages + tool calls inline).
- Built-in summarization-via-sub-LLM compaction strategy (v1: `drop_oldest` only; `summarize_oldest` Phase 2).
- "Reflection" / self-critique loops — those compose from the Agent + Loop + Decision primitives we already have, no new node needed.

## 10. Open questions for implementation

- Should `tool_name` live on the child node itself or on the edge from Agent → child? (Recommendation: on the child, as a new optional `tool_meta: Option<ToolMeta>` field. Edge metadata makes drag-and-drop UX worse and complicates copy-paste of tool subtrees.)
- How does the editor distinguish a "tool child" from any other child for layout purposes? (Recommendation: the `tool_meta.is_some()` flag is the only signal; no separate node kind.)
- Where does `max_agent_depth` live — per-template, per-tenant, global env var, or all three layered? (Recommendation: per-template with a config-default fallback. Tenant-level caps are a Phase 2 concern.)
- Should the Llm backend grow `tools: [...]` directly, or do we keep tool-schema construction at the executor `prepare` hook? (Recommendation: at the `prepare` hook — keeps the backend config wire format unchanged, lets adapter code own provider-specific tool serialization.)

These are settling-during-implementation calls, not architectural blockers.
