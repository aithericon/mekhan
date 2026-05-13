# 11. Scalability, Retention, and Provenance for Long-Running Nets

**Date:** 2026-02-06
**Status:** Accepted

## Context

We anticipate use cases involving "Long-Running Nets" (e.g., a `nomad-job-net`) that manage the lifecycle of thousands of jobs over extended periods (months or years).

The current architecture presents two critical scalability bottlenecks for this scale:

1.  **State Recovery (Hydration):** The engine reconstructs its state by replaying the entire NATS JetStream event log from sequence #0. As the history grows (millions of events), startup times degrade linearly, eventually becoming unacceptable.
2.  **Memory Usage (Token Accumulation):** Tokens in terminal places are currently retained in the in-memory `Marking` indefinitely unless explicitly consumed. For a net handling 100,000 jobs, retaining 100,000 "completed" tokens wastes RAM and degrades performance.
3.  **Provenance:** There is a requirement for long-term audit trails ("provenance"), but storing years of data in the active "Hot" event stream is inefficient and potentially costly.

## Decision

We adopt a **"Lean Engine"** philosophy where the core execution engine manages only the *active* state, offloading long-term retention and analytics to external systems.

### 1. State Snapshotting
To solve the startup/recovery bottleneck, we will implement **Snapshotting**.
*   **Mechanism:** The engine will periodically (e.g., every N events or T time) serialize its current state (the `Marking`) and the last processed `SequenceNumber`.
*   **Storage:** Snapshots will be stored in a dedicated NATS Key-Value bucket (`KV_SNAPSHOTS`) or object store.
*   **Hydration Flow:** On startup, the engine will:
    1.  Fetch the latest valid snapshot.
    2.  Initialize the in-memory state.
    3.  Replay only the events *after* the snapshot's sequence number from the NATS stream.

### 2. Token Lifecycle & "Sink Places"
To solve the memory bottleneck, we explicitly define that **completed work must be removed from memory**.
*   **Sink Places:** Nets should be designed with "Sink" logic. When a workflow completes, the final token should move to a transition that effectively "consumes" it without producing a new token (or produces it into a transient place that is immediately cleared).
*   **Data Loss vs. State:** Removing the token means the engine "forgets" the job exists. This is intentional. The record of the job persists in the *Event Log*, not in the *Active State*.

### 3. Externalized Provenance (Archiver Pattern)
To satisfy provenance requirements without bloating the engine:
*   **Hot Storage (NATS):** The NATS stream acts as the short-term buffer (e.g., 7-30 days retention) for operational replay and debugging.
*   **Cold Storage (Archiver):** A separate, dedicated "Archiver" process/service will subscribe to the NATS stream and persist all events to long-term storage (S3, Data Lake, etc.) for historical analysis and audit.
*   **Querying:** Analytics and provenance queries will run against the Cold Storage, not the active engine.

### 4. Handling "Zombie" Signals
Since the engine "forgets" completed jobs (by removing their tokens), late-arriving signals (e.g., a delayed callback for a finished job) will not find a matching token.
*   **Correlation:** Correlation remains the responsibility of the Net Topology (Guards checking `correlation_id`).
*   **Unmatched Signals:** If a signal arrives for a deleted token, it will spawn a "Signal Token" in the entry place. Since no matching "Job Token" exists, no transition will fire.
*   **Cleanup:** The Net Topology must include timeout/cleanup logic for these "orphan" signal tokens (e.g., a transition that consumes aged signal tokens) to prevent them from leaking memory.

## Consequences

**Positive:**
*   **Constant Startup Time:** Recovery time is bounded by the snapshot frequency, regardless of total history length.
*   **Bounded Memory:** RAM usage reflects only *active* concurrent work, not historical volume.
*   **Simplicity:** The core engine remains focused on execution logic, avoiding the complexity of embedded TSDBs or complex retention policies.

**Negative/Risks:**
*   **Implementation Effort:** Requires implementing the `SnapshotStore` trait, the snapshotting worker/logic, and the modified hydration strategy.
*   **Modeling Discipline:** Users must explicitly design their Petri Nets to "clean up" after themselves (Sink Places). Forgetting this will lead to memory leaks.
*   **External Dependency:** Provenance tracking now requires deploying and managing the external "Archiver" component.
