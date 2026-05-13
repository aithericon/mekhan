# ADR-07: Replace Resource Protocol with Bridged Subnets and Effects

**Status:** Accepted
**Date:** 2026-01-31
**Supersedes:** ARCHITECTURE.md (resource primitives), 05-claim-protocol.md, ADAPTER_GUIDE.md

## Context

The engine grew three overlapping mechanisms for integrating with external systems:

1. **Resource adapter protocol** — A rich vocabulary of place flags (`is_shared`, `is_export`, `claims_workflow`), ~15 NATS subject patterns, pool lifecycle tracking, state transition extraction, heartbeats, and capacity management. Adapters are peer processes that talk to the engine via NATS using custom message types.

2. **Cross-net bridges** — Two engine instances exchange tokens directly. One NATS subject pattern (`petri.bridge.{net_id}.{place}`). Declared on places via `bridge_out` / `bridge_reply`. Simple, composable.

3. **Effect transitions** — Side effects execute inside transitions. Handler returns output tokens. Full deterministic replay via stored results. Error routing via `_error` port. No NATS involvement — pure application-layer concept.

The resource protocol was designed for a world where external systems are long-lived peers that synchronize state bidirectionally with one shared net. This led to a growing surface area of special-cased place flags, custom NATS listeners, and protocol messages that duplicate what bridges and effects already provide more simply.

### What the resource protocol requires today

- 5 special place flags with interaction rules
- Pool state tracker (in-memory materialized view)
- State transition extractor (analyzes TransitionFired events for claim enter/leave)
- WorkflowResourceListener (token inject/remove/update commands)
- Claim protocol (5 message types, 2 listeners, request/grant/deny/invalidate/release)
- Lifecycle signal router (routes signals back to claiming workflow)
- Export command publisher (double-publish on token entry)
- Resource adapter SDK (handler trait, lifecycle publisher)
- ~15 NATS subject families

### What bridges + effects require

- `bridge_out` on places (already exists)
- `bridge_reply` on places (already exists)
- Effect handler trait (already exists)
- 1 NATS subject pattern for bridges
- 0 NATS subject patterns for effects

## Decision

**Remove the resource adapter protocol entirely.** Replace it with bridged subnets and effect transitions.

Every external system is modeled as its own Petri net (a "subnet"). Communication between the workflow net and adapter subnet happens exclusively through bridges and effects. No backwards compatibility layer.

### Core principle

An adapter is not a peer process with a custom protocol. An adapter **is a Petri net** — with its own places, transitions, tokens, and event log. It runs the same engine. It connects to workflow nets through bridges.

### What stays

- **`bridge_out`** — tokens leave this net, arrive at another
- **`bridge_reply`** — response half of request/reply bridges
- **Effect transitions** — side effects with replay, error routing via `_error`

### What goes

- `is_shared` place flag
- `is_export` place flag
- `claims_workflow` place flag
- Pool state tracker, pool lifecycle, heartbeats, capacity tracking
- State transition extractor
- WorkflowResourceListener
- Claim protocol messages (ClaimRequest, ClaimGranted, ClaimDenied, ClaimInvalidated, ClaimReleased)
- ClaimRequestListener, ClaimResponseListener, ClaimRequestHandler
- ClaimPublisher, SimpleClaimHandler, TokenInjectingClaimHandler
- Resource adapter SDK (resource-adapters/ directory)
- Export command publishing in NatsEventPublisher
- Resource subject routing (`petri.resources.*`, `petri.pools.*`, `petri.workflow.*.signal.*`)
- All NATS subjects except `petri.events.*`, `petri.bridge.*`, and `petri.effects.*`

## Architecture: Bridged Subnets

### Before: Adapter as peer process

```
 WORKFLOW NET                        ADAPTER (custom process)
 ┌──────────────┐                    ┌──────────────────────┐
 │ available    │◄── token inject ───│ watch external       │
 │ (is_shared)  │                    │ system, push tokens  │
 │              │                    │                      │
 │ request      │── export cmd ─────►│ receive command,     │
 │ (is_export)  │                    │ call external API    │
 │              │                    │                      │
 │ leased       │◄── signal ────────│ route lifecycle      │
 │ (claims_wf)  │                    │ events back          │
 └──────────────┘                    └──────────────────────┘
      15 NATS subjects, 5 place flags, custom protocol
```

### After: Adapter as subnet

```
 WORKFLOW NET                         ADAPTER NET
 ┌──────────────┐    bridge          ┌──────────────────────┐
 │              │                    │                      │
 │ request      │── bridge_out ─────►│ inbox               │
 │              │                    │   │                  │
 │              │                    │   ▼                  │
 │              │                    │ grant_transition     │
 │              │                    │ (guards, scripts)    │
 │              │                    │   │                  │
 │ claim_handle │◄── bridge_reply ──│ outbox (granted)     │
 │              │                    │                      │
 │              │                    │ workers/available    │
 │              │                    │ workers/claimed      │
 │              │                    │   (actual tokens)    │
 │              │                    │                      │
 │ invalidated  │◄── bridge ────────│ invalidation_outbox  │
 │              │                    │                      │
 └──────────────┘                    └──────────────────────┘
      1 NATS subject pattern, 2 place flags (bridge_out, bridge_reply)
```

Key insight: **the actual worker tokens never leave the adapter net**. The workflow receives a claim handle (a reference token) via bridge. The adapter net manages its own pool internally using normal Petri net transitions and guards.

### Claims as a bridged pattern

The claim-based resource coordination pattern maps directly to bridges:

| Old claim protocol | Bridged subnet equivalent |
|---|---|
| `ClaimRequest` message | Token bridged from workflow to adapter inbox |
| `ClaimGranted` message | Claim handle token bridged back via `bridge_reply` |
| `ClaimDenied` message | Denial token bridged back via `bridge_reply` |
| `ClaimInvalidated` message | Invalidation token bridged from adapter to workflow |
| `ClaimReleased` message | Release token bridged from workflow to adapter |
| `ClaimRequestHandler` trait | Transition in adapter net (with guard for availability) |
| `SimpleClaimHandler` | Standard Petri net: `available + request → claimed + granted` |
| Resource pool (in-memory HashMap) | Tokens in adapter net places |
| Heartbeats / pool state | Adapter net's own marking (queryable via API) |

The adapter's grant/deny logic becomes a normal transition with a guard:

```
# In adapter net:
# Grant transition fires when: inbox has request AND available has worker
grant_worker:
  inputs: [inbox/request, workers/available]
  guard: "available.capability matches request.requirements"
  outputs: [workers/claimed, outbox/granted]  # outbox is bridge_reply
  script: |
    #{
      claimed: #{ worker: available, claim_ref: request.workflow_id },
      granted: #{ handle_id: uuid(), resource_id: available.id, data: available }
    }
```

Denial is a separate transition that fires when no worker matches:

```
deny_request:
  inputs: [inbox/request]
  guard: "!any_available(request.requirements)"
  outputs: [outbox/denied]  # bridge_reply
```

Invalidation fires when external events remove a claimed worker:

```
invalidate_claim:
  inputs: [workers/claimed]
  guard: "claimed.worker.status == 'destroyed'"
  outputs: [invalidation_outbox/invalidated]  # bridge_out to workflow
```

All of this is standard Petri net modeling. No custom protocol. No special handlers. The adapter net is just a net.

### Effects for synchronous external calls

When the interaction is request/response (call an API, run a computation), use an effect transition instead of bridges:

```
# In workflow net:
call_api:
  effect_handler: "http_post"
  inputs: [request_data]
  outputs: [response, _error]

# The handler does the HTTP call. On success, tokens route to response.
# On failure, tokens route to _error place. Fully replayable.
```

Effects are strictly simpler than bridges for synchronous interactions because there is no second net to manage.

### Decision tree: Effect vs Bridge

```
Is the external system long-lived with its own state?
  YES → Model as adapter subnet, connect via bridges
        (worker pools, GPU clusters, database connections)
  NO  → Is the interaction request/response?
    YES → Use effect transition
          (HTTP calls, computations, SLURM submissions)
    NO  → Use bridge to a lightweight relay net
          (pub/sub, webhooks, message queues)
```

## Claims as an Explicit Pattern

The `claims_workflow` logic (where the engine implicitly tracked ownership) has been removed. Instead, ownership is managed explicitly using the **Claim Pattern**:

1. **Explicit Tokens**: Resources stay in the adapter net; the workflow net holds a `ClaimHandle` token that contains the resource ID and owner information.
2. **Standard Transitions**: Acquiring a claim is a standard bridge-out/bridge-reply interaction.
3. **Correlation**: Invalidation signals are routed back to the workflow via standard signal places, using the claim handle's ID for correlation in a transition guard.

This approach is more flexible, visible in the UI, and avoids special-case metadata in the engine core.

## Implementation plan

### Phase 1: Remove resource adapter protocol from engine

Delete from `core-engine/crates/nats/`:
- `pool_state_tracker.rs`
- `claim_request_listener.rs`
- `claim_response_listener.rs`
- `claim_publisher.rs`
- `workflow_resource_listener.rs`
- `state_transition_extractor.rs`
- `lifecycle_signal_router.rs` (if present)

Delete from `core-engine/crates/domain/`:
- `claim.rs` (ClaimHandle, ClaimRef, ClaimRequest, etc.)
- `pool_lifecycle.rs`

Delete from project root:
- `resource-adapters/` directory (SDK, mock adapter)

Clean up from `core-engine/crates/nats/src/subjects.rs`:
- Remove all `petri.pools.*` subjects
- Remove all `petri.claims.*` subjects
- Remove all `petri.resources.*` subjects
- Remove all `petri.workflow.*.signal.*` subjects
- Keep `petri.events.*` and `petri.bridge.*`

Clean up from `core-engine/crates/nats/src/publisher.rs`:
- Remove export command publishing
- Remove resource subject routing
- Remove state transition publishing
- Keep bridge-out publishing and standard event publishing

Clean up from `core-engine/crates/domain/src/place.rs`:
- Remove `is_shared` flag
- Remove `is_export` flag
- Remove `claims_workflow` flag
- Keep `bridge_out`, `bridge_reply`

Clean up from `core-engine/crates/application/src/firing.rs`:
- Remove any `is_export`-aware logic
- Keep bridge-out routing (already in `route_output_tokens`)

### Phase 2: Build example adapter subnet

Create a `scenarios/adapter_workers.json` that models a worker pool as a Petri net:
- `workers/available` — pool of available workers
- `workers/claimed` — workers currently claimed by workflows
- `inbox` — bridge inbound for claim requests
- `outbox` — bridge reply for grant/deny responses
- `invalidation_outbox` — bridge out for invalidation notifications
- Transitions: `grant_worker`, `deny_request`, `release_worker`, `invalidate_claim`

Create a matching `scenarios/workflow_with_claims.json`:
- `request` — bridge out to adapter inbox
- `claim_handle` — bridge reply receives granted handle
- `denied` — bridge reply receives denial
- `invalidated` — bridge inbound for invalidation
- `processing` — workflow-local state
- Transitions for the workflow logic

### Phase 3: Update documentation

- Replace `ARCHITECTURE.md` with simplified version (bridges + effects only)
- Replace `05-claim-protocol.md` with "Claims as Bridged Subnets" pattern guide
- Remove `ADAPTER_GUIDE.md`
- Update `06-cross-net-bridge.md` with adapter subnet examples

## Consequences

### Positive

- **Dramatic simplification.** ~15 NATS subject patterns → 2. Five place flags → 2. Multiple custom listeners → 0. The engine core shrinks significantly.
- **Adapters are Petri nets.** They get event sourcing, deterministic replay, the same tooling, and the same visualization for free. No separate adapter SDK to maintain.
- **Uniform composition model.** Everything is nets connected by bridges and effects. One mental model instead of three.
- **Claims are just tokens.** Grant/deny logic is a transition guard, not a custom handler trait. Visible in the net topology, not hidden in adapter code.
- **No protocol versioning.** Bridge messages are just token transfers. No custom message schemas to evolve.

### Negative

- **Breaking change.** All existing adapter code must be rewritten as Petri nets. No migration path.
- **Adapter nets need hosting.** Instead of a lightweight process with the adapter SDK, each adapter is an engine instance. This may feel heavyweight for simple adapters.
- **Pool observability moves.** Dashboard features (capacity, heartbeats) that were built into the engine become the adapter net's responsibility. They're still possible (query the adapter net's marking) but not built-in.

### Risks

- **Adapter net complexity.** Some adapters have complex logic (retry, circuit breaking, batching). This logic needs to be expressible as Petri net transitions. Effect handlers help — the complex logic lives in the handler, the net just orchestrates it.
- **Performance.** Two engine instances exchanging tokens via NATS adds latency compared to direct function calls. For most orchestration use cases this is acceptable. For tight loops, effect transitions (which are in-process) are the right choice.

## References

- [Cross-Net Bridge](../integration/cross-net-bridge.md) — Bridge specification (retained)
- [Execution Rules](../engine/execution-rules.md) — Transition firing rules (retained)
- [Core Concepts](../sdk/core-concepts.md) — Core Petri net concepts (retained)
