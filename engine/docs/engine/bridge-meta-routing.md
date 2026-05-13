# BridgeMetadata Routing — Design Report

## Problem

After ADR-18 removed `traceparent` from `BridgeMetadata`, the `bridge_reply` routing mechanism broke silently. Tokens produced to `bridge_reply` places fell into the local marking instead of being forwarded to the reply address, stalling multi-net pipelines.

## Root cause

`BridgeMetadata` serves two distinct purposes through a single "first wins" resolution:

1. **Reply routing** — `reply_to` / `reply_channels` carry the return address for request-reply bridge patterns.
2. **Origin tracking** — `correlation_id` / `source_net_id` identify where a token came from.

These are conflated into one struct, and the binding logic (`binding.rs:197`) uses **"first consumed token with any `bridge_meta` wins"** to select the metadata for the entire transition. There is no priority, no merge — just whichever arc is iterated first.

### How it broke

Before ADR-18, `BridgeMetadata` was only created when a token carried reply routing context OR `traceparent`. Simple bridge transfers and signal tokens that had no reply addresses and no traceparent got `bridge_meta: None`.

After ADR-18:
- `build_bridge_meta()` was changed to always return `Some` (since correlation_id is always present)
- Signal listeners were changed to always stamp `BridgeMetadata` for causality tracking

This meant tokens from signals and simple bridges now carried `BridgeMetadata { correlation_id, source_net_id, reply_to: None, reply_channels: None }`. When consumed alongside a token that DID have `reply_channels`, the empty metadata won the "first wins" race (because of arc iteration order), and `bridge_reply` routing silently failed:

```
join_exec_result consumes:
  arc 0: exec_result_inbox  → token with bridge_meta { correlation_id, reply_channels: None }  ← WINS
  arc 1: pending_execution  → token with bridge_meta { correlation_id, reply_channels: {result: ...} }  ← SHADOWED

→ consumed_bridge_meta = { reply_channels: None }
→ bridge_reply "result" lookup returns None
→ token falls through to local marking instead of bridging out
```

### The fix

`BridgeMetadata` is now only created when there is actual reply routing context (`reply_to` or `reply_channels`). Tokens without reply addresses get `bridge_meta: None`, which the binding skips when searching for the first token with metadata.

## Fragility analysis

The current design has three structural weaknesses:

### 1. "First wins" is order-dependent

`consumed_bridge_meta` is set by the first non-read-arc token that has `bridge_meta`. The arc iteration order comes from the topology definition. If someone reorders input arcs in a net definition, reply routing can silently break. There is no warning when a token with reply_channels is shadowed.

### 2. BridgeMetadata conflates routing and identity

`correlation_id` and `source_net_id` are provenance fields (where did this token come from?). `reply_to` and `reply_channels` are routing fields (where should replies go?). Putting both in the same struct means you can't have provenance without accidentally participating in the routing race.

ADR-18's causality model resolves provenance from the event log, so `correlation_id` / `source_net_id` on tokens become unnecessary for provenance. They're only needed for reply routing now. This is why the fix works: tokens without reply addresses don't need `BridgeMetadata` at all.

### 3. Silent failure on bridge_reply

When `bridge_reply` routing fails (no matching reply address), the token silently falls through to the local marking with a `tracing::warn!`. The transition appears to fire successfully. The only symptom is tokens accumulating in the bridge_reply place. There is no error event, no guard failure, no retry.

## Possible improvements (future)

- **Merge instead of first-wins**: when multiple consumed tokens carry `bridge_meta`, merge `reply_channels` maps rather than taking the first one.
- **Separate provenance from routing**: if provenance is tracked via the event log (ADR-18), `BridgeMetadata` can be renamed to `ReplyRouting` and only contain `reply_to` / `reply_channels`. The `correlation_id` moves to a separate concern.
- **Fail loudly on bridge_reply miss**: if a transition produces to a `bridge_reply` place and there's no matching reply address, emit an `EffectFailed`-style error event instead of silently dropping the token into the marking. Or at minimum, don't add the token to the marking — discard it with a clear error.
