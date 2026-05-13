# 14. Terminal Places & Net Completion Detection

**Date:** 2026-02-11
**Status:** Accepted
**Related:** [13-net-lifecycle.md](./13-net-lifecycle.md)

## Context

Petri nets in the engine have no built-in completion semantic. Transitions fire until quiescence (no more enabled transitions), but quiescence alone does not distinguish between "workflow finished successfully" and "workflow stuck waiting for input." External systems — dashboards, orchestrators, parent nets — need a declarative way to know when a net's work is done, and what the outcome was.

Existing `PlaceKind` variants (`Internal`, `Signal`, `BridgeIn`, `BridgeOut`, `BridgeReply`) model how places interact with the world outside the net, but none express "this place represents a final state." Without this, completion detection requires ad-hoc conventions (checking specific place names, polling token counts) that are fragile and not enforceable by the engine.

## Decision

Add `PlaceKind::Terminal` as a new place kind. A net is considered **complete** when evaluation reaches quiescent state AND at least one terminal place holds a token. The engine checks this condition at the end of each evaluation cycle and reports it via `EvaluateResult`.

### 1. Domain Model

`PlaceKind::Terminal` is a new variant in the `PlaceKind` enum (`petri-domain`). By convention, terminal places are sinks — no outgoing arcs. The first token to arrive at a terminal place marks the net as complete.

```rust
pub enum PlaceKind {
    Internal,
    Signal,
    BridgeIn { .. },
    BridgeOut { .. },
    BridgeReply,
    Terminal,  // ← new
}
```

Convenience constructor:

```rust
impl Place {
    pub fn terminal(name: impl Into<String>) -> Self { .. }
}
```

Helper on `PetriNet`:

```rust
impl PetriNet {
    /// Returns IDs of all places with `PlaceKind::Terminal`.
    pub fn terminal_places(&self) -> Vec<PlaceId> { .. }
}
```

### 2. Completion Detection

The evaluation engine checks for terminal completion at quiescence:

```rust
pub fn check_terminal_state(
    topology: &impl TopologyRepository,
    marking: &Marking,
) -> Option<TerminalReachedInfo>
```

Iterates all terminal places. If any holds a token, returns:

```rust
pub struct TerminalReachedInfo {
    pub place_id: String,
    pub exit_code: Option<serde_json::Value>,
}
```

The `exit_code` is extracted from the token's data if it has a `Data` color with an `exit_code` field. This is a convention, not enforced by schema.

### 3. Integration with Evaluation

`EvaluateResult` includes a `terminal_reached: Option<TerminalReachedInfo>` field. When evaluation reaches quiescence, `check_terminal_state()` is called and its result attached.

### 4. Eval Loop: NetCompleted Emission

The background eval loop in `NetRegistry` (`spawn_net_evaluation_loop`) acts on `terminal_reached` automatically:

1. If `result.terminal_reached` is `Some`, the eval loop **emits a `DomainEvent::NetCompleted`** event containing `net_id`, `terminal_place_id`, and `exit_code`.
2. The event is broadcast to SSE clients.
3. The eval loop **cancels the per-net `CancellationToken`**, stopping all per-net listeners (signal, bridge, etc.).
4. The eval loop **returns** — the net is done.

This makes terminal detection an end-to-end feature: a token arriving at a terminal place triggers completion detection, event emission, and resource cleanup without any external intervention.

### 5. SDK Integration

Scenario definitions use `Place::terminal("done")` to declare terminal places. The SDK compiles this to AIR format with `"kind": "terminal"`.

### 6. Multiple Terminal Places

A net may have multiple terminal places (e.g., `success` and `failure`). The engine reports whichever terminal place first receives a token. Different exit codes allow callers to distinguish success from failure:

```
[Input] → (Process) → [Success:Terminal]  // exit_code: 0
                    → [Failure:Terminal]  // exit_code: 1
```

## Consequences

### Positive

- **Declarative completion.** Nets declare their own completion criteria in the topology. No ad-hoc polling or naming conventions.
- **Enables lifecycle events.** Terminal detection is the trigger for `NetCompleted` events (see ADR-15), which in turn drive metadata projection and hibernation.
- **Composable.** Parent nets can bridge into a child's terminal place to detect sub-workflow completion.
- **Backward compatible.** Nets without terminal places behave exactly as before — `terminal_reached` is `None` at quiescence.

### Negative

- **Exit code convention.** The `exit_code` field is extracted by convention from token data, not enforced by schema. Tokens without this field simply report `exit_code: None`.
- **Single-winner semantics.** When multiple terminal places exist, only the first one with a token is reported. Concurrent token arrival at multiple terminals is resolved by iteration order (deterministic but arbitrary).
