# 12. Distributed Execution & Locking

**Date:** 2026-02-06
**Status:** Accepted

## Context

The engine currently operates as a single monolithic instance (or multiple isolated instances) where `NetRegistry` holds all active nets in memory. To scale to multiple hosts and handle high loads, we need:

1.  **Safe Concurrency:** Prevent two engine instances from running the same Net ID simultaneously, which would corrupt the event log logic (optimistic concurrency is insufficient if side-effects run twice).
2.  **Load Balancing:** Distribute incoming work (signals) across available worker nodes.
3.  **Affinity/Constraints:** Ensure certain nets run on specific hardware (e.g., nodes with GPU access, specific network zones).

## Decision

We adopt a **"Lock & Claim"** architecture using NATS as the coordination backbone.

### 1. Distributed Locking (NATS KV)
We will use a NATS Key-Value bucket `KV_NET_LOCKS` as a distributed lease manager.
*   **Key:** `{net_id}`
*   **Value:** JSON object containing `{ "node_id": "worker-1", "expires_at": <timestamp> }`.
*   **Mechanism:** Before hydrating or processing a net, a worker must acquire the lock using a CAS (Compare-And-Swap) operation.
*   **Lease:** Locks have a TTL. The active worker must periodically "heartbeat" (update the record) to retain the lock.

### 2. Consistency: Write-Ahead Log
To ensure the distributed system remains consistent, we strictly define NATS JetStream as the **Source of Truth**, not the local memory.
*   **Write Path:**
    1.  Engine calculates transition.
    2.  Engine publishes `Event` to NATS JetStream (Synchronous/Blocking wait for ACK).
    3.  **Only upon ACK**, the engine applies the event to its local `MemoryEventStore` (which acts as a Read-Through Cache).
*   **Failure:** If NATS rejects the write (or timeout), the engine rolls back the in-memory state (or simply crashes/restarts, as memory is just a cache).

### 3. Load Balancing (Queue Groups)
*   **Signal Listener:** All worker nodes subscribe to `petri.signal.>` using a NATS **Queue Group** (e.g., `petri-engine-workers`).
*   **Distribution:** NATS randomly delivers a signal to *one* available worker.
*   **Logic:**
    1.  Worker receives signal for `net-A`.
    2.  **Local Check:** Is `net-A` already running locally? -> Process it.
    3.  **Lock Check:** Is `net-A` locked by someone else? -> **NACK** (NATS redelivers to the owner or another node).
    4.  **Claim:** If unlocked, attempt to acquire lock -> Hydrate -> Process.

### 4. Constraints & Affinity
*   **Worker Tags:** Each engine instance starts with tags (e.g., `--tags=gpu,zone-payment`).
*   **Net Metadata:** Nets declare requirements (e.g., `requires: ["gpu"]`).
*   **Matching:** A worker will only attempt to claim a lock if it satisfies the net's requirements. If not, it NACKs the signal.

## Consequences

**Positive:**
*   **Horizontal Scalability:** We can add arbitrary worker nodes.
*   **High Availability:** If a node dies, its locks expire, and other nodes pick up the work.
*   **Hardware Efficiency:** Specialized workloads go to specialized nodes.

**Negative:**
*   **Latency:** "Cold start" (Lock + Hydrate) adds latency for the first signal after idle.
*   **Complexity:** Requires implementing the distributed lock manager and strict NACK handling.
