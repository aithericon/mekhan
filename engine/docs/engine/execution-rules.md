# Execution Rules

This document describes how the Petri-Lab engine executes workflows, including firing rules, priority ordering, and adapter behavior.

## Engine Modes

The engine operates in two modes:

| Mode | Behavior | Use Case |
|------|----------|----------|
| `Paused` | Manual step execution | Debugging, demonstrations |
| `Running` | Auto-execute until quiescent | Production, simulation |

### Mode Transitions

```
Paused ──(set running)──> Running
Running ──(set paused)──> Paused
Running ──(no enabled transitions)──> Running (Quiescent)
```

---

## Firing Rules

A transition is **enabled** (can fire) when ALL of the following are true:

### 1. Input Token Availability

Every input port must have sufficient tokens from its connected place:

```
Arc weight ≤ Available tokens in place
```

For multiple arcs from the same place, tokens are consumed independently per arc.

### 2. Guard Satisfaction

If a guard is defined, it must evaluate to `true`:

```rhai
// Guard must return true
task.priority > 0 && worker.available
```

Guards are evaluated with concrete token bindings.

### 3. Output Capacity

All output places must have capacity for produced tokens:

```
Current tokens + Produced tokens ≤ Capacity (or unlimited)
```

### 4. Token Binding

For guards that reference multiple inputs, valid token **combinations** must exist:

```rust
// Guard: "waiting.id == signal.id"
// Requires matching waiting token and signal token
```

The engine tries all combinations to find valid bindings.

---

## Priority Ordering

When multiple transitions are enabled, the engine uses **specificity priority**:

### Specificity = Input Count

Transitions with **more inputs** fire first:

```
t_three_inputs (3 inputs) → fires before
t_two_inputs (2 inputs) → fires before
t_one_input (1 input)
```

### Rationale

More specific transitions should "win" over general ones:

```rust
// SLA timeout with check-in (2 inputs) should beat
// SLA timeout without check-in (1 input)
#[step("t_sla_with_checkin", "SLA + Checkin")]
#[guard("waiting.id == checkin.id")]
fn sla_with_checkin(waiting: Waiting, checkin: CheckinSignal) -> ... { }

#[step("t_sla_bare", "SLA Timeout")]
fn sla_bare(waiting: Waiting) -> ... { }
```

### Tie Breaking

Among transitions with equal input counts, selection is **non-deterministic**.

---

## Token Binding Algorithm

The engine uses **combinatorial binding** to match tokens to guards:

### Algorithm

```
1. For each transition T:
   a. Get all input places connected to T
   b. Get all tokens from each place
   c. Generate all combinations of (token₁, token₂, ..., tokenₙ)
   d. For each combination:
      - Bind tokens to port variables
      - Evaluate guard
      - If guard passes → T is enabled with this binding
   e. Collect all valid bindings
```

### Example

```
Place A: [token_a1, token_a2]
Place B: [token_b1, token_b2, token_b3]

Guard: "a.id == b.ref_id"

Combinations tested:
  (token_a1, token_b1) → guard evaluated
  (token_a1, token_b2) → guard evaluated
  (token_a1, token_b3) → guard evaluated
  (token_a2, token_b1) → guard evaluated
  (token_a2, token_b2) → guard evaluated
  (token_a2, token_b3) → guard evaluated

Only combinations where guard returns true are valid bindings.
```

### Performance Note

Combinatorial explosion is possible with many tokens. Keep places reasonably sized.

---

## Execution Cycle

### Single Step

```
1. Find all enabled transitions (with valid bindings)
2. Sort by priority (input count descending)
3. Select highest priority enabled transition
4. Execute:
   a. Remove input tokens from source places
   b. Execute transition logic (Rhai script)
   c. Add output tokens to target places
5. Emit events (TransitionFired, TokensRemoved, TokensAdded)
6. Notify adapters of new tokens
```

### Continuous Mode

```
while (running):
    step = execute_single_step()
    if step.fired:
        continue
    else:
        state = Quiescent
        wait_for_external_event()
```

### Quiescent State

The engine becomes **quiescent** when no transitions are enabled:
- No tokens available
- All guards fail
- Waiting for external signals

---

## Atomic Execution

Each transition firing is **atomic**:

### Guarantees

1. **All-or-nothing**: Either all inputs consumed and outputs produced, or nothing
2. **No interleaving**: One firing completes before next begins
3. **Consistent state**: Observers see only complete states

### Implications

- No partial token consumption
- No race conditions between transitions
- Safe to reason about step-by-step

---

## Adapter Behavior

Adapters simulate external systems and inject tokens asynchronously.

### Adapter Execution Flow

```
1. Token arrives at trigger place
2. Engine notifies adapter scheduler
3. Adapter waits (latency_ms)
4. If check_token_exists:
   - Verify token still in place
   - If consumed → skip (cancel adapter)
5. Execute adapter logic
6. Inject result token into target place
7. Engine processes new token (may fire transitions)
```

### Standard Adapters

For normal external service simulation:

```rust
ctx.mock_adapter(
    &pending_place,
    "Payment Gateway",
    2000,  // 2 second latency
    format!(r#"#{{ target_place: "{}", data: #{{ id: token.id, success: true }} }}"#, signal_place.id()),
);
```

### Timeout Adapters

For SLA monitoring and timeout patterns:

```rust
ctx.timeout_adapter(
    &waiting_place,
    "SLA Monitor",
    30000,  // 30 second timeout
    format!(r#"#{{ target_place: "{}", data: #{{ id: token.id }} }}"#, timeout_signal.id()),
);
```

Key difference: `check_token_exists: true`
- Only fires if token still exists after delay
- Enables "race" patterns where processing beats timeout

### Adapter Logic

Must return:

```rhai
#{
    target_place: "place_id",
    data: #{ field1: value1, ... }
}
```

---

## Guard Evaluation

### Variables Available

Guards can access input port variables:

```rust
#[step("t_match", "Match")]
#[guard("task.id == signal.task_id && task.priority > 0")]
fn match_task(task: Task, signal: Signal) -> ... { }
```

Variable names match **parameter names**, not types.

### Supported Operations

| Operation | Syntax | Example |
|-----------|--------|---------|
| Equality | `==`, `!=` | `a.id == b.ref` |
| Comparison | `<`, `>`, `<=`, `>=` | `x.value > 100` |
| Logical | `&&`, `||`, `!` | `a && !b` |
| Field access | `.` | `token.field.subfield` |
| String compare | `==` | `status == "active"` |
| Boolean literal | `true`, `false` | `enabled == true` |

### Guard Failures

If guard evaluation throws an error (missing field, type mismatch):
- Binding is considered invalid
- Transition not enabled for that binding
- No crash, continues to next binding

---

## Logic Execution

### Rhai Environment

Logic scripts run in a Rhai environment with:

| Feature | Description |
|---------|-------------|
| Input variables | All input port tokens bound as variables |
| `now()` | Current timestamp (milliseconds) |
| `random()` | Random float 0.0-1.0 |
| Standard operators | Math, comparison, logic |
| Map literals | `#{ key: value }` |
| Array literals | `[a, b, c]` |
| String operations | Concatenation with `+` |

### Return Value

Logic must return a map of output port names to token values:

```rhai
#{
    port1: #{ field: value },
    port2: #{ other: data }
}
```

### Conditional Outputs

Use conditionals for branching:

```rhai
if score >= 80 {
    #{ approved: #{ id: task.id } }
} else {
    #{ rejected: #{ id: task.id, reason: "Low score" } }
}
```

Only one branch's output is produced.

### Error Handling

If logic execution fails:
- Transition does not fire
- Input tokens remain in place
- Error logged
- Engine continues with other transitions

---

## Event System

The engine emits events for all state changes:

### Event Types

| Event | Trigger | Data |
|-------|---------|------|
| `ScenarioDeployed` | New scenario loaded | Scenario definition |
| `TokensAdded` | Tokens added to place | Place ID, tokens |
| `TokensRemoved` | Tokens removed from place | Place ID, tokens |
| `TransitionFired` | Transition executed | Transition ID, inputs, outputs |
| `ModeChanged` | Run mode changed | New mode |
| `AdapterFired` | Adapter injected token | Adapter name, token |

### Event Ordering

Events are emitted in execution order:
1. `TokensRemoved` (inputs consumed)
2. `TransitionFired` (logic executed)
3. `TokensAdded` (outputs produced)

---

## Concurrency Model

### Single-Threaded Execution

The engine uses single-threaded execution for transitions:
- No concurrent transition firings
- Deterministic state progression
- Simple reasoning about behavior

### Adapter Concurrency

Adapters run in background tasks:
- Multiple adapters can be waiting simultaneously
- Token injection synchronized with engine
- Event notifications batched

---

## State Persistence

### In-Memory State

- Token positions (place → tokens)
- Run mode (paused/running)
- Event history (limited buffer)
- Adapter schedules

### Not Persisted (Currently)

- No durable storage
- Restart loses state
- Scenarios must be redeployed

---

## Debugging Tips

### Paused Mode

Use paused mode for debugging:
1. Deploy scenario
2. Keep engine paused
3. Step manually via API
4. Inspect state after each step

### Event Log

Watch the event log for:
- Which transitions fired
- Token flow through places
- Guard failures (transitions not firing)

### Common Issues

| Symptom | Possible Cause |
|---------|----------------|
| Transition won't fire | Guard fails, check token values |
| Wrong transition fires | Priority issue, check input counts |
| Tokens stuck | No enabled transition, check downstream |
| Adapter not firing | Token consumed before timeout |

---

## API Endpoints

### Execution Control

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/run-mode` | GET | Get current mode |
| `/api/run-mode` | PUT | Set mode (paused/running) |
| `/api/step` | POST | Execute single step |
| `/api/auto-evaluate` | POST | Execute until quiescent |

### State Inspection

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/topology` | GET | Full scenario state |
| `/api/topology/place/{id}/tokens` | GET | Tokens in place |
| `/api/events` | GET | Event history |

### Token Injection

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/topology/place/{id}/tokens` | POST | Add token to place |

---

## Next Steps

- [Core Concepts](../sdk/core-concepts.md) - Foundational concepts
- [SDK Macros](../sdk/macros.md) - Define workflows in Rust
- [AIR Format](./air-format.md) - JSON specification
