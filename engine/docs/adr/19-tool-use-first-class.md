# ADR 19: Tool use as a first-class LLM pipeline capability

**Status**: Accepted — sub-phase 2.5d-tools (2026-05-18)

**Context**: Prior to this ADR, LLM tool use in the clinic codebase was driven by a hand-rolled
loop in `server/src/services/pipeline/step_kinds/agent.rs` (lines 50–538). That loop was
clinic-specific, not part of mekhan, and was being deleted as part of the 2.5e cleanup wave.
The `patient_qa_v1` pipeline needed a replacement that (a) runs inside mekhan's executor-llm
path, (b) works with all supported LLM providers, and (c) follows the submitter-holds-loop
pattern so tool execution stays clinic-side with its tenant context and RLS.

---

## Decision

Tool use is a **first-class feature of mekhan's LLM executor**, not an opt-in extension or a
feature flag. Every Agent stage in every pipeline may declare `tool_names` in its config; the
executor's `run_agent_loop` handles the LLM ↔ tool conversation loop capped at `max_iterations`
(default: 16).

**Submitter-holds-loop pattern**: mekhan emits `tool_call` SSE events and parks the run;
the submitter (clinic) executes tools with its own tenant context and POSTs results back.
Mekhan never reaches into clinic's database or auth layer.

**MCP-compatible wire shape**: `ToolDefinition` uses `input_schema` as the field name,
matching MCP's `tools/list` response. Adapters normalize to provider dialects (`parameters`
for Ollama/OpenAI; `input_schema` natively for Anthropic) at the adapter boundary.

---

## Rig-loop divergences

The clinic's `agent.rs` rig-driven loop had the following behaviour. The new
`run_agent_loop` intentionally diverges in the following ways:

### 1. Parallel tool dispatch (divergence; semantically correct)

The rig loop dispatched tool calls within a single LLM turn **sequentially**. The new
`run_agent_loop` dispatches all calls in one turn **concurrently** via
`futures::future::join_all`. This is semantically correct: tool calls within one LLM turn
have no intra-turn ordering constraint (the LLM assembled them simultaneously). Parallel
dispatch reduces round-trip time when the LLM requests multiple tools in one response.

Clinic-side consequence: the MCP server must be re-entrant (it is; each call uses its own
Postgres connection pool checkout).

### 2. Tool-error propagation (same behaviour; documented for clarity)

Neither the rig loop nor `run_agent_loop` retries a failed tool call. An error is
propagated back to the LLM as a tool-result message. The LLM decides whether to retry by
calling differently in the next turn. This is the standard agentic pattern; retrying
transparently would hide failures from the LLM's reasoning.

### 3. max_iterations cap (new; rig had no cap)

The rig loop had no cap on tool-use iterations. A runaway tool loop could consume unbounded
tokens. `run_agent_loop` terminates with `LlmError::Api("max tool-iterations exceeded")`
after `max_iterations` turns. Default: 16 (configurable per-stage via scenario JSON).

**Rationale for 16**: typical multi-hop reasoning uses 2–4 turns. 16 provides a 4× safety
margin while bounding worst-case token spend. Values above 32 are discouraged without
explicit justification.

### 4. Structured-output mutual exclusion (constraint; not a divergence)

The rig loop could not combine JSON-schema structured output with tool calls; Anthropic's API
rejects this combination. `run_agent_loop` enforces `response_format == Text` for any request
with tools. Callers that need structured output must either (a) use a separate extraction
stage after the tool-use stage, or (b) use Anthropic's structured tool-use pattern (the
synthetic "extract" tool in the Anthropic adapter handles this for structured-output-only
invocations).

---

## Park/resume semantics

When the LLM emits one or more tool calls in a response:

1. `run_agent_loop` emits `SseEvent::ToolCall` for each call (before dispatching).
2. `tool_dispatcher.dispatch(call)` awaits a `oneshot::Receiver<ToolResultPayload>`.
3. The pool listener's `POST /v1/runs/{run_id}/tool_results` fulfills the oneshot.
4. Cloud-layer-workflow forwards the clinic's `POST /v1/pipelines/{run_id}/tool_results`
   to the pool listener's local endpoint.
5. After all oneshotsin a turn resolve, the loop appends tool-result messages and continues.

The lease extension (cap-routing `POST /v1/routes/extend/{token}`) is L's slice (cloud-layer-
workflow); mekhan's concern is only the local oneshot. The park occurs synchronously before
SSE emission, satisfying the wire-contracts §2 "park BEFORE tool_call emit" requirement.

---

## Why pool_listener carries the per-run oneshot map

`ToolResultsState` is injected into `spawn_pool_listener` as an explicit parameter. The agent
loop receives a `ToolDispatcher` impl that holds an `Arc<ToolResultsState>` reference.
This avoids a global mutex and keeps the oneshot map's lifetime tied to the listener's lifetime.

A `ToolResultsState::cleanup_run(run_id)` method allows the agent loop's caller to release
the map entry after the run completes, preventing memory accumulation on long-running pools.

---

## Per-provider adapter wiring

| Provider  | tools field     | call_id source              | args field        |
|-----------|-----------------|-----------------------------|-------------------|
| Ollama    | `tools[].function.parameters` | UUID generated by adapter | `function.arguments` (Value) |
| OpenAI    | `tools[].function.parameters` | `tool_calls[].id` (native) | `function.arguments` (JSON string → parsed) |
| Anthropic | `tools[].input_schema` (native) | `content[].id` (native) | `content[].input` (Value) |

Anthropic's structured-output mode uses a synthetic `"extract"` tool with `tool_choice: "any"`.
User-declared tools use `tool_choice: "auto"`. These two modes are mutually exclusive in a
single request; `run_agent_loop` always sets `response_format: Text` (the caller's
structured-output stage is separate from the tool-use stage).

---

## Consequences

- **Positive**: tool use works with Ollama (qwen3 family), OpenAI, and Anthropic without
  code changes to pipelines; scenarios declare `tool_names` and `max_tool_iterations` in config.
- **Positive**: parallel tool dispatch reduces latency for multi-tool turns.
- **Positive**: the oneshot map is explicit and bounded (cleanup_run removes entries).
- **Negative**: the `spawn_pool_listener` signature gained a `ToolResultsState` parameter;
  existing callers (executor_pool binary) must be updated. This is a one-time additive change.
- **Deferred**: `tool_invocations` persistence table (workstream #99); in-memory ring + tracing
  log is sufficient for 2.5d-tools. Tool-result streaming, cross-tenant sharing, and cancellation
  propagation are also deferred (see wire-contracts §8).
