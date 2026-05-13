# ADR-09: Namespaced Subject Hierarchy for Multi-Tenant Federation

**Status:** Proposed
**Date:** 2026-02-02
**Related:** 08-adr-advisory-state-kv-projections.md, 06-cross-net-bridge.md

## Context

The system currently operates on a single global NATS JetStream (`PETRI_GLOBAL`) with a flat subject hierarchy (`petri.events.>`). This provides a total ordering of events, simplifying replay, debugging, and the construction of global read models.

However, as we move towards a "federated" architecture—where independent research groups, labs, or administrative domains share the same NATS fabric—we face security and isolation challenges:

1.  **Authorization:** We need to prevent "Research Group A" from inadvertently (or maliciously) publishing events to "Research Group B's" nets.
2.  **Noisy Neighbor Filtering:** A net in Group A shouldn't have to filter out millions of messages from Group B at the application level; this should happen efficiently at the network edge.
3.  **Ownership:** It is currently unclear purely from the subject `petri.events.token.created` which entity owns the data.

Splitting into multiple physical streams breaks the global ordering guarantee, making consistent replay across cross-net interactions (like bridges) nearly impossible. We must retain the Single Global Stream while introducing logical boundaries.

## Decision

We will restructure the NATS subject hierarchy to include a mandatory **Tenant ID** (or Namespace) and **Net ID** at the root level.

We will strictly adhere to the following segment structure:

`petri.{tenant_id}.{net_id}.{category}.[...suffix]`

### 1. Subject Hierarchy Definition

| Segment | Description | Example |
|---|---|---|
| `petri` | Root prefix | `petri` |
| `{tenant_id}` | Administrative boundary (Lab, Group, User) | `lab-alpha`, `user-123`, `platform` |
| `{net_id}` | Unique Net Identifier | `gpu-scheduler-01`, `workflow-abc` |
| `{category}` | Message type | `events`, `commands`, `signals`, `bridge` |
| `[...suffix]` | Specific details | `token.created`, `inject` |

### 2. Updated Subject Patterns

**Events (Authoritative State)**
*   Old: `petri.events.token.created`
*   New: `petri.{tenant}.{net}.events.token.created`

**Commands (Control Plane)**
*   Old: `petri.commands.inject.token`
*   New: `petri.{tenant}.{net}.commands.inject.token`

**Signals (External Input)**
*   Old: `petri.signal.{net}.{place}`
*   New: `petri.{tenant}.{net}.signal.{place}`

**Bridge (Cross-Net Transfer)**
*   Old: `petri.bridge.{target_net}.{target_place}`
*   New: `petri.{target_tenant}.{target_net}.bridge.{target_place}`

*Note: For bridge transfers, the subject must reflect the **destination** so the receiving net can subscribe to it.*

### 3. NATS 2.0 Authorization Mapping

This hierarchy enables standard NATS Authorization (Auth Callout or static configuration) to enforce boundaries without application code changes.

**Example Policy: Research Lab Alpha**
*   **User:** `User-Lab-Alpha`
*   **Permissions:**
    *   **Allow Pub:** `petri.lab-alpha.>` (Own nets)
    *   **Allow Pub:** `petri.*.*.bridge.>` (Sending tokens to ANY net - *open federation*)
        *   *Alternative (Restricted):* `petri.lab-beta.>.bridge.>` (Can only send to Lab Beta)
    *   **Allow Sub:** `petri.lab-alpha.>` (Own nets)
    *   **Allow Sub:** `petri.summary.>` (Global advisory state - see ADR-08)
    *   **Deny Sub:** `petri.lab-beta.>` (Cannot spy on Lab Beta's raw events)

## Architecture

### Global Stream, Logical Partitions

The underlying JetStream `PETRI_GLOBAL` remains configured to capture `petri.>`.

*   **Total Ordering:** Use cases like "Global Audit" or "Replay" subscribe to `petri.>` (admin only) or specific subsets `petri.lab-alpha.>` and see events in exact causal order.
*   **Edge Filtering:** When "Net X" in "Lab Alpha" subscribes to its commands, it subscribes to `petri.lab-alpha.net-x.commands.>`. The NATS server filters this efficiently; the application receives zero noise from other tenants.

### Tenant Resolution

*   **Net Registry:** When a net is instantiated, it must now be configured with a `tenant_id`.
*   **Discovery:** A "Net ID" alone is no longer globally unique for addressing; a "Net Address" is now `{tenant_id}/{net_id}`.
    *   *Mitigation:* Net IDs are UUIDs or unique strings. If we enforce global uniqueness of Net IDs, the `tenant_id` is purely for auth, and we can still resolve a Net ID to a Tenant via a lookup if needed, but explicit addressing is preferred.

## Consequences

### Positive

*   **Security:** Hardware-enforced isolation. A compromised API key for "Lab Alpha" physically cannot inject commands into "Lab Beta's" infrastructure.
*   **Observability:** Logs and streams are self-describing. `petri.lab-alpha...` immediately identifies ownership.
*   **Multi-Tenancy:** We can host multiple unrelated organizations on the same NATS backbone ("The Platform") without them seeing each other's data.
*   **Ordering:** Preserves the Single Global Stream guarantee.

### Negative

*   **Verbosity:** Subject names become longer.
*   **Refactoring:** Significant changes to `Subjects` struct, `NatsEventPublisher`, and `SignalListener` to carry and respect `tenant_id`.
*   **Breaking Change:** Incompatible with the flat v1 subject hierarchy. Requires a "stop-the-world" migration or a fresh deployment.

## Migration Strategy

Since the system is pre-1.0:
1.  Implement the change as a hard break.
2.  Update `EngineConfig` to require a `TENANT_ID` env var (defaulting to `default` or `local` for dev).
3.  Update all Listeners/Publishers to prepend this ID.
