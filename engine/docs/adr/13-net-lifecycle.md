# 13. Net Lifecycle & Hibernation

**Date:** 2026-02-06
**Status:** Accepted

## Context

Many business processes (Petri Nets) are "bursty" or long-running with long periods of inactivity (e.g., waiting for a user approval or a scheduled timer).
*   **Problem:** Keeping 100,000 idle nets in memory (`NetRegistry`) wastes RAM and limits density.
*   **Goal:** Move to a "Serverless" resource model where memory is consumed only by *active* nets.

## Decision

We implement a **Wake-Run-Hibernate** lifecycle for all nets.

### 1. The Lifecycle

1.  **Cold (Hibernated):**
    *   State exists *only* in NATS JetStream (Event Log + Snapshots).
    *   No lock in `KV_NET_LOCKS`.
    *   No memory footprint on any worker.

2.  **Waking (Transitioning):**
    *   Trigger: A Signal arrives (`petri.signal.net-A...`) or a Timer fires.
    *   Action: A worker claims the lock and begins **Hydration** (loading events/snapshot).

3.  **Hot (Running):**
    *   Net is in `NetRegistry` on a specific worker.
    *   Worker holds the lock.
    *   Background evaluation loop is active.

4.  **Hibernating (Transitioning):**
    *   Trigger: The "Idle Garbage Collector" detects no activity for `X` minutes (configurable, e.g., 10m).
    *   Action:
        1.  Ensure all pending writes are ACK'd.
        2.  Stop background loop.
        3.  Delete lock from `KV_NET_LOCKS`.
        4.  Drop from `NetRegistry` (free memory).

### 2. Global Listeners vs. Per-Net Listeners
Previously, we spawned listeners *per net* (e.g., `petri.signal.net-A.>`). This is incompatible with hibernation (you can't have 100k listeners for 0 active nets).
*   **Change:** We move to **Global Listeners**.
*   **Signals:** A single consumer on `petri.signal.>` handles wake-up for ALL nets.
*   **Timers:** A single "Clockmaster" service (already existing) publishes wake-up events to NATS, which are treated as signals.

### 3. Safety
*   **Zombie Protection:** If a node decides to hibernate `net-A` exactly when a new signal arrives, the lock release must be atomic.
*   **Snapshots:** To make "Waking" fast enough to be invisible to users, **Snapshotting (ADR 11)** is a hard prerequisite for this lifecycle in production.

## Consequences

**Positive:**
*   **Infinite Capacity:** The system can "host" millions of nets, limited only by disk storage (NATS), not RAM.
*   **Cost Savings:** Workers can scale down to zero (or min replicas) during low-traffic periods.

**Negative:**
*   **Development Complexity:** Handling the "edge cases" of hibernation (race conditions during wake/sleep) is non-trivial.
*   **Latency:** The "First Byte" latency for an idle net includes hydration time.
