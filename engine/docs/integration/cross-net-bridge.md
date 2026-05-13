# Cross-Net Bridge

> First-class token transfer between independent Petri net instances via NATS JetStream.

## Overview

The **bridge-out** kind enables declarative cross-net token routing. When a transition produces a token into a place with `kind: bridge_out`, the token is **never added to local marking** — instead, a `TokenBridgedOut` domain event is emitted and the token is published to the remote net via NATS.

This is part of the **PlaceKind** system, alongside `internal`, `signal`, and `bridge_in`.

## Architecture

```
NET A (producer)                                          NET B (consumer)
═══════════════                                          ═══════════════

 ┌─────────┐     fire_transition()     ┌──────────────┐
 │ Source   │ ──── Produce ──────────► │ Outbox        │
 │ (token)  │                          │ bridge_out:   │
 └─────────┘                          │  net-b:inbox  │
                                       └──────┬───────┘
                                              │
                              place.bridge_out is Some
                              so token is DIVERTED:
                              NOT added to produced_tokens
                                              │
                                              ▼
                              ┌────────────────────────┐
                              │ events.append(          │
                              │   TransitionFired {     │
                              │     produced_tokens: [] │  ← empty (token excluded)
                              │   }                     │
                              │ )                       │
                              │                         │
                              │ events.append(          │
                              │   TokenBridgedOut {     │  ← auditable domain event
                              │     token, target_net,  │
                              │     target_place, ...   │
                              │   }                     │
                              │ )                       │
                              └────────┬───────────────┘
                                       │
                      NatsEventPublisher.append()
                      sees TokenBridgedOut, calls
                      try_publish_bridge_out()
                                       │
                                       ▼
                    ┌──────────────────────────────────────────────┐
                    │              NATS JetStream                   │
                    │  PETRI_GLOBAL stream                          │
                    │                                               │
                    │  subject: petri.bridge.net-b.inbox            │
                    │  payload: CrossNetTokenTransfer {             │
                    │    source_net_id: "net-a",                    │
                    │    source_place_name: "Outbox (bridge-out)",  │
                    │    token_color: { ... },                      │
                    │    correlation_id: "...",                      │
                    │  }                                            │
                    └────────────────────┬─────────────────────────┘
                                         │
                          CrossNetBridge inbound listener
                          filter: petri.bridge.net-b.>
                                         │
                                         ▼
                                  ┌─────────────┐    eval loop    ┌──────┐
                                  │ Inbox       │ ── Consume ───► │ Done │
                                  │ kind:       │                 │      │
                                  │  bridge_in  │                 └──────┘
                                  └─────────────┘
                                  create_token()
                                  + eval_notify
```

## NATS Subject Pattern

Bridge transfers use the subject pattern:

```
petri.bridge.{target_net_id}.{target_place_name}
```

This falls under the existing `petri.>` wildcard captured by the `PETRI_GLOBAL` stream. No additional streams are needed.

Each engine subscribes to `petri.bridge.{own_net_id}.>` to receive inbound tokens.

## Domain Event: TokenBridgedOut

Bridge transfers are first-class domain events — hash-chained, sequenced, and auditable like all other events.

```rust
DomainEvent::TokenBridgedOut {
    token: Token,                // The token being transferred
    source_place_id: PlaceId,    // Local place with bridge_out
    source_place_name: String,   // Human-readable name
    target_net_id: String,       // Remote net (e.g., "net-b")
    target_place_name: String,   // Remote place (e.g., "inbox")
    transition_id: TransitionId, // Transition that produced the token
    correlation_id: String,      // UUID for tracing the transfer
}
```

The state projection treats `TokenBridgedOut` as a no-op — the token never enters local marking, so there is nothing to project.

## Scenario Definition

Bridge targets are declared directly on places in the scenario JSON:

```json
{
  "places": [
    {
      "id": "outbox",
      "name": "Outbox",
      "type": "state",
      "bridge_out": {
        "target_net_id": "net-b",
        "target_place_name": "inbox"
      }
    }
  ]
}
```

The receiving net declares its inbox as a bridge_in place:

```json
{
  "places": [
    {
      "id": "inbox",
      "name": "Inbox",
      "type": "bridge_in"
    }
  ]
}
```

No environment variables or runtime configuration are needed for routing. The topology is the single source of truth.

## How It Works

### Outbound (producer net)

1. `fire_transition()` routes output tokens to places
2. If a place has `bridge_out` set, the token is **diverted** — it is not added to `produced_tokens`
3. A `TransitionFired` event is emitted with the bridge token excluded from `produced_tokens`
4. A `TokenBridgedOut` domain event is appended (hash-chained, auditable)
5. `NatsEventPublisher` detects the `TokenBridgedOut` event in `append()` and publishes a `CrossNetTokenTransfer` message to `petri.bridge.{target_net_id}.{target_place_name}`

### Inbound (consumer net)

1. `CrossNetBridge::run_inbound_listener()` subscribes to `petri.bridge.{own_net_id}.>`
2. On receiving a `CrossNetTokenTransfer`, it constructs a `PlaceId` directly from the target place name
3. Calls `service.create_token()` on the shared place
4. Notifies the evaluation loop to process the new token

## Place Property: bridge_out

```rust
pub struct Place {
    // ... existing fields ...

    /// If set, tokens produced here are forwarded to a remote net.
    /// The token is NOT added to local marking.
    pub bridge_out: Option<BridgeTarget>,
}

pub struct BridgeTarget {
    pub target_net_id: String,
    pub target_place_name: String,
}
```

Builder:

```rust
Place::state("Outbox")
    .with_bridge_out("net-b", "inbox")
```

## Bridge Rules

- **Diverted Tokens**: Bridge-out tokens are never part of the local marking. They disappear from the producer net after the `TokenBridgedOut` event is emitted.
- **Auditability**: The `TokenBridgedOut` event contains the full token data, providing a complete audit trail of what left the net.
- **Request-Reply**: Using `bridge_out` with a `reply_to` field automatically sets up a correlation ID for routing the reply back to the correct `bridge_reply` place.

## Running Cross-Net

Start two engine instances with different `NET_ID` values:

```bash
# Terminal 1: NATS server
nats-server -js

# Terminal 2: Net A (producer)
NET_ID=net-a PORT=3030 cargo run -p core-engine

# Terminal 3: Net B (consumer)
NET_ID=net-b PORT=3031 cargo run -p core-engine
```

Load scenarios:

```bash
# Load producer scenario (has bridge_out on outbox)
curl -X POST localhost:3030/api/scenario \
  -H 'Content-Type: application/json' \
  -d @scenarios/cross_net_a.json

# Load consumer scenario (has bridge_in on inbox)
curl -X POST localhost:3031/api/scenario \
  -H 'Content-Type: application/json' \
  -d @scenarios/cross_net_b.json

# Start both (validates bridge connections before activating)
aithericon activate cross-net-a
aithericon activate cross-net-b
```

Verify:

```bash
# Net A: outbox is empty (token was bridged, never entered marking)
curl localhost:3030/api/state | jq .

# Net B: token arrived in inbox, consumed into done
curl localhost:3031/api/state | jq .
```

## Edge Cases

| Case | Behavior |
|------|----------|
| NATS unavailable | Circuit breaker in publisher handles it. Event is still in local event store for audit. Token is lost from the remote net's perspective (same failure mode as any NATS publish). |
| Target place not found on remote | Inbound listener logs a warning and ACKs the message to avoid redelivery loops. |
| Multiple bridge-out places | Each emits its own `TokenBridgedOut` event. All are independently published. |
| Bridge token during `evaluate_until_quiescent` | `TokenBridgedOut` events are emitted inside `fire_transition()` before the next `select_next_transition` call. Local marking is consistent. |
| Replay / event sourcing | `TokenBridgedOut` is in the hash-chained event log. Replaying the log reconstructs the audit trail. The token itself is not in local marking (projection is a no-op). |

## Comparison with Previous Approach

| Aspect | Before (bolt-on) | After (first-class) |
|--------|-------------------|---------------------|
| Routing config | `BRIDGE_ROUTES` env var | Declarative `bridge_out` field in scenario JSON |
| When bridging happens | Eval loop scans `produced_tokens` post-hoc | `fire_transition()` diverts at output routing |
| Local marking | Token enters marking, then gets removed | Token never enters local marking |
| NATS publish | `forward_token()` does publish + `remove_token()` | Single `TokenBridgedOut` event; publisher handles NATS |
| Audit trail | Not in event log | `TokenBridgedOut` is a hash-chained domain event |
| Code path | Dual-write in eval loop + manual remove | Standard event append → decorator publish |

## Files

| File | Role |
|------|------|
| `domain/src/place.rs` | `BridgeTarget` struct, `bridge_out` field on `Place` |
| `domain/src/events.rs` | `TokenBridgedOut` domain event variant |
| `application/src/service.rs` | Bridge diversion in `fire_transition()` |
| `application/src/scenario_loader.rs` | `bridge_out` field on `ScenarioPlaceInput` |
| `api/src/dto.rs` | `BridgeTargetDto`, `bridge_out` on `ScenarioPlace` |
| `api/src/scenario_bridge.rs` | DTO-to-parser bridge_out passthrough |
| `nats/src/publisher.rs` | `try_publish_bridge_out()` in `NatsEventPublisher` |
| `nats/src/config.rs` | `net_id` field (from `NET_ID` env var) |
| `nats/src/cross_net_bridge.rs` | Inbound listener (unchanged) |
| `nats/src/subjects.rs` | `bridge_transfer()` subject builder |
